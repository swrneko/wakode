use chrono_tz::Tz;
use rusqlite::Connection;
use uuid::Uuid;
use wakode_core::{Category, EntityKind, Micros, Sid};

use crate::codec::{category_to_i64, kind_to_i64, sid_to_i64, uuid_to_blob};
use crate::dedup::dedup_hash;
use crate::dirty::{affected_days, mark_dirty};
use crate::error::StoreResult;
use crate::interner::Interner;

/// Отметка как она пришла с провода: строки ещё не интернированы.
#[derive(Debug, Clone)]
pub struct IncomingHeartbeat {
    pub time: Micros,
    pub entity: String,
    pub kind: EntityKind,
    pub category: Category,
    pub project: Option<String>,
    pub branch: Option<String>,
    pub language: Option<String>,
    pub editor: Option<String>,
    pub os: Option<String>,
    pub machine: Option<String>,
    pub plugin: Option<String>,
    pub is_write: bool,
    pub lines: Option<i64>,
    pub lineno: Option<i64>,
    pub cursorpos: Option<i64>,
    pub line_additions: Option<i64>,
    pub line_deletions: Option<i64>,
    pub project_root_count: Option<i64>,
    pub dependencies: Option<String>,
    pub ai_line_changes: Option<i64>,
    pub human_line_changes: Option<i64>,
    pub ai_meta: Option<String>,
}

/// Что случилось с отдельной отметкой батча.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Inserted,
    Duplicate,
}

/// Судьба каждой отметки батча, выровненная с входом по индексу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertReport {
    pub outcomes: Vec<Outcome>,
}

impl InsertReport {
    pub fn inserted(&self) -> usize {
        self.outcomes.iter().filter(|o| **o == Outcome::Inserted).count()
    }

    pub fn duplicates(&self) -> usize {
        self.outcomes.iter().filter(|o| **o == Outcome::Duplicate).count()
    }
}

