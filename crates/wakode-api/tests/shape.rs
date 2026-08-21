//! Сверка формы наших ответов с эталонами, снятыми с живого wakatime.com.
//!
//! Отдельный бинарь, а не часть `api.rs`: помощник нужен всем задачам
//! плана, а `api.rs` уже за две тысячи строк.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use wakode_api::{AppSettings, AppState};
use wakode_auth::{ApiKeyValue, MasterKey};
use wakode_store::{KeyRepo, NewApiKey, NewUser, SqliteStore, UserRepo};

/// Поля, которых мы не отдаём осознанно.
///
/// Список именно явный. Прощай помощник любое недостающее поле — он
/// перестал бы ловить случайно забытое, а это и есть его работа.
/// Добавление строки сюда обязано быть решением, а не умолчанием.
const NOT_OURS: &[&str] = &[
    // Аналитика ИИ-ассистированного кода: плагины редакторов её не
    // читают, а выдумывать значения хуже, чем не отдавать поле.
    // Решение записано в спеке, раздел «Проверенные формы ответов».
    "ai_",
    // Вторая половина той же аналитики: `human_*` стоят в `summaries`
    // рядом с `ai_additions` и означают то же измерение, поделённое
    // надвое — сколько строк написал человек, а сколько ассистент. Мы не
    // считаем ни одной из половин: правки строк доезжают в отметке
    // (`line_additions`), но ни в атрибуты интервала, ни в суммы по дням
    // не входят, — и отдавать одну половину нулём, не считая второй,
    // значило бы соврать числом вместо молчания.
    //
    // **Именами, а не префиксом `"human_"`.** Префикс гасил бы заодно
    // `human_readable_website` из `current.json` — поле профиля, к
    // ИИ-аналитике отношения не имеющее и нами отдаваемое
    // (`compat/user.rs`). Помощник молча перестал бы его проверять, то
    // есть список исключений начал бы прощать по случайности — ровно то,
    // против чего он и заведён.
    "human_additions",
    "human_deletions",
    "human_line_changes",
];

/// Совпадение по префиксу, а не по равенству: `"ai_"` гасит семейство
/// целиком, потому что аналитика ИИ-кода растёт от версии к версии и
/// перечислять её поимённо пришлось бы заново после каждого снимка.
///
/// Соседние `human_*` перечислены именами — см. комментарий выше. Правило,
/// по которому выбирают: префикс годится, когда всё семейство заведомо
/// наше исключение; имя обязательно, когда рядом живёт однокоренное поле,
/// которое мы отдаём.
fn skipped(key: &str) -> bool {
    NOT_OURS.iter().any(|prefix| key.starts_with(prefix))
}

/// Прочитать эталон по имени.
pub fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/wakatime/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("эталон {path} не читается: {err}"));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("эталон {path} не JSON: {err}"))
}

/// Совпадают ли формы: те же ключи на всех уровнях, те же типы значений.
///
/// Значения не сравниваются и сравниваться не могут: эталон снят с чужого
/// аккаунта, у него другие проекты и другое время. Совпасть обязана форма.
pub fn assert_shape_matches(ours: &Value, theirs: &Value) {
    let mut problems = Vec::new();
    compare(ours, theirs, "", &mut problems);
    assert!(
        problems.is_empty(),
        "форма разошлась с эталоном:\n{}",
        problems.join("\n")
    );
}

