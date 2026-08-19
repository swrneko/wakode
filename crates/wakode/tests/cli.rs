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
    // Единственная пара тестов, доказывающая, что бинарь вообще слушает.
    // Всё остальное здесь запускает подкоманды, которые завершаются, — и
    // `serve`, потерявшая `bind` или вызов `wakode_api::serve`, выглядела
    // бы снаружи ровно так же: процесс, который «стартовал».
    a_serving_child(&["serve"]);
}

#[test]
fn no_subcommand_means_serve() {
    // `cli/mod.rs` обещает «подразумевается, если подкоманда не указана»,
    // и это обещание не держалось ничем: `unwrap_or(Command::Serve)`,
    // заменённый на любую другую ветку, проходил весь набор зелёным.
    // Именно в этой форме бинарь и запускают из systemd —
    // `ExecStart=/usr/bin/wakode --config /etc/wakode.toml`.
    a_serving_child(&[]);
}

/// Поднятый дочерний `wakode serve`: процесс, адрес и журнал.
///
/// `dir` держится живым намеренно: в нём лежат конфиг, база и файл
/// журнала, и уничтожение папки раньше времени вырвало бы их из-под
/// работающего сервера.
struct Serving {
    // `child` объявлен раньше `dir` не для красоты: поля дропаются в
    // порядке объявления, и `Killed` обязан убить процесс раньше, чем
    // `TempDir` снесёт папку с конфигом, базой и журналом, которые этот
    // процесс держит открытыми. Порядок наоборот сносил бы папку из-под
    // ещё живого сервера — ровно то, чего docstring выше обещает не
    // делать.
    child: Killed,
    // Значение не читается ни одним тестом этой задачи: поле держат живым
    // ради побочного эффекта `Drop`, а не ради значения.
    #[expect(dead_code, reason = "TempDir держит папку живой через Drop; читать значение незачем")]
    dir: tempfile::TempDir,
    addr: std::net::SocketAddr,
    log: std::path::PathBuf,
}

impl Serving {
    /// Всё, что сервер написал в stderr к этому моменту.
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

/// Поднять бинарь с заданным хвостом аргументов и дождаться `/healthz`.
///
/// Журнал уходит в файл, а не в трубу: труба живого процесса читается
/// только до EOF, то есть до его смерти, а тестам задачи 3 журнал нужен,
/// пока сервер работает.
fn a_serving_child(tail: &[&str]) -> Serving {
    a_serving_child_after(tail, |_| {})
}

/// То же, но с шагом над готовым конфигом до запуска сервера.
///
/// Нужно там, где проверяется поведение, зависящее от **состояния базы на
/// старте**: завести пользователя после того, как сервер поднялся, уже
/// поздно — решения, принимаемые один раз при старте, приняты.
fn a_serving_child_after(tail: &[&str], before: impl FnOnce(&std::path::Path)) -> Serving {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("wakode.toml");

    // Порт занимается и отпускается: узнать свободный номер заранее иначе
    // нечем, а передать готовый слушатель дочернему процессу нельзя.
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

    // До запуска сервера, а не после: база к этому моменту ещё не занята
    // им, и подготовка идёт обычным путём — через сам бинарь.
    before(&config);

    let log = dir.path().join("server.log");
    let sink = std::fs::File::create(&log).unwrap();

    let mut args = vec!["--config".to_owned(), config.to_str().unwrap().to_owned()];
    args.extend(tail.iter().map(|arg| (*arg).to_owned()));

    let child = Killed(
        wakode()
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::from(sink))
            .spawn()
            .unwrap(),
    );

