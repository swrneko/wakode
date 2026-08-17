mod config;
mod startup;

use std::process::ExitCode;

use config::Config;

/// `#[tokio::main]`, а не свой `Runtime`: семантика та же, а форма — та,
/// в которую задача 14 добавит разбор подкоманд, не переписывая всё.
#[tokio::main]
async fn main() -> ExitCode {
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
            // Подъём HTTP-слоя — задача 9, `wakode-api` ещё не существует.
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
