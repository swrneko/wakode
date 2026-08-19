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
        // Намеренно не 900: `wakode_core::DEFAULT_TIMEOUT_SECS` равен
        // именно 900, и с ним любая проверка «тайм-аут пришёл из
        // настроек» была бы вакуумной — прошитая константа дала бы тот же
        // ответ. Ровно эта вакуумность и была найдена финальным ревью на
        // второй двери создания пользователя.
        default_timeout_secs: 777,
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

    let token = wakode_auth::SetupToken::generate();
    let state = AppState::new(store, Some(master.clone()), a_settings())
        .with_setup_token(Some(token.clone()));
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

    // То же самое для выданного токена настройки: его напечатанная форма
    // не должна попасть в дамп состояния ни при каких обстоятельствах.
    assert!(
        !dump.contains(&token.to_string()),
        "токен настройки утёк: {dump}"
    );
    assert!(
        !dump.contains("SetupToken"),
        "поле setup_token выведено через собственный Debug вместо .is_some(): {dump}"
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
async fn a_login_conflict_is_a_409_and_an_empty_login_a_400() {
    // Оба варианта сегодня недостижимы через HTTP: в `setup` логин уже
    // проверен, а `user_count > 0` отсекает дубликат. Регистрация из плана
    // 3b упрётся ровно сюда, и отвечать ей `500` на «такой логин занят»
    // значило бы отправить пользователя писать владельцу вместо того,
    // чтобы выбрать другой логин. Отображение держалось ни на чём —
    // мутация «убрать обе ветки» проходила весь набор зелёной.
    let taken = ApiError::from(StoreError::LoginTaken("swrneko".to_owned())).into_response();
    assert_eq!(taken.status(), StatusCode::CONFLICT);
    let json = json_body(taken).await;
    assert!(json.get("error").is_some(), "тело не JSON с error: {json}");
    assert!(
        !json.to_string().contains("swrneko"),
        "логин уехал клиенту: {json}"
    );

    let empty = ApiError::from(StoreError::LoginEmpty).into_response();
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
    let json = json_body(empty).await;
    assert!(json.get("error").is_some(), "тело не JSON с error: {json}");
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

    let server = tokio::spawn(wakode_api::serve(
        listener,
        a_state(&dir),
        std::future::pending::<()>(),
    ));

    let response = raw_get(addr, "/healthz").await;

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "сервер ответил не тем: {response}"
    );
    assert!(response.ends_with("ok"), "нет тела ответа: {response}");

    server.abort();
}

/// Сырой `GET` через настоящий сокет, без клиента HTTP.
///
/// Вынесено из `serve_actually_answers_on_a_real_socket`: второму тесту с
/// настоящим сокетом нужен тот же примитив.
async fn raw_get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();

    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

