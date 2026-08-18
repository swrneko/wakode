use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use uuid::Uuid;
use wakode_auth::ApiKeyValue;
use wakode_store::{KeyRepo, User, UserRepo};

use crate::error::ApiError;
use crate::state::AppState;

/// Пользователь, опознанный по API-ключу.
#[derive(Debug, Clone)]
pub struct KeyAuth {
    pub user: User,
    pub key_id: Uuid,
}

impl FromRequestParts<AppState> for KeyAuth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let raw = extract_raw_key(parts).ok_or(ApiError::Unauthorized("API-ключ не предъявлен"))?;

        let value =
            ApiKeyValue::parse(&raw).ok_or(ApiError::Unauthorized("API-ключ имеет неверный формат"))?;

        // Отпечаток считается под мастер-ключом. Его отсутствие — не вина
        // клиента, поэтому 500, а не 401: сервер не в состоянии проверить.
        let Some(master) = state.master_key.as_ref() else {
            tracing::error!("проверка ключа запрошена без мастер-ключа");
            return Err(ApiError::Internal);
        };

        let found = state.store.key_by_lookup(value.lookup(master)).await?;
        let key = found.ok_or(ApiError::Unauthorized("API-ключ не найден"))?;

        // Отозванный ключ отвергается со своей причиной: «отозван» и «не
        // существует» — разные ответы для владельца, и склеивать их значило
        // бы отправить его искать поломку в редакторе.
        if key.revoked_at.is_some() {
            return Err(ApiError::Unauthorized("API-ключ отозван"));
        }

        let user = state
            .store
            .user_by_id(key.user_id)
            .await?
            .ok_or(ApiError::Unauthorized("владелец ключа не найден"))?;

        Ok(KeyAuth {
            user,
            key_id: key.id,
        })
    }
}

/// Достать значение ключа из запроса.
///
/// Три источника: `Basic` (описан спекой WakaTime), `Bearer` (встречается у
/// части плагинов) и query-параметр `api_key`, которым пользуется cli.
/// Префикс `waka_` здесь не трогаем — его срезает `ApiKeyValue::parse`,
/// чтобы знание о формате ключа не размазывалось по двум крейтам.
fn extract_raw_key(parts: &Parts) -> Option<String> {
    if let Some(header) = parts.headers.get(axum::http::header::AUTHORIZATION) {
        let text = header.to_str().ok()?;
        if let Some(encoded) = credentials_of(text, "Basic") {
            let decoded = STANDARD.decode(encoded.trim()).ok()?;
            let text = String::from_utf8(decoded).ok()?;
            // Basic-схема допускает `логин:пароль`; wakatime-cli шлёт голый
            // ключ, но отрезать хвост после двоеточия дешевле, чем гадать.
            return Some(text.split(':').next().unwrap_or(&text).to_owned());
        }
        if let Some(token) = credentials_of(text, "Bearer") {
            return Some(token.trim().to_owned());
        }
    }

    // Значение берётся как есть, без percent-декодирования: это UUID с
    // необязательным префиксом `waka_`, а в нём нет ни одного символа,
    // который percent-кодирование затрагивает. Декодер здесь добавил бы
    // единственное новое поведение — превращение `%zz` в отказ разбора,
    // и то на значении, которое всё равно не ключ.
    let query = parts.uri.query()?;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == "api_key").then(|| value.to_owned())
    })
}

/// Значение заголовка `Authorization`, если он предъявлен названной схемой.
///
/// Имя схемы RFC 7235 объявляет регистронезависимым, поэтому сравнивается
/// без учёта регистра, — а вот само значение возвращается как есть:
/// base64 регистр различает, и общий `to_lowercase` по заголовку сломал бы
/// ровно тот разбор, ради которого он бы делался.
fn credentials_of<'a>(header: &'a str, scheme: &str) -> Option<&'a str> {
    let (name, credentials) = header.split_once(' ')?;
    name.eq_ignore_ascii_case(scheme).then_some(credentials)
}
