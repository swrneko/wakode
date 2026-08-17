use std::sync::Arc;

use chrono_tz::Tz;
use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::error::{StoreError, StoreResult};
use crate::heartbeats::{insert_heartbeats, IncomingHeartbeat, InsertReport};
use crate::interner::Interner;

/// Заявка писателю.
enum WriteJob {
    Insert {
        user: Uuid,
        batch: Vec<IncomingHeartbeat>,
        tz: Tz,
        reply: oneshot::Sender<StoreResult<InsertReport>>,
    },
    /// Сигнал остановиться. Писатель отвечает и выходит из цикла, уничтожая
    /// приёмник, — после этого все отправители получают `WriterGone`.
    Stop {
        ack: oneshot::Sender<()>,
    },
}

/// Ручка к пишущей задаче. Клонируется свободно, все копии шлют в один канал.
///
/// `Debug` выводится, хотя `WriteJob` его не реализует: `mpsc::Sender<T>`
/// реализует `Debug` без требования `T: Debug`.
#[derive(Debug, Clone)]
pub struct WriteHandle {
    tx: mpsc::Sender<WriteJob>,
}

/// Поднять пишущую задачу. Соединение переезжает к ней насовсем.
///
/// «Единственная» она про поток отметок, а не про базу целиком: редкие
/// одиночные записи — пользователи, ключи, сессии — идут своими
/// соединениями мимо этой очереди, чтобы логин не ждал за чужим батчем.
/// Разводит их не очередь, а сам SQLite: WAL плюс `busy_timeout` из
/// `conn.rs`.
///
/// `capacity` обязана быть больше нуля: канал ёмкости `0` роняет tokio
/// паникой изнутри `mpsc::channel`, и сообщение об этом будет про
/// внутренности tokio, а не про вызывающий код — на практике `capacity`
/// приходит из конфига HTTP-слоя, и `0` там — правдоподобная опечатка.
///
/// Паника внутри `insert_heartbeats` (например, `ids[cursor]` в
/// `heartbeats.rs` или один из `.expect("словарь отравлен паникой")` в
/// `interner.rs`) писателя **не убивает**: тело вставки идёт под
/// `catch_unwind`, отправитель получает `StoreError::TaskPanicked`, и
/// следующая заявка обрабатывается как ни в чём не бывало. До этого
/// паника уносила поток молча, а единственным сигналом наружу был
/// `WriterGone`, неотличимый от штатной остановки.
///
/// Оговорка: отравленный `RwLock` словаря `catch_unwind` не лечит — все
/// последующие заявки будут паниковать снова и снова возвращать
/// `TaskPanicked`. Это деградация, но не молчаливая, и в отличие от
/// прежнего поведения она видна вызывающему на каждой заявке.
pub fn spawn_writer(mut conn: Connection, interner: Arc<Interner>, capacity: usize) -> WriteHandle {
    let (tx, mut rx) = mpsc::channel::<WriteJob>(capacity);

    // Отдельный поток, а не задача tokio: работа тут блокирующая, и держать
    // её на исполнителе асинхронных задач нельзя.
    std::thread::spawn(move || {
        let mut stop_ack: Option<oneshot::Sender<()>> = None;

        while let Some(job) = rx.blocking_recv() {
            match job {
                WriteJob::Stop { ack } => {
                    // Подтверждение уходит не здесь, а после выхода из
                    // цикла — когда соединение уже отпущено. Ответить
                    // раньше значило бы сказать «остановился», продолжая
                    // держать базу: вызывающий, переоткрывающий файл сразу
                    // после `shutdown`, попал бы на живое соединение.
                    stop_ack = Some(ack);
                    break;
                }
                WriteJob::Insert { user, batch, tz, reply } => {
                    // Ответ уходит только после того, как транзакция
                    // закоммичена внутри insert_heartbeats. Отправитель мог
                    // уйти — это не наша беда, запись уже состоялась.
                    //
                    // Паника внутри вставки не должна убивать писателя
                    // навсегда: до этого она уносила поток молча, и
                    // единственным сигналом наружу был `WriterGone`,
                    // неотличимый от штатной остановки.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        insert_heartbeats(&mut conn, &interner, user, &batch, tz)
                    }))
                    .unwrap_or(Err(StoreError::TaskPanicked));

                    let _ = reply.send(result);
                }
            }
        }

        // Соединение закрывается до подтверждения: получивший `ack` вправе
        // считать, что база отпущена и файл можно переоткрывать.
        drop(conn);
        if let Some(ack) = stop_ack {
            let _ = ack.send(());
        }
    });

    WriteHandle { tx }
}

impl WriteHandle {
    /// Записать батч. Ждёт подтверждения коммита.
    pub async fn insert_heartbeats(
        &self,
        user: Uuid,
        batch: Vec<IncomingHeartbeat>,
        tz: Tz,
    ) -> StoreResult<InsertReport> {
        let (reply, wait) = oneshot::channel();
        let job = WriteJob::Insert { user, batch, tz, reply };

        // try_send, а не send: ждать места в очереди значило бы копить
        // запросы в памяти. Отказ здесь превращается в 503 с Retry-After,
        // и cli дошлёт отметки из собственной очереди.
        self.tx.try_send(job).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => StoreError::WriteQueueFull,
            mpsc::error::TrySendError::Closed(_) => StoreError::WriterGone,
        })?;

        wait.await.map_err(|_| StoreError::WriterGone)?
    }

    /// Остановить писателя, дождавшись, пока он разберёт принятое.
    ///
    /// Повторный вызов не ошибка: останов зовут и при штатном завершении,
    /// и из обработчика сигнала, и эти пути пересекаются.
    pub async fn shutdown(&self) -> StoreResult<()> {
        let (ack, wait) = oneshot::channel();
        match self.tx.send(WriteJob::Stop { ack }).await {
            Ok(()) => {
                let _ = wait.await;
                Ok(())
            }
            // Приёмник уже уничтожен — писатель остановлен, цель достигнута.
            Err(_) => Ok(()),
        }
    }
}
