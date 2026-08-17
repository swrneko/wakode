use std::collections::HashMap;
use std::fmt;
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
///
/// **Экземпляр предполагается один на файл базы.** [`load`](Self::load)
/// снимает состояние `strings` на момент вызова, а дальше карты пополняет
/// только [`intern_batch`](Self::intern_batch) — то есть память этого
/// экземпляра всегда идёт вровень с базой или впереди неё. Внутри процесса
/// это безопасно: `SqliteStore` держит один `Arc<Interner>` и раздаёт его
/// всем. А вот второй экземпляр на том же файле — второй процесс,
/// миграционная утилита — строк, интернированных первым после его `load`,
/// не увидит, и `resolve` вернёт по ним `None`. Отметка отрисуется без
/// имени файла: без ошибки, без записи в лог и без единого способа заметить
/// это, кроме как глазами в интерфейсе.
#[derive(Default)]
pub struct Interner {
    inner: RwLock<Maps>,
}

/// Печатает **размер** словаря, а не его содержимое.
///
/// Производный `Debug` вывалил бы обе карты целиком, а в них лежат пути к
/// файлам и названия проектов всех пользователей сразу — самое чувствительное,
/// что есть в базе трекера. Достаточно одного `tracing::debug!(?state)` или
/// `#[instrument]` в HTTP-слое, который держит `SqliteStore` (а тот держит
/// `Arc<Interner>`), чтобы весь словарь уехал в лог.
impl fmt::Debug for Interner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Отравленный замок здесь не повод паниковать, в отличие от `resolve`:
        // `Debug` зовут как раз из путей сообщения об ошибке, и паника внутри
        // форматирования подменила бы исходную причину собой.
        let maps = self.inner.read().unwrap_or_else(|poisoned| poisoned.into_inner());
        f.debug_struct("Interner")
            .field("strings", &maps.by_id.len())
            .finish()
    }
}

/// `Debug` намеренно не выводится: карты видны только через [`Interner`], а
/// его собственная реализация печатает размер вместо содержимого.
#[derive(Default)]
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
    /// **Зовётся вне открытой транзакции.** Метод открывает свою и коммитит
    /// её сам, прежде чем вписать что-либо в память: словарь монотонен —
    /// попавшая в него строка оттуда не уходит — и обязан быть долговечнее
    /// любой операции, которая им пользуется. Замок на запись берётся только
    /// после коммита и только чтобы вписать уже разрешённые номера — ни
    /// одного обращения к базе под ним.
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

        // Своя транзакция, а не транзакция вызывающего: иначе её откат унёс
        // бы строки из базы, но не из памяти, и словарь начал бы выдавать
        // номера, которым в `strings` ничего не соответствует. Следующая же
        // отметка с таким номером упёрлась бы во внешний ключ.
        let tx = conn.unchecked_transaction()?;
        // Повторы внутри батча закрываются этой картой, а не повторным
        // запросом за тем же значением: батч отметок приносит имя проекта по
        // разу на каждую отметку, а `DO UPDATE` — настоящая перезапись
        // строки, а не холостой ход, так что поход в базу за каждым
        // повторением был бы расточителен. Заодно карта держит ровно одну
        // `Arc` на новое значение — если бы каждый повтор заводил свою
        // `Arc::from`, `by_value` (вставляется первым) и `by_id`
        // (вставляется последним из `fresh`) хранили бы разные аллокации
        // одной и той же строки, и обещание докстрока «обе карты делят одну
        // копию текста» переставало бы быть правдой именно для повторяющихся
        // новых значений.
        let mut fresh: HashMap<&str, (Arc<str>, Sid)> = HashMap::new();
        let mut out = Vec::with_capacity(values.len());

        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO strings(value) VALUES (?1)
                 ON CONFLICT(value) DO UPDATE SET value = value
                 RETURNING id",
            )?;

            for (value, cached) in values.iter().zip(known) {
                if let Some(sid) = cached {
                    out.push(sid);
                    continue;
                }
                if let Some((_, sid)) = fresh.get(*value) {
                    out.push(*sid);
                    continue;
                }

                let id: i64 = stmt.query_row([value], |row| row.get(0))?;
                let sid = i64_to_sid(id)?;
                out.push(sid);
                fresh.insert(value, (Arc::from(*value), sid));
            }
        }

        tx.commit()?;

        // Замок берётся только теперь — на вписывание уже разрешённых
        // номеров, без единого запроса к базе под ним. Любая ошибка выше
        // оставляет словарь ровно таким, каким он был.
        {
            let mut maps = self.inner.write().expect("словарь отравлен паникой");
            for (text, sid) in fresh.into_values() {
                maps.by_value.insert(Arc::clone(&text), sid);
                maps.by_id.insert(sid, text);
            }
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{migrate, open_in_memory};

    #[test]
    fn debug_shows_the_size_of_the_dictionary_but_not_its_contents() {
        let mut conn = open_in_memory().unwrap();
        migrate(&mut conn).unwrap();

        let interner = Interner::load(&conn).unwrap();
        interner
            .intern_batch(&conn, &["/home/swrneko/секрет.rs", "секретный-проект"])
            .unwrap();

        let dump = format!("{interner:?}");

        assert!(
            !dump.contains("секрет.rs") && !dump.contains("секретный-проект"),
            "словарь не должен попадать в лог целиком: {dump}"
        );
        assert!(
            dump.contains('2'),
            "размер словаря печатать надо — по нему и отлаживают: {dump}"
        );
    }
}
