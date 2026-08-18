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

/// Завести пользователя.
///
/// **Логин триммится и не может быть пустым, и проверка стоит здесь.** У
/// создания пользователя две двери — экран первичной настройки и
/// `wakode user create`, — а в плане 3b добавятся регистрация и правка
/// профиля. Проверка у двери повторяется столько раз, сколько дверей, и
/// одной забытой хватает, чтобы инварианта не стало. Это не гипотеза: до
/// этой правки экран триммил и отвергал пустой, а CLI заводил
/// пользователя с логином `"  админ  "`. Форма входа появится в плане
/// 3b, и логин она получит из поля ввода — с пробелами, случайно
/// скопированными вместе со строкой, или без них; какой бы вариант она ни
/// выбрала, один из двух таких пользователей окажется недостижим. Решать
/// это надо здесь, а не там.
///
/// Тот же приём, что с порогом длины пароля в `wakode_auth::hash_password`:
/// дверь к записи одна, проверка стоит в ней.
pub fn insert_user(conn: &Connection, new: &NewUser) -> StoreResult<User> {
    let login = new.login.trim();
    if login.is_empty() {
        return Err(StoreError::LoginEmpty);
    }

    let id = Uuid::now_v7();
    let now = crate::clock::now();

    // Нарушение уникальности логина — вина вызывающего, а не поломка
    // хранилища, и наружу оно обязано уехать доменной ошибкой. Тип
    // `StoreError` для того и заведён: «вызывающий не должен разбирать коды
    // SQLite, чтобы понять, что произошло». Без этой ветки CLI печатал
    // `база данных: UNIQUE constraint failed: users.login: Error code 2067`
    // — то есть подавал занятый логин как отказ базы, да ещё языком, на
    // котором в этом проекте не говорят.
    let taken = |err: rusqlite::Error| match &err {
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            StoreError::LoginTaken(login.to_owned())
        }
        _ => StoreError::from(err),
    };

    conn.execute(
        "INSERT INTO users
           (id, login, email, password_hash, display_name, timezone,
            timeout_secs, is_admin, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        rusqlite::params![
            uuid_to_blob(id),
            login,
            new.email,
            new.password_hash,
            new.display_name,
            new.timezone.name(),
            new.timeout_secs,
            i64::from(new.is_admin),
            now.get(),
        ],
    )
    .map_err(taken)?;

    Ok(User {
        id,
        login: login.to_owned(),
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

/// Сколько всего пользователей в базе.
///
/// Нужно экрану первичной настройки: он открыт ровно до появления первого
/// пользователя и закрывается навсегда после.
pub fn user_count(conn: &Connection) -> StoreResult<i64> {
    let mut stmt = conn.prepare_cached("SELECT count(*) FROM users")?;
    Ok(stmt.query_row([], |row| row.get(0))?)
}

/// Найти пользователя по логину.
///
/// Логин триммится — той же рукой, что и в [`insert_user`]. Асимметрия
/// была бы хуже отсутствия обеих: в базе лежит обрезанный логин, а поиск
/// по `"  swrneko  "` его бы не нашёл, и `wakode key issue --user` со
/// случайным пробелом сообщал бы «нет пользователя» про существующего.
pub fn find_user_by_login(conn: &Connection, login: &str) -> StoreResult<Option<User>> {
    let login = login.trim();
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

/// Колонки в том порядке, в каком их ждёт [`UserRow`].
///
/// Одной константой на все запросы: список и порядок колонок обязаны
/// совпадать с разбором в [`read_row`], а две копии этого списка
/// расходятся молча — на месте `email` оказался бы `password_hash`, и
/// компилятор бы этого не заметил, обе колонки текстовые.
const USER_COLUMNS: &str = "id, login, email, password_hash, display_name, timezone,
                            timeout_secs, is_admin, created_at, updated_at";

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserRow> {
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
}

fn into_user(row: UserRow) -> StoreResult<User> {
    let (id, login, email, password_hash, display_name, zone, timeout_secs, admin, created, updated) =
        row;

    Ok(User {
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
    })
}

fn query_one(
    conn: &Connection,
    predicate: &str,
    params: &[&dyn rusqlite::ToSql],
) -> StoreResult<Option<User>> {
    let sql = format!("SELECT {USER_COLUMNS} FROM users WHERE {predicate}");
    let mut stmt = conn.prepare_cached(&sql)?;

    let row: Option<UserRow> = stmt.query_row(params, read_row).optional()?;

    match row {
        None => Ok(None),
        Some(row) => Ok(Some(into_user(row)?)),
    }
}

/// Все пользователи, от самого раннего к позднему.
///
/// Порядок по `created_at` делает вывод `wakode user list` устойчивым:
/// список, меняющий порядок между запусками, нельзя ни сравнить глазами,
/// ни зафиксировать тестом. `id` вторым ключом — потому что `created_at`
/// снимается с часов в микросекундах и у двух пользователей, заведённых
/// подряд одним скриптом, может совпасть; UUIDv7 внутри одной
/// микросекунды порядок уже не гарантирует, но делает его хотя бы
/// одинаковым от запуска к запуску.
///
/// Постранично не отдаётся: пользователей на selfhosted-инстансе десятки,
/// и курсор здесь был бы механикой без потребителя.
pub fn list_users(conn: &Connection) -> StoreResult<Vec<User>> {
    let sql = format!("SELECT {USER_COLUMNS} FROM users ORDER BY created_at, id");
    let mut stmt = conn.prepare_cached(&sql)?;

    // Строки собираются в `Vec` до разбора: `query_map` держит `stmt`
    // занятым, а `into_user` может отказать (битый UUID, неизвестная
    // таймзона) — и тогда `?` посреди итерации по живому курсору.
    let rows: Vec<UserRow> = stmt
        .query_map([], read_row)?
        .collect::<rusqlite::Result<_>>()?;

    rows.into_iter().map(into_user).collect()
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
    fn list_users_orders_by_created_at_not_by_insertion() {
        // Интеграционный тест на порядок не доказывает ничего: два
        // пользователя, вставленные подряд, ложатся по возрастанию и по
        // `created_at`, и по первичному ключу — `users` объявлена
        // `WITHOUT ROWID`, её обход идёт по кластерному индексу UUIDv7, а
        // тот монотонен по времени. Обход совпадает с `ORDER BY`
        // случайно, и сортировку можно снять незаметно. На том же уже
        // обжигались в `first_api_key` и в `load_heartbeats`.
        //
        // Здесь `created_at` задаётся напрямую и идёт против порядка
        // вставки — тогда `ORDER BY` становится единственным, что даёт
        // верный ответ. Сырой SQL в модульном тесте внутри `src/` для того
        // и позволен: три места в `tests/repository.rs` — про схему, а это
        // про то, чего через публичный интерфейс не выразить.
        let mut conn = open_in_memory().unwrap();
        migrate(&mut conn).unwrap();

        let named = |login: &str| NewUser {
            login: login.to_owned(),
            ..a_user()
        };
        let later = insert_user(&conn, &named("вставлен первым")).unwrap();
        let earlier = insert_user(&conn, &named("вставлен вторым")).unwrap();

        // Второму по вставке приписываем более раннее время.
        conn.execute(
            "UPDATE users SET created_at = ?2 WHERE id = ?1",
            rusqlite::params![uuid_to_blob(earlier.id), 1_000_i64],
        )
        .unwrap();
        conn.execute(
            "UPDATE users SET created_at = ?2 WHERE id = ?1",
            rusqlite::params![uuid_to_blob(later.id), 2_000_i64],
        )
        .unwrap();

        let listed = list_users(&conn).unwrap();
        let logins: Vec<&str> = listed.iter().map(|user| user.login.as_str()).collect();
        assert_eq!(logins, vec!["вставлен вторым", "вставлен первым"]);
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

    #[test]
    fn a_login_is_trimmed_and_cannot_be_empty() {
        // Инвариант живёт здесь, а не у дверей: дверей две сегодня
        // (экран настройки, `wakode user create`) и станет четыре в 3b.
        // До этой правки экран триммил, а CLI — нет, и логин `"  админ  "`
        // сохранялся с пробелами.
        let mut conn = open_in_memory().unwrap();
        migrate(&mut conn).unwrap();

        let named = |login: &str| NewUser {
            login: login.to_owned(),
            ..a_user()
        };

        let user = insert_user(&conn, &named("  админ  ")).unwrap();
        assert_eq!(user.login, "админ", "логин сохранён с пробелами");
        assert!(
            find_user_by_login(&conn, "админ").unwrap().is_some(),
            "по обрезанному логину пользователь не находится"
        );

        assert!(matches!(
            insert_user(&conn, &named("")),
            Err(StoreError::LoginEmpty)
        ));
        assert!(
            matches!(
                insert_user(&conn, &named("   ")),
                Err(StoreError::LoginEmpty)
            ),
            "логин из одних пробелов принят"
        );
    }

    #[test]
    fn a_taken_login_is_a_domain_error_not_a_raw_sqlite_failure() {
        // `StoreError` заведён ровно за этим: «вызывающий не должен
        // разбирать коды SQLite, чтобы понять, что произошло». Без
        // классификации CLI печатал занятый логин как
        // `база данных: UNIQUE constraint failed: users.login: Error code
        // 2067` — поломку хранилища вместо вины вызывающего, да ещё
        // языком, на котором в этом проекте не говорят.
        let mut conn = open_in_memory().unwrap();
        migrate(&mut conn).unwrap();

        insert_user(&conn, &a_user()).unwrap();
        let err = insert_user(&conn, &a_user()).unwrap_err();

        assert!(
            matches!(&err, StoreError::LoginTaken(login) if login == "swrneko"),
            "получили {err:?}"
        );
        assert!(
            !err.to_string().contains("UNIQUE"),
            "сырой текст SQLite уехал наружу: {err}"
        );
    }

    #[test]
    fn lookup_trims_the_login_the_same_way_insertion_does() {
        // Асимметрия хуже отсутствия обеих проверок: в базе лежит
        // обрезанный логин, а поиск по строке с пробелами его не находит,
        // и `wakode key issue --user "  swrneko  "` сообщает «нет
        // пользователя» про существующего.
        let mut conn = open_in_memory().unwrap();
        migrate(&mut conn).unwrap();

        insert_user(&conn, &a_user()).unwrap();
        assert!(
            find_user_by_login(&conn, "  swrneko  ").unwrap().is_some(),
            "вставка триммит, а поиск — нет"
        );
    }
}
