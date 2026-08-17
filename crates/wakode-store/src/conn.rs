use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::error::StoreResult;

/// Сколько ждать освобождения блокировки, прежде чем вернуть `SQLITE_BUSY`.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Открыть базу по пути, создав файл при отсутствии.
pub fn open(path: &Path) -> StoreResult<Connection> {
    let conn = Connection::open(path)?;
    configure(&conn)?;
    Ok(conn)
}

/// Открыть базу в памяти — для тестов.
pub fn open_in_memory() -> StoreResult<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    Ok(conn)
}

/// Прагмы, которые обязаны стоять на *каждом* соединении.
///
/// `journal_mode` — свойство файла и переживает переоткрытие, а вот
/// `foreign_keys` и `busy_timeout` живут ровно столько, сколько соединение.
/// Поэтому единая точка настройки: соединение, открытое мимо неё, будет
/// молча вести себя иначе.
fn configure(conn: &Connection) -> StoreResult<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opened_database_is_in_wal_mode() {
        let conn = open_in_memory().unwrap();
        // У базы в памяти WAL недоступен, режим остаётся memory —
        // проверяем на файле, ради которого прагма и ставится.
        let dir = tempfile::tempdir().unwrap();
        let conn_file = open(&dir.path().join("wakode.db")).unwrap();

        let mode: String = conn_file
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");

        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk, 1, "внешние ключи должны быть включены явно: в SQLite они выключены по умолчанию");
    }
}
