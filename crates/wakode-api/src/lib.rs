//! HTTP-слой wakode.
//!
//! Криптографии здесь нет: она целиком в `wakode-auth`, и список
//! зависимостей этого крейта — способ это проверить.

pub mod auth;
pub mod compat;
pub mod error;
pub mod health;
pub mod internal;
pub mod setup;
pub mod state;

pub use error::ApiError;
pub use state::{AppSettings, AppState};

use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

/// Навесить общие слои.
///
/// Вынесено отдельно от `router`, чтобы тесты могли собрать свой маршрут
/// с теми же слоями: проверять перехват паники на настоящем обработчике,
/// который паникует, иначе нечем.
///
/// Порядок вызовов `layer` — не оформление: каждый следующий оборачивает
/// собранное, поэтому `TraceLayer` здесь оказывается **снаружи**
/// `CatchPanicLayer` и видит уже готовый `500`. Переставь их местами — и
/// паника пойдёт вверх мимо журнала: запись о ней потеряет контекст
/// запроса, а строки о завершении не будет вовсе.
pub fn with_layers(router: Router) -> Router {
    router
        // Слоем, а не по маршрутам: ответы этого API поголовно зависят от
        // того, кто спрашивает, — от ключа, сессии или адреса пира, — и
        // маршрут, забывший заголовок, отдал бы чужой ответ в общий кеш.
        // Забыть слой нельзя: он навешивается на всё сразу.
        //
        // `if_not_present`, а не `overriding`: маршрут, которому
        // кешируемость положена, ставит свой `Cache-Control` и остаётся с
        // ним. Это точка расширения для плана 4 — встроенная SPA раздаёт
        // статику с хешами в именах, и делать её нехранимой значило бы
        // заставлять браузер выкачивать бандл на каждое открытие.
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        ))
        .layer(CatchPanicLayer::custom(handle_panic))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(request_span)
                // Уровень поднят с `DEBUG`: боевой фильтр в бинаре — `info`,
                // и на умолчании `tower-http` журнал запросов оказался бы
                // пуст, хотя слой стоит.
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
}

/// Что пишется о запросе: метод и путь **без query-строки**.
///
/// Пишется `path`, а не `uri`, потому что `DefaultMakeSpan` из
/// `TraceLayer::new_for_http()` пишет `uri` целиком, вместе с
/// `?api_key=…` — проверено прогоном и закреплено
/// `the_query_string_never_reaches_the_log`. Заголовков здесь нет по той
/// же причине: в `Authorization` лежит тот же ключ в base64.
///
/// **Инвариант, который эта функция не может проверить сама:** путь не
/// несёт секретов. Сегодня это так — `/healthz`, `/api/setup`,
/// `/api/setup/status`, — но держится соглашением, а не кодом. Маршрут
/// вида `/api/keys/{ключ}` уронил бы значение ключа в журнал открытым
/// текстом мимо всех проверок этого файла. Секрет ездит в заголовке, в
/// теле или в query — не в пути.
fn request_span(request: &axum::extract::Request) -> tracing::Span {
    tracing::info_span!(
        "request",
        method = %request.method(),
        path = request.uri().path(),
    )
}

/// Ответ на панику в обработчике.
fn handle_panic(err: Box<dyn std::any::Any + Send + 'static>) -> axum::response::Response {
    let message = err
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| err.downcast_ref::<&str>().copied())
        .unwrap_or("неизвестная паника");

    // Текст паники — в лог, но не клиенту: он содержит подробности кода.
    tracing::error!(panic = message, "паника в обработчике");
    ApiError::Internal.into_response()
}

/// Собрать маршрутизатор.
///
/// `method_not_allowed_fallback` ставится **после** всех `route`: он
/// раздаёт запасной обработчик уже зарегистрированным маршрутам, и
/// маршрут, добавленный ниже, останется с пустым `405` axum'а. Обещание
/// «тело всегда JSON» держится порядком этих строк, а не типом, поэтому
/// новые маршруты добавляются выше, а не ниже.
pub fn router(state: AppState) -> Router {
    with_layers(
        Router::new()
            .route("/healthz", get(health::healthz))
            .route("/api/setup/status", get(setup::status))
            .route("/api/setup", axum::routing::post(setup::setup))
            .fallback(|| async { ApiError::NotFound })
            .method_not_allowed_fallback(|| async { ApiError::MethodNotAllowed })
            .with_state(state),
    )
}

/// Поднять сервер на готовом слушателе и работать, пока не попросят перестать.
///
/// `shutdown` — футура, завершение которой означает «перестать принимать
/// новые соединения и дочитать начатые». Параметр обязателен и не имеет
/// умолчания намеренно: вызывающий обязан решить, чем его сервер
/// останавливается. Тому, кому останов не нужен (тесты одного запроса),
/// подходит `std::future::pending()`, и это видно в вызове.
///
/// Тип возврата — `()`, а не `io::Result<()>`. И без graceful shutdown, и
/// с ним футура axum объявлена как `io::Result<()>`, но документация
/// `with_graceful_shutdown` говорит прямо: этот `Result` тоже никогда не
/// становится `Err` — ошибки сокета обрабатываются сном и повтором приёма,
/// а `Ok(())` приходит только после завершения `shutdown`. `Result`,
/// который не может быть ошибкой, обязывает вызывающего писать `?` там,
/// где ветки отказа не существует, поэтому он отбрасывается здесь, а не
/// прокидывается наружу.
///
/// `into_make_service_with_connect_info` обязателен: экран первичной
/// настройки смотрит на адрес клиента, а без этого `ConnectInfo` в
/// обработчике не извлечётся. Держится тестом
/// `setup_over_a_real_socket_sees_the_client_address`.
///
/// Чего эта функция не делает: не ограничивает время дочитывания. Запрос,
/// который не закрывается никогда, задержит здесь навсегда. Предел ставит
/// вызывающий — см. `signal::wait_for_drain` в бинаре, — потому что
/// решение «бросить начатое» принимается на уровне процесса, а не
/// HTTP-слоя.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: AppState,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) {
    let _ = axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await;
}
