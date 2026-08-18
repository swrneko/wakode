//! Штатное завершение процесса: чего ждём и сколько.

use std::future::Future;
use std::time::Duration;

/// Сколько ждать, пока сервер сам дочитает начатые запросы.
///
/// Меньше, чем `TimeoutStopSec` systemd по умолчанию (90 секунд), и это
/// главное свойство числа: срабатывать обязан наш предел, а не чужой.
/// Чужой присылает SIGKILL, а SIGKILL не оставляет шанса остановить
/// писателя — то есть теряет ровно то, ради чего завершение по сигналу и
/// заведено.
pub const GRACE: Duration = Duration::from_secs(10);

/// Дождаться сигнала завершения. Возвращает имя пришедшего — для журнала.
///
/// SIGTERM шлют systemd и `docker stop`; SIGINT — Ctrl-C в терминале.
/// Ждём оба: инстанс запускают и так, и так.
///
/// Отказ установки обработчика не прерывает работу: сервер нужен
/// владельцу больше, чем красивое завершение, и оставшийся сигнал
/// продолжает работать. Оба сразу отказать не могут по-тихому — тогда
/// функция просто никогда не вернётся, а процесс останется таким же
/// убиваемым, как до этого плана.
pub async fn wait_for_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut terminate = signal(SignalKind::terminate()).ok();
        let mut interrupt = signal(SignalKind::interrupt()).ok();

        let on_terminate = async {
            match terminate.as_mut() {
                Some(stream) => {
                    stream.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        let on_interrupt = async {
            match interrupt.as_mut() {
                Some(stream) => {
                    stream.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            () = on_terminate => "SIGTERM",
            () = on_interrupt => "SIGINT",
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "Ctrl-C"
    }
}

/// Дождаться конца работы сервера, но не дольше `grace` после сигнала.
///
/// Возвращает `true`, если сервер закрыл начатые соединения сам.
///
/// Отсчёт начинается **от сигнала**, а не от вызова: до сигнала сервер
/// работает штатно сколько угодно долго, и предел, потёкший раньше,
/// убивал бы живой инстанс. Это не теоретическое замечание — ровно так
/// написалась бы функция с одним `select!` на три ветки.
pub async fn wait_for_drain(
    served: impl Future<Output = ()>,
    signalled: impl Future<Output = ()>,
    grace: Duration,
) -> bool {
    tokio::pin!(served);

    tokio::select! {
        () = &mut served => return true,
        () = signalled => {}
    }

    tokio::select! {
        () = &mut served => true,
        () = tokio::time::sleep(grace) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_server_that_stops_on_its_own_is_reported_as_drained() {
        let drained = wait_for_drain(
            std::future::ready(()),
            std::future::pending(),
            Duration::from_millis(50),
        )
        .await;
        assert!(drained);
    }

    #[tokio::test]
    async fn a_server_that_hangs_past_the_grace_is_abandoned() {
        // `false` здесь — это «начатые запросы брошены». Хуже, чем
        // дождаться, но лучше, чем SIGKILL от systemd: тот не оставляет
        // шанса остановить писателя.
        let drained = wait_for_drain(
            std::future::pending(),
            std::future::ready(()),
            Duration::from_millis(50),
        )
        .await;
        assert!(!drained);
    }

    #[tokio::test]
    async fn the_grace_starts_at_the_signal_not_at_the_start() {
        // Предел, отсчитываемый от запуска, убивал бы работающий сервер
        // через десять секунд после старта. Сигнала здесь нет вовсе, а
        // сервер работает вчетверо дольше предела — и это штатная работа,
        // а не превышение.
        let served = async {
            tokio::time::sleep(Duration::from_millis(120)).await;
        };
        let drained = wait_for_drain(served, std::future::pending(), Duration::from_millis(20)).await;
        assert!(drained, "предел потёк до сигнала");
    }
}
