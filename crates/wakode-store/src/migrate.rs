use rusqlite::Connection;

use crate::error::{StoreError, StoreResult};
use crate::schema::MIGRATIONS;

/// Применить к базе все недостающие миграции.
pub fn migrate(conn: &mut Connection) -> StoreResult<()> {
    apply(conn, MIGRATIONS)
}

/// Текущая версия схемы — она же число применённых миграций.
pub fn schema_version(conn: &Connection) -> StoreResult<i32> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Прокрутить набор миграций. Вынесено отдельно от [`migrate`], чтобы тесты
/// могли гонять механизм на игрушечном наборе, не завися от схемы волны 0.
fn apply(conn: &mut Connection, migrations: &[&str]) -> StoreResult<()> {
    let current = schema_version(conn)?;
    let target = i32::try_from(migrations.len())
        .map_err(|_| StoreError::OutOfRange("слишком много миграций"))?;

    if current > target {
        // База сделана более новой сборкой. Продолжать нельзя: мы не знаем,
        // что там за колонки, и молча испортим данные.
        return Err(StoreError::SchemaTooNew {
            found: current,
            supported: target,
        });
    }

    for (index, sql) in migrations.iter().enumerate().skip(current as usize) {
        let version = index as i32 + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        // Прагму нельзя параметризовать, поэтому число подставляется в текст.
        // Оно наше собственное и получено из длины массива, не из ввода.
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    /// Подменяем настоящий набор миграций на игрушечный: тест проверяет
    /// механизм прокрутки, а не содержимое схемы волны 0.
    const TOY: &[&str] = &[
        "CREATE TABLE a(x INTEGER)",
        "CREATE TABLE b(y INTEGER)",
    ];

    #[test]
    fn fresh_database_gets_every_migration() {
        let mut conn = open_in_memory().unwrap();
        apply(&mut conn, TOY).unwrap();

        assert_eq!(schema_version(&conn).unwrap(), 2);
        conn.execute("INSERT INTO a VALUES (1)", []).unwrap();
        conn.execute("INSERT INTO b VALUES (1)", []).unwrap();
    }

    #[test]
    fn applying_twice_changes_nothing() {
        let mut conn = open_in_memory().unwrap();
        apply(&mut conn, TOY).unwrap();
        apply(&mut conn, TOY).unwrap();

        assert_eq!(schema_version(&conn).unwrap(), 2);
    }

    #[test]
    fn only_the_missing_migrations_run() {
        let mut conn = open_in_memory().unwrap();
        apply(&mut conn, &TOY[..1]).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 1);

        // Второй прогон видит уже применённую первую миграцию и не пытается
        // создать таблицу `a` повторно — иначе упал бы на «table a already exists».
        apply(&mut conn, TOY).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 2);
    }

    #[test]
    fn database_from_the_future_is_refused() {
        let mut conn = open_in_memory().unwrap();
        apply(&mut conn, TOY).unwrap();

        let err = apply(&mut conn, &TOY[..1]).unwrap_err();
        assert!(
            matches!(err, StoreError::SchemaTooNew { found: 2, supported: 1 }),
            "получили {err:?}"
        );
    }

    #[test]
    fn a_failing_migration_leaves_the_version_untouched() {
        let mut conn = open_in_memory().unwrap();
        let broken: &[&str] = &["CREATE TABLE a(x INTEGER)", "ЭТО НЕ SQL"];

        assert!(apply(&mut conn, broken).is_err());
        assert_eq!(
            schema_version(&conn).unwrap(),
            1,
            "первая миграция должна остаться применённой, вторая — откатиться целиком"
        );
    }
}
