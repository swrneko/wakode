//! Журнал и перехват паники.
//!
//! Отдельный бинарь, а не часть `api.rs`, и не по вкусу. После задачи 13
//! **все** маршруты идут через `with_layers`, то есть каждый из шести
//! десятков тестов в `api.rs` дёргает те же callsite'ы `tracing`
//! (`request_span`, `tracing::error!` в `handle_panic`, события
//! `tower_http`) — и дёргает их без установленного подписчика. `tracing`
//! кеширует «интерес» к callsite глобально на процесс, поэтому соседи
//! отравляли кеш тем немногим тестам, у которых подписчик есть: набор
//! падал примерно в одном прогоне из четырёх под нагрузкой, без единой
//! правки кода.
//!
//! Здесь подписчик ровно один и ставится один раз на весь бинарь, а
//! разводятся тесты не подписчиками, а буферами: каждый пишет в свой,
//! потоко-локальный. Глобальный подписчик держит потолок уровня на
//! `TRACE` постоянно, поэтому кешу интереса нечего терять, и гонки не
//! остаётся по построению — а не потому, что её реже видно.

use std::cell::RefCell;
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use http_body_util::BodyExt;
use tower::ServiceExt;
use wakode_api::{router, AppSettings, AppState};
use wakode_store::SqliteStore;

fn a_store(dir: &tempfile::TempDir) -> SqliteStore {
    SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap()
}

fn a_state(dir: &tempfile::TempDir) -> AppState {
    AppState::new(
        a_store(dir),
        None,
        AppSettings {
            registration: false,
            session_ttl_days: 30,
            setup_from_any_address: false,
            default_timeout_secs: 900,
        },
    )
}

/// Тело ответа как JSON, с проверкой `Content-Type`.
async fn json_body(response: Response) -> serde_json::Value {
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

thread_local! {
    /// Буфер журнала текущего потока. `None` — тест журнал не собирает,
    /// и записи выбрасываются.
    static SINK: RefCell<Option<Arc<Mutex<Vec<u8>>>>> = const { RefCell::new(None) };
}

/// Писатель, раскладывающий записи по потоко-локальным буферам.
///
/// `tracing_subscriber` спрашивает писателя на каждое событие, и спрашивает
/// в том потоке, где событие случилось. Именно на этом и держится разводка:
/// подписчик один, а буферов столько, сколько тестов бежит.
struct ThreadLocalSink;

impl Write for ThreadLocalSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        SINK.with(|sink| {
            if let Some(buffer) = sink.borrow().as_ref() {
                buffer.lock().unwrap().extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Поставить единственного подписчика. Зовётся из каждого теста, ставит
/// один раз.
fn subscriber() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(|| ThreadLocalSink)
            .init();
    });
}

/// Выполнить запрос и вернуть ответ вместе с тем, что за это время
/// напечатал журнал.
///
/// Отсечка по уровню делается **здесь**, при разборе накопленного, а не
/// подписчиком: подписчик один на бинарь, и менять его потолок из теста
/// значило бы менять его соседям.
async fn response_and_log(app: axum::Router, request: Request<Body>) -> (Response, String) {
    response_and_log_at(tracing::Level::TRACE, app, request).await
}

/// То же, но видны только записи не ниже заданного уровня: так
/// проверяется, что запись переживёт боевой фильтр, а не только что в ней
/// нет лишнего.
async fn response_and_log_at(
    level: tracing::Level,
    app: axum::Router,
    request: Request<Body>,
) -> (Response, String) {
    subscriber();

    let buffer = Arc::new(Mutex::new(Vec::new()));
    SINK.with(|sink| *sink.borrow_mut() = Some(buffer.clone()));

    // `#[tokio::test]` без `flavor = "multi_thread"` крутит будущее на этом
    // же потоке, поэтому записи попадут в наш буфер.
    let response = app.oneshot(request).await.unwrap();

    SINK.with(|sink| *sink.borrow_mut() = None);

    let raw = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    (response, at_or_above(&raw, level))
}

/// Оставить строки не ниже заданного уровня.
///
/// Уровень стоит в начале строки формата `fmt`: `INFO  request{...}`.
/// Разбор строковый, потому что отсечку надо применять к уже собранному
/// выводу — подписчик общий, и настраивать его под каждый тест нельзя.
fn at_or_above(raw: &str, level: tracing::Level) -> String {
    let allowed: &[&str] = match level {
        tracing::Level::ERROR => &["ERROR"],
        tracing::Level::WARN => &["ERROR", "WARN"],
        tracing::Level::INFO => &["ERROR", "WARN", "INFO"],
        tracing::Level::DEBUG => &["ERROR", "WARN", "INFO", "DEBUG"],
        tracing::Level::TRACE => &["ERROR", "WARN", "INFO", "DEBUG", "TRACE"],
    };

    raw.lines()
        .filter(|line| {
            allowed
                .iter()
                .any(|name| line.split_whitespace().any(|word| word == *name))
        })
        .collect::<Vec<_>>()
        .join("\n")
}


/// Текст паники: он обязан оказаться в журнале и не оказаться у клиента.
const PANIC_TEXT: &str = "нарочно";

/// Паника с полезной нагрузкой `&'static str` — так выглядит `panic!` с
/// литералом, `unwrap` на `None` и `assert!` без сообщения. Литерал
/// повторяет `PANIC_TEXT` буквально: `panic!` с подстановкой дал бы уже
/// `String`, то есть другую ветку `handle_panic`.
async fn panics_with_a_str() -> &'static str {
    panic!("нарочно")
}