fn compare(ours: &Value, theirs: &Value, path: &str, out: &mut Vec<String>) {
    match (ours, theirs) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, their_value) in b {
                if skipped(key) {
                    continue;
                }
                match a.get(key) {
                    Some(our_value) => compare(our_value, their_value, &format!("{path}.{key}"), out),
                    None => out.push(format!("  нет поля {path}.{key}")),
                }
            }
            for key in a.keys() {
                if !b.contains_key(key) {
                    out.push(format!("  лишнее поле {path}.{key}"));
                }
            }
        }
        // У массива сверяется форма **первого** элемента, и только его.
        //
        // Однородность остальных — предположение, а не факт, и в этом же
        // репозитории лежит контрпример: `responses` эталона
        // `heartbeat-bulk.json` разнороден, `[0]` там повтор, а `[1]`
        // отказ, и форма отказа этой веткой не проверяется вовсе. Тест
        // батча закрывает это, зовя `assert_shape_matches` вторым разом на
        // `responses[1]` явно, — но знание живёт там, а не здесь, поэтому
        // оговорка стоит на самом помощнике. Сверяешь разнородный массив —
        // зови помощник поэлементно сам.
        //
        // Пустой наш массив против непустого чужого — не расхождение
        // формы: у нас может не быть данных.
        (Value::Array(a), Value::Array(b)) => {
            if let (Some(x), Some(y)) = (a.first(), b.first()) {
                compare(x, y, &format!("{path}[]"), out);
            }
        }
        // `null` с обеих сторон — совпадение; `null` у одной из сторон о
        // типе не говорит ничего, и придираться тут не к чему.
        (Value::Null, _) | (_, Value::Null) => {}
        (x, y) if kind(x) == kind(y) => {}
        (x, y) => out.push(format!("  {path}: у нас {}, у них {}", kind(x), kind(y))),
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        // Целое и дробное не различаются: `total_seconds` приходит то
        // `0` то `21839.3` в зависимости от данных, и это одна форма.
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Настройки по умолчанию. Повторяют `api.rs`: помощники в этом крейте
/// живут по бинарю, потому что `tests/` не делит модули между целями.
fn a_settings() -> AppSettings {
    AppSettings {
        registration: false,
        session_ttl_days: 30,
        setup_from_any_address: false,
        default_timeout_secs: 777,
    }
}

fn a_store(dir: &tempfile::TempDir) -> SqliteStore {
    SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap()
}

/// Состояние с мастер-ключом, пользователем и выданным ему ключом.
async fn a_state_with_a_key(dir: &tempfile::TempDir) -> (AppState, ApiKeyValue) {
    let master = MasterKey::generate();
    let store = a_store(dir);

    let user = store
        .create_user(NewUser {
            login: "swrneko".to_owned(),
            email: None,
            password_hash: "непрозрачно".to_owned(),
            display_name: None,
            timezone: "Europe/Moscow".parse().unwrap(),
            timeout_secs: 900,
            is_admin: false,
        })
        .await
        .unwrap();

    let value = ApiKeyValue::generate();
    store
        .create_key(NewApiKey {
            user_id: user.id,
            name: "рабочий ноутбук".to_owned(),
            key_encrypted: value.encrypt(&master).unwrap().as_bytes().to_vec(),
            key_lookup: value.lookup(&master),
        })
        .await
        .unwrap();

    (AppState::new(store, Some(master), a_settings()), value)
}

