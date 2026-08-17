//! Сессии: вставка, поиск по хешу токена, отзыв.
//!
//! Свободные функции по той же форме, что и `keys`; репозиторный трейт
//! появится в задаче 12.

use std::fmt;

use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;
use wakode_core::Micros;

use crate::clock;
use crate::codec::{blob_to_uuid, uuid_to_blob};
use crate::error::StoreResult;

/// Заявка на сессию.
///
/// `token_hash` — хеш токена, который лежит в куке у браузера. `Debug` тут
/// написан руками, как у [`crate::User`] и [`crate::NewApiKey`]: хеш —
/// производная от секрета, и лог не то место, где ей стоит оседать.
#[derive(Clone)]
pub struct NewSession {
    pub user_id: Uuid,
    pub token_hash: Vec<u8>,
    pub user_agent: Option<String>,
    pub expires_at: Micros,
}

impl fmt::Debug for NewSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewSession")
            .field("user_id", &self.user_id)
            .field("token_hash", &crate::REDACTED)
            .field("user_agent", &self.user_agent)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: Uuid,
    pub user_id: Uuid,
    pub user_agent: Option<String>,
    pub created_at: Micros,
    pub expires_at: Micros,
    pub revoked_at: Option<Micros>,
}

pub fn insert_session(conn: &Connection, new: &NewSession) -> StoreResult<Session> {
    let id = Uuid::now_v7();
    let now = clock::now();

    conn.execute(
        "INSERT INTO sessions
           (id, user_id, token_hash, user_agent, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            uuid_to_blob(id),
            uuid_to_blob(new.user_id),
            new.token_hash,
            new.user_agent,
            now.get(),
            new.expires_at.get(),
        ],
    )?;

    Ok(Session {
        id,
        user_id: new.user_id,
        user_agent: new.user_agent.clone(),
        created_at: now,
        expires_at: new.expires_at,
        revoked_at: None,
    })
}

pub fn find_session_by_token_hash(
    conn: &Connection,
    token_hash: &[u8],
) -> StoreResult<Option<Session>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, user_id, user_agent, created_at, expires_at, revoked_at
         FROM sessions WHERE token_hash = ?1",
    )?;

    let row = stmt
        .query_row([token_hash], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })
        .optional()?;

    let Some((id, user_id, user_agent, created, expires, revoked)) = row else {
        return Ok(None);
    };

    Ok(Some(Session {
        id: blob_to_uuid(&id)?,
        user_id: blob_to_uuid(&user_id)?,
        user_agent,
        created_at: Micros::new(created),
        expires_at: Micros::new(expires),
        revoked_at: revoked.map(Micros::new),
    }))
}

/// Отозвать сессию.
///
/// `AND revoked_at IS NULL` — та же гарантия, что и в `revoke_key`: повторный
/// отзыв не должен переписывать момент отзыва текущим временем. Ретрай HTTP
/// или двойной клик в настройках — штатный сценарий, не повод терять
/// исходную метку.
pub fn revoke_session(conn: &Connection, id: Uuid) -> StoreResult<()> {
    conn.execute(
        "UPDATE sessions SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        rusqlite::params![uuid_to_blob(id), clock::now().get()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN_HASH: &[u8] = &[222, 173, 190, 239];

    #[test]
    fn debug_hides_the_token_hash_but_keeps_the_user_agent() {
        let new = NewSession {
            user_id: Uuid::now_v7(),
            token_hash: TOKEN_HASH.to_vec(),
            user_agent: Some("wakode-cli/0.1".to_owned()),
            expires_at: Micros::from_secs(1_755_000_000),
        };

        let dump = format!("{new:?}");

        assert!(
            !dump.contains(&format!("{TOKEN_HASH:?}")),
            "хеш токена утёк в Debug: {dump}"
        );
        assert!(dump.contains(crate::REDACTED), "заглушки не видно: {dump}");
        assert!(
            dump.contains("wakode-cli/0.1"),
            "user-agent прятать не надо: {dump}"
        );
    }
}
