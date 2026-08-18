use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use http_body_util::BodyExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;
use wakode_api::{router, ApiError, AppSettings, AppState};
use wakode_auth::{ApiKeyValue, MasterKey, SessionToken};
use wakode_core::{Category, EntityKind, Micros};
use wakode_store::{
    ApiKey, HeartbeatRepo, IncomingHeartbeat, KeyRepo, NewApiKey, NewSession, NewUser, SessionRepo,
    SqliteStore, StoreError, UserRepo,
};

/// Настройки по умолчанию: регистрация закрыта, настройка — только с
/// петлевого адреса. Тесты, которым важно другое, перекрывают нужное поле
/// по имени: `registration` и `setup_from_any_address` — два соседних
/// `bool`, и перепутать их местами компилятор не поможет никогда.
fn a_settings() -> AppSettings {
    AppSettings {
        registration: false,
        session_ttl_days: 30,
        setup_from_any_address: false,
    }
}

pub fn a_store(dir: &tempfile::TempDir) -> SqliteStore {
    SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap()
}

pub fn a_state(dir: &tempfile::TempDir) -> AppState {
    AppState::new(a_store(dir), None, a_settings())
}

/// Тело ответа как JSON. `Content-Type` проверяется здесь же: тест с
/// именем «json error» обязан краснеть, если тело отдали `text/plain`.
async fn json_body(response: axum::response::Response) -> serde_json::Value {
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

/// Завести пользователя и выдать ему ключ.
///
/// Возвращает и предъявляемое значение, и запись ключа: тесты, которым
/// нужен `id` выданного ключа, иначе доставали бы его через `first_key`,
/// а это работает только пока ключ в базе один.
async fn a_user_with_a_key(
    store: &SqliteStore,
    master: &MasterKey,
    login: &str,
) -> (ApiKeyValue, ApiKey) {
    let user = store
        .create_user(NewUser {
            login: login.to_owned(),
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
    let key = store
        .create_key(NewApiKey {
            user_id: user.id,
            name: "рабочий ноутбук".to_owned(),
            key_encrypted: value.encrypt(master).unwrap().as_bytes().to_vec(),
            key_lookup: value.lookup(master),
        })
        .await
        .unwrap();

    (value, key)
}

/// Состояние с мастер-ключом, пользователем и выданным ему ключом.
async fn a_state_with_a_key(dir: &tempfile::TempDir) -> (AppState, ApiKeyValue) {
    let master = MasterKey::generate();
    let store = a_store(dir);
    let (value, _) = a_user_with_a_key(&store, &master, "swrneko").await;

    (AppState::new(store, Some(master), a_settings()), value)
}

/// Пробный маршрут: единственный смысл — потребовать `KeyAuth`.
///
/// Маршрутов два, потому что у `KeyAuth` два поля: `key_id` не читается
/// ниоткуда, кроме второго маршрута, и без него `key_id: Uuid::nil()`
/// прошёл бы весь набор зелёным.
fn app_requiring_a_key(state: AppState) -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route(
            "/кто-я",
            get(|auth: wakode_api::auth::KeyAuth| async move { auth.user.login }),
        )
        .route(
            "/каким-ключом",
            get(|auth: wakode_api::auth::KeyAuth| async move { auth.key_id.to_string() }),
        )
        .with_state(state)
}

/// Ответ на запрос к пробному маршруту с заголовком `Authorization`.
/// Схема и значение подставляются как есть — тесту про регистр схемы нужно
/// управлять и тем, и другим.
async fn who_am_i_with_authorization(state: &AppState, scheme: &str, credentials: &str) -> Response {
    app_requiring_a_key(state.clone())
        .oneshot(
            Request::builder()
                .uri("/кто-я")
                .header("authorization", format!("{scheme} {credentials}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn healthz_answers_ok() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ok");
}

#[tokio::test]
async fn an_unknown_path_is_a_json_error_not_an_empty_404() {
    // Совместимые клиенты разбирают тело ответа. Пустая 404 без тела
    // выглядит для них как сломанный сервер, а не как «нет такого пути».
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(Request::builder().uri("/нет-такого").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let json = json_body(response).await;
    assert!(json.get("error").is_some(), "нет поля error: {json}");
}

#[tokio::test]
async fn a_wrong_method_is_a_json_error_too() {
    // Дыра в обещании «тело всегда JSON»: `fallback` ловит только
    // несовпадение пути. Путь, у которого есть обработчик на другой метод,
    // до него не доходит, и axum сам отдаёт пустой 405 — для клиента это
    // ровно то же «сломанный сервер», ради которого заведён `fallback`.
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let json = json_body(response).await;
    assert!(json.get("error").is_some(), "нет поля error: {json}");
}

#[tokio::test]
async fn state_debug_prints_neither_the_master_key_nor_the_dictionary() {
    // Урок финального ревью плана 2: утечка появилась на стыке трёх
    // по отдельности разумных решений, и увидеть её можно было только
    // на собранном состоянии целиком.
    let dir = tempfile::tempdir().unwrap();
    let master = wakode_auth::MasterKey::generate();
    let store = a_store(&dir);

    // Словарь наполняется по-настоящему. Без этого вторая половина имени
    // теста была бы вакуумной: в пустом словаре нечему утечь, и проверка
    // осталась бы зелёной даже при `Debug`, печатающем его содержимое.
    let user = store
        .create_user(NewUser {
            login: "владелец".to_owned(),
            email: None,
            password_hash: "непрозрачные байты".to_owned(),
            display_name: None,
            timezone: "Europe/Moscow".parse().unwrap(),
            timeout_secs: 900,
            is_admin: false,
        })
        .await
        .unwrap();

    let secret_path = "/home/владелец/секретный-проект/увольнение.md";
    let secret_project = "секретный-проект";
    store
        .record_heartbeats(
            user.id,
            vec![IncomingHeartbeat {
                time: Micros::from_secs(1_755_000_000),
                entity: secret_path.to_owned(),
                kind: EntityKind::File,
                category: Category::Coding,
                project: Some(secret_project.to_owned()),
                branch: None,
                language: None,
                editor: None,
                os: None,
                machine: None,
                plugin: None,
                is_write: false,
                lines: None,
                lineno: None,
                cursorpos: None,
                line_additions: None,
                line_deletions: None,
                project_root_count: None,
                dependencies: None,
                ai_line_changes: None,
                human_line_changes: None,
                ai_meta: None,
            }],
            "Europe/Moscow".parse().unwrap(),
        )
        .await
        .unwrap();

    let state = AppState::new(store, Some(master.clone()), a_settings());
    let dump = format!("{state:?}");

    // Словарь непустой — иначе проверки ниже ничего не значат.
    assert!(
        dump.contains("Interner { strings: ") && !dump.contains("Interner { strings: 0 }"),
        "словарь пуст, проверка утечки была бы вакуумной: {dump}"
    );

    assert!(!dump.contains(secret_path), "путь к файлу утёк: {dump}");
    assert!(
        !dump.contains(secret_project),
        "название проекта утекло: {dump}"
    );

    assert!(!dump.contains(&master.to_base64()), "мастер-ключ утёк: {dump}");

    // Одной проверки на подстроку base64 недостаточно: `MasterKey` печатает
    // себя как `MasterKey("<скрыт>")` при любом способе его вывести, так что
    // сравнение с исходным base64 остаётся зелёным, даже если `AppState`
    // выводит поле `master_key` через его собственный `Debug` вместо
    // `.is_some()`. Корректная реализация печатает `master_key: true/false`,
    // а тип `MasterKey` в дампе не появляется вообще.
    assert!(
        !dump.contains("MasterKey"),
        "поле master_key выведено через собственный Debug вместо .is_some(): {dump}"
    );
}

#[tokio::test]
async fn a_storage_error_does_not_leak_its_text_to_the_client() {
    // Докстринг `From<StoreError>` обещает, что подробности схемы и путей
    // наружу не уезжают. Без этого теста обещание держалось бы только
    // тем, что никто пока не написал `ApiError::BadRequest(err.to_string())`.
    let leaky = StoreError::Corrupt("строка 42 таблицы heartbeats: /home/владелец/тайна".to_owned());
    let text = leaky.to_string();

    let response = ApiError::from(leaky).into_response();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let json = json_body(response).await;
    let body = json.to_string();
    assert!(!body.contains("heartbeats"), "имя таблицы утекло: {body}");
    assert!(!body.contains("тайна"), "путь утёк: {body}");
    assert!(!body.contains(&text), "текст ошибки утёк целиком: {body}");
}

#[tokio::test]
async fn a_full_write_queue_is_a_retryable_503_not_a_500() {
    // `spawn_writer` обещает: «Отказ здесь превращается в 503 с
    // Retry-After, и cli дошлёт отметки из собственной очереди». Обещание
    // жило в комментарии чужого крейта и не держалось ничем. На 500 клиент
    // отметки выбросит — то есть переполнение очереди на пике съедало бы
    // рабочее время владельца молча.
    let response = ApiError::from(StoreError::WriteQueueFull).into_response();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let retry = response
        .headers()
        .get(axum::http::header::RETRY_AFTER)
        .expect("нет Retry-After: клиент не узнает, что повтор осмыслен");
    assert!(
        retry.to_str().unwrap().parse::<u32>().is_ok(),
        "Retry-After не число секунд: {retry:?}"
    );
}

#[tokio::test]
async fn serve_actually_answers_on_a_real_socket() {
    // Единственный тест, который проходит через настоящий сокет. Всё
    // остальное здесь зовёт `router` напрямую через `oneshot`, поэтому
    // выпотрошенная `serve` (или потерянный `.await` в ней) не роняла
    // ничего: снаружи это процесс, который «стартовал» и не слушает.
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(wakode_api::serve(listener, a_state(&dir)));

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();

    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "сервер ответил не тем: {response}"
    );
    assert!(response.ends_with("ok"), "нет тела ответа: {response}");

    server.abort();
}

#[tokio::test]
async fn a_valid_key_in_the_basic_header_identifies_the_user() {
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;

    let response = who_am_i_with_authorization(&state, "Basic", &STANDARD.encode(value.to_string())).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"swrneko");
}

#[tokio::test]
async fn the_waka_prefix_is_accepted() {
    // Префикс срезает `ApiKeyValue::parse`: знание о формате ключа живёт
    // в `wakode-auth` целиком, и этот тест — проверка того, что HTTP-слой
    // не завёл собственную копию этого знания.
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;

    let prefixed = STANDARD.encode(format!("waka_{value}"));
    let response = who_am_i_with_authorization(&state, "Basic", &prefixed).await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_key_in_the_query_string_works_too() {
    // wakatime-cli умеет и так; ось «плагины пишут к нам» на этом держится.
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;
    let app = app_requiring_a_key(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/кто-я?api_key={value}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn no_credentials_at_all_is_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let app = app_requiring_a_key(state);

    let response = app
        .oneshot(Request::builder().uri("/кто-я").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_unknown_key_is_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let app = app_requiring_a_key(state);

    let stranger = ApiKeyValue::generate();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/кто-я?api_key={stranger}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // Причина проверяется не ради текста: без неё тест остаётся зелёным и
    // тогда, когда ключ из query вообще не читается — «не предъявлен» это
    // тоже 401, и тест доказывал бы не то, что обещает именем.
    let json = json_body(response).await;
    let message = json["error"].as_str().unwrap();
    assert!(
        message.contains("не найден"),
        "ключ не дошёл до поиска в базе: {message}"
    );
}

#[tokio::test]
async fn a_revoked_key_says_so_instead_of_pretending_it_never_existed() {
    // Владелец, отозвавший ключ и забывший об этом, иначе будет искать
    // поломку в редакторе. Хранилище это различает — терять различие
    // на пути наружу нельзя.
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;

    let key = state.store.first_key().await.unwrap().unwrap();
    state.store.revoke_key(key.id).await.unwrap();

    let app = app_requiring_a_key(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/кто-я?api_key={value}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let json = json_body(response).await;
    let message = json["error"].as_str().unwrap();
    assert!(message.contains("отозван"), "причина не названа: {message}");
}

#[tokio::test]
async fn without_a_master_key_the_answer_is_honest_not_a_panic() {
    // После шага 4 старта это состояние недостижимо. Но недостижимое
    // сегодня становится достижимым при первом рефакторинге старта, и
    // паника в экстракторе — худший способ об этом узнать.
    let dir = tempfile::tempdir().unwrap();
    let store = a_store(&dir);
    let app = app_requiring_a_key(AppState::new(store, None, a_settings()));

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/кто-я?api_key={}", ApiKeyValue::generate()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn each_key_identifies_its_own_owner() {
    // Набор с единственным пользователем оставляет зелёным `user_by_id`,
    // подменённый на «взять владельца первого ключа»: ровно такой дефект
    // (`WHERE key_lookup = ?1` → `WHERE ?1 IS NOT NULL`) прошёл мимо тестов
    // в прошлом плане, потому что строка в таблице была одна.
    let dir = tempfile::tempdir().unwrap();
    let master = MasterKey::generate();
    let store = a_store(&dir);
    let (first, _) = a_user_with_a_key(&store, &master, "первый").await;
    let (second, _) = a_user_with_a_key(&store, &master, "вторая").await;
    let state = AppState::new(store, Some(master), a_settings());

    for (value, owner) in [(first, "первый"), (second, "вторая")] {
        let response = app_requiring_a_key(state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/кто-я?api_key={value}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            owner,
            "ключ опознал не своего владельца"
        );
    }
}

#[tokio::test]
async fn key_auth_carries_the_id_of_the_key_that_opened_it() {
    // Поле никем ещё не читается, и `key_id: Uuid::nil()` прошёл бы весь
    // набор. По нему пойдёт журналирование и отзыв ключа «изнутри» —
    // молча нулевой идентификатор будет стоить расследования.
    let dir = tempfile::tempdir().unwrap();
    let master = MasterKey::generate();
    let store = a_store(&dir);
    let (value, key) = a_user_with_a_key(&store, &master, "swrneko").await;
    let app = app_requiring_a_key(AppState::new(store, Some(master), a_settings()));

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/каким-ключом?api_key={value}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(String::from_utf8(body.to_vec()).unwrap(), key.id.to_string());
}

#[tokio::test]
async fn the_authorization_scheme_is_case_insensitive() {
    // RFC 7235 объявляет имя схемы регистронезависимым, а `strip_prefix`
    // регистр различает. Плагин, шлющий `basic`, получал бы 401 без
    // единой зацепки в логе.
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;

    let encoded = STANDARD.encode(value.to_string());
    for scheme in ["Basic", "basic", "BASIC"] {
        let response = who_am_i_with_authorization(&state, scheme, &encoded).await;
        assert_eq!(response.status(), StatusCode::OK, "схема {scheme} не принята");
    }

    // `Bearer` шлёт часть плагинов; заодно это единственное место, где
    // проверяется, что такая схема вообще разбирается.
    for scheme in ["Bearer", "bearer", "BEARER"] {
        let response = who_am_i_with_authorization(&state, scheme, &value.to_string()).await;
        assert_eq!(response.status(), StatusCode::OK, "схема {scheme} не принята");
    }
}

/// Ответ на запрос к `/кто-я` с произвольным URI и произвольным набором
/// заголовков. Нужен тестам, где важно, что заголовок и query спорят.
async fn who_am_i(state: &AppState, uri: &str, headers: &[(&str, &str)]) -> Response {
    let mut request = Request::builder().uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }

    app_requiring_a_key(state.clone())
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_foreign_authorization_header_does_not_cancel_the_key_in_the_query() {
    // Владелец ставит перед wakode nginx с собственным basic-auth, а
    // wakatime-cli кладёт ключ в query. Разбор заголовка обрывался на
    // первой же неудаче и выходил из всей функции — все отметки получали
    // 401, причём с ответом «ключ не предъявлен», хотя он был предъявлен.
    // Именно такой ответ и отправляет владельца чинить не то.
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;
    let uri = format!("/кто-я?api_key={value}");

    let proxy_basic = format!("Basic {}", STANDARD.encode("admin:secret"));
    let foreign = [
        // Прокси со своим basic-auth: заголовок разбирается, но ключ не наш.
        proxy_basic.as_str(),
        // Не base64 вовсе.
        "Basic ??? не base64 ???",
        // Схема, о которой мы ничего не знаем.
        "Digest username=\"admin\", realm=\"wakode\"",
        // Заголовок без схемы вообще.
        "простотекст",
    ];

    for header in foreign {
        let response = who_am_i(&state, &uri, &[("authorization", header)]).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "заголовок {header:?} погасил ключ из query"
        );
    }
}

#[tokio::test]
async fn a_non_ascii_authorization_header_does_not_cancel_the_key_either() {
    // Отдельно от предыдущего: заголовок, который вообще не представим
    // строкой, отсекается раньше разбора схемы, и это была своя ветка
    // выхода из всей функции.
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;

    let mut request = Request::builder().uri(format!("/кто-я?api_key={value}"));
    request = request.header(
        "authorization",
        axum::http::HeaderValue::from_bytes(&[0x42, 0xff, 0xfe]).unwrap(),
    );

    let response = app_requiring_a_key(state)
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_basic_scheme_accepts_the_login_password_form() {
    // Basic-схема по RFC — это `логин:пароль`. wakatime-cli шлёт голый
    // ключ, но плагин, положивший его в поле логина, обязан работать:
    // строка, отрезающая хвост после двоеточия, ради этого и стоит.
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;

    let with_colon = STANDARD.encode(format!("{value}:"));
    let response = who_am_i_with_authorization(&state, "Basic", &with_colon).await;
    assert_eq!(response.status(), StatusCode::OK, "форма `ключ:` не принята");

    let with_password = STANDARD.encode(format!("{value}:неважно"));
    let response = who_am_i_with_authorization(&state, "Basic", &with_password).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "форма `ключ:пароль` не принята"
    );
}

#[tokio::test]
async fn spaces_around_the_credentials_are_tolerated_in_both_schemes() {
    // Обе схемы терпимы к пробелам, но по разным причинам, и обе причины
    // записаны комментариями в `api_key.rs` — значит, обе обязаны быть
    // покрыты, иначе комментарий утверждает больше, чем держит код.
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;

    // `Basic`: `trim` стоит до декодирования. В base64 пробелы не значат
    // ничего, а `ApiKeyValue::parse` их уже не увидит — он получит
    // результат декодирования, а не исходную строку.
    let padded = format!("  {}  ", STANDARD.encode(value.to_string()));
    let response = who_am_i_with_authorization(&state, "Basic", &padded).await;
    assert_eq!(response.status(), StatusCode::OK, "Basic не стерпел пробелы");

    // `Bearer`: своего `trim` нет и не нужно — пробелы срезает
    // `ApiKeyValue::parse`, которому значение достаётся как есть.
    let response = who_am_i_with_authorization(&state, "Bearer", &format!(" {value} ")).await;
    assert_eq!(response.status(), StatusCode::OK, "Bearer не стерпел пробелы");
}

#[tokio::test]
async fn only_the_parameter_named_api_key_is_taken() {
    // Имя параметра не проверялось ни одним тестом: разбор, берущий
    // первый попавшийся параметр, проходил весь набор зелёным. Такой
    // разбор принял бы `?redirect=...` за ключ и отвечал бы «неверный
    // формат» вместо «не предъявлен».
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;

    // Ключ не первый — соседний параметр не должен его заслонить.
    let response = who_am_i(&state, &format!("/кто-я?плагин=vim&api_key={value}"), &[]).await;
    assert_eq!(response.status(), StatusCode::OK);

    // Похожее имя — не то же самое имя.
    let response = who_am_i(&state, &format!("/кто-я?not_api_key={value}"), &[]).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let json = json_body(response).await;
    assert!(
        json["error"].as_str().unwrap().contains("не предъявлен"),
        "причина не та: {json}"
    );
}

#[tokio::test]
async fn a_malformed_key_says_so_instead_of_pretending_it_was_not_found() {
    // «Не разобрали» и «не нашли» — разные события: первое означает, что
    // плагин настроен неправильно, второе — что ключ отозван или база не
    // та. Различие есть в коде и до этого теста не держалось ничем.
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;

    let response = who_am_i(&state, "/кто-я?api_key=это-не-uuid", &[]).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let json = json_body(response).await;
    let message = json["error"].as_str().unwrap();
    assert!(message.contains("формат"), "причина не названа: {message}");
}

/// Пробный маршрут: единственный смысл — потребовать `SessionAuth`.
///
/// Маршрутов два по той же причине, что и у `KeyAuth`: `session_id` не
/// читается больше ниоткуда, и без второго маршрута `Uuid::nil()` прошёл бы
/// весь набор зелёным.
fn app_requiring_a_session(state: AppState) -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route(
            "/я",
            get(|auth: wakode_api::auth::SessionAuth| async move { auth.user.login }),
        )
        .route(
            "/какая-сессия",
            get(|auth: wakode_api::auth::SessionAuth| async move { auth.session_id.to_string() }),
        )
        .with_state(state)
}

/// Завести сессию с заданным сроком и вернуть её токен.
async fn a_session(state: &AppState, user_id: uuid::Uuid, expires_at: Micros) -> SessionToken {
    let token = SessionToken::generate();
    state
        .store
        .create_session(NewSession {
            user_id,
            token_hash: token.hash(),
            user_agent: Some("Firefox".to_owned()),
            expires_at,
        })
        .await
        .unwrap();
    token
}

/// Момент, отстоящий от **настоящего** «сейчас» на заданное число секунд.
///
/// Сроки сессий в тестах считаются отсюда, а не константами вроде
/// `Micros::from_secs(4_000_000_000)`. Разница не косметическая: с
/// абсолютными константами набор доказывал лишь то, что часы сервера
/// находятся где-то между 1970 и 2096 годом. Часы, замороженные на 2033-м
/// или отставшие на десять лет, проходили весь набор зелёными — а на живом
/// инстансе с уехавшим RTC (типовое на бездисковых VM и в контейнерах)
/// сессия со сроком в 30 дней не истекает никогда, и украденная cookie
/// живёт вечно. Со смещением от «сейчас» окно доказательства сужается со
/// ста двадцати шести лет до секунд.
fn from_now(secs: i64) -> Micros {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("часы теста до эпохи");
    Micros::new(now.as_micros() as i64 + secs * 1_000_000)
}

/// Срок, до которого сессия заведомо жива.
fn live() -> Micros {
    from_now(3600)
}

/// Срок, который заведомо истёк.
fn expired() -> Micros {
    from_now(-1)
}

/// Заголовок `cookie` с токеном сессии.
///
/// Собирается строкой, а не через `Cookie::new`: тесту важно ровно то, что
/// приезжает по проводу, а не то, как его умеет собрать та же библиотека,
/// которой значение потом разбирают.
fn session_cookie(token: &SessionToken) -> String {
    format!("{}={token}", wakode_api::auth::SESSION_COOKIE)
}

/// Ответ пробного маршрута на запрос с заданным заголовком `cookie`.
/// Заголовок подставляется как есть — тестам про соседние cookie и про
/// мусорное значение нужно управлять им целиком.
async fn me_with_cookie(state: &AppState, cookie: &str) -> Response {
    app_requiring_a_session(state.clone())
        .oneshot(
            Request::builder()
                .uri("/я")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn a_live_session_identifies_the_user() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let token = a_session(&state, user.id, live()).await;

    let response = me_with_cookie(&state, &session_cookie(&token)).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"swrneko");
}

#[tokio::test]
async fn an_expired_session_is_refused() {
    // Хранилище отдаёт `expires_at` как есть — доменной валидации в нём нет
    // по построению. Не проверить срок здесь значит пускать по вечным
    // сессиям.
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let token = a_session(&state, user.id, expired()).await;

    let response = me_with_cookie(&state, &session_cookie(&token)).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // Причина проверяется не ради текста: без неё тест остаётся зелёным и
    // тогда, когда сессия просто не нашлась, — то есть доказывал бы не то,
    // что обещает именем.
    let json = json_body(response).await;
    let message = json["error"].as_str().unwrap();
    assert!(message.contains("истек"), "причина не та: {message}");
}

#[tokio::test]
async fn a_revoked_session_says_so() {
    // «Отозвана» и «не существует» — разные события: первое владелец сделал
    // сам (вышел на другом устройстве), второе означает чужой или устаревший
    // токен. Хранилище это различает, и терять различие на пути наружу
    // незачем: токен предъявляет тот, у кого он и так есть.
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let token = a_session(&state, user.id, live()).await;

    let found = state
        .store
        .session_by_token_hash(token.hash())
        .await
        .unwrap()
        .unwrap();
    state.store.revoke_session(found.id).await.unwrap();

    let response = me_with_cookie(&state, &session_cookie(&token)).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let json = json_body(response).await;
    let message = json["error"].as_str().unwrap();
    assert!(message.contains("отозв"), "причина не та: {message}");
}

#[tokio::test]
async fn a_session_both_revoked_and_expired_says_it_was_revoked() {
    // Порядок проверок — обещание комментария в `session.rs`, а без этого
    // теста он не держится ничем: сессия, отозванная и просроченная разом,
    // при перестановке проверок молча начала бы отвечать «истекла».
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let token = a_session(&state, user.id, expired()).await;

    let found = state
        .store
        .session_by_token_hash(token.hash())
        .await
        .unwrap()
        .unwrap();
    state.store.revoke_session(found.id).await.unwrap();

    let response = me_with_cookie(&state, &session_cookie(&token)).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let json = json_body(response).await;
    let message = json["error"].as_str().unwrap();
    assert!(message.contains("отозв"), "названа не та причина: {message}");
}

#[tokio::test]
async fn no_cookie_is_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;

    let response = app_requiring_a_session(state)
        .oneshot(Request::builder().uri("/я").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let json = json_body(response).await;
    assert!(
        json["error"].as_str().unwrap().contains("не предъявлена"),
        "причина не та: {json}"
    );
}

#[tokio::test]
async fn an_unknown_token_is_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let stranger = SessionToken::generate();

    let response = me_with_cookie(&state, &session_cookie(&stranger)).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // Без проверки причины тест остаётся зелёным и тогда, когда cookie
    // вообще не читается: «не предъявлена» — это тоже 401.
    let json = json_body(response).await;
    assert!(
        json["error"].as_str().unwrap().contains("не найдена"),
        "токен не дошёл до поиска в базе: {json}"
    );
}

#[tokio::test]
async fn a_garbage_cookie_says_the_format_is_wrong() {
    // «Не разобрали» и «не нашли» — разные события: первое означает, что
    // cookie испортил кто-то по дороге, второе — что сессия закончилась.
    // Различие есть в коде, и держаться оно обязано тестом.
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;

    let response = me_with_cookie(
        &state,
        &format!("{}=not-a-real-token", wakode_api::auth::SESSION_COOKIE),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let json = json_body(response).await;
    assert!(
        json["error"].as_str().unwrap().contains("формат"),
        "причина не та: {json}"
    );

    // Значение не из ASCII до разбора токена не доходит вовсе: заголовок
    // не представим строкой, и `CookieJar` его молча пропускает — ответ
    // получается «сессия не предъявлена». Отдельная причина сюда не
    // дотягивается, но 401 обязан остаться 401.
    let response = me_with_cookie(
        &state,
        &format!("{}=это-не-токен", wakode_api::auth::SESSION_COOKIE),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_session_cookie_is_found_among_its_neighbours() {
    // В браузере cookie никогда не приезжает одна. Разбор, берущий из
    // заголовка первую пару, проходил бы весь остальной набор зелёным и
    // ломался бы ровно у настоящего браузера — этот тест держит только его.
    // (Разбор, берущий заголовок целиком, ломает и `a_live_session_...`;
    // он тут ни при чём.)
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let token = a_session(&state, user.id, live()).await;

    let crowded = format!("theme=dark; {}; lang=ru", session_cookie(&token));
    let response = me_with_cookie(&state, &crowded).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"swrneko");
}

/// Завести пользователя без ключа: сессии ключа не требуют.
async fn a_user(store: &SqliteStore, login: &str) -> wakode_store::User {
    store
        .create_user(NewUser {
            login: login.to_owned(),
            email: None,
            password_hash: "непрозрачно".to_owned(),
            display_name: None,
            timezone: "Europe/Moscow".parse().unwrap(),
            timeout_secs: 900,
            is_admin: false,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn each_session_identifies_its_own_owner() {
    // Набор с единственным пользователем оставляет зелёным `user_by_id`,
    // подменённый на «взять любого»: ровно такой дефект уже проходил мимо
    // тестов дважды — в задаче 10 прошлого плана и в задаче 10 этого.
    let dir = tempfile::tempdir().unwrap();
    let store = a_store(&dir);
    let first = a_user(&store, "первый").await;
    let second = a_user(&store, "вторая").await;
    let state = AppState::new(store, None, a_settings());

    let tokens = [
        (a_session(&state, first.id, live()).await, "первый"),
        (a_session(&state, second.id, live()).await, "вторая"),
    ];

    for (token, owner) in tokens {
        let response = me_with_cookie(&state, &session_cookie(&token)).await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            owner,
            "сессия опознала не своего владельца"
        );
    }
}

#[tokio::test]
async fn session_auth_carries_the_id_of_the_session_that_opened_it() {
    // Поле никем ещё не читается, и `session_id: Uuid::nil()` прошёл бы
    // весь набор. По нему пойдёт выход из системы («отозвать текущую
    // сессию») — молча нулевой идентификатор отзовёт не ту.
    let dir = tempfile::tempdir().unwrap();
    let store = a_store(&dir);
    let user = a_user(&store, "swrneko").await;
    let state = AppState::new(store, None, a_settings());

    // Сессий две: с единственной строкой в таблице «взять любую» неотличимо
    // от «взять свою».
    let _other = a_session(&state, user.id, live()).await;
    let token = a_session(&state, user.id, live()).await;
    let session = state
        .store
        .session_by_token_hash(token.hash())
        .await
        .unwrap()
        .unwrap();

    let response = app_requiring_a_session(state.clone())
        .oneshot(
            Request::builder()
                .uri("/какая-сессия")
                .header("cookie", session_cookie(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        String::from_utf8(body.to_vec()).unwrap(),
        session.id.to_string()
    );
}

/// Запрос с подставленным адресом клиента: `ConnectInfo` в тестах не
/// появляется сам — его кладёт слой, которого при `oneshot` нет.
fn with_peer(mut request: Request<Body>, peer: &str) -> Request<Body> {
    let addr: std::net::SocketAddr = peer.parse().unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(addr));
    request
}

/// Тело запроса первичной настройки с заданным логином.
fn setup_body(login: &str) -> Body {
    Body::from(format!(
        r#"{{"login":"{login}","password":"достаточно длинный","timezone":"Europe/Moscow"}}"#
    ))
}

/// Запрос настройки с заданным телом, пришедший с заданного адреса.
async fn setup_from(state: AppState, peer: &str, body: Body) -> Response {
    router(state)
        .oneshot(with_peer(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .body(body)
                .unwrap(),
            peer,
        ))
        .await
        .unwrap()
}

/// Ответ `/api/setup/status` как JSON.
async fn setup_status(state: AppState) -> serde_json::Value {
    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

#[tokio::test]
async fn setup_is_needed_while_the_database_has_no_users() {
    let dir = tempfile::tempdir().unwrap();

    let json = setup_status(a_state(&dir)).await;

    assert_eq!(json["needed"], serde_json::json!(true));
}

#[tokio::test]
async fn setup_from_loopback_creates_the_first_admin() {
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);
    let store = state.store.clone();

    let response = setup_from(state, "127.0.0.1:54321", setup_body("swrneko")).await;

    assert_eq!(response.status(), StatusCode::CREATED);
    let json = json_body(response).await;

    let created = store.user_by_login("swrneko").await.unwrap().unwrap();
    assert_eq!(
        json["id"],
        serde_json::json!(created.id.to_string()),
        "ответ вернул не идентификатор заведённого пользователя: {json}"
    );
    assert!(
        created.is_admin,
        "первый пользователь обязан быть администратором"
    );
    assert_ne!(
        created.password_hash, "достаточно длинный",
        "пароль сохранён как есть вместо хеша"
    );
}

#[tokio::test]
async fn setup_from_a_foreign_address_is_refused_by_default() {
    // Окно между стартом и первым входом — это окно, в которое чужой
    // занимает инстанс. Дефолт закрыт.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);
    let store = state.store.clone();

    let response = setup_from(state, "203.0.113.7:40000", setup_body("чужой")).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    // Отказ обязан быть отказом и в базе: 403 с уже заведённым
    // администратором был бы худшим из исходов — инстанс занят, а лог
    // говорит, что не занят.
    assert_eq!(
        store.user_count().await.unwrap(),
        0,
        "чужой запрос отклонён по коду, но пользователя завёл"
    );
}

#[tokio::test]
async fn a_foreign_address_is_allowed_when_the_owner_says_so() {
    // Зеркало предыдущего: без него «запрещать всегда» прошло бы проверку
    // на запрет и выглядело бы правильным.
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(a_store(&dir), None, AppSettings { setup_from_any_address: true, ..a_settings() });

    let response = setup_from(state, "203.0.113.7:40000", setup_body("за-прокси")).await;

    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn an_ipv6_loopback_is_loopback_too() {
    // Стек по умолчанию слушает и `::`, и петлевой клиент приезжает как
    // `::1`. Проверка через сравнение с `127.0.0.1` прошла бы весь
    // остальной набор зелёной и отказывала бы владельцу на его же машине.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);

    let response = setup_from(state, "[::1]:54321", setup_body("шестая-версия")).await;

    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn setup_closes_forever_after_the_first_user() {
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);

    let first = setup_from(state.clone(), "127.0.0.1:54321", setup_body("первый")).await;
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = setup_from(state.clone(), "127.0.0.1:54322", setup_body("второй")).await;
    assert_eq!(second.status(), StatusCode::FORBIDDEN);
    assert!(
        state.store.user_by_login("второй").await.unwrap().is_none(),
        "второй администратор всё-таки завёлся"
    );

    // Текст, а не только код: `403` здесь и `403` за чужой адрес — разные
    // события, и владелец, ткнувший в закрытый экран, должен прочитать
    // именно то, которое случилось. До этой проверки подмена текста этой
    // ветки проходила зелёной.
    let json = json_body(second).await;
    assert!(
        json["error"].as_str().unwrap().contains("уже выполнена"),
        "причина не названа: {json}"
    );

    let json = setup_status(state).await;
    assert_eq!(json["needed"], serde_json::json!(false));
}

#[tokio::test]
async fn the_address_is_checked_before_the_database() {
    // Держит порядок проверок, записанный комментарием в `setup.rs`.
    // Без этого теста комментарий утверждал бы порядок, который свободно
    // переставляется, — а вместе с ним уезжает и обещание «чужой запрос
    // не ходит в базу».
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);
    setup_from(state.clone(), "127.0.0.1:54321", setup_body("первый")).await;

    let response = setup_from(state, "203.0.113.7:40000", setup_body("чужой")).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let json = json_body(response).await;
    let message = json["error"].as_str().unwrap();
    assert!(
        message.contains("локального адреса"),
        "проверка адреса ушла ниже обращения к базе: {message}"
    );
}

/// Тело настройки с произвольным логином и паролем.
fn setup_body_with(login: &str, password: &str) -> Body {
    Body::from(format!(
        r#"{{"login":"{login}","password":"{password}","timezone":"Europe/Moscow"}}"#
    ))
}

#[tokio::test]
async fn an_empty_password_is_refused() {
    // Инстанс в момент настройки ещё открыт: администратор с пустым
    // паролем — это занятый чужим инстанс с эндпоинтом, закрытым навсегда.
    // Отказ обязан быть 400 с внятным текстом, а не 500 и не «непонятно».
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);
    let store = state.store.clone();

    let response = setup_from(state, "127.0.0.1:54321", setup_body_with("swrneko", "")).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json = json_body(response).await;
    assert!(
        json["error"].as_str().unwrap().contains("пароль"),
        "причина не названа: {json}"
    );
    assert_eq!(
        store.user_count().await.unwrap(),
        0,
        "администратор с пустым паролем всё-таки завёлся"
    );
}

#[tokio::test]
async fn a_short_password_is_refused_too() {
    // Отдельно от пустого: проверка `is_empty()` прошла бы предыдущий тест
    // и пропустила бы пароль в один символ.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);

    let response = setup_from(state, "127.0.0.1:54321", setup_body_with("swrneko", "1234567")).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_password_of_exactly_the_minimum_length_is_accepted() {
    // Граница проверяется с обеих сторон: `>` вместо `>=` отрезал бы
    // ровно допустимый пароль, и владелец не понял бы, почему.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);

    let response = setup_from(state, "127.0.0.1:54321", setup_body_with("swrneko", "12345678")).await;

    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn an_empty_login_is_refused() {
    // Логин из ничего или из одних пробелов — это учётная запись, в
    // которую нельзя войти с формы, заведённая на эндпоинте, который
    // после неё закрывается навсегда.
    let dir = tempfile::tempdir().unwrap();

    for login in ["", "   "] {
        let dir_for_case = tempfile::tempdir_in(dir.path()).unwrap();
        let state = a_state(&dir_for_case);
        let store = state.store.clone();

        let response = setup_from(
            state,
            "127.0.0.1:54321",
            setup_body_with(login, "достаточно длинный"),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "логин {login:?} принят"
        );
        let json = json_body(response).await;
        assert!(
            json["error"].as_str().unwrap().contains("логин"),
            "причина не названа: {json}"
        );
        assert_eq!(
            store.user_count().await.unwrap(),
            0,
            "администратор с логином {login:?} всё-таки завёлся"
        );
    }
}

#[tokio::test]
async fn a_login_is_stored_without_its_stray_spaces() {
    // Логин, набранный со случайным пробелом по краю, иначе навсегда
    // останется недостижимым с формы входа: экран настройки к тому моменту
    // уже закрыт.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);
    let store = state.store.clone();

    let response = setup_from(
        state,
        "127.0.0.1:54321",
        setup_body_with("  swrneko  ", "достаточно длинный"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(
        store.user_by_login("swrneko").await.unwrap().is_some(),
        "логин сохранён вместе с пробелами"
    );
}

#[tokio::test]
async fn the_created_password_verifies() {
    // «Хеш не равен открытому паролю» — слишком слабое утверждение: ему
    // удовлетворяет и произвольный мусор, и хеш от другой строки. Такой
    // администратор не смог бы войти никогда, а узнал бы об этом на живом
    // инстансе, где эндпоинт уже закрыт навсегда.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);
    let store = state.store.clone();

    let response = setup_from(state, "127.0.0.1:54321", setup_body("swrneko")).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let created = store.user_by_login("swrneko").await.unwrap().unwrap();
    assert!(
        wakode_auth::verify_password("достаточно длинный", &created.password_hash).unwrap(),
        "сохранённый хеш не открывается присланным паролем"
    );
    assert!(
        !wakode_auth::verify_password("другой пароль", &created.password_hash).unwrap(),
        "хеш открывается чем угодно"
    );
}

#[tokio::test]
async fn a_bad_timezone_is_a_bad_request_not_a_500() {
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);

    let response = setup_from(
        state,
        "127.0.0.1:54321",
        Body::from(
            r#"{"login":"кто","password":"достаточно длинный","timezone":"Марс/Олимп"}"#,
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json = json_body(response).await;
    let message = json["error"].as_str().unwrap();
    assert!(message.contains("таймзона"), "причина не названа: {message}");

    // Текст собирается из пользовательского ввода, и до этой проверки
    // подстановка `request.password` вместо `request.timezone` не роняла
    // ничего. Сегодня это было бы эхо клиенту самому себе, но задача 13
    // добавляет журналирование — и тогда ценой той же опечатки стал бы
    // пароль администратора в логе.
    assert!(
        message.contains("Марс/Олимп"),
        "в сообщении нет самой таймзоны: {message}"
    );
    assert!(
        !message.contains("достаточно длинный"),
        "в сообщение уехал пароль: {message}"
    );
}

#[tokio::test]
async fn a_wrong_method_on_setup_is_a_json_error_too() {
    // Маршрут настройки добавлен выше `method_not_allowed_fallback` —
    // иначе он остался бы с пустым 405 axum'а мимо `ApiError`. Обещание
    // «тело всегда JSON» держится порядком строк в `router`, а порядок —
    // этим тестом.
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/setup")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let json = json_body(response).await;
    assert!(json.get("error").is_some(), "нет поля error: {json}");
}

#[tokio::test]
async fn a_broken_body_is_a_json_error_not_a_bare_400() {
    // `ApiError` обещает «тело всегда JSON с полем error», и обещание это
    // общее для маршрута, а не только для веток, которые пишет обработчик.
    // Отказ самого экстрактора приезжает мимо `ApiError` и отдаёт
    // `text/plain` — для совместимого клиента это неотличимо от сломанного
    // сервера.
    let dir = tempfile::tempdir().unwrap();

    let cases = [
        // Не JSON вовсе.
        Body::from("это не json"),
        // JSON, но не тот: поля `timezone` нет.
        Body::from(r#"{"login":"кто","password":"достаточно длинный"}"#),
    ];

    for body in cases {
        let dir_for_case = tempfile::tempdir_in(dir.path()).unwrap();
        let response = setup_from(a_state(&dir_for_case), "127.0.0.1:54321", body).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let json = json_body(response).await;
        assert!(json.get("error").is_some(), "нет поля error: {json}");
    }
}

#[tokio::test]
async fn a_foreign_address_is_refused_before_the_body_is_even_read() {
    // Разбор тела стоит после адресной проверки не случайно: экстрактор в
    // сигнатуре обработчика отработал бы раньше первой строки тела
    // функции, и чужой с кривым телом услышал бы про формат JSON вместо
    // «сюда нельзя».
    let dir = tempfile::tempdir().unwrap();

    let response = setup_from(
        a_state(&dir),
        "203.0.113.7:40000",
        Body::from("это не json"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn setup_over_a_real_socket_sees_the_client_address() {
    // Второй тест, проходящий через настоящий сокет, и единственный, кто
    // держит `into_make_service_with_connect_info` в `serve`. С обычным
    // `into_make_service` расширения `ConnectInfo` в запросе нет, извлечь
    // его нечем — и настройка с петлевого адреса, то есть единственный
    // способ поднять инстанс, отвечала бы 500. Через `oneshot` этого не
    // видно: там адрес кладёт сам тест.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);
    let store = state.store.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(wakode_api::serve(listener, state));

    let body = r#"{"login":"swrneko","password":"достаточно длинный","timezone":"Europe/Moscow"}"#;
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!(
                "POST /api/setup HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();

    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();

    assert!(
        response.starts_with("HTTP/1.1 201 Created"),
        "настройка с петлевого адреса не прошла: {response}"
    );
    // Код ответа сам по себе не доказывает, что дошло до базы.
    let created = store.user_by_login("swrneko").await.unwrap().unwrap();
    assert!(created.is_admin, "заведён не администратор");

    server.abort();
}

#[tokio::test]
async fn the_password_threshold_counts_characters_not_bytes() {
    // Порог обещан пользователю в символах. Кириллический пароль в шесть
    // символов — это двенадцать байт, и проверка по `len()` пропустила бы
    // его, оставшись зелёной на всех остальных тестах.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);

    let response = setup_from(
        state,
        "127.0.0.1:54321",
        setup_body_with("swrneko", "пароли"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn setup_closes_even_when_registration_is_open() {
    // Комментарий в `setup.rs` обещает, что закрытие не зависит от
    // `registration`: здесь заводится администратор, а не обычный аккаунт.
    // Все остальные тесты настройки идут с выключенной регистрацией, и без
    // этого обещание держалось бы только тем, что никто не написал
    // `if !state.registration && count > 0`.
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(a_store(&dir), None, AppSettings { registration: true, ..a_settings() });

    let first = setup_from(state.clone(), "127.0.0.1:54321", setup_body("первый")).await;
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = setup_from(state, "127.0.0.1:54322", setup_body("второй")).await;
    assert_eq!(second.status(), StatusCode::FORBIDDEN);
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
            .route("/жив", axum::routing::get(|| async { "да" })),
    )
}

/// Писатель журнала в память: `tracing_subscriber` берёт по писателю на
/// каждое событие, поэтому буфер общий и под замком.
struct LogBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Выполнить запрос и вернуть ответ вместе с тем, что за это время
/// напечатал журнал.
///
/// Подписчик ставится через `set_default`, а не `init`: он глобальный на
/// весь тестовый бинарь, а тесты бегут параллельно — общий подписчик
/// смешал бы вывод соседей и сделал бы проверки «в журнале нет секрета»
/// случайными. `set_default` действует на текущий поток, а `#[tokio::test]`
/// без `flavor = "multi_thread"` крутит будущее на нём же, так что запрос
/// проходит под этим подписчиком.
///
/// Уровень `TRACE` — чтобы проверять содержимое строк, а не отсечку по
/// уровню; для отсечки есть `response_and_log_at`.
async fn response_and_log(app: axum::Router, request: Request<Body>) -> (Response, String) {
    response_and_log_at(tracing::Level::TRACE, app, request).await
}

/// То же, но с заданным потолком уровня: так проверяется, что запись
/// вообще переживёт боевой фильтр, а не только что в ней нет лишнего.
async fn response_and_log_at(
    level: tracing::Level,
    app: axum::Router,
    request: Request<Body>,
) -> (Response, String) {
    let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_ansi(false)
        .with_writer({
            let buffer = buffer.clone();
            move || LogBuffer(buffer.clone())
        })
        .finish();

    let response = {
        let _guard = tracing::subscriber::set_default(subscriber);
        app.oneshot(request).await.unwrap()
    };

    let log = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    (response, log)
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
