//! Пользователи: вставка и чтение.
//!
//! Свободные функции, а не репозиторный трейт — тот появится в задаче 12.
//! Здесь только строительные леса поверх `rusqlite::Connection`.

use std::fmt;

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
///
/// Именно поэтому `Debug` тут написан руками (см. ниже): крейт не умеет
/// отличить секрет от обычной строки, и производный вывод напечатал бы хеш
/// дословно.
#[derive(Clone)]
pub struct NewUser {
    pub login: String,
    pub email: Option<String>,
    pub password_hash: String,
    pub display_name: Option<String>,
    pub timezone: Tz,
    pub timeout_secs: i64,
    pub is_admin: bool,
}

#[derive(Clone)]
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

/// Всё, кроме `password_hash`, — как есть.
///
/// Логин, почта и отображаемое имя секретами не являются, и прятать их
/// значило бы сделать отладку невозможной ради нулевой выгоды. А вот
/// argon2-хеш в логе ломают оффлайн, никуда не торопясь, — и попадает он
/// туда одним `?user` в `tracing`, который никто не заметит на ревью.
impl fmt::Debug for User {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("login", &self.login)
            .field("email", &self.email)
            .field("password_hash", &crate::REDACTED)
            .field("display_name", &self.display_name)
            .field("timezone", &self.timezone)
            .field("timeout_secs", &self.timeout_secs)
            .field("is_admin", &self.is_admin)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// То же, что у [`User`]: наружу не уходит только `password_hash`.
impl fmt::Debug for NewUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewUser")
            .field("login", &self.login)
            .field("email", &self.email)
            .field("password_hash", &crate::REDACTED)
            .field("display_name", &self.display_name)
            .field("timezone", &self.timezone)
            .field("timeout_secs", &self.timeout_secs)
            .field("is_admin", &self.is_admin)
            .finish()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{migrate, open_in_memory};

    const HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$СОЛЬ$СЕКРЕТ";

    fn a_user() -> NewUser {
        NewUser {
            login: "swrneko".to_owned(),
            email: Some("swrneko@example.org".to_owned()),
            password_hash: HASH.to_owned(),
            display_name: Some("Швырнеко".to_owned()),
            timezone: "Europe/Moscow".parse().unwrap(),
            timeout_secs: 900,
            is_admin: false,
        }
    }

    #[test]
    fn debug_hides_the_password_hash_but_keeps_everything_else() {
        let mut conn = open_in_memory().unwrap();
        migrate(&mut conn).unwrap();

        let new = a_user();
        let user = insert_user(&conn, &new).unwrap();

        for dump in [format!("{new:?}"), format!("{user:?}")] {
            assert!(!dump.contains(HASH), "хеш пароля утёк в Debug: {dump}");
            assert!(!dump.contains("СЕКРЕТ"), "хеш пароля утёк в Debug: {dump}");
            assert!(dump.contains(crate::REDACTED), "заглушки не видно: {dump}");
            // Логин, почта и имя — не секреты: без них Debug бесполезен.
            assert!(dump.contains("swrneko"), "логин прятать не надо: {dump}");
            assert!(dump.contains("swrneko@example.org"), "почту прятать не надо: {dump}");
            assert!(dump.contains("Швырнеко"), "имя прятать не надо: {dump}");
        }
    }
}