    let mut serving = Serving { dir, child, addr, log };

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last: String;
    loop {
        if let Some(status) = serving.child.0.try_wait().unwrap() {
            panic!(
                "процесс `serve` завершился, не начав слушать: {status}\nstderr:\n{}",
                serving.log()
            );
        }
        match healthz(addr) {
            Ok(response) if response.starts_with("HTTP/1.1 200 OK") => {
                assert!(response.ends_with("ok"), "нет тела ответа: {response}");
                return serving;
            }
            Ok(response) => last = format!("сервер ответил не тем: {response}"),
            Err(err) => last = format!("соединение не установилось: {err}"),
        }
        if Instant::now() >= deadline {
            panic!("{last}\nstderr сервера:\n{}", serving.log());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
#[test]
fn sigterm_stops_the_server_cleanly_and_stops_the_writer() {
    // Три утверждения об одном: процесс уходит сам (а не висит с
    // проглоченным сигналом), уходит успехом (systemd иначе считает
    // штатную остановку отказом и пишет `Failed with result exit-code`),
    // и по дороге останавливает писателя.
    //
    // Последнее — единственный наблюдаемый признак инварианта «shutdown
    // зовётся всегда», который до этого плана не держался ничем: до
    // появления обработчика сигнала SIGTERM убивал процесс на месте.
    a_signal_stops_the_server_cleanly(libc::SIGTERM, "SIGTERM");
}

#[cfg(unix)]
#[test]
fn sigint_stops_the_server_cleanly_too() {
    // `wait_for_signal` слушает SIGTERM и SIGINT одним `select!`, и до
    // этого теста ветка SIGINT не была нужна ни одному тесту набора: её
    // можно было выкинуть целиком (Ctrl-C перестал бы останавливать
    // сервер штатно) — набор остался бы зелёным.
    a_signal_stops_the_server_cleanly(libc::SIGINT, "SIGINT");
}

/// Отправить ребёнку сигнал и проверить штатную остановку по нему же.
///
/// Общий код для SIGTERM и SIGINT: у обоих одно и то же наблюдаемое
/// поведение, и дублировать тело теста ради разницы в одном идентификаторе
/// значило бы держать два места, которые обязаны меняться синхронно.
///
/// `signal="{name}"` проверяется **точным** значением поля, а не
/// подстрокой «сигнал завершения»: без этого перепутанные местами имена
/// SIGTERM и SIGINT внутри `wait_for_signal` не роняли бы ничего — сама
/// строка «сигнал завершения» осталась бы на месте, поменялось бы только
/// значение поля.
#[cfg(unix)]
fn a_signal_stops_the_server_cleanly(signal: libc::c_int, name: &str) {
    let mut serving = a_serving_child(&["serve"]);
    let pid = serving.child.0.id();

    // Безопасность: `pid` взят у живого ребёнка, которого мы сами
    // породили, и до `wait` его номер не переиспользуется.
    assert_eq!(
        unsafe { libc::kill(pid as libc::pid_t, signal) },
        0,
        "kill не отправился"
    );

    let status = wait_for_exit(&mut serving.child.0, Duration::from_secs(20))
        .unwrap_or_else(|| panic!("процесс не завершился по {name} за двадцать секунд"));

    assert!(
        status.success(),
        "{name} — штатная остановка, а не отказ: {status}\n{}",
        serving.log()
    );

    let log = serving.log();
    assert!(
        log.contains(&format!("signal=\"{name}\"")),
        "в журнале нет точного имени пришедшего сигнала ({name}):\n{log}"
    );
    assert!(
        log.contains("писатель остановлен"),
        "останов писателя не отработал по пути сигнала:\n{log}"
    );
}

#[cfg(unix)]
#[test]
fn sigterm_during_an_unfinished_request_still_exits_and_says_so() {
    // Три остальных теста на SIGTERM бьют по серверу без единого
    // незакрытого запроса: сигнал и завершение `served` там происходят
    // почти одновременно, и разрыв между «сигнал получен» и «сервер
    // дочитал начатое» ничем не проверяется. Здесь запрос держится
    // нарочно недочитанным: `Content-Length` объявляет тело длиннее, чем
    // отправлено, и соединение не закрывается — обработчик "/api/setup"
    // застревает на чтении тела.
    //
    // Это ловит два класса регрессии разом: канал `signalled` в
    // `main.rs::serve`, если его выкинуть, оставляет `wait_for_drain` без
    // способа понять, что сигнал вообще пришёл, — первый `select!` ждёт
    // только `served`, а тот не завершится, пока не дочитан именно этот
    // запрос, то есть никогда. Процесс не отвечал бы на SIGTERM вовсе и
    // висел бы до SIGKILL от systemd, убивая писателя на месте — то есть
    // ровно то, ради чего предел заведён. Соседняя мутация — перепутать
    // `if drained` и `if !drained` — вместо этого дала бы неверную строку
    // в журнале при верном факте останова.
    let mut serving = a_serving_child(&["serve"]);
    let pid = serving.child.0.id();

    let mut stream = std::net::TcpStream::connect(serving.addr).unwrap();
    stream
        .write_all(
            "POST /api/setup HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Content-Length: 1000\r\n\r\n{\"login\":\"недописанный"
                .as_bytes(),
        )
        .unwrap();

    // Без паузы SIGTERM обгоняет `accept()` на сервере: соединение сидит
    // в очереди ядра, ещё не подхвачено циклом приёма, и «начатых, но не
    // дочитанных» запросов с точки зрения axum попросту нет — сервер
    // закрывается мгновенно, а сценарий этого теста не воспроизводится.
    // Проверено вручную: без паузы «в срок» отчитывается за миллисекунды,
    // с ней — ровно через `GRACE`.
    std::thread::sleep(Duration::from_millis(300));

    assert_eq!(
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) },
        0,
        "kill не отправился"
    );

    // `GRACE` — десять секунд; запас даёт время на старт процесса и сам
    // останов писателя. Внутренний обработчик от клиента не зависит:
    // соединение остаётся открытым нами специально, всё время процесса.
    let status = wait_for_exit(&mut serving.child.0, Duration::from_secs(20)).expect(
        "процесс не завершился в течение предела дренажа: канал сигнала не дошёл до \
         wait_for_drain, и SIGTERM убил бы писателя на месте, как до всего этого плана",
    );

    assert!(
        status.success(),
        "SIGTERM с незакрытым запросом — тоже штатная остановка: {status}\n{}",
        serving.log()
    );

    let log = serving.log();
    assert!(
        log.contains("не все соединения закрылись в срок"),
        "запрос не был брошен по истечении предела, хотя не мог быть дочитан:\n{log}"
    );
    assert!(
        !log.contains("начатые запросы дочитаны"),
        "журнал сообщает об успешном дочитывании при заведомо недочитанном запросе:\n{log}"
    );

    // Соединение держим у себя до конца: закрыть его раньше значило бы
    // дать серверу дочитать (оборванным телом) раньше SIGTERM, и сценарий
    // перестал бы быть «начатый, не дочитанный запрос».
    drop(stream);
}

/// Дождаться завершения процесса, но не дольше срока.
#[cfg(unix)]
fn wait_for_exit(
    child: &mut std::process::Child,
    within: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
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
    let revoked_at = stored.revoked_at;
    drop(conn);

    // Повтор — успех, и время отзыва не переписывается. Ретрай и двойной
    // клик в настройках обязаны быть безобидными.
    let again = revoke(&config, &master, &id.to_string());
    assert!(again.status.success(), "повторный отзыв объявлен отказом");
    assert!(
        String::from_utf8_lossy(&again.stdout).contains("уже был отозван"),
        "повтор неотличим от первого отзыва: {}",
        String::from_utf8_lossy(&again.stdout)
    );

    let conn = wakode_store::open(&db).unwrap();
    assert_eq!(
        wakode_store::first_api_key(&conn).unwrap().unwrap().revoked_at,
        revoked_at,
        "повторный отзыв переписал время: «когда отозвали» стало «когда в последний раз пытались»"
    );
}

#[test]
fn revoking_a_key_that_does_not_exist_is_a_failure_not_a_shrug() {
    // Опечатка в UUID — самый вероятный способ ошибиться в этой
    // подкоманде. Пока ответом было «отозван», владелец, отзывающий
    // утёкший ключ, считал инцидент закрытым, а ключ продолжал работать.
    let (_dir, config, master) = a_setup();
    assert!(
        create_user(&config, "swrneko", "достаточно длинный пароль")
            .status
            .success()
    );

    let stranger = uuid::Uuid::now_v7().to_string();
    let output = revoke(&config, &master, &stranger);

    assert!(
        !output.status.success(),
        "отзыв несуществующего ключа объявлен успехом"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&stranger),
        "в отказе нет идентификатора, по которому искать опечатку: {stderr}"
    );
}

