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
    // администратора, не открывая настройку всему интернету. Пир — не
    // петлевой и без единого заголовка посредника: `address_allows_setup`
    // отказала бы уже на первой строке (`!peer.ip().is_loopback()`), и
    // только токен решает исход. Тест с петлевым пиром и заголовком
    // прокси доказывал бы только половину имени — что токен отменяет
    // именно прокси-ветку, а не «любой адрес» буквально.
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
                .body(setup_body("админ"))
                .unwrap(),
            "203.0.113.5:41234",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn an_empty_setup_token_header_is_not_a_presentation() {
    // Ловушка для плана 4: экран настройки отправляет заголовок
    // безусловно и оставляет его пустым, когда `token_required == false`
    // (петлевая машина владельца, токен вводить незачем). Пустое значение
    // обязано провалиться в адресную ветку, а не отказать как неверный
    // токен, — иначе форма ломала бы настройку там, где токен не нужен
    // вовсе. Пир петлевой и без заголовков посредника, поэтому `201`
    // здесь может дать только адресная ветка.
    //
    // Пробельные значения проверяются наравне с пустым: без них `.trim()`
    // в `presented_token` не держался ничем — мутация `let trimmed =
    // value;` проходила по всему workspace зелёной, а заголовок из одних
    // пробелов отказывал бы на машине владельца ровно так же, как пустой.
    // Форма, шлющая `" "`, — не выдумка: пробел легко приезжает вместе с
    // вставкой из буфера.
    //
    // Состояние заводится своё на каждое значение: первый же успешный
    // запрос создаёт администратора и закрывает эндпоинт навсегда.
    for empty in ["", " ", "   ", "\t "] {
        let dir = tempfile::tempdir().unwrap();
        let state = a_state(&dir).with_setup_token(Some(wakode_auth::SetupToken::generate()));

        let response = router(state)
            .oneshot(with_peer(
                Request::builder()
                    .method("POST")
                    .uri("/api/setup")
                    .header("content-type", "application/json")
                    .header("x-wakode-setup-token", empty)
                    .body(setup_body("админ"))
                    .unwrap(),
                "127.0.0.1:41234",
            ))
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "заголовок токена {empty:?} отказал вместо прохода по адресу"
        );
    }
}

#[tokio::test]
async fn an_unreadable_setup_token_header_is_a_presentation_not_an_absence() {
    // Регрессия фикс-раунда 1: `to_str().unwrap_or_default()` на не-UTF-8
    // значении даёт пустую строку — ту же, что у настоящего
    // непредъявления. Пустив её в общую проверку на пустоту, получаем
    // нечитаемый заголовок, неотличимый от отсутствующего: запрос
    // проваливается в адресную ветку, а не отказывает как мусорный токен.
    //
    // Пир — петлевой и без заголовков посредника: `201` здесь может дать
    // только ошибочный провал в адресную ветку, `403` — только то, что
    // нечитаемое значение по-прежнему считается предъявлением.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir).with_setup_token(Some(wakode_auth::SetupToken::generate()));

    let mut request = Request::builder()
        .method("POST")
        .uri("/api/setup")
        .header("content-type", "application/json");
    request = request.header(
        "x-wakode-setup-token",
        axum::http::HeaderValue::from_bytes(&[0xff, 0xfe, 0x80]).unwrap(),
    );

    let response = router(state)
        .oneshot(with_peer(
            request.body(setup_body("админ")).unwrap(),
            "127.0.0.1:41234",
        ))
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "нечитаемый заголовок токена молча провалился в адресную ветку и завёл администратора"
    );
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
    //
    // Пир — петлевой, без единого заголовка посредника: адресная ветка
    // сама по себе пропустила бы запрос. `403` здесь может дать только
    // дедупликация — иначе тест не отличил бы «отказ из-за дубликатов»
    // от «отказ по адресу», и мутация, стирающая факт предъявления при
    // дубликатах (`presented_token` возвращает `None` вместо явного
    // отказа), проходила бы зелёной.
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
            "127.0.0.1:41234",
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

