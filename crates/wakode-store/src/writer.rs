use std::sync::Arc;

use chrono_tz::Tz;
use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::error::{StoreError, StoreResult};
use crate::heartbeats::{insert_heartbeats, IncomingHeartbeat, InsertReport};
use crate::interner::Interner;

/// Заявка на запись и канал для ответа.
struct WriteJob {
    user: Uuid,
    batch: Vec<IncomingHeartbeat>,
    tz: Tz,
    reply: oneshot::Sender<StoreResult<InsertReport>>,
}

/// Ручка к пишущей задаче. Клонируется свободно, все копии шлют в один канал.
///
/// `Debug` выводится, хотя `WriteJob` его не реализует: `mpsc::Sender<T>`
/// реализует `Debug` без требования `T: Debug`.
#[derive(Debug, Clone)]
pub struct WriteHandle {
    tx: mpsc::Sender<WriteJob>,
}

/// Поднять пишущую задачу. Соединение переезжает к ней насовсем: больше
/// никто в базу не пишет.
pub fn spawn_writer(mut conn: Connection, interner: Arc<Interner>, capacity: usize) -> WriteHandle {
    let (tx, mut rx) = mpsc::channel::<WriteJob>(capacity);

    // Отдельный поток, а не задача tokio: работа тут блокирующая, и держать
    // её на исполнителе асинхронных задач нельзя.
    std::thread::spawn(move || {
        while let Some(job) = rx.blocking_recv() {
            let result = insert_heartbeats(&mut conn, &interner, job.user, &job.batch, job.tz);
            // Ответ уходит только после того, как транзакция закоммичена
            // внутри insert_heartbeats. Отправитель мог уйти — это не наша
            // беда, запись уже состоялась.
            let _ = job.reply.send(result);
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
        let job = WriteJob { user, batch, tz, reply };

        // try_send, а не send: ждать места в очереди значило бы копить
        // запросы в памяти. Отказ здесь превращается в 503 с Retry-After,
        // и cli дошлёт отметки из собственной очереди.
        self.tx.try_send(job).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => StoreError::WriteQueueFull,
            mpsc::error::TrySendError::Closed(_) => StoreError::WriterGone,
        })?;

        wait.await.map_err(|_| StoreError::WriterGone)?
    }
}
