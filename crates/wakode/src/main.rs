mod cli;
mod config;
mod signal;
mod startup;

use std::process::ExitCode;

use anyhow::Context as _;
use clap::Parser;
use wakode_api::{AppSettings, AppState};
use wakode_store::UserRepo;

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


    // Ни один фаллибельный вызов между `start` и `shutdown` не выходит
    // через `?`: и чтение версии схемы, и сама подкоманда сходятся в
    // `outcome`, а он возвращается уже после останова. Это не педантизм —
    // первая версия строки с версией схемы была написана через `?` и
    // молча унесла управление мимо останова.
    //
    // `serve` возвращается по сигналу (`signal::wait_for_signal` в паре с
    // `signal::wait_for_drain`), и останов писателя ниже — не задел на
    // будущее, а работающий путь: SIGTERM больше не убивает процесс мимо
    // `shutdown`, и это держится тестом
    // `sigterm_stops_the_server_cleanly_and_stops_the_writer`. Подкоманды
    // при этом по-прежнему идут своими соединениями мимо очереди писателя
    // (`create_user`, `create_key`) и коммитятся до возврата — конструкция
    // ниже нужна им лишь постольку, поскольку общий путь `run` один на
    // все ветки `dispatch`.
    // Версия схемы — то, чего в журнале не хватало больше всего: миграции
    // применяет `SqliteStore::open` молча, и владелец, обновивший сборку,
    // видел «сервер поднят», не видя, применилось ли что-нибудь.
    //
    // Ошибка идёт в тот же `outcome`, а не через `?`: `?` здесь унёс бы
    // управление мимо `shutdown` ровно так же, как в `dispatch` ниже —
    // и первая же версия этой строки именно так и была написана.
    let outcome = match started.store.schema_version().await {
        Ok(schema) => {
            tracing::info!(schema, "база открыта, миграции применены");
            dispatch(&started, args.command.unwrap_or(Command::Serve)).await
        }
        Err(err) => Err(anyhow::Error::new(err).context("не удалось прочитать версию схемы")),
    };

    match started.store.shutdown().await {
        // Строка не косметическая: до неё у инварианта «останов зовётся
        // всегда» не было наблюдаемого признака, и тест на SIGTERM не мог
        // отличить «остановили писателя» от «процесс умер вовремя».
        Ok(()) => tracing::info!("писатель остановлен, база отпущена"),
        // Отказ останова не подменяет собой отказ подкоманды: подменив,
        // мы сообщили бы про писателя вместо того, что владелец просил
        // сделать. Отдельной строкой в журнал — и всё.
        Err(err) => tracing::warn!(error = %err, "останов писателя завершился с ошибкой"),
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

/// Шаг 6 старта: поднять HTTP-слой и работать до сигнала.
async fn serve(started: &startup::Startup) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(&started.config.server.listen)
        .await
        .with_context(|| format!("не удалось занять адрес {}", started.config.server.listen))?;

    // Пишется после `bind`, а не до: до него это было бы обещанием, а не
    // фактом, — ровно та тихая ложь о состоянии, из-за которой владелец
    // идёт искать причину в брандмауэре.
    tracing::info!(listen = %started.config.server.listen, "сервер поднят");

    // Токен заводится, только пока настройка не выполнена: после первого
    // пользователя эндпоинт закрыт навсегда, и печатать секрет в журнал
    // на каждый перезапуск было бы раздачей секрета без назначения.
    //
    // `?` здесь безопасен: `serve` вызывается из `dispatch`, чей результат
    // уходит в `outcome`, а `outcome` возвращается уже после останова
    // писателя. Мимо `shutdown` управление не уходит.
    let setup_token = if started.store.user_count().await? == 0 {
        let token = wakode_auth::SetupToken::generate();
        // Единственное место в проекте, где секрет пишется в журнал
        // намеренно. Обоснование — в докстринге `SetupToken`.
        tracing::info!(
            token = %token,
            header = wakode_api::setup::SETUP_TOKEN_HEADER,
            "администратора ещё нет: первичная настройка открыта по этому токену"
        );
        Some(token)
    } else {
        None
    };

    let state = AppState::new(
        started.store.clone(),
        started.master_key.clone(),
        app_settings(&started.config),
    )
    .with_setup_token(setup_token);

    // Сигнал нужен обеим сторонам: сервер по нему перестаёт принимать
    // соединения, а предел ожидания по нему же начинает течь. Ждать одну
    // футуру дважды нельзя, поэтому факт сигнала раздаётся через канал.
    let (signalled, wait_signalled) = tokio::sync::oneshot::channel();
    let shutdown = async move {
        let name = signal::wait_for_signal().await;
        tracing::info!(signal = name, "сигнал завершения: закрываем приём новых соединений");
        let _ = signalled.send(());
    };

    let served = wakode_api::serve(listener, state, shutdown);
    let drained = signal::wait_for_drain(
        served,
        async move {
            let _ = wait_signalled.await;
        },
        signal::GRACE,
    )
    .await;

    if drained {
        tracing::info!("начатые запросы дочитаны");
    } else {
        tracing::warn!(
            grace_secs = signal::GRACE.as_secs(),
            "не все соединения закрылись в срок; бросаем начатое и завершаемся"
        );
    }

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
        // Не 900: `wakode_core::DEFAULT_TIMEOUT_SECS` равен именно 900, и
        // с ним проверка проводки была бы вакуумной.
        config.durations.timeout_secs = 777;

        let settings = app_settings(&config);
        assert_eq!(settings.default_timeout_secs, 777);
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
