mod cli;
mod config;
mod startup;

use std::process::ExitCode;

use anyhow::Context as _;
use clap::Parser;
use wakode_api::{AppSettings, AppState};

use crate::cli::{Cli, Command, KeyCommand, MasterKeyCommand, UserCommand};
use crate::config::Config;

/// `#[tokio::main]`, а не свой `Runtime`: семантика та же, а форма короче.
#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // `{err:#}`, а не `{err}`: anyhow в альтернативной форме
            // печатает всю цепочку причин через двоеточие. Без неё
            // владелец увидел бы «хранилище» вместо «хранилище: база
            // заблокирована другим процессом» — то есть ровно ту часть,
            // которая говорит, что чинить.
            //
            // Ненулевой код возврата и причина в stderr — то, на что
            // смотрит systemd и на что опирается `set -e`.
            eprintln!("wakode: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Подписчик `tracing`.
///
/// Без него события никуда не идут: макросы отрабатывают вхолостую.
///
/// Список — белый: крейт, который начнёт писать под своей целью, придётся
/// добавить сюда, иначе он будет молчать. `RUST_LOG` перекрывает всё
/// целиком. За целями стоят: `wakode` (этот файл), `wakode_api`
/// (собственные события и span запроса) и `tower_http` (записи о
/// завершении запроса, поднятые до `INFO` в `with_layers`). `wakode_store`
/// и `wakode_auth` не пишут вообще ничего, поэтому их здесь и нет.
///
/// **Журнал уходит в stderr, а не в stdout.** Stdout подкоманд — это
/// данные: значение выданного ключа, список пользователей,
/// идентификатор заведённого. Строка журнала, попавшая в тот же поток,
/// уехала бы в `wakode user list | …` наравне с данными.
fn init_tracing() {
    // Раскраска — только когда stderr действительно терминал. В журнале
    // systemd или в файле escape-последовательности превращаются в мусор
    // вокруг каждого значения, и `grep schema=1` по такому логу ничего не
    // находит: между именем поля и значением стоят коды.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stderr());

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(ansi)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "wakode=info,wakode_api=info,tower_http=info".into()),
        )
        .init();
}

async fn run() -> anyhow::Result<()> {
    // `parse`, а не `try_parse`: на кривых аргументах clap сам печатает
    // подсказку и выходит с ненулевым кодом, и заворачивать это в свою
    // ошибку значило бы потерять подсказку.
    let args = Cli::parse();

    // Единственная ветка, которой не нужны ни конфиг, ни база: мастер-ключ
    // генерируют до того, как что-либо существует. Матчится вся ветка
    // `MasterKey(_)`, а не одна `Generate`, — тогда `unreachable!` ниже
    // остаётся недостижимым и после добавления новых операций с ключом.
    if let Some(Command::MasterKey(command)) = &args.command {
        return master_key(command);
    }

    let config = Config::load(args.config.as_deref())?;

    // Пути печатаются абсолютными. Относительный путь в журнале ничего не
    // говорит: рабочий каталог под systemd задаёт unit, а не тот, кто
    // читает лог, и `database="./wakode.db"` отсылает владельца искать
    // файл там, где его нет. Отказ конфигурации это уже делал (задача 6),
    // успешный путь — нет.
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from(config::DEFAULT_CONFIG_PATH));
    tracing::info!(
        config = %shown(&config_path),
        database = %shown(&config.database.path),
        "конфигурация прочитана"
    );

    let started = startup::start(config, std::env::var("WAKODE_MASTER_KEY").ok()).await?;

    // Версия схемы — то, чего в журнале не хватало больше всего: миграции
    // применяет `SqliteStore::open` молча, и владелец, обновивший сборку,
    // видел «сервер поднят», не видя, применилось ли что-нибудь.
    tracing::info!(
        schema = started.store.schema_version().await?,
        "база открыта, миграции применены"
    );

    // Результат придерживается до останова писателя: `?` прямо здесь
    // унёс бы управление мимо `shutdown`.
    //
    // Честно про цену этой конструкции сегодня: она почти ничего не
    // спасает. Подкоманды идут своими соединениями мимо очереди писателя
    // (`create_user`, `create_key`) и коммитятся до возврата, а `serve`
    // возвращается только на io-ошибке — штатного завершения по сигналу
    // ещё нет, и SIGTERM убивает процесс, не дав `shutdown` отработать.
    // Смысл появится вместе с эндпоинтом приёма отметок: через очередь
    // пойдёт поток записей, и вот тогда потеря принятого станет
    // настоящей. Ставится заранее, потому что дописать останов задним
    // числом к готовому `serve` — это вспомнить о нём, а вспоминают не
    // всегда.
    let outcome = dispatch(&started, args.command.unwrap_or(Command::Serve)).await;

    if let Err(err) = started.store.shutdown().await {
        // Отказ останова не подменяет собой отказ подкоманды: подменив,
        // мы сообщили бы про писателя вместо того, что владелец просил
        // сделать. Отдельной строкой в журнал — и всё.
        tracing::warn!(error = %err, "останов писателя завершился с ошибкой");
    }

    outcome
}

