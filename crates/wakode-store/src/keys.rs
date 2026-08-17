//! API-ключи: вставка, поиск по отпечатку, отзыв.
//!
//! Свободные функции, как и в `users` — репозиторный трейт появится в
//! задаче 12.

use std::fmt;

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
///
/// Оба поля — производные от самого ключа, и `Debug` у них написан руками по
/// той же причине, что у [`crate::User`]: имя ключа отлаживать помогает, его
/// байты — нет.
#[derive(Clone)]
pub struct NewApiKey {
    pub user_id: Uuid,
    pub name: String,
    pub key_encrypted: Vec<u8>,
    pub key_lookup: Vec<u8>,
}

#[derive(Clone)]
pub struct ApiKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub key_encrypted: Vec<u8>,
    pub created_at: Micros,
    pub last_used_at: Option<Micros>,
    pub revoked_at: Option<Micros>,
}

/// Шифротекст ключа наружу не печатается.
///
/// Он бесполезен без мастер-ключа, но лог живёт дольше и путешествует дальше
/// базы: выгрузка логов в чужой сборщик и утечка мастер-ключа — два разных
/// инцидента, и складывать материал для их пересечения в лог незачем.
impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKey")
            .field("id", &self.id)
            .field("user_id", &self.user_id)
            .field("name", &self.name)
            .field("key_encrypted", &crate::REDACTED)
            .field("created_at", &self.created_at)
            .field("last_used_at", &self.last_used_at)
            .field("revoked_at", &self.revoked_at)
            .finish()
    }
}

/// Ни шифротекст, ни отпечаток наружу не печатаются.
///
/// Отпечаток считается из значения ключа детерминированно, то есть по нему
/// проверяют догадку о ключе, не обращаясь к сервису вовсе.
impl fmt::Debug for NewApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewApiKey")
            .field("user_id", &self.user_id)
            .field("name", &self.name)
            .field("key_encrypted", &crate::REDACTED)
            .field("key_lookup", &crate::REDACTED)
            .finish()
    }
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

/// Самый ранний API-ключ в базе, если он есть.
///
/// Последовательность старта берёт им две вещи разом: сам факт наличия
/// ключей (без мастер-ключа стартовать нельзя) и шифротекст для проверки,
/// что мастер-ключ тот самый. Порядок по `created_at` делает проверку
/// воспроизводимой — «какой-нибудь» ключ означал бы, что она то проходит,
/// то нет.
pub fn first_api_key(conn: &Connection) -> StoreResult<Option<ApiKey>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, user_id, name, key_encrypted, created_at, last_used_at, revoked_at
         FROM api_keys ORDER BY created_at, id LIMIT 1",
    )?;

    let row = stmt
        .query_row([], |row| {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{insert_user, migrate, open_in_memory, NewUser};

    /// Байты подобраны так, чтобы их отпечаток `Debug` (`[222, 173, ...]`)
    /// не мог совпасть с куском времени или UUID случайно.
    const ENCRYPTED: &[u8] = &[222, 173, 190, 239];
    const LOOKUP: &[u8] = &[13, 240, 13, 240];

    #[test]
    fn debug_hides_the_key_material_but_keeps_the_name() {
        let mut conn = open_in_memory().unwrap();
        migrate(&mut conn).unwrap();

        let user = insert_user(
            &conn,
            &NewUser {
                login: "swrneko".to_owned(),
                email: None,
                password_hash: "непрозрачные байты из плана 3".to_owned(),
                display_name: None,
                timezone: "UTC".parse().unwrap(),
                timeout_secs: 900,
                is_admin: false,
            },
        )
        .unwrap();

        let new = NewApiKey {
            user_id: user.id,
            name: "ноутбук".to_owned(),
            key_encrypted: ENCRYPTED.to_vec(),
            key_lookup: LOOKUP.to_vec(),
        };
        let key = insert_api_key(&conn, &new).unwrap();

        for dump in [format!("{new:?}"), format!("{key:?}")] {
            assert!(
                !dump.contains(&format!("{ENCRYPTED:?}")),
                "шифротекст ключа утёк в Debug: {dump}"
            );
            assert!(
                !dump.contains(&format!("{LOOKUP:?}")),
                "отпечаток ключа утёк в Debug: {dump}"
            );
            assert!(dump.contains(crate::REDACTED), "заглушки не видно: {dump}");
            assert!(dump.contains("ноутбук"), "имя ключа прятать не надо: {dump}");
        }
    }
}
