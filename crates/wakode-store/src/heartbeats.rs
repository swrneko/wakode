use chrono_tz::Tz;
use rusqlite::Connection;
use uuid::Uuid;
use wakode_core::{Attrs, Category, EntityKind, Heartbeat, Micros, Sid};

use crate::codec::{
    category_to_i64, i64_to_category, i64_to_kind, i64_to_sid, kind_to_i64, sid_to_i64,
    uuid_to_blob,
};
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
///
/// Идентификатор носит **вариант**, а не отчёт рядом с ним: строки у
/// повтора нет, а значит, нет и её идентификатора, и пара
/// `(Outcome, Uuid)` позволила бы приписать повтору идентификатор
/// несуществующей строки. Здесь это непредставимо.
///
/// Наружу этот идентификатор уезжает ответом WakaTime-совместимого
/// эндпоинта. Чем отвечать на повтор — решает HTTP-слой: у него на это
/// есть снятая с живого константа, у хранилища её быть не должно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Строка записана; идентификатор — её.
    Inserted(Uuid),
    /// Строку отбил уникальный индекс: в базе ничего не появилось.
    Duplicate,
}

/// Судьба каждой отметки батча, выровненная с входом по индексу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertReport {
    pub outcomes: Vec<Outcome>,
}

impl InsertReport {
    pub fn inserted(&self) -> usize {
        self.outcomes.iter().filter(|o| matches!(o, Outcome::Inserted(_))).count()
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

            // Идентификатор заводится здесь, потому что здесь же он уезжает
            // в колонку `id`: посчитанный вторым местом по тому же правилу,
            // он разошёлся бы с записанным молча.
            let id = Uuid::now_v7();

            let affected = stmt.execute(rusqlite::params![
                uuid_to_blob(id),
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
                Outcome::Inserted(id)
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
        .filter(|(_, outcome)| matches!(outcome, Outcome::Inserted(_)))
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

/// Поднять отметки пользователя за полуинтервал `[from, to)`.
///
/// Границы совпадают с тем, что отдаёт `wakode_core::heartbeat_window`, —
/// то есть уже расширены на таймаут в обе стороны. Сортировка по времени
/// делается индексом `hb_time`, а не в Rust: движок длительностей всё равно
/// отсортирует вход, но упорядоченная выборка дешевле и делает результат
/// воспроизводимым.
pub fn load_heartbeats(
    conn: &Connection,
    user: Uuid,
    from: Micros,
    to: Micros,
) -> StoreResult<Vec<Heartbeat>> {
    let mut stmt = conn.prepare_cached(
        "SELECT time, entity_id, kind, category,
                project_id, branch_id, language_id, editor_id, os_id, machine_id
         FROM heartbeats
         WHERE user_id = ?1 AND time >= ?2 AND time < ?3
         ORDER BY time",
    )?;

    let rows = stmt.query_map(
        rusqlite::params![uuid_to_blob(user), from.get(), to.get()],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<i64>>(9)?,
            ))
        },
    )?;

    let mut out = Vec::new();
    for row in rows {
        let (time, entity, kind, category, project, branch, language, editor, os, machine) = row?;
        out.push(Heartbeat {
            time: Micros::new(time),
            attrs: Attrs {
                entity: i64_to_sid(entity)?,
                kind: i64_to_kind(kind)?,
                category: i64_to_category(category)?,
                project: project.map(i64_to_sid).transpose()?,
                branch: branch.map(i64_to_sid).transpose()?,
                language: language.map(i64_to_sid).transpose()?,
                editor: editor.map(i64_to_sid).transpose()?,
                os: os.map(i64_to_sid).transpose()?,
                machine: machine.map(i64_to_sid).transpose()?,
            },
        });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::users::{insert_user, NewUser};
    use crate::{migrate, open_in_memory};

    fn a_user() -> NewUser {
        NewUser {
            login: "swrneko".to_owned(),
            email: None,
            password_hash: "непрозрачные байты из плана 3".to_owned(),
            display_name: None,
            timezone: "Europe/Moscow".parse().unwrap(),
            timeout_secs: 900,
            is_admin: false,
        }
    }

    /// Отметка со всеми заполненными полями.
    ///
    /// Значения — те же, что в `every_attribute_survives_the_round_trip`
    /// (`tests/repository.rs`), чтобы два теста не разошлись описанием одной
    /// и той же отметки. Расхождение ровно одно: `is_write` здесь `false`.
    /// Числа обязаны быть **попарно различны**, иначе перестановка двух
    /// соседних `INTEGER`-параметров запишет то же самое и не поймается, а
    /// `true` дало бы в колонке `is_write` единицу — ровно то же, что в
    /// соседней `lines`.
    fn full_heartbeat() -> IncomingHeartbeat {
        IncomingHeartbeat {
            time: Micros::from_secs(1_755_000_000),
            entity: "сущность".to_owned(),
            kind: EntityKind::App,
            category: Category::Debugging,
            project: Some("проект".to_owned()),
            branch: Some("ветка".to_owned()),
            language: Some("язык".to_owned()),
            editor: Some("редактор".to_owned()),
            os: Some("ос".to_owned()),
            machine: Some("машина".to_owned()),
            plugin: Some("плагин".to_owned()),
            is_write: false,
            lines: Some(1),
            lineno: Some(2),
            cursorpos: Some(3),
            line_additions: Some(4),
            line_deletions: Some(5),
            project_root_count: Some(6),
            dependencies: Some("зависимости".to_owned()),
            ai_line_changes: Some(7),
            human_line_changes: Some(8),
            ai_meta: Some("мета".to_owned()),
        }
    }

    /// Двенадцать колонок `INSERT`, которых не читает никто.
    ///
    /// `load_heartbeats` берёт десять колонок, `Attrs` несёт девять полей —
    /// а `plugin_id`, `is_write` и десять числовых и текстовых колонок за
    /// ними не читает ни один путь кода. Отсутствие читателя не делает
    /// ошибку неважной: оно делает её бесшумной и необратимой. Переставленные
    /// местами `lines` и `lineno` ничего не уронят и никуда не попадут — они
    /// будут неправильно писаться с первого дня, а всплывёт это в волне 1,
    /// когда в базе уже лежат месяцы отметок, которые задним числом не
    /// расшить. Сырой `SELECT` здесь — единственный доступный читатель.
    #[test]
    fn every_unread_column_lands_in_the_place_the_insert_promised() {
        let mut conn = open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        let user = insert_user(&conn, &a_user()).unwrap();
        let interner = Interner::load(&conn).unwrap();

        insert_heartbeats(
            &mut conn,
            &interner,
            user.id,
            &[full_heartbeat()],
            user.timezone,
        )
        .unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT plugin_id, is_write, lines, lineno, cursorpos,
                        line_additions, line_deletions, project_root_count,
                        dependencies, ai_line_changes, human_line_changes, ai_meta
                 FROM heartbeats",
            )
            .unwrap();
        let row = stmt
            .query_row([], |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                ))
            })
            .unwrap();

        let (
            plugin,
            is_write,
            lines,
            lineno,
            cursorpos,
            line_additions,
            line_deletions,
            project_root_count,
            dependencies,
            ai_line_changes,
            human_line_changes,
            ai_meta,
        ) = row;

        let plugin_sid = interner.lookup("плагин").expect("плагин интернирован");
        assert_eq!(plugin, Some(sid_to_i64(plugin_sid)), "plugin_id");
        assert_eq!(is_write, 0, "is_write");
        assert_eq!(lines, Some(1), "lines");
        assert_eq!(lineno, Some(2), "lineno");
        assert_eq!(cursorpos, Some(3), "cursorpos");
        assert_eq!(line_additions, Some(4), "line_additions");
        assert_eq!(line_deletions, Some(5), "line_deletions");
        assert_eq!(project_root_count, Some(6), "project_root_count");
        assert_eq!(dependencies.as_deref(), Some("зависимости"), "dependencies");
        assert_eq!(ai_line_changes, Some(7), "ai_line_changes");
        assert_eq!(human_line_changes, Some(8), "human_line_changes");
        assert_eq!(ai_meta.as_deref(), Some("мета"), "ai_meta");
    }

    /// Идентификатор из отчёта — это идентификатор записанной строки.
    ///
    /// Читается сырым `SELECT`: `load_heartbeats` колонку `id` не берёт, и
    /// другого читателя у неё нет. Отчёт со свежим, но чужим `Uuid` внешне
    /// неотличим от верного — HTTP-слой отдаст его клиенту, клиент сохранит,
    /// и не найдёт по нему ничего никогда. Ровно поэтому идентификатор и
    /// заведён в `wakode-store`, а не пересчитан в `wakode-api`.
    #[test]
    fn the_reported_id_is_the_id_of_the_row_that_was_written() {
        let mut conn = open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        let user = insert_user(&conn, &a_user()).unwrap();
        let interner = Interner::load(&conn).unwrap();

        let report = insert_heartbeats(
            &mut conn,
            &interner,
            user.id,
            &[full_heartbeat()],
            user.timezone,
        )
        .unwrap();

        let Outcome::Inserted(reported) = report.outcomes[0] else {
            panic!("отметка обязана была вставиться: {:?}", report.outcomes)
        };

        let stored: Vec<u8> = conn
            .query_row("SELECT id FROM heartbeats", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stored, uuid_to_blob(reported), "отчёт назвал не ту строку");
    }

    /// Повторная доставка не переписывает метку уже помеченного дня.
    ///
    /// Это и есть наблюдаемый эффект фильтра по `Outcome::Inserted`:
    /// `mark_dirty` делает `DO UPDATE SET marked_at = excluded.marked_at`, и
    /// без фильтра батч, целиком отбитый дедупликацией, всё равно обновил бы
    /// `marked_at` — то есть день, уже пересчитанный волной 1, пачкался бы
    /// заново. Через `dirty_days_for` этого не видно: он отдаёт даты без
    /// `marked_at`, поэтому колонка читается сырым `SELECT`.
    #[test]
    fn redelivered_batch_leaves_the_mark_of_an_already_dirty_day_alone() {
        let mut conn = open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        let user = insert_user(&conn, &a_user()).unwrap();
        let interner = Interner::load(&conn).unwrap();

        let batch = [full_heartbeat()];
        let first =
            insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();
        assert_eq!(first.inserted(), 1);

        // Метка отодвигается в заведомо невозможное прошлое, а не сравнивается
        // с прежней: оба вызова берут системные часы, и два показания подряд
        // могут совпасть до микросекунды — тест зазеленел бы там, где фильтра
        // нет. Ноль же с `clock::now()` не совпадёт никогда.
        conn.execute("UPDATE dirty_days SET marked_at = 0", []).unwrap();

        let again =
            insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();
        assert_eq!(again.duplicates(), 1, "тот же батч обязан отбиться целиком");

        let marked_at: i64 = conn
            .query_row("SELECT marked_at FROM dirty_days", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            marked_at, 0,
            "день, которому повтор ничего не добавил, не должен помечаться заново"
        );
    }
}