/// Паника с полезной нагрузкой `String` — так выглядит любой `panic!` с
/// подстановкой, включая `unwrap` на `Err`. Два маршрута, потому что
/// `handle_panic` разбирает эти два случая разными ветками, и с одним
/// маршрутом вторая ветка была бы украшением.
async fn panics_with_a_string() -> &'static str {
    panic!("{PANIC_TEXT} и с подстановкой")
}

/// Паника с полезной нагрузкой, которая не `&str` и не `String`. Так
/// паникует `panic_any`, а из чужого кода — некоторые `Box<dyn Error>`.
/// `handle_panic` обязан такую пережить и написать хоть что-то: без этого
/// владелец получил бы `500` вообще без записи в журнале.
async fn panics_with_something_else() -> &'static str {
    std::panic::panic_any(42u32)
}

/// Маршруты, которые паникуют, и соседний, который нет.
///
/// Состояние сюда не заводится: перехват паники и журналирование до
/// хранилища не дотягиваются, а лишний `SqliteStore` в тесте только
/// создавал бы впечатление, что дотягиваются.
fn app_that_panics() -> axum::Router {
    wakode_api::with_layers(
        axum::Router::new()
            .route("/взрыв", axum::routing::get(panics_with_a_str))
            .route("/взрыв-строкой", axum::routing::get(panics_with_a_string))
            .route("/взрыв-числом", axum::routing::get(panics_with_something_else))
            .route("/жив", axum::routing::get(|| async { "да" })),
    )
}

