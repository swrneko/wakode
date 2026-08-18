mod config;
mod startup;

use std::process::ExitCode;

use config::Config;

/// `#[tokio::main]`, а не свой `Runtime`: семантика та же, а форма — та,
/// в которую задача 14 добавит разбор подкоманд, не переписывая всё.
#[tokio::main]
async fn main() -> ExitCode {
    // Без подписчика события `tracing` никуда не идут: макросы
    // отрабатывают вхолостую.
    //
    // Список — белый: крейт, который начнёт писать под своей целью,
    // придётся добавить сюда, иначе он будет молчать. `RUST_LOG`
    // перекрывает всё целиком.
    //
    // Что за целями стоит на самом деле, по состоянию на задачу 13:
    // `wakode_api` (собственные события и span запроса) и `tower_http`
    // (записи о завершении запроса, поднятые до `INFO` в `with_layers`).
    // `wakode` — задел: сам бинарь пока пишет только через `println!`
    // и `eprintln!`, ни одного вызова `tracing::` в нём нет. Директива
    // оставлена, чтобы подкоманды задачи 14 не молчали по недосмотру;
    // `wakode_store` и `wakode_auth` не пишут вообще ничего, поэтому их
    // здесь и нет.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "wakode=info,wakode_api=info,tower_http=info".into()),
        )
        .init();

    let config = match Config::load(None) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("wakode: {err}");
            return ExitCode::FAILURE;
        }
    };

    let master_key_raw = std::env::var("WAKODE_MASTER_KEY").ok();

    match startup::start(config, master_key_raw).await {
        Ok(started) => {
            // Каркас HTTP-слоя уже есть (`wakode-api`, задача 9), но
            // поднимает его подкоманда `serve` — а разбора подкоманд ещё
            // нет, это задача 14. До тех пор `main` доводит старт и выходит.
            //
            // Сказано ровно то, что произошло: старт пройден. Написать
            // «слушаю <адрес>» было бы утверждением о состоянии, которого
            // нет, — и владелец, выкативший такую сборку под systemd,
            // пошёл бы искать причину в брандмауэре и в плагине редактора,
            // но не в том, что сервер не поднимался. Задача 7 существует
            // ради устранения ровно такой тихой лжи о состоянии; отдавать
            // её из собственного `main` было бы смешно.
            println!(
                "wakode: старт пройден, HTTP-слой ещё не поднят \
                 (адрес из конфигурации {}, мастер-ключ {})",
                started.config.server.listen,
                if started.master_key.is_some() {
                    "задан"
                } else {
                    "не задан"
                }
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("wakode: {err}");
            ExitCode::FAILURE
        }
    }
}
