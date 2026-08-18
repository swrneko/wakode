use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;
use uuid::Uuid;
use wakode_auth::SessionToken;
use wakode_core::Micros;
use wakode_store::{SessionRepo, User, UserRepo};

use crate::error::ApiError;
use crate::state::AppState;

/// Имя cookie с токеном сессии.
pub const SESSION_COOKIE: &str = "wakode_session";

/// Пользователь, опознанный по сессии.
#[derive(Debug, Clone)]
pub struct SessionAuth {
    pub user: User,
    pub session_id: Uuid,
}

impl FromRequestParts<AppState> for SessionAuth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let jar = CookieJar::from_headers(&parts.headers);
        let raw = jar
            .get(SESSION_COOKIE)
            .ok_or(ApiError::Unauthorized("сессия не предъявлена"))?;

        let token = SessionToken::parse(raw.value())
            .ok_or(ApiError::Unauthorized("токен сессии имеет неверный формат"))?;

        let session = state
            .store
            .session_by_token_hash(token.hash())
            .await?
            .ok_or(ApiError::Unauthorized("сессия не найдена"))?;

        // Отзыв проверяется раньше срока: это осознанное действие владельца
        // («выйти на всех устройствах»), и для сессии, которая и отозвана, и
        // просрочена, честнее назвать причиной то, что он сделал сам.
        if session.revoked_at.is_some() {
            return Err(ApiError::Unauthorized("сессия отозвана"));
        }

        // Срок проверяется здесь: хранилище отдаёт `expires_at` как есть,
        // доменной валидации в нём нет по построению.
        let now = now_at(std::time::SystemTime::now()).ok_or_else(|| {
            tracing::error!("системные часы стоят до эпохи: срок сессии не проверить");
            ApiError::Internal
        })?;
        if is_expired(session.expires_at, now) {
            return Err(ApiError::Unauthorized("сессия истекла"));
        }

        // Недостижимо: `sessions.user_id` объявлен
        // `REFERENCES users(id) ON DELETE CASCADE` при включённом
        // `PRAGMA foreign_keys` (`schema.rs`, `conn.rs`), так что сессии без
        // владельца в базе не бывает. Ветка оставлена страховкой на случай
        // базы, открытой с выключенными внешними ключами, — и отвечает `500`,
        // а не `401`: это нарушенный инвариант хранилища, а не вина клиента,
        // и сказать «вы не авторизованы» значило бы отправить владельца
        // чинить сессию вместо базы. Тестом не покрыта, потому что
        // публичного пути к ней нет.
        let user = state.store.user_by_id(session.user_id).await?.ok_or_else(|| {
            tracing::error!(session_id = %session.id, "у сессии нет владельца: нарушен внешний ключ");
            ApiError::Internal
        })?;

        Ok(SessionAuth {
            user,
            session_id: session.id,
        })
    }
}

/// Истёк ли срок сессии к моменту `now`.
///
/// Равенство считается истечением: `expires_at` — это момент, когда сессия
/// кончается, а не последняя микросекунда, когда она ещё работает. Иначе
/// сессия, заведённая с нулевым сроком (`expires_at == created_at`), одну
/// микросекунду была бы жива.
fn is_expired(expires_at: Micros, now: Micros) -> bool {
    expires_at <= now
}

/// Показание часов как микросекунды от эпохи UTC.
///
/// Отдельно от `SystemTime::now()`, чтобы поведение сломанных часов
/// проверялось тестом, а не рассуждением.
///
/// **Ошибка часов обязана запирать дверь, а не открывать.** Часы до эпохи
/// (голая VM без RTC, битый контейнер) — это `None` и `500` выше по стеку:
/// насколько они врут, отсюда не видно, а подставить вместо них ноль
/// значило бы выключить проверку срока целиком — при `now = 0` не истекла
/// ни одна сессия, и все просроченные стали бы вечными.
///
/// Часы настолько в будущем, что микросекунды не влезают в `i64` (это
/// примерно 292 000 лет от эпохи), насыщаются до `i64::MAX`: это «позже любого мыслимого
/// `expires_at`», то есть всё просрочено. Отказ в ту же безопасную
/// сторону, а без насыщения приведение `as i64` завернуло бы такое время
/// в отрицательное — и снова сделало бы сессии вечными.
fn now_at(clock: std::time::SystemTime) -> Option<Micros> {
    let since_epoch = clock.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(Micros::new(
        i64::try_from(since_epoch.as_micros()).unwrap_or(i64::MAX),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_boundary_of_expiry_is_pinned() {
        // Граница выражается только здесь: попасть запросом ровно в
        // микросекунду `expires_at` через живые часы нельзя, поэтому
        // мутация `<=` → `<` не роняет ни одного интеграционного теста.
        let expires_at = Micros::from_secs(1_000);

        assert!(
            !is_expired(expires_at, expires_at.saturating_sub(Micros::new(1))),
            "за микросекунду до срока сессия ещё жива"
        );
        assert!(
            is_expired(expires_at, expires_at),
            "ровно в `expires_at` сессия уже мертва"
        );
        assert!(
            is_expired(expires_at, expires_at.saturating_add(Micros::new(1))),
            "после срока сессия мертва"
        );
    }

    #[test]
    fn broken_clocks_lock_the_door_instead_of_opening_it() {
        // Часы до эпохи: ответа нет, а не «ноль». Ноль означал бы, что не
        // истекла ни одна сессия, то есть проверка срока молча выключена.
        let before_epoch = std::time::UNIX_EPOCH - std::time::Duration::from_secs(1);
        assert_eq!(now_at(before_epoch), None);

        // Часы, ушедшие за пределы `i64` микросекунд: всё просрочено.
        let far_future = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1 << 60);
        let now = now_at(far_future).expect("часы после эпохи обязаны читаться");
        assert!(
            is_expired(Micros::new(i64::MAX), now),
            "переполнение обязано насыщаться вверх, а не заворачиваться в отрицательное"
        );
    }
}
