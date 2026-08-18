//! HTTP-слой wakode.
//!
//! Криптографии здесь нет: она целиком в `wakode-auth`, и список
//! зависимостей этого крейта — способ это проверить.

pub mod compat;
pub mod error;
pub mod health;
pub mod internal;
pub mod state;

pub use error::ApiError;
pub use state::AppState;

use axum::routing::get;
use axum::Router;

/// Собрать маршрутизатор.
///
/// `method_not_allowed_fallback` ставится **после** всех `route`: он
/// раздаёт запасной обработчик уже зарегистрированным маршрутам, и
/// маршрут, добавленный ниже, останется с пустым `405` axum'а. Обещание
/// «тело всегда JSON» держится порядком этих строк, а не типом, поэтому
/// новые маршруты добавляются выше, а не ниже.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health::healthz))
        .fallback(|| async { ApiError::NotFound })
        .method_not_allowed_fallback(|| async { ApiError::MethodNotAllowed })
        .with_state(state)
}

/// Поднять сервер на готовом слушателе.
///
/// `into_make_service_with_connect_info` обязателен: экран первичной
/// настройки смотрит на адрес клиента, а без этого `ConnectInfo` в
/// обработчике не извлечётся.
pub async fn serve(listener: tokio::net::TcpListener, state: AppState) -> std::io::Result<()> {
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
}