/// Ответ на `GET` по пути с предъявленным ключом.
async fn get_with_a_key(state: AppState, key: &ApiKeyValue, uri: &str) -> axum::response::Response {
    wakode_api::router(state)
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .header(
                    axum::http::header::AUTHORIZATION,
                    format!("Basic {}", STANDARD.encode(key.to_string())),
                )
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Ответ на `POST` по пути с предъявленным ключом.
async fn post(
    state: AppState,
    key: &ApiKeyValue,
    uri: &str,
    body: &str,
) -> axum::response::Response {
    wakode_api::router(state)
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(uri)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(
                    axum::http::header::AUTHORIZATION,
                    format!("Basic {}", STANDARD.encode(key.to_string())),
                )
                .body(axum::body::Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Тело ответа как JSON, с проверкой `Content-Type`.
async fn json_body(response: axum::response::Response) -> Value {
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .map(|value| value.to_str().unwrap().to_owned())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("application/json"),
        "тело отдано не как JSON: {content_type:?}"
    );

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn the_helper_notices_a_missing_field() {
    let theirs = serde_json::json!({"data": {"id": "x", "text": "y"}});
    let ours = serde_json::json!({"data": {"id": "x"}});
    let mut problems = Vec::new();
    compare(&ours, &theirs, "", &mut problems);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains(".data.text"), "{problems:?}");
}

#[test]
fn the_helper_notices_a_wrong_type() {
    let theirs = serde_json::json!({"total_seconds": 1.5});
    let ours = serde_json::json!({"total_seconds": "1.5"});
    let mut problems = Vec::new();
    compare(&ours, &theirs, "", &mut problems);
    assert_eq!(problems.len(), 1, "{problems:?}");
}

#[test]
fn the_helper_passes_when_the_shapes_agree() {
    // Прямая проверка публичной точки входа: соседние тесты бьют по
    // приватной `compare`, и без этой пары `assert_shape_matches` можно
    // было бы выпотрошить, не покраснев ни одним тестом.
    let theirs = serde_json::json!({"data": {"id": "их", "seconds": 1.5}});
    let ours = serde_json::json!({"data": {"id": "наш", "seconds": 7}});
    assert_shape_matches(&ours, &theirs);
}

#[test]
#[should_panic(expected = "форма разошлась с эталоном")]
fn the_helper_panics_when_the_shapes_disagree() {
    let theirs = serde_json::json!({"data": {"id": "x", "text": "y"}});
    let ours = serde_json::json!({"data": {"id": "x"}});
    assert_shape_matches(&ours, &theirs);
}

#[test]
fn fixtures_come_from_disk_and_keep_the_duplicate_id_constant() {
    // Читает настоящий файл: сломанный путь в `fixture` уронит этот тест.
    //
    // Сверяется при этом не что попало, а значение, которое обезличивание
    // однажды уже съело. `00000000-0000-4000-a000-000000000000` — ответ
    // WakaTime на отметку-дубликат, и нибблы версии 4 в нём не случайны:
    // это не `Uuid::nil()`, и задача 4 обязана сверяться с этой строкой,
    // а не с нулевым UUID. Решение — в
    // `.claude/docs/decisions/duplicate-heartbeats-are-a-success.md`.
    let bulk = fixture("heartbeat-bulk");
    let duplicate = &bulk["responses"][0][0];
    assert_eq!(duplicate["id"], "00000000-0000-4000-a000-000000000000", "{bulk}");
    assert_eq!(duplicate["skip"], "Too many duplicate heartbeats.", "{bulk}");
}

#[test]
fn the_helper_notices_a_field_we_invented() {
    let theirs = serde_json::json!({"grand_total": 1});
    let ours = serde_json::json!({"grand_total": 1, "wakode_extra": 2});
    let mut problems = Vec::new();
    compare(&ours, &theirs, "", &mut problems);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("лишнее поле .wakode_extra"), "{problems:?}");
}

#[test]
fn an_empty_array_of_ours_is_not_a_mismatch_but_a_wrong_element_is() {
    // Обе половины заявления из комментария над веткой `Value::Array`.
    let theirs = serde_json::json!({"data": [{"digital": "0:00"}]});
    let mut problems = Vec::new();
    compare(&serde_json::json!({"data": []}), &theirs, "", &mut problems);
    assert!(problems.is_empty(), "{problems:?}");
    compare(&serde_json::json!({"data": [{"digital": 0}]}), &theirs, "", &mut problems);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains(".data[].digital"), "{problems:?}");
}

#[test]
fn a_null_on_either_side_says_nothing_about_the_type() {
    let mut problems = Vec::new();
    compare(&serde_json::json!({"city": null}), &serde_json::json!({"city": "Москва"}), "", &mut problems);
    compare(&serde_json::json!({"city": "Москва"}), &serde_json::json!({"city": null}), "", &mut problems);
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn the_helper_forgives_only_the_fields_we_declared() {
    // Зеркало: `ai_*` и `human_*` прощаются, соседнее незнакомое поле —
    // нет. Без этой половины список исключений мог бы прощать всё подряд.
    let theirs = serde_json::json!({
        "ai_sessions": 3,
        "human_additions": 7,
        "sessions": 3,
        "additions": 7,
    });
    let ours = serde_json::json!({});
    let mut problems = Vec::new();
    compare(&ours, &theirs, "", &mut problems);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems.iter().any(|p| p.contains(".sessions")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains(".additions")), "{problems:?}");
}

#[tokio::test]
async fn the_current_user_has_the_shape_wakatime_has() {
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = get_with_a_key(state, &key, "/api/v1/users/current").await;

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_shape_matches(&json_body(response).await, &fixture("current"));
}

#[tokio::test]
async fn an_accepted_heartbeat_has_the_shape_wakatime_has() {
    // Эталон снят с живого: `{"data": {"id"}}` и ничего больше. Полей
    // `entity`, `type` и `time` рядом с ним нет, хотя прежняя редакция
    // спеки их обещала.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = wakode_api::router(state)
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/users/current/heartbeats")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(
                    axum::http::header::AUTHORIZATION,
                    format!("Basic {}", STANDARD.encode(key.to_string())),
                )
                .body(axum::body::Body::from(
                    r#"{"entity":"/дом/проект/файл.rs","type":"file","time":1755500000.0}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), axum::http::StatusCode::CREATED);
    assert_shape_matches(&json_body(response).await, &fixture("heartbeat-single"));
}

/// Отметка со всеми атрибутами, какие мы умеем читать.
///
/// Атрибуты заполняются все до одного не для красоты: массив ответа,
/// оставшийся пустым, помощник прощает по документированной ветке, и форма
/// его элементов не проверялась бы тогда ничем.
fn a_full_heartbeat(time: f64) -> String {
    format!(
        r#"{{"entity":"/дом/вакода/сводки.rs","type":"file","time":{time},
            "category":"coding","project":"вакода","branch":"master",
            "language":"Rust","editor":"nvim","operating_system":"Linux",
            "machine":"ноутбук"}}"#
    )
}