#[tokio::test]
async fn a_panicking_handler_becomes_a_500() {
    let app = app_that_panics();

    let response = app
        .oneshot(Request::builder().uri("/взрыв").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn the_process_survives_a_panicking_handler() {
    // Отдельно от предыдущего: 500 могла бы прийти и от упавшей задачи.
    // Здесь проверяется, что после паники сервер продолжает отвечать.
    let app = app_that_panics();

    let _ = app
        .clone()
        .oneshot(Request::builder().uri("/взрыв").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let response = app
        .oneshot(Request::builder().uri("/жив").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_panic_response_is_json_like_every_other_error() {
    let app = app_that_panics();

    let response = app
        .oneshot(Request::builder().uri("/взрыв").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let json = json_body(response).await;
    assert!(json.get("error").is_some(), "тело не JSON с error: {json}");
    assert!(
        !json.to_string().contains("нарочно"),
        "текст паники уехал клиенту: {json}"
    );
}

/// Значение, которое клиент присылает как ключ. В журнале ему не место ни
/// в каком виде: `api_key` в открытом тексте равносилен самому ключу.
const SECRET_KEY: &str = "waka_00000000-1111-2222-3333-444444444444";

#[tokio::test]
async fn the_query_string_never_reaches_the_log() {
    // `?api_key=…` — штатный способ прислать ключ у совместимых клиентов.
    // `DefaultMakeSpan` из `TraceLayer::new_for_http()` пишет поле `uri`
    // целиком, вместе с query-строкой, то есть кладёт ключ в журнал
    // открытым текстом.
    let app = wakode_api::with_layers(
        axum::Router::new().route("/тихо", axum::routing::get(|| async { "да" })),
    );

    let (response, log) = response_and_log(
        app,
        Request::builder()
            .uri(format!("/тихо?api_key={SECRET_KEY}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    // Без этого проверка «секрета нет» проходила бы и на журнале, который
    // не пишет ничего вообще.
    assert!(
        log.contains("/тихо"),
        "запрос не журналируется вовсе, проверять нечего:\n{log}"
    );
    assert!(
        !log.contains(SECRET_KEY),
        "ключ из query-строки попал в журнал:\n{log}"
    );
}

#[tokio::test]
async fn the_authorization_header_never_reaches_the_log() {
    // Второй путь того же ключа: совместимые клиенты шлют его схемой
    // `Basic` в base64. Base64 — не шифрование, в журнале это тот же ключ
    // открытым текстом, поэтому проверяется и предъявленное значение, и
    // сам ключ.
    let credentials = STANDARD.encode(SECRET_KEY);
    let app = wakode_api::with_layers(
        axum::Router::new().route("/тихо", axum::routing::get(|| async { "да" })),
    );

    let (response, log) = response_and_log(
        app,
        Request::builder()
            .uri("/тихо")
            .header("authorization", format!("Basic {credentials}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        log.contains("/тихо"),
        "запрос не журналируется вовсе, проверять нечего:\n{log}"
    );
    assert!(
        !log.contains(&credentials),
        "заголовок `Authorization` попал в журнал:\n{log}"
    );
    assert!(
        !log.contains(SECRET_KEY),
        "ключ из заголовка попал в журнал:\n{log}"
    );
}

#[tokio::test]
async fn a_panic_message_reaches_the_log_but_not_the_client() {
    // Половина «не уехало клиенту» выполняется и реализацией, которая молчит
    // вообще: владелец получил бы `500` без единой зацепки о причине.
    // Обе полезные нагрузки паники проверяются здесь же — `handle_panic`
    // разбирает их разными ветками.
    for path in ["/взрыв", "/взрыв-строкой"] {
        let (response, log) = response_and_log(
            app_that_panics(),
            Request::builder().uri(path).body(Body::empty()).unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            log.contains(PANIC_TEXT),
            "текста паники нет в журнале ({path}):\n{log}"
        );

        let json = json_body(response).await;
        assert!(
            !json.to_string().contains(PANIC_TEXT),
            "текст паники уехал клиенту ({path}): {json}"
        );
    }
}

#[tokio::test]
async fn a_panicking_request_is_journalled_as_a_completed_500() {
    // Порядок слоёв: `TraceLayer` снаружи `CatchPanicLayer`, и держится он
    // только тем, в каком порядке написаны вызовы `layer`. Если перевернуть,
    // паника пойдёт вверх мимо журнала: запись о ней потеряет контекст
    // запроса, а строки о завершении с кодом не будет вовсе — владелец
    // увидит панику без единого признака, какой запрос её вызвал.
    let (_, log) = response_and_log(
        app_that_panics(),
        Request::builder().uri("/взрыв").body(Body::empty()).unwrap(),
    )
    .await;

    let about_the_panic = log
        .lines()
        .find(|line| line.contains("паника в обработчике"))
        .unwrap_or_else(|| panic!("нет записи о панике:\n{log}"));
    assert!(
        about_the_panic.contains("/взрыв"),
        "запись о панике вне контекста запроса: {about_the_panic}"
    );
    assert!(
        log.contains("status=500"),
        "завершение запроса не журналируется:\n{log}"
    );
}

#[tokio::test]
async fn the_real_router_journals_a_request_without_its_query() {
    // Всё остальное про журнал проверяется на своих маршрутах через
    // `with_layers`. Здесь проверяется проводка: боевой `router` эти слои
    // тоже несёт, а не собирается голым.
    let dir = tempfile::tempdir().unwrap();

    let (response, log) = response_and_log(
        router(a_state(&dir)),
        Request::builder()
            .uri(format!("/healthz?api_key={SECRET_KEY}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        log.contains("/healthz"),
        "запрос к боевому маршрутизатору не журналируется:\n{log}"
    );
    assert!(!log.contains(SECRET_KEY), "ключ попал в журнал:\n{log}");
}

#[tokio::test]
async fn a_finished_request_is_journalled_at_info() {
    // Умолчание `tower-http` — `DEBUG`, а бинарь фильтрует по `info`: со
    // слоем на месте журнал запросов оказался бы пуст, и владелец увидел бы
    // тишину, неотличимую от «сервер не получает запросов».
    let (_, log) = response_and_log_at(
        tracing::Level::INFO,
        wakode_api::with_layers(
            axum::Router::new().route("/тихо", axum::routing::get(|| async { "да" })),
        ),
        Request::builder().uri("/тихо").body(Body::empty()).unwrap(),
    )
    .await;

    assert!(
        log.contains("/тихо") && log.contains("status=200"),
        "на уровне info о запросе не написано ничего:\n{log}"
    );
}

#[tokio::test]
async fn an_unusual_panic_payload_is_survived_and_named() {
    // Ветка `unwrap_or("неизвестная паника")`. Реализация, паникующая на
    // разборе чужой полезной нагрузки, уронила бы соединение — то есть
    // ровно то, ради устранения чего вся эта задача и написана.
    let (response, log) = response_and_log(
        app_that_panics(),
        Request::builder()
            .uri("/взрыв-числом")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        log.contains("неизвестная паника"),
        "паника с чужой нагрузкой не названа в журнале:\n{log}"
    );

    let json = json_body(response).await;
    assert!(json.get("error").is_some(), "тело не JSON с error: {json}");
}

/// Отказ первичной настройки виден в журнале — и виден при боевом фильтре.
///
/// Сам факт отказа виден и без этой строки: `TraceLayer` пишет
/// завершение каждого запроса, и `403` на `POST /api/setup` в журнале
/// будет (держится `a_finished_request_is_journalled_at_info`).
/// Уникальна здесь **причина**: отказов два, лечатся они по-разному, и
/// без причины владелец из журнала не поймёт, что ему чинить. Без этого
/// теста удаление строки проходило зелёным по всему workspace —
/// проверено мутацией.
/// Уровень взят `WARN`, а не `TRACE`, потому что боевой фильтр бинаря —
/// `info`, и запись, не переживающая его, бесполезна ровно тогда, когда
/// нужна.
#[tokio::test]
async fn a_refused_setup_is_journalled_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();

    // Пир петлевой, заголовок посредника есть — та самая установка, ради
    // которой отказ и заведён: nginx на том же хосте, клиент снаружи.
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/setup")
        .header("content-type", "application/json")
        .header("x-forwarded-for", "203.0.113.5")
        .body(Body::from(
            r#"{"login":"admin","password":"достаточно длинный","timezone":"Europe/Moscow"}"#,
        ))
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:41234".parse::<std::net::SocketAddr>().unwrap(),
    ));

    let (response, log) =
        response_and_log_at(tracing::Level::WARN, router(a_state(&dir)), request).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        log.contains("первичная настройка отклонена"),
        "отказ настройки не попал в журнал:\n{log}"
    );
    // Не просто «отклонена», а почему: одного сообщения без причины
    // владельцу мало — отказов два, и лечатся они по-разному.
    assert!(
        log.contains("обратный прокси"),
        "в журнале не названа причина отказа:\n{log}"
    );
}

/// Неверный токен виден в журнале причиной, но не своим значением.
///
/// Тест живёт здесь же, а не в `api.rs`: строка проверяется по подстроке
/// в собранном журнале, а `tracing` кеширует интерес к каждому callsite
/// на весь процесс — соседи из `api.rs` дёргают тот же обработчик без
/// подписчика и отравляют кеш.
#[tokio::test]
async fn a_wrong_setup_token_is_journalled_without_its_value() {
    let dir = tempfile::tempdir().unwrap();
    let presented = "SGVsbG8sIHRoaXMgaXMgbm90IHRoZSB0b2tlbg";
    let state =
        a_state(&dir).with_setup_token(Some(wakode_auth::SetupToken::generate()));

    let mut request = Request::builder()
        .method("POST")
        .uri("/api/setup")
        .header("content-type", "application/json")
        .header("x-wakode-setup-token", presented)
        .body(Body::from(
            r#"{"login":"admin","password":"достаточно длинный","timezone":"Europe/Moscow"}"#,
        ))
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:41234".parse::<std::net::SocketAddr>().unwrap(),
    ));

    let (response, log) =
        response_and_log_at(tracing::Level::WARN, router(state), request).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        log.contains("предъявлен неверный токен первичной настройки"),
        "отказ по токену не попал в журнал:\n{log}"
    );
    assert!(
        !log.contains(presented),
        "предъявленный токен утёк в журнал:\n{log}"
    );
}

/// Успешная настройка по токену не пишет адресный отказ.
///
/// Урок задачи 3: `warn!` про «первичная настройка отклонена» стоит внутри
/// ветки `None` разбора предъявленного токена. Вынеси его наружу — и
/// успешная настройка по прокси-адресу заодно сообщила бы о несуществующем
/// отказе.
#[tokio::test]
async fn a_successful_token_setup_does_not_log_an_address_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let token = wakode_auth::SetupToken::generate();
    let state = a_state(&dir).with_setup_token(Some(token.clone()));

    let mut request = Request::builder()
        .method("POST")
        .uri("/api/setup")
        .header("content-type", "application/json")
        .header("x-wakode-setup-token", token.to_string())
        .header("x-forwarded-for", "203.0.113.5")
        .body(Body::from(
            r#"{"login":"admin","password":"достаточно длинный","timezone":"Europe/Moscow"}"#,
        ))
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:41234".parse::<std::net::SocketAddr>().unwrap(),
    ));

    let (response, log) =
        response_and_log_at(tracing::Level::WARN, router(state), request).await;

    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(
        !log.contains("первичная настройка отклонена"),
        "успешная настройка по токену записана как отказ:\n{log}"
    );
}
