//! Пользователи: вставка и чтение.
//!
//! Свободные функции, а не репозиторный трейт — тот появится в задаче 12.
//! Здесь только строительные леса поверх `rusqlite::Connection`.

use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;
use wakode_core::Micros;

use crate::codec::{blob_to_uuid, uuid_to_blob};
use crate::error::{StoreError, StoreResult};

/// Что нужно, чтобы завести пользователя.
///
/// `password_hash` — непрозрачная строка: argon2 живёт в плане 3, хранилище
/// про него ничего не знает и знать не должно.
#[derive(Debug, Clone)]
pub struct NewUser {
    pub login: String,
    pub email: Option<String>,
    pub password_hash: String,
    pub display_name: Option<String>,
    pub timezone: Tz,
    pub timeout_secs: i64,
    pub is_admin: bool,
}

#[derive(Debug, Clone)]
pub struct User {
    pub id: Uuid,
    pub login: String,
    pub email: Option<String>,
    pub password_hash: String,
    pub display_name: Option<String>,
    pub timezone: Tz,
    pub timeout_secs: i64,
    pub is_admin: bool,
    pub created_at: Micros,
    pub updated_at: Micros,
}

pub fn insert_user(conn: &Connection, new: &NewUser) -> StoreResult<User> {
    let id = Uuid::now_v7();
    let now = crate::clock::now();

    conn.execute(
        "INSERT INTO users
           (id, login, email, password_hash, display_name, timezone,
            timeout_secs, is_admin, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        rusqlite::params![
            uuid_to_blob(id),
            new.login,
            new.email,
            new.password_hash,
            new.display_name,
            new.timezone.name(),
            new.timeout_secs,
            i64::from(new.is_admin),
            now.get(),
        ],
    )?;

    Ok(User {
        id,
        login: new.login.clone(),
        email: new.email.clone(),
        password_hash: new.password_hash.clone(),
        display_name: new.display_name.clone(),
        timezone: new.timezone,
        timeout_secs: new.timeout_secs,
        is_admin: new.is_admin,
        created_at: now,
        updated_at: now,
    })
}

pub fn find_user_by_login(conn: &Connection, login: &str) -> StoreResult<Option<User>> {
    query_one(conn, "login = ?1", rusqlite::params![login])
}

pub fn find_user_by_id(conn: &Connection, id: Uuid) -> StoreResult<Option<User>> {
    query_one(conn, "id = ?1", rusqlite::params![uuid_to_blob(id)])
}

/// Сырые колонки строки `users` — ровно то, что умеет отдать `rusqlite`.
///
/// Разбор идёт в два шага намеренно: замыкание `query_row` обязано вернуть
/// `rusqlite::Result`, а наши ошибки (битый UUID, неизвестная таймзона) в
/// этот тип не влезают. Попытка сделать всё одним шагом даёт `Result` внутри
/// `Result` и нечитаемую цепочку `?`.
type UserRow = (
    Vec<u8>,
    String,
    Option<String>,
    String,
    Option<String>,
    String,
    i64,
    i64,
    i64,
    i64,
);

fn query_one(
    conn: &Connection,
    predicate: &str,
    params: &[&dyn rusqlite::ToSql],
) -> StoreResult<Option<User>> {
    let sql = format!(
        "SELECT id, login, email, password_hash, display_name, timezone,
                timeout_secs, is_admin, created_at, updated_at
         FROM users WHERE {predicate}"
    );
    let mut stmt = conn.prepare_cached(&sql)?;

    let row: Option<UserRow> = stmt
        .query_row(params, |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        })
        .optional()?;

    let Some((id, login, email, password_hash, display_name, zone, timeout_secs, admin, created, updated)) =
        row
    else {
        return Ok(None);
    };

    Ok(Some(User {
        id: blob_to_uuid(&id)?,
        login,
        email,
        password_hash,
        display_name,
        timezone: zone
            .parse()
            .map_err(|_| StoreError::Corrupt(format!("неизвестная таймзона: {zone}")))?,
        timeout_secs,
        is_admin: admin != 0,
        created_at: Micros::new(created),
        updated_at: Micros::new(updated),
    }))
}