#[tokio::test]
async fn serve_returns_when_asked_to_stop_and_releases_the_port() {
    // Без этого теста завершение по сигналу держится обещанием: `serve`,
    // потерявшая `with_graceful_shutdown`, снаружи выглядит точно так же —
    // сервер работает, — а SIGTERM в бинаре просто убивал бы процесс мимо
    // остановки писателя.
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(wakode_api::serve(listener, a_state(&dir), async move {
        let _ = stopped.await;
    }));

    // Сначала — что сервер вообще поднялся: иначе «вернулась сразу»
    // прошло бы этот тест зелёным.
    assert!(
        raw_get(addr, "/healthz").await.starts_with("HTTP/1.1 200 OK"),
        "сервер не ответил до сигнала"
    );

    stop.send(()).unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("serve не вернулась через пять секунд после сигнала")
        .expect("задача с serve упала");

    // Порт отпущен — доказательство, что слушатель уничтожен, а не просто
    // функция вернулась, оставив приём соединений жить.
    tokio::net::TcpListener::bind(addr)
        .await
        .expect("порт всё ещё занят: слушатель пережил останов");
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

/// Запрос настройки с петлевого адреса и заданными доп. заголовками.
async fn setup_from_loopback_with(state: AppState, headers: &[(&str, &str)]) -> Response {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/setup")
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }

    router(state)
        .oneshot(with_peer(
            request.body(setup_body("через-прокси")).unwrap(),
            "127.0.0.1:54321",
        ))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_loopback_peer_is_not_enough_when_the_request_came_through_a_proxy() {
    // Дыра, найденная финальным ревью ветки, и она не теоретическая. При
    // штатной установке — nginx на том же хосте, `proxy_pass
    // http://127.0.0.1:9000` — TCP-пиром всегда оказывается сам прокси,
    // то есть `127.0.0.1`. Проверка «пир петлевой» истинна для кого угодно
    // из интернета, и экран первичной настройки стоит открытым до первого
    // постучавшегося. Владелец делает `systemctl start wakode`, идёт
    // заводить админа через браузер — а инстанс уже занят.
    let dir = tempfile::tempdir().unwrap();

    for header in [
        "x-forwarded-for",
        "forwarded",
        "x-forwarded-proto",
        "x-forwarded-host",
        "x-real-ip",
        "via",
    ] {
        // Папка держится в переменной: `a_state(&tempfile::tempdir()...)`
        // удаляет её в конце того же выражения, и тест проходил бы только
        // потому, что `403` отдаётся до похода в базу. Стоило бы
        // переставить проверки — и он краснел бы `500`-кой вместо
        // внятного утверждения.
        let dir = tempfile::tempdir().unwrap();
        let state = a_state(&dir);
        let response = setup_from_loopback_with(state, &[(header, "203.0.113.7")]).await;

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "заголовок {header} не закрыл настройку"
        );
        let json = json_body(response).await;
        assert!(
            json["error"].as_str().unwrap().contains("прокси"),
            "причина не названа: {json}"
        );
    }

    // Зеркало: без заголовков посредника настройка с петлевого адреса
    // проходит. Без этой половины «запрещать всегда» выглядело бы верным.
    let response = setup_from_loopback_with(a_state(&dir), &[]).await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn the_owner_behind_a_proxy_can_still_say_so() {
    // Заголовок посредника не должен переигрывать явное разрешение
    // владельца: иначе установка за прокси стала бы ненастраиваемой
    // вообще, и починить её было бы можно только через CLI.
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(
        a_store(&dir),
        None,
        AppSettings {
            setup_from_any_address: true,
            ..a_settings()
        },
    );

    let response = setup_from_loopback_with(state, &[("x-forwarded-for", "203.0.113.7")]).await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn an_unknown_field_in_the_setup_body_is_an_error() {
    // Та же логика, что у `deny_unknown_fields` в конфиге (задача 6):
    // опечатка в имени поля иначе молча даёт умолчание. Сегодня безвредно
    // — все три поля обязательны, — но в 3b поля станут необязательными,
    // и `"is_admin": false`, проглоченный молча, будет ровно тем дефектом,
    // который в конфиге уже чинили.
    let dir = tempfile::tempdir().unwrap();

    let response = setup_from(
        a_state(&dir),
        "127.0.0.1:54321",
        Body::from(
            r#"{"login":"кто","password":"достаточно длинный","timezone":"UTC","is_admin":false}"#,
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_created_admin_gets_the_timeout_from_the_settings() {
    // Вторая дверь создания пользователя. Первая (CLI) уже покрыта
    // `the_timeout_from_the_config_reaches_the_created_user`, а эта
    // держалась ни на чём: фикстура задавала 900, что равно
    // `wakode_core::DEFAULT_TIMEOUT_SECS`, и прошитая константа дала бы
    // тот же ответ. Ровно та форма дефекта, из-за которой секция
    // `[durations]` и оказалась мёртвой.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);
    let store = state.store.clone();

    let response = setup_from(state, "127.0.0.1:54321", setup_body("swrneko")).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let created = store.user_by_login("swrneko").await.unwrap().unwrap();
    assert_eq!(
        created.timeout_secs, 777,
        "тайм-аут взят из константы, а не из настроек"
    );
}

/// Ответ `/api/setup/status` как JSON.
async fn setup_status(state: AppState) -> serde_json::Value {
    let response = router(state)
        .oneshot(with_peer(
            Request::builder()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
            "127.0.0.1:54321",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

#[tokio::test]
async fn the_status_asks_for_a_token_when_the_address_alone_would_be_refused() {
    // Токен выдан — иначе поле обязано молчать, см.
    // `an_instance_with_no_issued_token_never_asks_for_one`: требовать то,
    // чего сервер не выдавал, значило бы предъявить экрану поле, которое
    // не примет ни одно значение.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir).with_setup_token(Some(wakode_auth::SetupToken::generate()));
    let app = router(state);

    let response = app
        .oneshot(with_peer(
            Request::builder()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
            "203.0.113.5:41234",
        ))
        .await
        .unwrap();

    let status = json_body(response).await;
    assert_eq!(status["needed"], true);
    assert_eq!(
        status["token_required"], true,
        "чужому адресу настройка без токена не откроется, и статус обязан это сказать"
    );
}

#[tokio::test]
async fn a_loopback_client_without_proxy_headers_needs_no_token() {
    // Зеркало предыдущего. Без него «всегда true» прошло бы: экран
    // настройки на машине владельца спрашивал бы токен, которого он не
    // должен предъявлять. Токен при этом выдан — доказывает, что решает
    // именно адрес, а не отсутствие токена на инстансе.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir).with_setup_token(Some(wakode_auth::SetupToken::generate()));
    let app = router(state);

    let response = app
        .oneshot(with_peer(
            Request::builder()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
            "127.0.0.1:41234",
        ))
        .await
        .unwrap();

    let status = json_body(response).await;
    assert_eq!(status["token_required"], false);
}

#[tokio::test]
async fn a_proxy_header_makes_the_status_ask_for_a_token() {
    // Тот самый случай, ради которого всё это: пир петлевой, потому что
    // прокси стоит на том же хосте, а клиент — кто угодно.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir).with_setup_token(Some(wakode_auth::SetupToken::generate()));
    let app = router(state);

    let response = app
        .oneshot(with_peer(
            Request::builder()
                .uri("/api/setup/status")
                .header("x-forwarded-for", "203.0.113.5")
                .body(Body::empty())
                .unwrap(),
            "127.0.0.1:41234",
        ))
        .await
        .unwrap();

    let status = json_body(response).await;
    assert_eq!(status["token_required"], true);
}

#[tokio::test]
async fn an_instance_open_to_any_address_never_asks_for_a_token() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(
        a_store(&dir),
        None,
        AppSettings { setup_from_any_address: true, ..a_settings() },
    )
    .with_setup_token(Some(wakode_auth::SetupToken::generate()));
    let app = router(state);

    let response = app
        .oneshot(with_peer(
            Request::builder()
                .uri("/api/setup/status")
                .header("x-forwarded-for", "203.0.113.5")
                .body(Body::empty())
                .unwrap(),
            "203.0.113.5:41234",
        ))
        .await
        .unwrap();

    let status = json_body(response).await;
    assert_eq!(status["token_required"], false);
}

#[tokio::test]
async fn an_instance_with_no_issued_token_never_asks_for_one() {
    // Инстанс без выпущенного токена не может его требовать: администратор
    // уже заведён (или токен ещё не выдан), и предъявить экрану нечего —
    // поле токена лишь ввело бы владельца в заблуждение, будто где-то есть
    // значение, которое сервер примет.
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir)); // без with_setup_token

    let response = app
        .oneshot(with_peer(
            Request::builder()
                .uri("/api/setup/status")
                .header("x-forwarded-for", "203.0.113.5")
                .body(Body::empty())
                .unwrap(),
            "203.0.113.5:41234",
        ))
        .await
        .unwrap();

    let status = json_body(response).await;
    assert_eq!(
        status["token_required"], false,
        "токена нет, и требовать его статус не должен: {status}"
    );
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
    let server = tokio::spawn(wakode_api::serve(
        listener,
        state,
        std::future::pending::<()>(),
    ));

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

#[tokio::test]
async fn a_correct_token_opens_setup_from_any_address() {
    // Ради этого всё и делается: владелец за обратным прокси заводит
    // администратора, не открывая настройку всему интернету.
    let dir = tempfile::tempdir().unwrap();
    let token = wakode_auth::SetupToken::generate();
    let state = a_state(&dir).with_setup_token(Some(token.clone()));

    let response = router(state)
        .oneshot(with_peer(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .header("x-wakode-setup-token", token.to_string())
                .header("x-forwarded-for", "203.0.113.5")
                .body(setup_body("админ"))
                .unwrap(),
            "127.0.0.1:41234",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn a_wrong_token_is_refused_even_from_a_loopback_address() {
    // Предъявление токена — утверждение «я знаю секрет», и ложное
    // утверждение получает свой отказ. Провалиться в адресную ветку и
    // пройти по петлевому адресу оно не должно: владелец, вставивший
    // токен с опечаткой, иначе не узнал бы об опечатке вовсе, а на
    // следующей машине услышал бы про адрес, держа токен в руках.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir).with_setup_token(Some(wakode_auth::SetupToken::generate()));

    let response = router(state)
        .oneshot(with_peer(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .header("x-wakode-setup-token", "SGVsbG8sIHRoaXMgaXMgbm90IHRoZSB0b2tlbg")
                .body(setup_body("админ"))
                .unwrap(),
            "127.0.0.1:41234",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = json_body(response).await;
    let text = body["error"].as_str().unwrap();
    assert!(text.contains("токен"), "отказ не назвал причину: {text}");
}

#[tokio::test]
async fn a_token_presented_to_an_instance_that_issued_none_is_refused() {
    // Инстанс с уже заведённым администратором токена не выдаёт, и
    // «токена нет» обязано означать отказ, а не «сравнивать не с чем,
    // значит проходи».
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir); // без with_setup_token

    let response = router(state)
        .oneshot(with_peer(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .header(
                    "x-wakode-setup-token",
                    wakode_auth::SetupToken::generate().to_string(),
                )
                .body(setup_body("админ"))
                .unwrap(),
            "203.0.113.5:41234",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn two_setup_token_headers_are_refused() {
    // Урок парковки задачи 11: `CookieJar::get` при дубликатах отдаёт
    // последнюю пару, и «какое из двух значений считается предъявленным»
    // — источник тихих расхождений. Здесь ответ дан явно: два токена —
    // это не предъявление, а попытка угадать, какой из них мы возьмём.
    //
    // Верный токен стоит **первым**, а мусор — вторым, и это не
    // случайность: `HeaderMap::get` при дубликатах отдаёт первое
    // значение, и реализация на нём молча приняла бы верный токен, даже
    // не заметив второго заголовка. Поставь мусор первым — и такая
    // реализация тоже отказала бы, только не по той причине, которую
    // тест называет в имени.
    let dir = tempfile::tempdir().unwrap();
    let token = wakode_auth::SetupToken::generate();
    let state = a_state(&dir).with_setup_token(Some(token.clone()));

    let response = router(state)
        .oneshot(with_peer(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .header("x-wakode-setup-token", token.to_string())
                .header("x-wakode-setup-token", "мусор")
                .body(setup_body("админ"))
                .unwrap(),
            "203.0.113.5:41234",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_refusal_never_echoes_the_presented_token() {
    // Тело отказа уезжает клиенту и попадает в чужие скриншоты. Эхо
    // предъявленного значения — тот же класс дефекта, что подстановка
    // пароля в сообщение о таймзоне, найденная в задаче 12.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir).with_setup_token(Some(wakode_auth::SetupToken::generate()));
    let presented = "SGVsbG8sIHRoaXMgaXMgbm90IHRoZSB0b2tlbg";

    let response = router(state)
        .oneshot(with_peer(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .header("x-wakode-setup-token", presented)
                .body(setup_body("админ"))
                .unwrap(),
            "127.0.0.1:41234",
        ))
        .await
        .unwrap();

    let dump = format!("{:?}", json_body(response).await);
    assert!(
        !dump.contains(presented),
        "предъявленный токен вернулся клиенту: {dump}"
    );
}

#[tokio::test]
async fn without_a_token_the_address_still_decides() {
    // Зеркало всей ветки: токен не должен был отменить прежнюю защиту.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir).with_setup_token(Some(wakode_auth::SetupToken::generate()));

    let response = setup_from(state, "203.0.113.5:41234", setup_body("админ")).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = json_body(response).await;
    assert!(body["error"].as_str().unwrap().contains("локального адреса"));
}