/// Записать батч отметок.
///
/// Вставка отметок и пометка затронутых дней идут **в одной транзакции**:
/// успех сообщается только после её коммита, потому что cli, получив успех,
/// стирает отметки из своей очереди. Интернирование строк в эту транзакцию
/// **не входит**: оно уже позади и коммитит свою собственную — словарь
/// монотонен, а его копия в памяти про откат ничего не знает, так что общий
/// с отметками откат вынул бы строки из базы, оставив номера в памяти.
pub fn insert_heartbeats(
    conn: &mut Connection,
    interner: &Interner,
    user: Uuid,
    batch: &[IncomingHeartbeat],
    tz: Tz,
) -> StoreResult<InsertReport> {
    if batch.is_empty() {
        return Ok(InsertReport { outcomes: Vec::new() });
    }

    let now = crate::clock::now();

    // Все строки батча одним заходом: меньше запросов и меньше времени под
    // замком словаря.
    let mut texts: Vec<&str> = Vec::new();
    for hb in batch {
        texts.push(&hb.entity);
        for optional in [
            &hb.project, &hb.branch, &hb.language,
            &hb.editor, &hb.os, &hb.machine, &hb.plugin,
        ] {
            if let Some(value) = optional {
                texts.push(value);
            }
        }
    }
    // Строки интернируются **до** открытия транзакции и коммитятся своей.
    // Словарь монотонен: строка, попавшая в него, оттуда уже не уходит, и
    // держать её в одной транзакции с отметками нельзя — откат вставки унёс
    // бы строки из базы, но не из памяти интернера. Осиротевшая строка в
    // `strings`, на которую в итоге никто не сослался, стоит нескольких
    // байт и будет переиспользована при следующей же попытке.
    let ids = interner.intern_batch(conn, &texts)?;

    let tx = conn.transaction()?;
    let mut cursor = 0usize;
    let mut outcomes = Vec::with_capacity(batch.len());

    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO heartbeats
               (id, user_id, time, received_at, entity_id, kind, category,
                project_id, branch_id, language_id, editor_id, os_id,
                machine_id, plugin_id, is_write, lines, lineno, cursorpos,
                line_additions, line_deletions, project_root_count,
                dependencies, ai_line_changes, human_line_changes, ai_meta,
                dedup_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24,
                     ?25, ?26)",
        )?;

        for hb in batch {
            let entity = ids[cursor];
            cursor += 1;

            let project = take_next(&ids, &mut cursor, hb.project.is_some());
            let branch = take_next(&ids, &mut cursor, hb.branch.is_some());
            let language = take_next(&ids, &mut cursor, hb.language.is_some());
            let editor = take_next(&ids, &mut cursor, hb.editor.is_some());
            let os = take_next(&ids, &mut cursor, hb.os.is_some());
            let machine = take_next(&ids, &mut cursor, hb.machine.is_some());
            let plugin = take_next(&ids, &mut cursor, hb.plugin.is_some());

            let attrs = wakode_core::Attrs {
                entity,
                kind: hb.kind,
                category: hb.category,
                project,
                branch,
                language,
                editor,
                os,
                machine,
            };
            let hash = dedup_hash(user, hb.time, &attrs, hb.is_write);

            let affected = stmt.execute(rusqlite::params![
                uuid_to_blob(Uuid::now_v7()),
                uuid_to_blob(user),
                hb.time.get(),
                now.get(),
                sid_to_i64(entity),
                kind_to_i64(hb.kind),
                category_to_i64(hb.category),
                project.map(sid_to_i64),
                branch.map(sid_to_i64),
                language.map(sid_to_i64),
                editor.map(sid_to_i64),
                os.map(sid_to_i64),
                machine.map(sid_to_i64),
                plugin.map(sid_to_i64),
                i64::from(hb.is_write),
                hb.lines,
                hb.lineno,
                hb.cursorpos,
                hb.line_additions,
                hb.line_deletions,
                hb.project_root_count,
                hb.dependencies,
                hb.ai_line_changes,
                hb.human_line_changes,
                hb.ai_meta,
                hash,
            ])?;
            // `INSERT OR IGNORE` возвращает 0, если строку отбил уникальный
            // индекс по (user_id, dedup_hash) — это и есть признак повтора.
            //
            // Предупреждение: `OR IGNORE` глушит не только конфликт
            // уникальности, но и любой другой — `NOT NULL`, `CHECK`. Колонка
            // или ограничение, добавленные будущей миграцией, превратят
            // потерю отметки в тихий `Duplicate`, и наружу это не всплывёт
            // никак.
            outcomes.push(if affected == 1 {
                Outcome::Inserted
            } else {
                Outcome::Duplicate
            });
        }
    }

    // Только реально вставленные: повторная доставка очереди cli — штатный
    // сценарий, и день, уже пересчитанный волной 1, не должен пачкаться
    // заново из-за отметки, которая ничего не изменила в базе.
    let inserted_times = batch
        .iter()
        .zip(&outcomes)
        .filter(|(_, outcome)| **outcome == Outcome::Inserted)
        .map(|(hb, _)| hb.time);
    let days = affected_days(inserted_times, tz);
    mark_dirty(&tx, user, &days, now)?;

    tx.commit()?;

    Ok(InsertReport { outcomes })
}

/// Достать следующий номер строки из результата пакетного интернирования.
///
/// `intern_batch` возвращает номера ровно в том порядке, в каком строки были
/// сложены в запрос: сперва `entity` отметки, затем её заполненные
/// необязательные поля. Курсор идёт по тому же порядку, поэтому вызовы выше
/// обязаны повторять порядок укладки один в один — иначе проект отметки
/// получит номер ветки, и это не поймает ни один тип.
fn take_next(ids: &[Sid], cursor: &mut usize, present: bool) -> Option<Sid> {
    if !present {
        return None;
    }
    let sid = ids[*cursor];
    *cursor += 1;
    Some(sid)
}
