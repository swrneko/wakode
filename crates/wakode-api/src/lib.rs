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

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health::healthz))
        .fallback(|| async { ApiError::NotFound })
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
