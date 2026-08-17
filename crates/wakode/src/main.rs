mod config;
mod startup;

use std::process::ExitCode;

use config::Config;

fn main() -> ExitCode {
    let config = match Config::load(None) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("wakode: {err}");
            return ExitCode::FAILURE;
        }
    };

    let master_key_raw = std::env::var("WAKODE_MASTER_KEY").ok();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("wakode: не удалось создать среду выполнения: {err}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(startup::start(config, master_key_raw)) {
        Ok(started) => {
            // Подъём HTTP-слоя на `started.store` и `started.master_key` —
            // задача 9: там ещё не существует `wakode-api`. Пока старт
            // считается пройденным, если дошёл сюда без ошибки.
            println!(
                "wakode: старт пройден, слушаю {} (мастер-ключ {})",
                started.config.server.listen,
                if started.master_key.is_some() { "задан" } else { "не задан" }
            );
            let _ = started.store;
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("wakode: {err}");
            ExitCode::FAILURE
        }
    }
}
