use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

/// Сколько просить подождать при переполненной очереди записи.
///
/// Число условное: очередь разбирается за миллисекунды, а секунда — то, что
/// не превращает всплеск в поток повторов.
const RETRY_AFTER_SECS: u32 = 1;

/// Ошибка, отдаваемая клиенту.
///
/// Тело всегда JSON с полем `error`: совместимые клиенты разбирают ответ,
/// и пустое тело для них неотличимо от сломанного сервера. «Всегда» здесь
/// буквально — включая `405`, который axum иначе отдал бы пустым телом
/// мимо этого типа (см. `method_not_allowed_fallback` в `lib.rs`).
#[derive(Debug)]
pub enum ApiError {
    /// Учётные данные не предъявлены или не подошли.
    ///
    /// Причина уезжает клиенту текстом там, где предъявляют **ключ**:
    /// «ключ отозван» и «ключа не существует» — разные ответы, склеить их
    /// значило бы спрятать от владельца, что произошло, а перебирать
    /// 122 бита энтропии по этому оракулу нечего.
    ///
    /// Для формы входа (задача 11) это рассуждение не переносится: там
    /// «нет такого пользователя» и «неверный пароль» обязаны быть
    /// неразличимы, иначе получается перечисление учётных записей.
    Unauthorized(&'static str),
    Forbidden(&'static str),
    NotFound,
    MethodNotAllowed,
    BadRequest(String),
    /// Нагрузка временная, повторить стоит. Отдельно от `Internal`, потому
    /// что клиент различает их поведением: `503` с `Retry-After` он дошлёт,
    /// `500` — выбросит.
    Unavailable,
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
            ApiError::MethodNotAllowed => (StatusCode::METHOD_NOT_ALLOWED, "метод не поддержан"),
            ApiError::BadRequest(why) => (StatusCode::BAD_REQUEST, why.as_str()),
            ApiError::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "сервер перегружен, повторите позже",
            ),
            ApiError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
        };

        let body = (status, Json(ErrorBody { error: message })).into_response();

        match self {
            // `Retry-After` — не украшение: без него клиент не знает, что
            // повтор осмыслен, и очередь отметок в редакторе просто теряется.
            ApiError::Unavailable => (
                [(header::RETRY_AFTER, RETRY_AFTER_SECS.to_string())],
                body,
            )
                .into_response(),
            _ => body,
        }
    }
}

impl From<wakode_store::StoreError> for ApiError {
    /// Ошибки хранилища наружу не пробрасываются текстом: они содержат
    /// подробности схемы и путей, которые клиенту знать незачем. В лог они
    /// уходят целиком — владельцу инстанса они как раз нужны.
    fn from(err: wakode_store::StoreError) -> Self {
        use wakode_store::StoreError;

        match err {
            // Переполнение очереди — не поломка, а нагрузка. Отдать на неё
            // `500` значило бы сказать клиенту «не повторяй»: `wakode-cli`
            // хранит отметки у себя и дошлёт их, но только если понял, что
            // отказ временный. Это обещание записано в `spawn_writer`.
            StoreError::WriteQueueFull => {
                tracing::warn!("очередь записи переполнена");
                ApiError::Unavailable
            }
            err => {
                tracing::error!(error = %err, "ошибка хранилища");
                ApiError::Internal
            }
        }
    }
}