/// Отозвать ключ по идентификатору.
fn revoke(config: &std::path::Path, master: &str, id: &str) -> std::process::Output {
    wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "key",
            "revoke",
            "--id",
            id,
        ])
        .env("WAKODE_MASTER_KEY", master)
        .output()
        .unwrap()
}

#[test]
fn a_short_password_is_refused_by_the_cli_too() {
    // Один инвариант, два входа. Пока порог стоял только в HTTP-экране,
    // `wakode user create` заводил администратора с паролем «1» — и
    // владелец, воспользовавшийся CLI вместо экрана, получал ровно то, от
    // чего экран его берёг.
    let (dir, config, _) = a_setup();

    let output = create_user(&config, "слабый", "1234567");
    assert!(!output.status.success(), "семисимвольный пароль принят");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("короче"),
        "причина не названа: {stderr}"
    );

    // И пользователя не осталось: отказ обязан быть до записи.
    let conn = wakode_store::open(&dir.path().join("wakode.db")).unwrap();
    assert!(
        wakode_store::find_user_by_login(&conn, "слабый")
            .unwrap()
            .is_none(),
        "пользователь со слабым паролем всё-таки заведён"
    );
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

#[test]
fn without_the_admin_flag_the_user_is_not_an_admin() {
    // Односторонняя проверка: тест «с флагом получается админ» есть, а
    // мутация «игнорировать флаг и всегда ставить true» не роняла ничего
    // во всём workspace. `wakode user create --login teammate` без флага —
    // самый частый вызов этой подкоманды, и регрессия в проводке молча
    // раздавала бы админство всем. Узнали бы об этом в 3b, когда появятся
    // админские эндпоинты.
    let (_dir, config, _) = a_setup();
    assert!(
        create_user(&config, "коллега", "достаточно длинный пароль")
            .status
            .success()
    );

    let listed = wakode()
        .args(["--config", config.to_str().unwrap(), "user", "list"])
        .output()
        .unwrap();
    let text = String::from_utf8(listed.stdout).unwrap();
    let line = text
        .lines()
        .find(|line| line.contains("коллега"))
        .expect(&format!("в списке нет пользователя: {text}"));
    assert!(
        !line.contains("админ"),
        "пользователь без флага оказался администратором: {line}"
    );
}