/// Записать отметки и убедиться, что они приняты: сводка по непринятым
/// отметкам была бы пустой, и все проверки ниже вырождаются молча.
async fn record(state: &AppState, key: &ApiKeyValue, times: &[f64]) {
    for &time in times {
        let response = post(
            state.clone(),
            key,
            "/api/v1/users/current/heartbeats",
            &a_full_heartbeat(time),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::CREATED, "{time}");
    }
}

/// Непустые ли массивы дня — иначе сверять их форму бессмысленно.
fn assert_the_day_has_something_in_every_array(day: &Value) {
    for key in [
        "projects",
        "languages",
        "editors",
        "operating_systems",
        "categories",
        "machines",
    ] {
        assert!(
            !day[key].as_array().unwrap_or_else(|| panic!("{key} не массив: {day}")).is_empty(),
            "{key} пуст: форму его элемента сверка не проверит вовсе"
        );
    }
}

#[tokio::test]
async fn a_summary_of_one_day_has_the_shape_wakatime_has() {
    // Эталон снят за 18 августа 2026 года по московскому времени, и день
    // здесь тот же: 09:00Z — это полдень в Москве, середина суток, куда
    // ни один перевод часов не дотянется.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;
    record(&state, &key, &[1_787_043_600.0, 1_787_043_660.0]).await;

    let response = get_with_a_key(
        state,
        &key,
        "/api/v1/users/current/summaries?start=2026-08-18&end=2026-08-18",
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let ours = json_body(response).await;

    assert_eq!(ours["data"].as_array().unwrap().len(), 1, "{ours}");
    assert_the_day_has_something_in_every_array(&ours["data"][0]);

    assert_shape_matches(&ours, &fixture("summaries-one-day"));
}

#[tokio::test]
async fn a_week_of_summaries_has_the_shape_wakatime_has() {
    // `summaries-week` нужен отдельно от одного дня: `cumulative_total` и
    // `daily_average` там считаны по семи дням, и разойдись мы с ними
    // ключом или типом — на одном дне это не проявится, потому что все
    // числа совпадут с итогом единственного дня.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;
    record(&state, &key, &[1_787_043_600.0, 1_787_043_660.0]).await;

    let response = get_with_a_key(
        state,
        &key,
        "/api/v1/users/current/summaries?start=2026-08-12&end=2026-08-18",
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let ours = json_body(response).await;

    assert_eq!(ours["data"].as_array().unwrap().len(), 7, "{ours}");
    assert_shape_matches(&ours, &fixture("summaries-week"));

    // Помощник сверяет у массива только первый элемент, а первым здесь
    // стоит пустой день: без второго вызова форма непустого дня недели не
    // проверялась бы ничем.
    let worked = &ours["data"][6];
    assert_the_day_has_something_in_every_array(worked);
    assert_shape_matches(worked, &fixture("summaries-week")["data"][0]);
}

#[tokio::test]
async fn an_empty_day_has_every_key_a_day_with_work_has() {
    // `data[0]` эталона `summaries-month` — день без работы (2026-07-20),
    // и все семь его массивов пусты. Именно поэтому месяц не годится на
    // сверку формы элементов и годится ровно на это: у пустого дня те же
    // девять ключей и те же типы скаляров, что у рабочего.
    let theirs = fixture("summaries-month");
    let their_empty_day = &theirs["data"][0];
    assert_eq!(theirs["data"].as_array().unwrap().len(), 30, "эталон не месяц");
    assert_eq!(
        their_empty_day["grand_total"]["total_seconds"], 0.0,
        "первый день эталона не пуст, и тест проверяет не то, что обещает"
    );
    assert_eq!(their_empty_day["range"]["date"], "2026-07-20");

    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = get_with_a_key(
        state,
        &key,
        "/api/v1/users/current/summaries?start=2026-07-20&end=2026-07-20",
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let ours = json_body(response).await;

    assert_eq!(ours["data"][0]["grand_total"]["total_seconds"], 0.0, "{ours}");
    assert_shape_matches(&ours["data"][0], their_empty_day);
}

/// Секунды от эпохи прямо сейчас — тем же способом, каким их узнаёт
/// эндпоинт статусбара.
///
/// «Сегодня» у него берётся от системных часов, и подсунуть ему другое
/// «сейчас» нечем: часов в состоянии приложения нет. Значит, отметки для
/// него приходится ставить настоящим временем, а не литералом с эталона.
fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("часы позже эпохи")
        .as_secs_f64()
}

#[tokio::test]
async fn the_statusbar_of_today_has_the_shape_wakatime_has() {
    // **Эталон снят в пустой день, и сверка с ним слабее, чем выглядит.**
    // `range.date`, `grand_total.decimal` и `grand_total.text` там пустые
    // строки, а все семь массивов пусты — это след момента съёмки, а не
    // форма протокола. Последствие называется прямо: ветка массивов в
    // помощнике сверяет первый элемент, только если он есть у **обеих**
    // сторон, поэтому форма элементов `projects[]`, `languages[]` и
    // остальных пяти массивов этим вызовом не проверяется вовсе.
    // Проверяются им скаляры верхнего уровня — `data` и
    // `has_team_features`, — и они измерены.
    //
    // Форму элементов проверяет второй вызов, и он не выдумка. Ключи
    // `data` статусбара и ключи `data[]` сводок совпадают дословно —
    // девять у обоих, `categories dependencies editors grand_total
    // languages machines operating_systems projects range`, — то есть это
    // один и тот же тип, снятый дважды. `summaries-one-day.json` снят в
    // непустой день и даёт ту форму, которой у статусбарного эталона нет.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;
    // Отметки настоящим временем: «сегодня» эндпоинт узнаёт у системных
    // часов. Пара `сейчас - 60` и `сейчас` даёт непустой день в любой
    // момент суток — даже сразу после локальной полуночи, когда ранняя
    // отметка попадает во вчера: сегодняшний кусок интервала начинается с
    // полуночи и всё равно непуст.
    let now = now_secs();
    record(&state, &key, &[now - 60.0, now]).await;

    let response = get_with_a_key(state, &key, "/api/v1/users/current/statusbar/today").await;

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let ours = json_body(response).await;

    assert_shape_matches(&ours, &fixture("statusbar-today"));

    assert_the_day_has_something_in_every_array(&ours["data"]);
    assert_shape_matches(&ours["data"], &fixture("summaries-one-day")["data"][0]);
}

#[tokio::test]
async fn the_all_time_total_has_the_shape_wakatime_has() {
    // В отличие от статусбарного, этот эталон снят на непустом аккаунте и
    // массивов не содержит вовсе: одиннадцать скаляров в двух уровнях
    // объектов. Оговорка про первый элемент массива здесь не работает — ей
    // не на чем сработать, — так что сверка покрывает форму целиком.
    //
    // Отметки всё равно нужны настоящие: диапазон считается от **первой**
    // отметки пользователя, и на пустом пользователе половина полей
    // выродилась бы в ноль и сегодняшнюю дату. Отдельный тест на такого
    // пользователя есть в `api.rs`.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;
    let now = now_secs();
    record(&state, &key, &[now - 60.0, now]).await;

    let response =
        get_with_a_key(state, &key, "/api/v1/users/current/all_time_since_today").await;

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_shape_matches(&json_body(response).await, &fixture("all-time-since-today"));
}

#[tokio::test]
async fn a_bulk_response_has_the_shape_wakatime_has() {
    // Эталон снят с живого и разнороден: `responses[0]` — повтор,
    // `responses[1]` — отказ. Наш батч подобран так, чтобы позиции
    // совпали: отметка сперва отправляется одиночным эндпоинтом, чтобы в
    // батче она уже была повтором.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;
    let one = r#"{"entity":"/дом/проект/файл.rs","type":"file","time":1755500000.0}"#;

    let first = post(state.clone(), &key, "/api/v1/users/current/heartbeats", one).await;
    assert_eq!(first.status(), axum::http::StatusCode::CREATED);

    let response = post(
        state,
        &key,
        "/api/v1/users/current/heartbeats.bulk",
        &format!(r#"[{one},{{"entity":"","type":"file","time":1755500001.0}}]"#),
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::ACCEPTED);
    let ours = json_body(response).await;
    let theirs = fixture("heartbeat-bulk");

    assert_shape_matches(&ours, &theirs);
    // Помощник сверяет у массива только первый элемент — остальные
    // однородны по построению. Здесь они не однородны, и без второго
    // вызова форма отказа не проверялась бы ничем.
    assert_shape_matches(&ours["responses"][1], &theirs["responses"][1]);
}