/// Ответ статуса не должен попасть в общий кеш.
///
/// `token_required` считается по адресу пира и по заголовкам посредника,
/// то есть ответ разный для разных клиентов. Кеш перед инстансом отдал бы
/// ответ одного клиента другому: экран настройки спрятал бы поле токена
/// там, где сервер его требует, и форма получила бы `403` без объяснения.
///
/// Почему именно `no-store`, а не `Vary`: `Vary` перечисляет **заголовки**
/// запроса, а решающий вход здесь — адрес TCP-пира, которого в запросе
/// нет вовсе. Ключ кеша, учитывающий его, построить нечем, поэтому
/// единственный честный ответ — не хранить.
#[tokio::test]
async fn the_setup_status_is_never_cached() {
    let dir = tempfile::tempdir().unwrap();

    let response = router(a_state(&dir))
        .oneshot(with_peer(
            Request::builder()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
            "127.0.0.1:41234",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .map(|value| value.to_str().unwrap()),
        Some("no-store"),
        "ответ статуса отдан кешируемым"
    );
}

/// Маршрут вправе объявить себя кешируемым, и слой ему не мешает.
///
/// Зеркало предыдущего, и не для симметрии: без него подмена
/// `if_not_present` на `overriding` не роняет ничего. Цена такой подмены
/// придёт в плане 4 — встроенная SPA раздаёт статику с хешами в именах,
/// и `no-store` поверх неё заставил бы браузер выкачивать бандл на каждое
/// открытие страницы, причём молча.
#[tokio::test]
async fn a_route_that_declares_itself_cacheable_keeps_its_own_header() {
    async fn a_cacheable_asset() -> impl axum::response::IntoResponse {
        ([("cache-control", "public, max-age=31536000, immutable")], "бандл")
    }

    let app = wakode_api::with_layers(
        axum::Router::new().route("/статика", axum::routing::get(a_cacheable_asset)),
    );

    let response = app
        .oneshot(Request::builder().uri("/статика").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .map(|value| value.to_str().unwrap()),
        Some("public, max-age=31536000, immutable"),
        "слой затёр собственный заголовок маршрута"
    );
}

/// Ответ `/api/v1/users/current` с предъявленным ключом.
async fn current_user_response(state: AppState, key: &ApiKeyValue) -> Response {
    router(state)
        .oneshot(
            Request::builder()
                .uri("/api/v1/users/current")
                .header(
                    axum::http::header::AUTHORIZATION,
                    format!("Basic {}", STANDARD.encode(key.to_string())),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn the_current_user_needs_a_key() {
    // Без этого теста маршрут, забывший `KeyAuth`, отдавал бы чужой
    // профиль кому угодно, а тест формы остался бы зелёным: форма-то
    // правильная.
    let dir = tempfile::tempdir().unwrap();
    let (state, _key) = a_state_with_a_key(&dir).await;

    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/api/v1/users/current")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_timeout_is_reported_in_minutes_not_seconds() {
    // Единица — часть контракта, а по форме её не видно: и минуты, и
    // секунды это `number`. Мутация «убрать деление на 60» не роняет
    // сверку формы вообще ничем.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let body = json_body(current_user_response(state, &key).await).await;

    // Пользователь заведён с 900 секундами (`a_user_with_a_key`), а
    // эталон при тех же 900 секундах отдаёт `timeout: 15`.
    assert_eq!(body["data"]["timeout"], 15, "{body}");
}

#[tokio::test]
async fn the_current_user_is_the_owner_of_the_presented_key() {
    // Пользователей двое, и ключ предъявляется вторым: обработчик,
    // берущий «первого попавшегося» или отдающий прошитый профиль, сверку
    // формы прошёл бы зелёным — форма-то правильная.
    let dir = tempfile::tempdir().unwrap();
    let master = MasterKey::generate();
    let store = a_store(&dir);
    a_user_with_a_key(&store, &master, "swrneko").await;

    let neighbour = store
        .create_user(NewUser {
            login: "соседка".to_owned(),
            email: Some("соседка@example.org".to_owned()),
            password_hash: "непрозрачно".to_owned(),
            display_name: Some("Соседка".to_owned()),
            timezone: "Asia/Tokyo".parse().unwrap(),
            timeout_secs: 1_800,
            is_admin: false,
        })
        .await
        .unwrap();

    let value = ApiKeyValue::generate();
    store
        .create_key(NewApiKey {
            user_id: neighbour.id,
            name: "её ноутбук".to_owned(),
            key_encrypted: value.encrypt(&master).unwrap().as_bytes().to_vec(),
            key_lookup: value.lookup(&master),
        })
        .await
        .unwrap();

    let state = AppState::new(store, Some(master), a_settings());
    let body = json_body(current_user_response(state, &value).await).await;

    assert_eq!(body["data"]["id"], neighbour.id.to_string(), "{body}");
    assert_eq!(body["data"]["username"], "соседка", "{body}");
    assert_eq!(body["data"]["display_name"], "Соседка", "{body}");
    assert_eq!(body["data"]["email"], "соседка@example.org", "{body}");
    assert_eq!(body["data"]["timezone"], "Asia/Tokyo", "{body}");
    assert_eq!(body["data"]["timeout"], 30, "{body}");
}

#[tokio::test]
async fn a_user_without_a_display_name_is_shown_by_login() {
    // `display_name` в базе необязателен, а плагин печатает его в
    // статус-баре: пустая строка там хуже логина. `full_name` при этом
    // остаётся пустым — полное имя выдумывать не из чего.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let body = json_body(current_user_response(state, &key).await).await;

    assert_eq!(body["data"]["display_name"], "swrneko", "{body}");
    assert_eq!(body["data"]["full_name"], serde_json::Value::Null, "{body}");
}

#[tokio::test]
async fn the_times_are_the_users_own_and_printed_the_wakatime_way() {
    // Два заявления сразу, и ни одного из них не видно по форме: время
    // подменённое константой — такая же `string`, и сырое число
    // микросекунд, отданное строкой, — тоже. Поэтому здесь напечатанное
    // разбирается обратно и сверяется с тем, что легло в базу.
    //
    // `created_at` и `updated_at` у свежего пользователя равны, и пути
    // обновления в хранилище пока нет: перестановка этих двух полей
    // местами тестом не ловится и ничего не меняет, пока они равны.
    let dir = tempfile::tempdir().unwrap();
    let master = MasterKey::generate();
    let store = a_store(&dir);

    let user = store
        .create_user(NewUser {
            login: "хронометр".to_owned(),
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

    let state = AppState::new(store, Some(master), a_settings());
    let body = json_body(current_user_response(state, &value).await).await;

    for (field, expected) in [("created_at", user.created_at), ("modified_at", user.updated_at)] {
        let printed = body["data"][field]
            .as_str()
            .unwrap_or_else(|| panic!("{field} отдан не строкой: {body}"));
        let parsed = chrono::DateTime::parse_from_rfc3339(printed)
            .unwrap_or_else(|err| panic!("{field} = {printed:?} — не RFC 3339: {err}"));

        assert!(
            printed.ends_with('Z'),
            "{field} = {printed:?}: эталон печатает UTC как `Z`"
        );
        assert_eq!(
            parsed.timestamp(),
            expected.get() / 1_000_000,
            "{field} = {printed:?} — не время этого пользователя"
        );
    }
}

#[tokio::test]
async fn a_wrong_method_on_the_current_user_is_a_json_error_too() {
    // Маршрут добавлен выше `method_not_allowed_fallback`; окажись он
    // ниже — остался бы с пустым 405 axum'а мимо `ApiError`. Порядок
    // строк в `router` держится этим тестом, а не чтением кода.
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/users/current")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let json = json_body(response).await;
    assert!(json.get("error").is_some(), "нет поля error: {json}");
}

/// Ответ на `POST /api/v1/users/current/heartbeats` с предъявленным ключом.
async fn post_heartbeat_response(state: AppState, key: &ApiKeyValue, body: &str) -> Response {
    router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/users/current/heartbeats")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(
                    axum::http::header::AUTHORIZATION,
                    format!("Basic {}", STANDARD.encode(key.to_string())),
                )
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn a_heartbeat_is_accepted_and_gets_an_id() {
    // Отметка **читается обратно**, и это не педантизм. Мутация «не звать
    // `record_heartbeats` вовсе, отдавая свежий `Uuid::now_v7()`» прогнана:
    // без чтения не падал ни один тест всего набора. Ответ правильной формы
    // с правдоподобным идентификатором получается и без единой записи в
    // базу, то есть трекер, который ничего не трекает, проходил бы зелёным.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = post_heartbeat_response(
        state.clone(),
        &key,
        r#"{"entity":"/дом/проект/файл.rs","type":"file","time":1755500000.0}"#,
    )
    .await;

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    assert!(
        uuid::Uuid::parse_str(body["data"]["id"].as_str().unwrap()).is_ok(),
        "идентификатор не UUID: {body}"
    );

    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let stored = state
        .store
        .heartbeats_in_range(
            user.id,
            Micros::from_secs(1_755_499_999),
            Micros::from_secs(1_755_500_001),
        )
        .await
        .unwrap();

    assert_eq!(stored.len(), 1, "отметка не доехала до базы: {stored:?}");
    assert_eq!(stored[0].time, Micros::from_secs(1_755_500_000));
    assert_eq!(
        state.store.resolve(stored[0].attrs.entity).as_deref(),
        Some("/дом/проект/файл.rs")
    );
}

#[tokio::test]
async fn a_duplicate_heartbeat_is_a_success_with_the_zero_id_and_no_second_row() {
    // Форма ответа на повтор снята с живого и записана в
    // `.claude/docs/decisions/duplicate-heartbeats-are-a-success.md`:
    // идентификатор из нулей **с нибблами версии 4**, то есть не
    // `Uuid::nil()`. Свежий идентификатор здесь был бы ложью — строки в
    // базе нет, и клиент, сохранивший его, не нашёл бы по нему ничего.
    //
    // Чего этот тест не утверждает: ответ на повтор у **этого** эндпоинта с
    // живого не снимался вовсе. Измерен только батч, и оттуда взят сам
    // идентификатор; и код `201`, и отсутствие поля `skip` рядом с ним —
    // экстраполяция. Оба расхождения и цена каждого записаны в
    // `.claude/docs/decisions/duplicate-heartbeats-are-a-success.md`.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;
    let body = r#"{"entity":"/дом/проект/файл.rs","type":"file","time":1755500000.0}"#;

    let first = post_heartbeat_response(state.clone(), &key, body).await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first_id = json_body(first).await["data"]["id"].as_str().unwrap().to_owned();

    let second = post_heartbeat_response(state.clone(), &key, body).await;
    assert_eq!(second.status(), StatusCode::CREATED);
    let second_id = json_body(second).await["data"]["id"].as_str().unwrap().to_owned();

    assert_ne!(first_id, second_id, "повтор получил идентификатор вставки");
    assert_eq!(second_id, "00000000-0000-4000-a000-000000000000");

    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let stored = state
        .store
        .heartbeats_in_range(
            user.id,
            Micros::from_secs(1_755_499_999),
            Micros::from_secs(1_755_500_001),
        )
        .await
        .unwrap();
    assert_eq!(stored.len(), 1, "повтор всё-таки записался: {stored:?}");
}

#[tokio::test]
async fn an_absurd_time_is_a_bad_request_and_not_a_heartbeat_from_the_epoch() {
    // `1e30` — совершенно обычный `f64`, и `serde_json` разбирает его без
    // возражений (а вот `1e400` отбивает сам, ещё до обработчика, — с ним
    // этот тест был бы вакуумным и зеленел бы без единой нашей проверки).
    // Дальше `Micros::from_secs_f64` — это `as i64`, то есть насыщение до
    // `i64::MAX`. Без проверки такая отметка легла бы в базу временем, до
    // которого календарь не доживает, и всё, что её потом читает, считало
    // бы по ней.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = post_heartbeat_response(
        state.clone(),
        &key,
        r#"{"entity":"/дом/проект/файл.rs","type":"file","time":1e30}"#,
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert!(body.get("error").is_some(), "нет поля error: {body}");

    // И ничего не записалось — ни на краю календаря, ни в эпохе.
    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let stored = state
        .store
        .heartbeats_in_range(user.id, Micros::new(i64::MIN), Micros::new(i64::MAX))
        .await
        .unwrap();
    assert!(stored.is_empty(), "абсурдное время всё-таки записалось: {stored:?}");
}

#[tokio::test]
async fn a_heartbeat_needs_a_key_and_lands_on_its_owner() {
    // Два заявления, и второе не следует из первого: обработчик, берущий
    // «первого попавшегося» пользователя, отдал бы тот же `201` с тем же
    // UUID, а отметка ушла бы чужому. Ни сверка формы, ни тест приёма
    // такого не видят.
    let dir = tempfile::tempdir().unwrap();
    let master = MasterKey::generate();
    let store = a_store(&dir);
    a_user_with_a_key(&store, &master, "swrneko").await;

    let neighbour = store
        .create_user(NewUser {
            login: "соседка".to_owned(),
            email: None,
            password_hash: "непрозрачно".to_owned(),
            display_name: None,
            timezone: "Asia/Tokyo".parse().unwrap(),
            timeout_secs: 1_800,
            is_admin: false,
        })
        .await
        .unwrap();
    let value = ApiKeyValue::generate();
    store
        .create_key(NewApiKey {
            user_id: neighbour.id,
            name: "её ноутбук".to_owned(),
            key_encrypted: value.encrypt(&master).unwrap().as_bytes().to_vec(),
            key_lookup: value.lookup(&master),
        })
        .await
        .unwrap();

    let state = AppState::new(store, Some(master), a_settings());
    let body = r#"{"entity":"/дом/проект/файл.rs","type":"file","time":1755500000.0}"#;

    let anonymous = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/users/current/heartbeats")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let accepted = post_heartbeat_response(state.clone(), &value, body).await;
    assert_eq!(accepted.status(), StatusCode::CREATED);

    let owner = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let (from, to) = (Micros::from_secs(1_755_499_999), Micros::from_secs(1_755_500_001));
    assert_eq!(
        state.store.heartbeats_in_range(neighbour.id, from, to).await.unwrap().len(),
        1,
        "отметка не досталась владелице предъявленного ключа"
    );
    assert!(
        state.store.heartbeats_in_range(owner.id, from, to).await.unwrap().is_empty(),
        "отметка досталась не тому пользователю"
    );
}

#[tokio::test]
async fn every_attribute_of_the_body_lands_where_the_protocol_names_it() {
    // `to_store` перекладывает восемнадцать полей по именам, и перестановка
    // любых двух однотипных — `branch` с `language`, `editor` с `machine` —
    // не ловится ни типом, ни сверкой формы, ни тестом приёма. Строки
    // подобраны попарно различными: с одинаковыми перестановка была бы
    // невидима и здесь.
    //
    // Тело несёт заодно поле, которого мы не знаем: докстринг
    // `IncomingBody` обещает, что плагин с полем из будущей версии не
    // получит отказ, и `deny_unknown_fields`, поставленный туда однажды,
    // обязан покраснеть здесь.
    //
    // Чего этот тест не проверяет: числовые поля (`lines`, `lineno`,
    // `cursorpos`, `line_*`, `project_root_count`) и `dependencies`
    // публичным путём не читает никто — `load_heartbeats` их не берёт.
    // Их укладку сторожит сырой `SELECT` в модульных тестах
    // `wakode-store/src/heartbeats.rs`, но уже после `to_store`.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = post_heartbeat_response(
        state.clone(),
        &key,
        r#"{
            "entity": "/дом/проект/файл.rs",
            "type": "app",
            "category": "debugging",
            "time": 1755500000.0,
            "project": "проект",
            "branch": "ветка",
            "language": "язык",
            "editor": "редактор",
            "operating_system": "ос",
            "machine": "машина",
            "is_write": true,
            "непонятное_поле_из_будущего_плагина": 42
        }"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED, "{:?}", response);

    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let stored = state
        .store
        .heartbeats_in_range(
            user.id,
            Micros::from_secs(1_755_499_999),
            Micros::from_secs(1_755_500_001),
        )
        .await
        .unwrap();
    assert_eq!(stored.len(), 1, "{stored:?}");
    let attrs = stored[0].attrs;

    assert_eq!(attrs.kind, EntityKind::App);
    assert_eq!(attrs.category, Category::Debugging);
    for (name, sid, expected) in [
        ("entity", Some(attrs.entity), "/дом/проект/файл.rs"),
        ("project", attrs.project, "проект"),
        ("branch", attrs.branch, "ветка"),
        ("language", attrs.language, "язык"),
        ("editor", attrs.editor, "редактор"),
        ("operating_system", attrs.os, "ос"),
        ("machine", attrs.machine, "машина"),
    ] {
        let sid = sid.unwrap_or_else(|| panic!("{name} потерялось: {attrs:?}"));
        assert_eq!(state.store.resolve(sid).as_deref(), Some(expected), "{name}");
    }
}

#[tokio::test]
async fn a_wrong_method_on_the_heartbeats_path_is_a_json_error_too() {
    // Маршрут добавлен выше `method_not_allowed_fallback`; окажись он
    // ниже — остался бы с пустым 405 axum'а мимо `ApiError`. Порядок строк
    // в `router` держится этим тестом, а не чтением кода.
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/users/current/heartbeats")
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
async fn a_body_we_cannot_parse_is_refused_with_a_code_the_plugin_drops() {
    // Разница между `400` и `500` тут не косметическая, и это главное, что
    // проверяет тест. `4xx` плагин выбрасывает; `5xx` он кладёт в офлайновую
    // очередь и шлёт снова — то есть неразбираемое тело возвращалось бы
    // вечно, а очередь копилась бы за счёт отметок, которые ещё можно было
    // бы записать.
    //
    // Дверей в этот код три, и они не одинаковы. Тело, не разобравшееся
    // как JSON, и незнакомый `type` идут через `JsonRejection`: у
    // `EntityKind` нет запасного варианта, в отличие от `Category`
    // (см. докстринг поля `kind` в `compat/heartbeats.rs`), так что
    // отметка от плагина новой версии попадает именно сюда. А вот
    // отсутствующая `entity` с задачи 4 идёт **мимо** разбора — через
    // проверку в `to_store`, ради имени поля в ответе батча, — и приносит
    // наш собственный текст, а не сообщение serde. Проверяемое утверждение
    // от этого не меняется: код обязан быть один и тот же, каким бы путём
    // отказ ни пришёл.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    for (why, body) in [
        ("вовсе не JSON", "не json ни с какой стороны"),
        ("JSON, но не объект", "[1, 2, 3]"),
        (
            "нет обязательного entity",
            r#"{"type":"file","time":1755500000.0}"#,
        ),
        (
            "незнакомый type от плагина новой версии",
            r#"{"entity":"/дом/проект/файл.rs","type":"тетрадка","time":1755500000.0}"#,
        ),
    ] {
        let response = post_heartbeat_response(state.clone(), &key, body).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{why}: этот код плагин будет ретраить вечно"
        );
        let json = json_body(response).await;
        assert!(json.get("error").is_some(), "{why}: нет поля error: {json}");
    }

    // И ни одна из четырёх не записалась.
    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let stored = state
        .store
        .heartbeats_in_range(user.id, Micros::new(i64::MIN), Micros::new(i64::MAX - 1))
        .await
        .unwrap();
    assert!(stored.is_empty(), "неразобранное тело всё-таки записалось: {stored:?}");
}

#[tokio::test]
async fn a_category_we_do_not_know_is_forgiven_where_a_type_we_do_not_know_is_not() {
    // Ассиметрия намеренная, и держится она тестом, а не докстрингом:
    // незнакомая категория даёт `Category::Unknown` и отметка записывается,
    // незнакомый `type` — отказ. Обоснование — в докстринге поля `kind`.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = post_heartbeat_response(
        state.clone(),
        &key,
        r#"{"entity":"/дом/проект/файл.rs","type":"file","time":1755500000.0,"category":"пилотирование"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let stored = state
        .store
        .heartbeats_in_range(
            user.id,
            Micros::from_secs(1_755_499_999),
            Micros::from_secs(1_755_500_001),
        )
        .await
        .unwrap();
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(
        stored[0].attrs.category,
        Category::Unknown,
        "незнакомая категория обязана сохраниться как «данные есть, мы их не поняли»"
    );

    // Вторая половина контраста, ради которой тест и назван так. Без неё
    // имя обещало бы противопоставление, а тело доказывало бы одну
    // сторону: обратную мутацию — начать прощать незнакомый `type` — этот
    // тест переживал бы зелёным, и ловил бы её только соседний, про
    // неразбираемое тело.
    let response = post_heartbeat_response(
        state.clone(),
        &key,
        r#"{"entity":"/дом/проект/файл.rs","type":"голограмма","time":1755500002.0}"#,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "незнакомый вид сущности обязан быть отказом, а не догадкой"
    );

    let stored = state
        .store
        .heartbeats_in_range(
            user.id,
            Micros::from_secs(1_755_500_002),
            Micros::from_secs(1_755_500_003),
        )
        .await
        .unwrap();
    assert!(stored.is_empty(), "отвергнутая отметка всё-таки записалась: {stored:?}");
}

/// Ответ на `POST /api/v1/users/current/heartbeats.bulk` с предъявленным ключом.
async fn post_bulk_response(state: AppState, key: &ApiKeyValue, body: &str) -> Response {
    router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/users/current/heartbeats.bulk")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(
                    axum::http::header::AUTHORIZATION,
                    format!("Basic {}", STANDARD.encode(key.to_string())),
                )
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Сколько отметок владельца лежит в базе.
async fn stored_count(state: &AppState) -> usize {
    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    state
        .store
        .heartbeats_in_range(user.id, Micros::new(i64::MIN), Micros::new(i64::MAX - 1))
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn a_duplicate_element_is_a_success_with_a_skip_note() {
    // Форма снята с живого и записана в
    // `.claude/docs/decisions/duplicate-heartbeats-are-a-success.md`:
    // повтор — **успешный** элемент с кодом 202, нулевым идентификатором
    // и полем `skip`. Идентификатор сверяется строкой целиком, а не через
    // `Uuid::nil()`: в нём стоят нибблы версии 4, и реализация на
    // `Uuid::nil()` разошлась бы с эталоном на четырёх невидимых битах.
    //
    // Одно утверждение ниже с живого **не** снято: код принятого элемента.
    // В снимке `heartbeat-bulk.json` принятых элементов нет вовсе — проба
    // слала повтор и негодную отметку, — а сам decision-док пишет в этой
    // клетке осторожное «2xx». Наш `201` взят у одиночного эндпоинта, где
    // он измерен. Экстраполяция, а не факт, и здесь она названа.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    // Две одинаковые отметки в одном батче: вторая обязана оказаться
    // повтором первой.
    let response = post_bulk_response(
        state.clone(),
        &key,
        r#"[{"entity":"/дом/ф.rs","type":"file","time":1755500000.0},
            {"entity":"/дом/ф.rs","type":"file","time":1755500000.0}]"#,
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = json_body(response).await;

    assert_eq!(body["responses"][0][1], 201, "вставка объявлена не вставкой: {body}");
    assert_eq!(body["responses"][1][1], 202, "повтор объявлен неуспехом: {body}");
    assert_eq!(
        body["responses"][1][0]["id"], "00000000-0000-4000-a000-000000000000",
        "у повтора обязан быть нулевой идентификатор версии 4 — записи-то не было: {body}"
    );
    assert_ne!(
        body["responses"][0][0]["id"], body["responses"][1][0]["id"],
        "повтор получил идентификатор вставки: {body}"
    );
    assert_eq!(
        body["responses"][1][0]["skip"], "Too many duplicate heartbeats.",
        "повтор не объяснён полем skip: {body}"
    );

    // Без чтения базы весь тест доказывал бы только форму: ответ такой же
    // формы получается и у реализации, которая не пишет ничего.
    assert_eq!(stored_count(&state).await, 1, "повтор всё-таки записался");
}

#[tokio::test]
async fn a_bad_element_fails_alone_without_failing_the_batch() {
    // Это и есть весь смысл батча: один негодный элемент не должен
    // отменять соседние. Реализация, отвечающая 400 на весь запрос,
    // заставила бы плагин потерять годные отметки — `400` он выбрасывает,
    // а не копит.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = post_bulk_response(
        state.clone(),
        &key,
        r#"[{"entity":"/дом/ф.rs","type":"file","time":1755500000.0},
            {"entity":"","type":"file","time":1755500001.0}]"#,
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = json_body(response).await;

    assert_eq!(body["responses"][0][1], 201, "{body}");
    assert_eq!(body["responses"][1][1], 400, "{body}");
    assert!(
        body["responses"][1][0]["errors"]["entity"].is_array(),
        "отказ обязан прийти массивом под именем поля протокола: {body}"
    );
    assert!(
        body["responses"][1][0]["id"].is_null(),
        "у отвергнутого элемента не может быть идентификатора: {body}"
    );

    assert_eq!(stored_count(&state).await, 1, "негодный элемент утянул за собой соседа");
}

#[tokio::test]
async fn a_mixed_batch_keeps_every_outcome_at_its_own_index() {
    // Все три исхода разом, и негодный — **первым**. Отвергнутые элементы
    // до хранилища не доезжают, поэтому позиция отметки в отчёте
    // хранилища сдвинута относительно позиции в запросе; на однородном
    // батче такая ошибка не видна вовсе, а клиенту сопоставлять элементы
    // нечем, кроме индекса.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = post_bulk_response(
        state.clone(),
        &key,
        r#"[{"entity":"","type":"file","time":1755500000.0},
            {"entity":"/дом/первый.rs","type":"file","time":1755500001.0},
            {"entity":"/дом/первый.rs","type":"file","time":1755500001.0},
            {"entity":"/дом/второй.rs","type":"file","time":1755500002.0}]"#,
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = json_body(response).await;

    assert_eq!(body["responses"].as_array().unwrap().len(), 4, "{body}");
    assert_eq!(body["responses"][0][1], 400, "отказ съехал с нулевой позиции: {body}");
    assert_eq!(body["responses"][1][1], 201, "вставка съехала с первой позиции: {body}");
    assert_eq!(body["responses"][2][1], 202, "повтор съехал со второй позиции: {body}");
    assert_eq!(body["responses"][3][1], 201, "вставка съехала с третьей позиции: {body}");

    assert!(body["responses"][0][0]["errors"]["entity"].is_array(), "{body}");
    assert_eq!(
        body["responses"][2][0]["id"], "00000000-0000-4000-a000-000000000000",
        "{body}"
    );

    // Идентификаторы двух вставок обязаны быть разными настоящими UUID:
    // равные означали бы, что одну строку приписали двум отметкам.
    let first = body["responses"][1][0]["id"].as_str().unwrap().to_owned();
    let second = body["responses"][3][0]["id"].as_str().unwrap().to_owned();
    assert!(uuid::Uuid::parse_str(&first).is_ok(), "{body}");
    assert!(uuid::Uuid::parse_str(&second).is_ok(), "{body}");
    assert_ne!(first, second, "{body}");

    // И ровно те две отметки, что объявлены вставленными, лежат в базе.
    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let stored = state
        .store
        .heartbeats_in_range(user.id, Micros::new(i64::MIN), Micros::new(i64::MAX - 1))
        .await
        .unwrap();
    assert_eq!(stored.len(), 2, "{stored:?}");
    let mut entities: Vec<String> = stored
        .iter()
        .map(|hb| state.store.resolve(hb.attrs.entity).unwrap().to_string())
        .collect();
    entities.sort();
    assert_eq!(entities, vec!["/дом/второй.rs", "/дом/первый.rs"]);
}

#[tokio::test]
async fn an_empty_batch_is_accepted_with_nothing_to_report() {
    // Пустой батч ничем не плох — он просто ни о чём не просит. Отказ на
    // нём был бы выдуманной ошибкой, а `202` с пустым `responses` — это
    // тождественный случай отображения «n отметок на входе, n ответов на
    // выходе».
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = post_bulk_response(state.clone(), &key, "[]").await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = json_body(response).await;
    assert_eq!(body["responses"], serde_json::json!([]), "{body}");
    assert_eq!(stored_count(&state).await, 0);
}

#[tokio::test]
async fn a_batch_where_every_element_is_bad_is_still_accepted() {
    // Негодность сообщается кодом элемента, и это место у неё
    // единственное: верхний код, зависящий от содержимого, заставил бы
    // клиента разветвляться на `202` против `400` с разными формами тела
    // ещё до разбора. Порога «слишком много негодных» не существует.
    //
    // Два разных способа быть негодным: пустая сущность (проверка
    // `to_store`, имя поля есть) и незнакомый вид (разбор упал, имени
    // поля нет вовсе).
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = post_bulk_response(
        state.clone(),
        &key,
        r#"[{"entity":"","type":"file","time":1755500000.0},
            {"entity":"/дом/ф.rs","type":"голограмма","time":1755500001.0}]"#,
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = json_body(response).await;
    assert_eq!(body["responses"][0][1], 400, "{body}");
    assert_eq!(body["responses"][1][1], 400, "{body}");
    assert!(body["responses"][0][0]["errors"]["entity"].is_array(), "{body}");
    assert!(
        body["responses"][1][0]["errors"]["non_field_errors"].is_array(),
        "элементу без виноватого поля нужен ключ, у которого поля нет: {body}"
    );
    assert_eq!(stored_count(&state).await, 0);
}

#[tokio::test]
async fn a_batch_over_the_limit_is_refused_whole_and_stores_nothing() {
    // Предел — **наш**, а не WakaTime: спека называет 25 клеткой таблицы
    // без ссылки и без пробы, и похоже это на размер порции клиента, а не
    // на правило сервера. Довод записан у константы `MAX_BATCH`; здесь
    // проверяется поведение на границе.
    //
    // Ровно предел проходит, предел плюс один отвергается целиком. Без
    // первой половины тест зеленел бы и на реализации, отвергающей всё
    // подряд.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let batch = |count: i64, from: i64| {
        let elements: Vec<String> = (0..count)
            .map(|i| {
                format!(
                    r#"{{"entity":"/дом/ф.rs","type":"file","time":{}.0}}"#,
                    from + i
                )
            })
            .collect();
        format!("[{}]", elements.join(","))
    };

    let at_limit = post_bulk_response(state.clone(), &key, &batch(1000, 1_755_500_000)).await;
    assert_eq!(at_limit.status(), StatusCode::ACCEPTED);
    assert_eq!(
        json_body(at_limit).await["responses"].as_array().unwrap().len(),
        1000
    );
    assert_eq!(stored_count(&state).await, 1000);

    let over = post_bulk_response(state.clone(), &key, &batch(1001, 1_755_600_000)).await;
    assert_eq!(
        over.status(),
        StatusCode::BAD_REQUEST,
        "батч сверх предела обязан быть отказом всему запросу"
    );
    let body = json_body(over).await;
    assert!(body.get("error").is_some(), "нет поля error: {body}");

    // Ни одной отметки из отвергнутого батча: обработать первые
    // `MAX_BATCH` и промолчать про хвост значило бы потерять отметки,
    // не сказав об этом.
    assert_eq!(
        stored_count(&state).await,
        1000,
        "часть отвергнутого батча всё-таки записалась"
    );
}

#[tokio::test]
async fn a_wrong_method_on_the_bulk_path_is_a_json_error_too() {
    // Маршрут батча обязан стоять **выше** `method_not_allowed_fallback`:
    // тот раздаёт запасной обработчик уже зарегистрированным маршрутам, и
    // добавленный ниже остался бы с пустым `405` axum'а — без тела, мимо
    // обещания «ответ всегда JSON с полем error». Инвариант из
    // `.claude/rules/ARCHITECTURE.md`, и ломали его не раз.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = router(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/users/current/heartbeats.bulk")
                .header(
                    axum::http::header::AUTHORIZATION,
                    format!("Basic {}", STANDARD.encode(key.to_string())),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let json = json_body(response).await;
    assert!(json.get("error").is_some(), "нет поля error: {json}");
}
