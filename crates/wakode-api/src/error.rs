use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

/// Ошибка, отдаваемая клиенту.
///
/// Тело всегда JSON с полем `error`: совместимые клиенты разбирают ответ,
/// и пустое тело для них неотличимо от сломанного сервера.
#[derive(Debug)]
pub enum ApiError {
    /// Учётные данные не предъявлены или не подошли. Причина уезжает
    /// клиенту текстом: «ключ отозван» и «ключа не существует» — разные
    /// ответы, и склеивать их значит прятать от владельца, что произошло.
    Unauthorized(&'static str),
    Forbidden(&'static str),
    NotFound,
    BadRequest(String),
    Internal,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            ApiError::Unauthorized(why) => (StatusCode::UNAUTHORIZED, *why),
            ApiError::Forbidden(why) => (StatusCode::FORBIDDEN, *why),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "нет такого пути"),
            ApiError::BadRequest(why) => (StatusCode::BAD_REQUEST, why.as_str()),
            ApiError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
        };
        (status, Json(ErrorBody { error: message })).into_response()
    }
}

impl From<wakode_store::StoreError> for ApiError {
    /// Ошибки хранилища наружу не пробрасываются текстом: они содержат
    /// подробности схемы и путей, которые клиенту знать незачем.
    fn from(err: wakode_store::StoreError) -> Self {
        tracing::error!(error = %err, "ошибка хранилища");
        ApiError::Internal
    }
}