/// Путь для журнала: абсолютный, если это выразимо.
///
/// `std::path::absolute` не ходит в файловую систему и не разрешает
/// символические ссылки — она отвечает на вопрос «относительно чего»,
/// а именно его владелец и задаёт себе, читая лог. Отказ (пустой путь,
/// недоступный рабочий каталог) оставляет путь как есть: сказать
/// «./wakode.db» лучше, чем не сказать ничего.
fn shown(path: &std::path::Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn master_key(command: &MasterKeyCommand) -> anyhow::Result<()> {
    match command {
        MasterKeyCommand::Generate => {
            println!("{}", wakode_auth::MasterKey::generate().to_base64());
            Ok(())
        }
    }
}

async fn dispatch(started: &startup::Startup, command: Command) -> anyhow::Result<()> {
    match command {
        Command::MasterKey(_) => unreachable!("обработано в `run` до чтения конфига"),
        Command::Migrate => {
            // Миграции уже применил `start` — `SqliteStore::open` делает и
            // открытие, и миграцию. Сюда мы попадаем, только если они
            // прошли, поэтому остаётся сообщить об этом и выйти.
            println!("миграции применены");
            Ok(())
        }
        Command::User(UserCommand::Create {
            login,
            admin,
            timezone,
        }) => {
            cli::user::create(
                &started.store,
                login,
                admin,
                timezone,
                started.config.durations.timeout_secs,
            )
            .await
        }
        Command::User(UserCommand::List) => cli::user::list(&started.store).await,
        Command::Key(KeyCommand::Issue { user, name }) => {
            cli::key::issue(&started.store, started.master_key.as_ref(), user, name).await
        }
        Command::Key(KeyCommand::Revoke { id }) => cli::key::revoke(&started.store, id).await,
        Command::Backup { to } => cli::backup::backup(&started.store, &to).await,
        Command::Serve => serve(started).await,
    }
}

/// Шаг 6 старта: поднять HTTP-слой.
async fn serve(started: &startup::Startup) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(&started.config.server.listen)
        .await
        .with_context(|| format!("не удалось занять адрес {}", started.config.server.listen))?;

    // Пишется после `bind`, а не до: до него это было бы обещанием, а не
    // фактом, — ровно та тихая ложь о состоянии, из-за которой владелец
    // идёт искать причину в брандмауэре.
    tracing::info!(listen = %started.config.server.listen, "сервер поднят");

    let state = AppState::new(
        started.store.clone(),
        started.master_key.clone(),
        app_settings(&started.config),
    );

    wakode_api::serve(listener, state).await?;
    Ok(())
}

/// Что из конфигурации видит HTTP-слой.
///
/// Отдельной функцией ради теста ниже. `registration` и
/// `setup_from_any_address` — два соседних `bool`, и перестановку их
/// местами компилятор не поймает никогда: цена ошибки у владельца,
/// включившего регистрацию, — экран первичной настройки, открытый всему
/// интернету, пока в базе нет пользователей.
fn app_settings(config: &Config) -> AppSettings {
    AppSettings {
        registration: config.auth.registration,
        session_ttl_days: config.auth.session_ttl_days,
        setup_from_any_address: config.auth.setup_from_any_address,
        default_timeout_secs: config.durations.timeout_secs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_reaches_the_state_without_swapping_its_flags() {
        // Значения намеренно **разные**: с одинаковыми перестановка двух
        // соседних `bool` неразличима, и тест был бы вакуумным.
        //
        // Чего этот тест не доказывает: что `serve` зовёт именно
        // `app_settings`. Сквозная проверка «конфиг → отказ по адресу»
        // через настоящий сокет невозможна — нужен второй, не-петлевой
        // адрес клиента, а тест бежит на том же хосте.
        let mut config = Config::default();
        config.auth.registration = true;
        config.auth.session_ttl_days = 7;
        config.auth.setup_from_any_address = false;

        let settings = app_settings(&config);
        assert!(settings.registration);
        assert_eq!(settings.session_ttl_days, 7);
        assert!(!settings.setup_from_any_address);

        // И зеркально: иначе «всегда `registration = true`» прошло бы.
        config.auth.registration = false;
        config.auth.setup_from_any_address = true;

        let settings = app_settings(&config);
        assert!(!settings.registration);
        assert!(settings.setup_from_any_address);
    }
}
