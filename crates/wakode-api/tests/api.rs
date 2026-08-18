use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use http_body_util::BodyExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;
use wakode_api::{router, ApiError, AppState};
use wakode_auth::{ApiKeyValue, MasterKey};
use wakode_core::{Category, EntityKind, Micros};
use wakode_store::{
    ApiKey, HeartbeatRepo, IncomingHeartbeat, KeyRepo, NewApiKey, NewUser, SqliteStore, StoreError,
    UserRepo,
};

pub fn a_store(dir: &tempfile::TempDir) -> SqliteStore {
    SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap()
}

pub fn a_state(dir: &tempfile::TempDir) -> AppState {
    AppState::new(a_store(dir), None, false, 30, false)
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

    (AppState::new(store, Some(master), false, 30, false), value)
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

    let state = AppState::new(store, Some(master.clone()), false, 30, false);
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
    let app = app_requiring_a_key(AppState::new(store, None, false, 30, false));

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
    let state = AppState::new(store, Some(master), false, 30, false);

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
    let app = app_requiring_a_key(AppState::new(store, Some(master), false, 30, false));

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
