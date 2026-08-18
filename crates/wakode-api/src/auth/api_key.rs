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
        let candidates = candidates(parts);
        if candidates.is_empty() {
            return Err(ApiError::Unauthorized("API-ключ не предъявлен"));
        }

        // Разбирается первый подошедший, а не первый попавшийся: источники
        // соперничают, и негодный кандидат не отменяет годного.
        let value = candidates
            .iter()
            .find_map(|raw| ApiKeyValue::parse(raw))
            .ok_or(ApiError::Unauthorized("API-ключ имеет неверный формат"))?;

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

        // Недостижимо: `api_keys.user_id` объявлен
        // `REFERENCES users(id) ON DELETE CASCADE` при включённом
        // `PRAGMA foreign_keys` (`schema.rs`, `conn.rs`), так что ключа без
        // владельца в базе не бывает. Ветка оставлена страховкой на случай
        // базы, открытой с выключенными внешними ключами, — и отвечает `500`,
        // а не `401`: это нарушенный инвариант хранилища, а не вина клиента,
        // и сказать «вы не авторизованы» значило бы отправить владельца
        // чинить ключ вместо базы. Тестом не покрыта, потому что публичного
        // пути к ней нет.
        let user = state.store.user_by_id(key.user_id).await?.ok_or_else(|| {
            tracing::error!(key_id = %key.id, "у ключа нет владельца: нарушен внешний ключ");
            ApiError::Internal
        })?;

        Ok(KeyAuth {
            user,
            key_id: key.id,
        })
    }
}

/// Всё, что в запросе похоже на предъявленный ключ, в порядке
/// предпочтения.
///
/// Три источника: `Basic` (описан спекой WakaTime), `Bearer` (встречается у
/// части плагинов) и query-параметр `api_key`, которым пользуется cli.
/// Префикс `waka_` здесь не трогаем — его срезает `ApiKeyValue::parse`,
/// чтобы знание о формате ключа не размазывалось по двум крейтам.
///
/// **Возвращается список, а не первое найденное.** Источники соперничают:
/// владелец ставит перед wakode прокси с собственным basic-auth, а cli
/// кладёт ключ в query — и тогда заголовок разбирается успешно, но
/// содержит `admin`, а не ключ. Пока побеждал первый источник, такая
/// установка отвечала `401` на каждую отметку, причём с формулировкой
/// «API-ключ не предъявлен», хотя он был предъявлен. Кто из кандидатов
/// настоящий, решает `ApiKeyValue::parse`, а не порядок разбора.
///
/// Различие «не предъявлен» и «неверный формат» при этом сохраняется:
/// пустой список — первое, непустой без единого разобравшегося — второе.
fn candidates(parts: &Parts) -> Vec<String> {
    [from_header(parts), from_query(parts)]
        .into_iter()
        .flatten()
        .collect()
}

/// Ключ из заголовка `Authorization`, если он там есть и разбирается.
///
/// Каждый отказ здесь — `None`, а не отказ всего разбора: заголовок может
/// принадлежать не нам (прокси, браузер), и тогда его невнятность не
/// должна ничего значить.
fn from_header(parts: &Parts) -> Option<String> {
    let text = parts
        .headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;

    if let Some(encoded) = credentials_of(text, "Basic") {
        // `trim` до декодирования: в base64 пробелы не значат ничего, а
        // вот `ApiKeyValue::parse` их уже не увидит — он получит результат
        // декодирования, а не исходную строку.
        let decoded = STANDARD.decode(encoded.trim()).ok()?;
        let decoded = String::from_utf8(decoded).ok()?;
        // Basic-схема допускает `логин:пароль`; wakatime-cli шлёт голый
        // ключ, но отрезать хвост после двоеточия дешевле, чем гадать.
        return Some(decoded.split(':').next().unwrap_or(&decoded).to_owned());
    }

    // Без `trim`: пробелы по краям срезает `ApiKeyValue::parse`, и второй
    // раз делать то же самое здесь незачем — в отличие от `Basic`, где
    // до `parse` стоит декодирование.
    credentials_of(text, "Bearer").map(str::to_owned)
}

/// Ключ из query-параметра `api_key`.
///
/// Значение берётся как есть, без percent-декодирования: это UUID с
/// необязательным префиксом `waka_`, а в нём нет ни одного символа,
/// который percent-кодирование затрагивает. Декодер здесь добавил бы
/// единственное новое поведение — превращение `%zz` в отказ разбора,
/// и то на значении, которое всё равно не ключ.
fn from_query(parts: &Parts) -> Option<String> {
    parts.uri.query()?.split('&').find_map(|pair| {
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