#[test]
fn the_cli_holds_the_same_login_invariant_as_the_setup_screen() {
    // Экран первичной настройки триммит логин и отвергает пустой, а CLI
    // до этой правки не делал ни того, ни другого: `--login ""` заводил
    // пользователя с пустым логином, `--login "  админ  "` — с пробелами.
    // Войти вторым нельзя никогда: форма входа триммит. Инвариант переехал
    // в `insert_user`, то есть в единственную дверь записи.
    let (dir, config, _) = a_setup();

    let empty = create_user(&config, "", "достаточно длинный пароль");
    assert!(!empty.status.success(), "пустой логин принят");

    let spaces = create_user(&config, "   ", "достаточно длинный пароль");
    assert!(!spaces.status.success(), "логин из пробелов принят");

    assert!(
        create_user(&config, "  админ  ", "достаточно длинный пароль")
            .status
            .success()
    );

    let conn = wakode_store::open(&dir.path().join("wakode.db")).unwrap();
    assert!(
        wakode_store::find_user_by_login(&conn, "админ")
            .unwrap()
            .is_some(),
        "логин сохранён с пробелами — войти им будет нельзя"
    );
}

#[test]
fn the_timeout_from_the_config_reaches_the_created_user() {
    // Секция `[durations]` не читалась вообще: обе двери создания
    // пользователя прошивали `wakode_core::DEFAULT_TIMEOUT_SECS`. Владелец
    // писал `timeout_secs = 300`, перезапускал, заводил пользователя — и в
    // базе оказывалось 900, без единого слова куда-либо, а способа
    // исправить строку в 3a не было вовсе.
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("wakode.toml");
    std::fs::write(
        &config,
        format!(
            "[database]\npath = {:?}\n\n[durations]\ntimeout_secs = 300\n",
            dir.path().join("wakode.db").to_str().unwrap()
        ),
    )
    .unwrap();

    assert!(
        create_user(&config, "swrneko", "достаточно длинный пароль")
            .status
            .success()
    );

    let conn = wakode_store::open(&dir.path().join("wakode.db")).unwrap();
    let user = wakode_store::find_user_by_login(&conn, "swrneko")
        .unwrap()
        .unwrap();
    assert_eq!(
        user.timeout_secs, 300,
        "тайм-аут взят из константы, а не из конфига"
    );
}

