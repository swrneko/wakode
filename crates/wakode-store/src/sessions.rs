//! Сессии: вставка, поиск по хешу токена, отзыв.
//!
//! Свободные функции по той же форме, что и `keys`; репозиторный трейт
//! появится в задаче 12.

use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;
use wakode_core::Micros;

use crate::clock;
use crate::codec::{blob_to_uuid, uuid_to_blob};
use crate::error::StoreResult;

#[derive(Debug, Clone)]
pub struct NewSession {
    pub user_id: Uuid,
    pub token_hash: Vec<u8>,
    pub user_agent: Option<String>,
    pub expires_at: Micros,
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

pub fn revoke_session(conn: &Connection, id: Uuid) -> StoreResult<()> {
    conn.execute(
        "UPDATE sessions SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        rusqlite::params![uuid_to_blob(id), clock::now().get()],
    )?;
    Ok(())
}
