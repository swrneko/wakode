//! Подкоманды `wakode` — через настоящий процесс, а не через вызов функций.
//!
//! Иначе разбор аргументов остаётся непроверенным целиком: `clap` — это и
//! есть та часть, которая решает, принят ли `--password` и дошёл ли
//! `--config` до конфигурации.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn wakode() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wakode"))
}

/// Дочерний процесс, который будет убит в любом случае.
///
/// `Child` из std не убивает процесс в `Drop` — он его отвязывает. Провал
/// утверждения в тесте, поднявшем `serve`, оставил бы висеть процесс,
/// занявший порт, и следующий прогон падал бы уже не по своей вине.
struct Killed(std::process::Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Конфиг во временной папке плюс мастер-ключ.
fn a_setup() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("wakode.toml");
    std::fs::write(
        &config,
        format!(
            "[database]\npath = {:?}\n",
            dir.path().join("wakode.db").to_str().unwrap()
        ),
    )
    .unwrap();

    let master = String::from_utf8(
        wakode()
            .args(["master-key", "generate"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();

    (dir, config, master)
}

/// Завести пользователя через CLI, скормив пароль в stdin.
fn create_user(config: &std::path::Path, login: &str, password: &str) -> std::process::Output {
    let mut child = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "user",
            "create",
            "--login",
            login,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(format!("{password}\n").as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn master_key_generate_prints_a_usable_key() {
    let output = wakode().args(["master-key", "generate"]).output().unwrap();
    assert!(output.status.success());

    let printed = String::from_utf8(output.stdout).unwrap();
    assert!(
        wakode_auth::MasterKey::from_base64(printed.trim()).is_ok(),
        "напечатано не то: {printed}"
    );
    // Дважды подряд — разные ключи. Константа, разбирающаяся из base64,
    // прошла бы проверку выше и оставила бы все инстансы с одним ключом.
    let again = String::from_utf8(
        wakode()
            .args(["master-key", "generate"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_ne!(printed, again);
}

#[test]
fn migrate_creates_the_schema() {
    let (dir, config, _) = a_setup();

    let status = wakode()
        .args(["--config", config.to_str().unwrap(), "migrate"])
        .status()
        .unwrap();
    assert!(status.success());

    let conn = wakode_store::open(&dir.path().join("wakode.db")).unwrap();
    assert_eq!(wakode_store::schema_version(&conn).unwrap(), 1);
}

#[test]
fn user_create_reads_the_password_from_stdin_not_a_flag() {
    // Пароль флагом остался бы в истории оболочки и в выводе `ps`.
    //
    // Годный пароль при этом подаётся в stdin намеренно. Без него отказ
    // был бы двусмысленным: `Command::output` даёт ребёнку пустой stdin,
    // подкоманда отказала бы по пустому паролю — и тест был бы зелёным
    // ровно так же, если бы `--password` принимался. Проверено:
    // реализация, объявившая этот флаг, оставляла тест зелёным.
    let (dir, config, _) = a_setup();
    assert!(
        wakode()
            .args(["--config", config.to_str().unwrap(), "migrate"])
            .status()
            .unwrap()
            .success()
    );

    let mut child = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "user",
            "create",
            "--login",
            "swrneko",
            "--password",
            "секрет",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Отказ разбора аргументов закрывает stdin раньше, чем мы успеваем в
    // него написать, и `write_all` возвращает EPIPE. Это и есть ожидаемый
    // исход, а не сбой теста, — поэтому результат отбрасывается.
    let _ = child
        .stdin
        .as_mut()
        .unwrap()
        .write_all("достаточно длинный пароль\n".as_bytes());
    let rejected = child.wait_with_output().unwrap();

    assert!(
        !rejected.status.success(),
        "флаг --password принят, а не должен был: {}",
        String::from_utf8_lossy(&rejected.stdout)
    );

    // И пользователь не заведён: отказ, случившийся после записи, оставил
    // бы в базе учётку с паролем, засветившимся в `ps`.
    let conn = wakode_store::open(&dir.path().join("wakode.db")).unwrap();
    assert!(
        wakode_store::find_user_by_login(&conn, "swrneko")
            .unwrap()
            .is_none(),
        "пользователь заведён вопреки отказу"
    );
}

#[test]
fn user_create_then_list_shows_the_user() {
    let (_dir, config, _) = a_setup();

    let created = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "user",
            "create",
            "--login",
            "swrneko",
            "--admin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all("достаточно длинный\n".as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let listed = wakode()
        .args(["--config", config.to_str().unwrap(), "user", "list"])
        .output()
        .unwrap();
    let text = String::from_utf8(listed.stdout).unwrap();
    assert!(text.contains("swrneko"), "в списке нет пользователя: {text}");
    assert!(
        !text.contains("$argon2id$"),
        "хеш пароля попал в вывод списка: {text}"
    );
    assert!(text.contains("админ"), "признак админа потерян: {text}");

    // В stdout только записи, каждая начинается с идентификатора. Строка
    // журнала, ушедшая в тот же поток, попала бы в `wakode user list | …`
    // наравне с данными — поэтому подписчик пишет в stderr.
    for line in text.lines() {
        let id = line.split('\t').next().unwrap_or_default();
        assert!(
            uuid::Uuid::parse_str(id).is_ok(),
            "в stdout попало не запись списка: {line:?}"
        );
    }
}

#[test]
fn user_create_stores_a_hash_that_opens() {
    // «В списке нет `$argon2id$`» устраивает и реализацию, кладущую в
    // `password_hash` мусор: такой пользователь не смог бы войти никогда,
    // а узнал бы об этом владелец на живом инстансе. Проверяется то, ради
    // чего хеш и заводится, — что паролем он открывается.
    let (dir, config, _) = a_setup();
    let password = "достаточно длинный пароль";

    let created = create_user(&config, "swrneko", password);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let conn = wakode_store::open(&dir.path().join("wakode.db")).unwrap();
    let user = wakode_store::find_user_by_login(&conn, "swrneko")
        .unwrap()
        .expect("пользователь не сохранён");

    assert!(
        wakode_auth::verify_password(password, &user.password_hash).unwrap(),
        "сохранённый хеш не открывается тем паролем, который был введён"
    );
    assert!(
        !wakode_auth::verify_password("другой пароль", &user.password_hash).unwrap(),
        "хеш открывается чем угодно"
    );
    // Пароль в открытом виде в базе не лежит: `verify_password` выше
    // прошёл бы и по нему, если бы хеширования не было вовсе.
    assert!(!user.password_hash.contains(password));
}

#[test]
fn user_create_stores_the_timezone_it_was_given() {
    // Таймзона режет сутки: пользователь, заведённый в UTC вместо своей,
    // получил бы разбиение по чужим дням — и заметил бы это не сразу, а на
    // сводках, где часть вечера уехала в следующий день.
    let (dir, config, _) = a_setup();

    let mut child = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "user",
            "create",
            "--login",
            "swrneko",
            "--timezone",
            "Europe/Moscow",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all("достаточно длинный пароль\n".as_bytes())
        .unwrap();
    let created = child.wait_with_output().unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let conn = wakode_store::open(&dir.path().join("wakode.db")).unwrap();
    let user = wakode_store::find_user_by_login(&conn, "swrneko")
        .unwrap()
        .unwrap();
    assert_eq!(user.timezone, chrono_tz::Tz::Europe__Moscow);

    // Умолчание — UTC, и оно тоже не «что придётся».
    let second = create_user(&config, "второй", "достаточно длинный пароль");
    assert!(second.status.success());
    let user = wakode_store::find_user_by_login(&conn, "второй")
        .unwrap()
        .unwrap();
    assert_eq!(user.timezone, chrono_tz::Tz::UTC);
}

#[test]
fn an_unknown_timezone_is_refused_before_the_password_is_asked() {
    // Опечатка в имени зоны иначе всплыла бы после ввода пароля, и вводить
    // пришлось бы заново. Отказ до приглашения — то, что делает
    // `cli::user::create`, и это его наблюдаемое свойство.
    let (_dir, config, _) = a_setup();

    let refused = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "user",
            "create",
            "--login",
            "swrneko",
            "--timezone",
            "Europe/Мосвка",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert!(!refused.status.success(), "кривая таймзона принята");
    let stderr = String::from_utf8(refused.stderr).unwrap();
    assert!(
        stderr.contains("Europe/Мосвка"),
        "причина не названа: {stderr}"
    );
    assert!(
        !stderr.contains("Пароль"),
        "пароль спрошен до разбора таймзоны: {stderr}"
    );
}

#[test]
fn key_issue_prints_the_value_once_and_it_authenticates() {
    let (dir, config, master) = a_setup();

    // Пользователь нужен, чтобы было кому выдавать.
    let created = create_user(&config, "swrneko", "пароль подлиннее");
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let issued = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "key",
            "issue",
            "--user",
            "swrneko",
            "--name",
            "ноутбук",
        ])
        .env("WAKODE_MASTER_KEY", &master)
        .output()
        .unwrap();
    assert!(
        issued.status.success(),
        "{}",
        String::from_utf8_lossy(&issued.stderr)
    );

    let printed = String::from_utf8(issued.stdout).unwrap();
    let value = printed
        .lines()
        .find_map(|line| wakode_auth::ApiKeyValue::parse(line.trim()))
        .unwrap_or_else(|| panic!("значение ключа не напечатано: {printed}"));

    // Ключ действительно лежит в базе и зашифрован тем самым мастер-ключом.
    // Проверка через расшифровку сильнее сравнения отпечатков: она
    // доказывает и что ключ сохранён, и что сохранён он под правильным
    // мастер-ключом — то есть что `key issue` не перепутал ключи местами.
    let conn = wakode_store::open(&dir.path().join("wakode.db")).unwrap();
    let stored = wakode_store::first_api_key(&conn).unwrap().unwrap();
    let master_key = wakode_auth::MasterKey::from_base64(&master).unwrap();
    let decrypted = wakode_auth::ApiKeyValue::decrypt(
        &wakode_auth::EncryptedKey::from_bytes(stored.key_encrypted),
        &master_key,
    )
    .unwrap();
    assert_eq!(decrypted, value);
}

#[test]
fn key_issue_without_a_master_key_fails_loudly() {
    // Без мастер-ключа шифровать нечем. Молчаливая выдача незашифрованного
    // ключа была бы худшим исходом.
    let (_dir, config, _) = a_setup();

    let issued = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "key",
            "issue",
            "--user",
            "кого-нет",
            "--name",
            "ноутбук",
        ])
        .env_remove("WAKODE_MASTER_KEY")
        .output()
        .unwrap();

    assert!(!issued.status.success());
    let stderr = String::from_utf8(issued.stderr).unwrap();
    assert!(
        stderr.contains("WAKODE_MASTER_KEY"),
        "причина не названа: {stderr}"
    );
}

#[test]
fn backup_produces_a_readable_copy() {
    let (dir, config, _) = a_setup();
    assert!(
        wakode()
            .args(["--config", config.to_str().unwrap(), "migrate"])
            .status()
            .unwrap()
            .success()
    );

    let dest = dir.path().join("копия.db");
    let status = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "backup",
            "--to",
            dest.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());

    let conn = wakode_store::open(&dest).unwrap();
    assert_eq!(wakode_store::schema_version(&conn).unwrap(), 1);
}

#[test]
fn serve_comes_up_and_answers() {
    // Единственный тест, который доказывает, что бинарь вообще слушает.
    // Всё остальное здесь запускает подкоманды, которые завершаются, — и
    // `serve`, потерявшая `bind` или вызов `wakode_api::serve`, выглядела
    // бы снаружи ровно так же: процесс, который «стартовал».
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("wakode.toml");

    // Порт занимается и отпускается: узнать свободный номер заранее иначе
    // нечем, а передать готовый слушатель дочернему процессу нельзя.
    // Окно между отпусканием и `bind` в ребёнке существует; ошибка в нём
    // будет видна как отказ старта в stderr, который тест печатает.
    let addr = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap()
    };

    std::fs::write(
        &config,
        format!(
            "[server]\nlisten = \"{addr}\"\n\n[database]\npath = {:?}\n",
            dir.path().join("wakode.db").to_str().unwrap()
        ),
    )
    .unwrap();

    let mut child = Killed(
        wakode()
            .args(["--config", config.to_str().unwrap(), "serve"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    // Присваивается на каждом витке до проверки срока, поэтому начального
    // значения не имеет: любое было бы мёртвым.
    let mut last: String;
    let answered = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!("процесс `serve` завершился, не начав слушать: {status}");
        }
        match healthz(addr) {
            Ok(response) if response.starts_with("HTTP/1.1 200 OK") => break response,
            Ok(response) => last = format!("сервер ответил не тем: {response}"),
            Err(err) => last = format!("соединение не установилось: {err}"),
        }
        if Instant::now() >= deadline {
            // Процесс убивается до чтения stderr: пока он жив, поток не
            // дойдёт до EOF, и `read_to_string` повис бы вместо того,
            // чтобы показать, на что жаловался сервер.
            let _ = child.0.kill();
            let _ = child.0.wait();
            let mut log = String::new();
            if let Some(stderr) = child.0.stderr.as_mut() {
                let _ = stderr.read_to_string(&mut log);
            }
            panic!("{last}\nstderr сервера:\n{log}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    assert!(answered.ends_with("ok"), "нет тела ответа: {answered}");
}

/// Сырой `GET /healthz` через настоящий сокет.
///
/// HTTP-клиента в зависимостях нет и заводить его ради одной строки не за
/// чем; образец — `wakode-api/tests/api.rs::serve_actually_answers_on_a_real_socket`.
fn healthz(addr: std::net::SocketAddr) -> std::io::Result<String> {
    let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

#[test]
fn the_config_flag_is_not_ignored() {
    // Подкоманда, читающая `./wakode.toml` мимо флага, пошла бы не в ту
    // базу — и `wakode user create` завёл бы пользователя там, где сервер
    // его не увидит. Остальные тесты этого файла работают с временным
    // конфигом и краснели бы тоже, но по причине, которую пришлось бы
    // выяснять; здесь причина названа.
    let (_dir, config, _) = a_setup();
    let elsewhere = tempfile::tempdir().unwrap();
    let named = elsewhere.path().join("другой.toml");
    std::fs::write(
        &named,
        format!(
            "[database]\npath = {:?}\n",
            elsewhere.path().join("другая.db").to_str().unwrap()
        ),
    )
    .unwrap();

    let created = create_user(&named, "swrneko", "достаточно длинный пароль");
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    // Пользователь ушёл в базу названного конфига, а не в базу того,
    // который лежит рядом под именем по умолчанию.
    let named_db = wakode_store::open(&elsewhere.path().join("другая.db")).unwrap();
    assert!(
        wakode_store::find_user_by_login(&named_db, "swrneko")
            .unwrap()
            .is_some()
    );

    let listed = wakode()
        .args(["--config", config.to_str().unwrap(), "user", "list"])
        .output()
        .unwrap();
    let text = String::from_utf8(listed.stdout).unwrap();
    assert!(
        !text.contains("swrneko"),
        "пользователь виден через другой конфиг: {text}"
    );
}

#[test]
fn the_password_prompt_goes_to_stderr_not_stdout() {
    // `wakode user create ... | ...` — обычный способ разобрать вывод.
    // Приглашение, ушедшее в stdout, попало бы в этот конвейер и сломало
    // бы разбор, а в неинтерактивном сценарии ещё и осталось бы в файле.
    let (_dir, config, _) = a_setup();

    let created = create_user(&config, "swrneko", "достаточно длинный пароль");
    assert!(created.status.success());

    let stdout = String::from_utf8(created.stdout).unwrap();
    let stderr = String::from_utf8(created.stderr).unwrap();
    assert!(!stdout.contains("Пароль"), "приглашение в stdout: {stdout}");
    assert!(stderr.contains("Пароль"), "приглашения нет вовсе: {stderr}");
}

#[test]
fn an_empty_password_is_refused() {
    // Пустая строка в stdin — это не «пароль по умолчанию», а
    // неинтерактивный сценарий, забывший его передать. Завести такого
    // пользователя значило бы открыть вход по пустому паролю.
    let (_dir, config, _) = a_setup();

    let created = create_user(&config, "swrneko", "");
    assert!(!created.status.success(), "пустой пароль принят");
    let stderr = String::from_utf8(created.stderr).unwrap();
    assert!(stderr.contains("пароль"), "причина не названа: {stderr}");
}

#[test]
fn key_revoke_marks_the_key() {
    let (dir, config, master) = a_setup();
    assert!(
        create_user(&config, "swrneko", "достаточно длинный пароль")
            .status
            .success()
    );

    let issued = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "key",
            "issue",
            "--user",
            "swrneko",
            "--name",
            "ноутбук",
        ])
        .env("WAKODE_MASTER_KEY", &master)
        .output()
        .unwrap();
    assert!(issued.status.success());

    let db = dir.path().join("wakode.db");
    let id = {
        let conn = wakode_store::open(&db).unwrap();
        let stored = wakode_store::first_api_key(&conn).unwrap().unwrap();
        assert!(stored.revoked_at.is_none(), "ключ отозван до отзыва");
        stored.id
    };

    let revoked = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "key",
            "revoke",
            "--id",
            &id.to_string(),
        ])
        .env("WAKODE_MASTER_KEY", &master)
        .output()
        .unwrap();
    assert!(
        revoked.status.success(),
        "{}",
        String::from_utf8_lossy(&revoked.stderr)
    );

    let conn = wakode_store::open(&db).unwrap();
    let stored = wakode_store::first_api_key(&conn).unwrap().unwrap();
    assert!(stored.revoked_at.is_some(), "отзыв не записан");
}

#[test]
fn a_missing_named_config_is_refused_before_anything_is_created() {
    // Опечатка в пути к конфигу иначе даёт работу с умолчаниями, то есть
    // с базой `./wakode.db` рядом с рабочим каталогом.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("нет-такого.toml");

    let output = wakode()
        .args(["--config", missing.to_str().unwrap(), "migrate"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("нет-такого.toml"),
        "путь не назван: {stderr}"
    );
}
