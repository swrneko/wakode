//! API-ключи: вставка, поиск по отпечатку, отзыв.
//!
//! Свободные функции, как и в `users` — репозиторный трейт появится в
//! задаче 12.

use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;
use wakode_core::Micros;

use crate::clock;
use crate::codec::{blob_to_uuid, uuid_to_blob};
use crate::error::StoreResult;

/// Новый API-ключ.
///
/// `key_encrypted` — значение ключа под мастер-ключом, чтобы показать его в
/// настройках. `key_lookup` — детерминированный отпечаток того же значения:
/// по зашифрованному искать нельзя, а аутентификация обязана найти ключ за
/// один запрос. Оба считает план 3; сюда приезжают готовые байты.
#[derive(Debug, Clone)]
pub struct NewApiKey {
    pub user_id: Uuid,
    pub name: String,
    pub key_encrypted: Vec<u8>,
    pub key_lookup: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ApiKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub key_encrypted: Vec<u8>,
    pub created_at: Micros,
    pub last_used_at: Option<Micros>,
    pub revoked_at: Option<Micros>,
}

pub fn insert_api_key(conn: &Connection, new: &NewApiKey) -> StoreResult<ApiKey> {
    let id = Uuid::now_v7();
    let now = clock::now();

    conn.execute(
        "INSERT INTO api_keys
           (id, user_id, name, key_encrypted, key_lookup, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            uuid_to_blob(id),
            uuid_to_blob(new.user_id),
            new.name,
            new.key_encrypted,
            new.key_lookup,
            now.get(),
        ],
    )?;

    Ok(ApiKey {
        id,
        user_id: new.user_id,
        name: new.name.clone(),
        key_encrypted: new.key_encrypted.clone(),
        created_at: now,
        last_used_at: None,
        revoked_at: None,
    })
}

/// Найти ключ по отпечатку.
///
/// Отозванные ключи тоже находятся: слой аутентификации должен различать
/// «такого ключа не было» и «ключ отозван» — это разные ответы пользователю.
pub fn find_key_by_lookup(conn: &Connection, lookup: &[u8]) -> StoreResult<Option<ApiKey>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, user_id, name, key_encrypted, created_at, last_used_at, revoked_at
         FROM api_keys WHERE key_lookup = ?1",
    )?;

    let row = stmt
        .query_row([lookup], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .optional()?;

    let Some((id, user_id, name, key_encrypted, created, used, revoked)) = row else {
        return Ok(None);
    };

    Ok(Some(ApiKey {
        id: blob_to_uuid(&id)?,
        user_id: blob_to_uuid(&user_id)?,
        name,
        key_encrypted,
        created_at: Micros::new(created),
        last_used_at: used.map(Micros::new),
        revoked_at: revoked.map(Micros::new),
    }))
}

/// Отозвать ключ.
///
/// `AND revoked_at IS NULL` в запросе — не лишнее условие: повторный отзыв
/// уже отозванного ключа не должен переписывать `revoked_at` текущим
/// временем, иначе «когда ключ отозвали» превратится в «когда его в
/// последний раз пытались отозвать». Повтор — обычное дело: ретрай HTTP,
/// двойной клик в настройках.
pub fn revoke_key(conn: &Connection, id: Uuid) -> StoreResult<()> {
    conn.execute(
        "UPDATE api_keys SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        rusqlite::params![uuid_to_blob(id), clock::now().get()],
    )?;
    Ok(())
}

pub fn touch_key_used(conn: &Connection, id: Uuid) -> StoreResult<()> {
    conn.execute(
        "UPDATE api_keys SET last_used_at = ?2 WHERE id = ?1",
        rusqlite::params![uuid_to_blob(id), clock::now().get()],
    )?;
    Ok(())
}
