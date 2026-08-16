use std::collections::BTreeSet;

use chrono::NaiveDate;
use chrono_tz::Tz;
use rusqlite::{Connection, Transaction};
use uuid::Uuid;
use wakode_core::{local_day_of, Micros};

use crate::codec::uuid_to_blob;
use crate::error::{StoreError, StoreResult};

/// Какие локальные дни пользователя затронуты набором моментов.
///
/// День берётся через `local_day_of`, а не через смещение UTC: у зон с
/// переходом времени эти два ответа расходятся, и ключ пометки должен
/// совпадать с ключом, по которому потом будет считаться сводка.
pub fn affected_days(times: impl IntoIterator<Item = Micros>, tz: Tz) -> BTreeSet<NaiveDate> {
    times.into_iter().map(|t| local_day_of(t, tz)).collect()
}

pub fn mark_dirty(
    tx: &Transaction<'_>,
    user: Uuid,
    days: &BTreeSet<NaiveDate>,
    now: Micros,
) -> StoreResult<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO dirty_days(user_id, local_date, marked_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(user_id, local_date) DO UPDATE SET marked_at = excluded.marked_at",
    )?;
    for day in days {
        stmt.execute(rusqlite::params![
            uuid_to_blob(user),
            day.to_string(),
            now.get()
        ])?;
    }
    Ok(())
}

/// Помеченные дни пользователя в порядке возрастания.
///
/// Волна 1 будет по ним пересчитывать кеш сводок; здесь функция нужна ещё и
/// затем, чтобы тест пометки читал результат через тот же интерфейс, а не
/// через собственный `SELECT`.
pub fn dirty_days_for(conn: &Connection, user: Uuid) -> StoreResult<Vec<NaiveDate>> {
    let mut stmt = conn.prepare_cached(
        "SELECT local_date FROM dirty_days WHERE user_id = ?1 ORDER BY local_date",
    )?;
    let rows = stmt.query_map([uuid_to_blob(user)], |row| row.get::<_, String>(0))?;

    let mut days = Vec::new();
    for row in rows {
        let text = row?;
        // Дата в базе записана только `mark_dirty`, форматом `NaiveDate::to_string`.
        // Непарсящееся значение означает порчу базы, а не ошибку ввода.
        days.push(
            text.parse::<NaiveDate>()
                .map_err(|_| StoreError::Corrupt(format!("дата в dirty_days: {text}")))?,
        );
    }
    Ok(days)
}
