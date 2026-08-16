use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rusqlite::Connection;
use wakode_core::Sid;

use crate::codec::i64_to_sid;
use crate::error::StoreResult;

/// Словарь интернированных строк, целиком поднятый в память.
///
/// Обе стороны отображения нужны: писателю — `строка → номер`, читателю —
/// `номер → строка`. Строки лежат за `Arc`, поэтому обе карты делят одну
/// копию текста, а `resolve` не копирует ничего.
///
/// Писатель ровно один — пишущая задача, — поэтому запись под замком редка,
/// а чтение почти никогда не встречает конкуренции.
#[derive(Debug, Default)]
pub struct Interner {
    inner: RwLock<Maps>,
}

#[derive(Debug, Default)]
struct Maps {
    by_value: HashMap<Arc<str>, Sid>,
    by_id: HashMap<Sid, Arc<str>>,
}

impl Interner {
    /// Поднять словарь из базы. Зовётся один раз при старте.
    pub fn load(conn: &Connection) -> StoreResult<Self> {
        let mut stmt = conn.prepare("SELECT id, value FROM strings")?;
        let mut maps = Maps::default();

        let rows = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let value: String = row.get(1)?;
            Ok((id, value))
        })?;

        for row in rows {
            let (id, value) = row?;
            let sid = i64_to_sid(id)?;
            let text: Arc<str> = Arc::from(value);
            maps.by_value.insert(Arc::clone(&text), sid);
            maps.by_id.insert(sid, text);
        }

        Ok(Self {
            inner: RwLock::new(maps),
        })
    }

    pub fn resolve(&self, sid: Sid) -> Option<Arc<str>> {
        let maps = self.inner.read().expect("словарь отравлен паникой");
        maps.by_id.get(&sid).cloned()
    }

    pub fn lookup(&self, value: &str) -> Option<Sid> {
        let maps = self.inner.read().expect("словарь отравлен паникой");
        maps.by_value.get(value).copied()
    }

    /// Выдать номера для набора строк, вставив недостающие.
    ///
    /// Возвращает номера **в том же порядке и той же длины**, что вход:
    /// вызывающий подставляет их в колонки отметки по позиции. Повторы
    /// внутри одного батча дают один номер.
    ///
    /// Запрос к базе идёт вне замка: замок берётся дважды и ненадолго —
    /// сперва на чтение, чтобы понять, чего не хватает, потом на запись,
    /// чтобы вписать найденное. Само обращение к SQLite между ними держит
    /// только `Connection`, а не `RwLock`.
    pub fn intern_batch(&self, conn: &Connection, values: &[&str]) -> StoreResult<Vec<Sid>> {
        // Сначала пробуем закрыть всё, что уже известно, под лёгким замком.
        let known: Vec<Option<Sid>> = {
            let maps = self.inner.read().expect("словарь отравлен паникой");
            values
                .iter()
                .map(|value| maps.by_value.get(*value).copied())
                .collect()
        };

        if known.iter().all(Option::is_some) {
            return Ok(known.into_iter().map(Option::unwrap).collect());
        }

        // Разрешаем недостающие значения через базу, замок при этом не
        // держим вовсе. Повторы внутри батча дедуплицируем заранее, чтобы не
        // слать лишние запросы за одним и тем же значением.
        let mut stmt = conn.prepare_cached(
            "INSERT INTO strings(value) VALUES (?1)
             ON CONFLICT(value) DO UPDATE SET value = value
             RETURNING id",
        )?;

        let mut resolved: HashMap<&str, Sid> = HashMap::new();
        for (value, cached) in values.iter().zip(&known) {
            if cached.is_some() || resolved.contains_key(*value) {
                continue;
            }
            let id: i64 = stmt.query_row([value], |row| row.get(0))?;
            let sid = i64_to_sid(id)?;
            resolved.insert(value, sid);
        }
        drop(stmt);

        // Короткая запись только для того, чтобы вписать разрешённое в карты
        // — никакого обращения к базе под замком.
        {
            let mut maps = self.inner.write().expect("словарь отравлен паникой");
            for (value, sid) in &resolved {
                if !maps.by_value.contains_key(*value) {
                    let text: Arc<str> = Arc::from(*value);
                    maps.by_value.insert(Arc::clone(&text), *sid);
                    maps.by_id.insert(*sid, text);
                }
            }
        }

        let out = values
            .iter()
            .zip(known)
            .map(|(value, cached)| cached.unwrap_or_else(|| resolved[value]))
            .collect();

        Ok(out)
    }
}