#[test]
fn the_startup_log_names_absolute_paths_and_the_schema() {
    // Относительный путь в журнале не говорит ничего: рабочий каталог под
    // systemd задаёт unit, а не тот, кто читает лог. Число миграций
    // отсутствовало вовсе — `SqliteStore::open` применяет их молча, и
    // владелец, обновивший сборку, видел «сервер поднят», не видя,
    // применилось ли что-нибудь.
    // Флаг и путь к базе задаются **относительными**, а рабочий каталог
    // процесса — временной папкой. Иначе проверка вакуумна: `a_setup`
    // отдаёт абсолютные пути, и вывод «как есть» неотличим от вывода
    // через `std::path::absolute`.
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("wakode.toml");
    std::fs::write(&config, "[database]\npath = \"wakode.db\"\n").unwrap();

    let output = wakode()
        .current_dir(dir.path())
        .args(["--config", "wakode.toml", "migrate"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = String::from_utf8_lossy(&output.stderr);
    // `canonicalize` у путей внутри временной папки: на macOS `/tmp` —
    // символическая ссылка, и `absolute` в дочернем процессе даёт
    // `/private/...`. Сравнивать надо разрешённые пути, иначе тест
    // рассказывает про символические ссылки, а не про журнал.
    let root = dir.path().canonicalize().unwrap();
    assert!(
        log.contains(root.join("wakode.db").to_str().unwrap())
            || log.contains(dir.path().join("wakode.db").to_str().unwrap()),
        "путь к базе в журнале не абсолютный:\n{log}"
    );
    assert!(
        log.contains(root.join("wakode.toml").to_str().unwrap())
            || log.contains(dir.path().join("wakode.toml").to_str().unwrap()),
        "путь к конфигу в журнале не абсолютный:\n{log}"
    );
    assert!(
        log.contains("schema=1"),
        "в журнале не названа версия схемы:\n{log}"
    );
}

#[test]
fn a_duplicate_login_is_refused_in_plain_words() {
    // Занятый логин — вина вызывающего. Пока он ехал наружу сырым текстом
    // SQLite (`база данных: UNIQUE constraint failed: users.login: Error
    // code 2067`, продублированным цепочкой причин), он подавался как
    // поломка базы и расходился по форме с соседней подкомандой, где
    // сказано «нет пользователя {login}».
    let (_dir, config, _) = a_setup();
    assert!(
        create_user(&config, "swrneko", "достаточно длинный пароль")
            .status
            .success()
    );

    let again = create_user(&config, "swrneko", "другой длинный пароль");
    assert!(!again.status.success(), "дубликат логина принят");

    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(
        stderr.contains("swrneko") && stderr.contains("уже есть"),
        "причина не названа по-человечески: {stderr}"
    );
    assert!(
        !stderr.contains("UNIQUE") && !stderr.contains("2067"),
        "сырой текст SQLite уехал владельцу: {stderr}"
    );
}

#[test]
fn the_setup_token_from_the_log_opens_setup_through_a_proxy() {
    // Сквозная проводка: токен из журнала — тот самый, который принимает
    // сервер, и он снимает ровно тот отказ, который иначе получает
    // запрос с прокси-заголовком. Мутация «в состояние уходит None»
    // роняет этот тест и не роняет ни одного теста в wakode-api.
    let serving = a_serving_child(&["serve"]);

    let token = wait_for_setup_token(&serving);

    // Сначала — что отказ вообще есть. Без этой половины 201 ниже
    // ничего не доказывал бы: он получился бы и без токена.
    let refused = raw_setup(serving.addr, Some("203.0.113.5"), None);
    assert!(
        refused.starts_with("HTTP/1.1 403"),
        "запрос через посредника без токена обязан быть отвергнут: {refused}"
    );

    let created = raw_setup(serving.addr, Some("203.0.113.5"), Some(&token));
    assert!(
        created.starts_with("HTTP/1.1 201"),
        "токен из журнала не открыл настройку: {created}\nжурнал:\n{}",
        serving.log()
    );
}

/// Дождаться строки с токеном в журнале и вернуть его значение.
fn wait_for_setup_token(serving: &Serving) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let log = serving.log();
        if let Some(token) = log
            .split_whitespace()
            .find_map(|field| field.strip_prefix("token="))
        {
            return token.to_owned();
        }
        if Instant::now() >= deadline {
            panic!("токен настройки не появился в журнале:\n{log}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Сырой `POST /api/setup` через настоящий сокет.
fn raw_setup(
    addr: std::net::SocketAddr,
    forwarded_for: Option<&str>,
    token: Option<&str>,
) -> String {
    // Не `br#"..."#`: сырой байтовый литерал в Rust обязан быть ASCII, а
    // пароль — намеренно кириллический (проверяет ту же границу, что и
    // `the_password_threshold_counts_characters_not_bytes` в `wakode-api`).
    let body = r#"{"login":"admin","password":"достаточно длинный","timezone":"Europe/Moscow"}"#
        .as_bytes();

    let mut request = format!(
        "POST /api/setup HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(value) = forwarded_for {
        request.push_str(&format!("X-Forwarded-For: {value}\r\n"));
    }
    if let Some(value) = token {
        request.push_str(&format!("X-Wakode-Setup-Token: {value}\r\n"));
    }
    request.push_str("\r\n");

    let mut stream =
        std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(body).unwrap();

    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

/// Настроенный инстанс токен не выпускает и в журнал не пишет.
///
/// Зеркало `the_setup_token_from_the_log_opens_setup_through_a_proxy`, и
/// заведено не для симметрии. Финальное ревью ветки показало мутацией,
/// что условие `user_count().await? == 0` в `main.rs::serve` не держалось
/// ничем: замена на `>= 0` проходила по всему workspace зелёной. Цена
/// такой мутации у владельца — свежий 32-байтовый секрет в journald на
/// каждый перезапуск боевого инстанса, где настройка давно закрыта, и
/// `token_required: true` в ответе удалённому клиенту.
///
/// Утверждение отрицательное, поэтому рядом стоит положительное: без
/// него сломанный захват журнала (пустой файл) выглядел бы как успех.
#[test]
fn a_configured_instance_never_prints_a_setup_token() {
    let serving = a_serving_child_after(&["serve"], |config| {
        let created = create_user(config, "swrneko", "достаточно длинный пароль");
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
    });

    let log = serving.log();
    assert!(
        log.contains("сервер поднят"),
        "журнал сервера не прочитан — отрицательная проверка ниже была бы пустой:\n{log}"
    );
    assert!(
        !log.contains("token="),
        "инстанс с администратором напечатал токен первичной настройки:\n{log}"
    );
}
