# wakode-store Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Построить крейт `wakode-store` — слой хранения на SQLite за асинхронным репозиторным трейтом: миграции, интернирование строк, дедупликация отметок, чтение диапазонов, пользователи, ключи и сессии.

**Architecture:** Синхронный `rusqlite` внутри, асинхронный трейт снаружи. Записи идут через единственную пишущую задачу с каналом и подтверждением по `oneshot` — это убирает борьбу за блокировку SQLite и даёт групповую фиксацию. Чтения идут мимо неё, по отдельным соединениям в WAL-режиме, через `spawn_blocking`. Строковые атрибуты хранятся номерами, словарь целиком живёт в памяти.

**Tech Stack:** Rust (edition 2024), `rusqlite` с фичей `bundled` (SQLite вкомпилирован в бинарь), `tokio`, `uuid` (v7), `xxhash-rust` (стабильный хеш для дедупликации), `thiserror`, `chrono-tz`. Зависит от `wakode-core`.

## Global Constraints

- Спека: `docs/superpowers/specs/2026-08-15-wakode-design.md`, разделы 4, 5, 6, 8, 9.
- Крейт `wakode-core` **не трогаем**, кроме одной точечной правки в задаче 4 (явные дискриминанты `EntityKind`), которая делается по образцу уже существующего решения для `Category`.
- **Драйвер — `rusqlite` с фичей `bundled`.** Никакого `sqlx`. `bundled` обязателен: цель проекта — развернуть один бинарь без установки libsqlite3 на сервере.
- **Миграции свои, на `PRAGMA user_version`.** Массив SQL-строк, прокрутка недостающих в транзакции, бамп версии. Библиотека миграций не подключается.
- **Весь словарь интернированных строк грузится в память при старте**, в обе стороны (`строка → id`, `id → строка`), под `RwLock`.
- **Трейт репозитория асинхронный, реализация внутри синхронная.** Без этого обещание «Postgres добавляется позже без переписывания логики» пустое.
- **Криптографии в этом крейте нет.** Хранилище пишет непрозрачные байты: `password_hash: String`, `key_encrypted: Vec<u8>`, `key_lookup: Vec<u8>`. Argon2, шифрование мастер-ключом и HMAC живут в плане 3 — иначе выбор примитивов окажется зашит в план про SQLite.
- **Валидации домена в этом крейте нет.** Отсечка отметок из будущего (спека §8) — обязанность HTTP-слоя плана 3. Хранилище пишет, что дали.
- Время везде — микросекунды от эпохи UTC, `i64` в базе, `wakode_core::Micros` в Rust.
- Первичные ключи — UUIDv7, хранятся `BLOB` по 16 байт (`Uuid::as_bytes()`).
- Все публичные типы выводят `Debug`.
- Каждая задача заканчивается коммитом. Сообщения коммитов на русском, **без каких-либо упоминаний ИИ-ассистентов** (`Co-Authored-By`, Claude, Anthropic, «Generated with»). Жёсткий запрет владельца репозитория.
- Тесты `store` — интеграционные и **не утверждают ни текста SQL, ни формы таблиц** (спека §9). Сквозной набор через репозиторный трейт появляется в задаче 12 — именно он и есть гарантия того, что переезд на Postgres не потребует правки тестов. Тесты задач 6–11 зовут функции напрямую: трейта на тот момент ещё нет, и это строительные леса, а не окончательный уровень проверки. SQL разрешён ровно в трёх местах, и все три проверяют саму схему, а не поведение поверх неё: состав таблиц, уникальность индекса `hb_dedup` и срабатывание `ON DELETE CASCADE`.
- Повтор при `SQLITE_BUSY` (спека §8) отдельным кодом **не пишется**: его делает сам SQLite по прагме `busy_timeout`, которую ставит `conn.rs`. Своя петля поверх неё дублировала бы механизм и мешала ему.
- Вывод тестов чистый: ноль предупреждений, включая этап компиляции.

## Отклонения от спеки, принятые сознательно

1. **Колонка `type` переименована в `kind`.** В `wakode-core` поле называется `kind` (`Attrs::kind: EntityKind`), и код отображения читается вдвое легче, когда имена совпадают. Плюс `type` — слово, которое в SQL-контекстах регулярно требует экранирования.
2. **Колонка `category` объявлена `NOT NULL`.** В спеке она nullable, но в core `Attrs::category` — не `Option`: отсутствующая категория уже отображена в `Category::Coding` на границе десериализации, а нераспознанная — в `Category::Unknown`. Nullable-колонка добавила бы третье состояние, которого в домене нет.
3. **Таблицы волны 1** (`rules`, `overrides`, `manual_entries`, `imports`) в миграции 1 **не создаются**. Спека говорит «схема заложена под все волны сразу», но таблица, к которой нет кода, неизбежно разъезжается с тем кодом, который для неё однажды напишут. Миграции ровно для того и нужны, чтобы добавить их в волне 1. `teams` и `team_members` — исключение: спека прямо требует их с первого дня.

## Файловая структура

```
crates/wakode-store/
  Cargo.toml
  src/
    lib.rs           # реэкспорт публичного API
    error.rs         # StoreError
    clock.rs         # единственная точка обращения к системным часам
    conn.rs          # открытие соединения, прагмы
    schema.rs        # DDL миграций — массив версий
    migrate.rs       # прокрутка по user_version
    codec.rs         # Uuid↔BLOB, Sid↔i64, доменные enum↔INTEGER
    dedup.rs         # детерминированный хеш отметки
    interner.rs      # словарь строк в памяти
    heartbeats.rs    # вставка с дедупом, чтение диапазона
    dirty.rs         # пометка затронутых локальных дней
    users.rs         # пользователи
    keys.rs          # API-ключи
    sessions.rs      # сессии
    writer.rs        # пишущая задача, канал, backpressure
    repo.rs          # асинхронные трейты и их SQLite-реализация
  tests/
    repository.rs    # интеграционные тесты через трейт
```

Разбиение по ответственности: `codec.rs` знает только про представление типов в базе, `interner.rs` — только про словарь, `writer.rs` — только про сериализацию записей во времени. Ни один из трёх не знает про два других.

---

### Task 1: Каркас крейта и соединение с базой

**Files:**
- Modify: `Cargo.toml` (корневой — добавить member и workspace-зависимости)
- Create: `crates/wakode-store/Cargo.toml`
- Create: `crates/wakode-store/src/lib.rs`
- Create: `crates/wakode-store/src/error.rs`
- Create: `crates/wakode-store/src/conn.rs`

**Interfaces:**
- Consumes: ничего.
- Produces: `StoreError` (enum ошибок крейта), `StoreResult<T> = Result<T, StoreError>`, `open(path: &Path) -> StoreResult<Connection>`, `open_in_memory() -> StoreResult<Connection>`.

- [ ] **Step 1: Добавить крейт в workspace**

В корневом `Cargo.toml` в `[workspace] members` добавь `"crates/wakode-store"`. В `[workspace.dependencies]` добавь строки ниже, но **версии не выдумывай** — выполни `cargo add` в следующем шаге и впиши то, что он разрешит.

- [ ] **Step 2: Завести крейт и подтянуть зависимости**

```bash
cargo new --lib crates/wakode-store --name wakode-store --vcs none
cd crates/wakode-store
cargo add rusqlite --features bundled
cargo add tokio --features rt,sync,macros
cargo add uuid --features v7
cargo add xxhash-rust --features xxh3
cargo add thiserror
cargo add wakode-core --path ../wakode-core
cargo add --dev tempfile
cargo add --dev tokio --features rt-multi-thread,macros
cd ../..
```

Затем приведи `crates/wakode-store/Cargo.toml` к стилю `wakode-core`: `edition`, `license` через `workspace = true`, а версии зависимостей вынеси в `[workspace.dependencies]` корневого манифеста и сошлись на них через `{ workspace = true }`. Фичи остаются в манифесте крейта.

Проверь, что `bundled` действительно включён — без него на машине без libsqlite3 сборка упадёт, и это выяснится только на чужом сервере.

- [ ] **Step 3: Написать падающий тест на открытие базы**

`crates/wakode-store/src/conn.rs`:

```rust
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
```

- [ ] **Step 4: Запустить тест и убедиться, что он падает**

Run: `cargo test -p wakode-store`
Expected: FAIL — функций `open`/`open_in_memory` не существует.

- [ ] **Step 5: Реализовать ошибки**

`crates/wakode-store/src/error.rs`:

```rust
use thiserror::Error;

/// Ошибки слоя хранения.
///
/// Тип намеренно не пробрасывает `rusqlite::Error` наружу как есть в тех
/// случаях, когда у ошибки есть доменный смысл: вызывающий не должен
/// разбирать коды SQLite, чтобы понять, что очередь записи переполнена.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("база данных: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("схема базы новее, чем понимает эта сборка: в файле версия {found}, поддерживается до {supported}")]
    SchemaTooNew { found: i32, supported: i32 },

    #[error("очередь записи переполнена, повторите позже")]
    WriteQueueFull,

    #[error("пишущая задача остановлена")]
    WriterGone,

    #[error("значение не помещается в тип: {0}")]
    OutOfRange(&'static str),

    #[error("повреждённые данные в базе: {0}")]
    Corrupt(String),

    #[error("фоновая задача упала")]
    TaskPanicked,
}

pub type StoreResult<T> = Result<T, StoreError>;
```

- [ ] **Step 6: Реализовать открытие соединения**

`crates/wakode-store/src/conn.rs`:

```rust
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
```

`synchronous = NORMAL` в паре с WAL — общепринятый компромисс: коммит не ждёт fsync, но при падении процесса данные целы; потерять можно только при отказе питания, и то последние транзакции.

- [ ] **Step 7: Завести единственную точку обращения к часам**

`crates/wakode-store/src/clock.rs`:

```rust
use std::time::{SystemTime, UNIX_EPOCH};

use wakode_core::Micros;

/// Текущее время в микросекундах от эпохи UTC.
///
/// Единственное место в крейте, где берутся системные часы. Вынесено
/// отдельным модулем, чтобы обращений к ним не расползлось: `received_at`,
/// `created_at` и отметки об использовании ключа должны браться из одного
/// источника, иначе в пределах одной транзакции появятся расходящиеся
/// значения «сейчас».
pub(crate) fn now() -> Micros {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Micros::new(since_epoch.as_micros() as i64)
}
```

- [ ] **Step 8: Собрать `lib.rs`**

```rust
//! Слой хранения wakode: SQLite за репозиторным трейтом.

pub(crate) mod clock;
pub mod conn;
pub mod error;

pub use conn::{open, open_in_memory};
pub use error::{StoreError, StoreResult};
```

- [ ] **Step 9: Запустить тесты**

Run: `cargo test -p wakode-store`
Expected: PASS, один тест. Предупреждений нет — `clock::now` пока никем не зовётся, поэтому пометь модуль `#[allow(dead_code)]` и сними пометку в задаче 7, когда появится первый вызов.

- [ ] **Step 10: Коммит**

```bash
git add Cargo.toml Cargo.lock crates/wakode-store
git commit -m "feat(store): каркас крейта и настройка соединения с SQLite"
```

---

### Task 2: Механизм миграций на user_version

**Files:**
- Create: `crates/wakode-store/src/migrate.rs`
- Create: `crates/wakode-store/src/schema.rs`
- Modify: `crates/wakode-store/src/lib.rs`

**Interfaces:**
- Consumes: `StoreError`, `StoreResult` из задачи 1.
- Produces: `migrate(conn: &mut Connection) -> StoreResult<()>`, `schema_version(conn: &Connection) -> StoreResult<i32>`, `schema::MIGRATIONS: &[&str]`.

`MIGRATIONS[0]` — миграция до версии 1, `MIGRATIONS[1]` — до версии 2, и так далее. Текущая версия базы равна числу применённых миграций.

- [ ] **Step 1: Написать падающие тесты механизма**

`crates/wakode-store/src/migrate.rs`:

```rust
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
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p wakode-store`
Expected: FAIL — `apply`/`schema_version` не существуют.

- [ ] **Step 3: Реализовать прокрутку**

`crates/wakode-store/src/migrate.rs`:

```rust
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
```

- [ ] **Step 4: Завести пустой набор миграций**

`crates/wakode-store/src/schema.rs`:

```rust
/// Миграции по порядку: `MIGRATIONS[0]` поднимает схему до версии 1.
///
/// Массив только дополняется. Уже применённую миграцию **править нельзя** —
/// у существующих баз она не переприменится, и схема разъедется с кодом.
/// Изменение схемы = новая строка в конце.
pub const MIGRATIONS: &[&str] = &[];
```

- [ ] **Step 5: Подключить модули**

В `lib.rs` добавь `pub mod migrate;`, `pub mod schema;` и `pub use migrate::{migrate, schema_version};`.

- [ ] **Step 6: Запустить тесты**

Run: `cargo test -p wakode-store`
Expected: PASS, шесть тестов.

- [ ] **Step 7: Коммит**

```bash
git add crates/wakode-store
git commit -m "feat(store): миграции схемы на PRAGMA user_version"
```

---

### Task 3: Схема волны 0

**Files:**
- Modify: `crates/wakode-store/src/schema.rs`
- Create: `crates/wakode-store/tests/repository.rs`

**Interfaces:**
- Consumes: `migrate` из задачи 2.
- Produces: `MIGRATIONS` с единственной миграцией — схемой волны 0.

- [ ] **Step 1: Написать падающий тест схемы**

`crates/wakode-store/tests/repository.rs`:

```rust
use wakode_store::{migrate, open_in_memory, schema_version};

/// Единственный тест в этом файле, которому позволено знать про SQL:
/// он проверяет саму схему, а не поведение поверх неё.
#[test]
fn wave_zero_schema_creates_every_table() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    assert_eq!(schema_version(&conn).unwrap(), 1);

    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    let tables: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .filter(|name: &String| !name.starts_with("sqlite_"))
        .collect();

    assert_eq!(
        tables,
        vec![
            "api_keys",
            "dirty_days",
            "heartbeats",
            "sessions",
            "strings",
            "team_members",
            "teams",
            "users",
        ]
    );
}

#[test]
fn heartbeat_dedup_index_is_unique() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    let unique: i64 = conn
        .query_row(
            "SELECT [unique] FROM pragma_index_list('heartbeats') WHERE name = 'hb_dedup'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unique, 1, "без уникальности индекса дедупликация не работает");
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p wakode-store --test repository`
Expected: FAIL — таблиц нет, `MIGRATIONS` пуст.

- [ ] **Step 3: Написать схему**

`crates/wakode-store/src/schema.rs` — замени пустой массив:

```rust
pub const MIGRATIONS: &[&str] = &[WAVE_ZERO];

/// Схема волны 0.
///
/// Таблицы слоёв поверх сырья (`rules`, `overrides`, `manual_entries`,
/// `imports`) сюда не входят: к ним нет кода до волны 1, а таблица без кода
/// неизбежно разъезжается с тем кодом, который для неё однажды напишут.
/// `teams` — исключение, спека требует их с первого дня.
const WAVE_ZERO: &str = r#"
CREATE TABLE strings (
  id    INTEGER PRIMARY KEY,
  value TEXT NOT NULL UNIQUE
);

CREATE TABLE users (
  id            BLOB PRIMARY KEY,
  login         TEXT NOT NULL UNIQUE,
  email         TEXT,
  password_hash TEXT NOT NULL,
  display_name  TEXT,
  timezone      TEXT NOT NULL,
  timeout_secs  INTEGER NOT NULL,
  is_admin      INTEGER NOT NULL DEFAULT 0,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE api_keys (
  id            BLOB PRIMARY KEY,
  user_id       BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name          TEXT NOT NULL,
  key_encrypted BLOB NOT NULL,
  key_lookup    BLOB NOT NULL UNIQUE,
  created_at    INTEGER NOT NULL,
  last_used_at  INTEGER,
  revoked_at    INTEGER
) WITHOUT ROWID;

CREATE TABLE sessions (
  id         BLOB PRIMARY KEY,
  user_id    BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash BLOB NOT NULL UNIQUE,
  user_agent TEXT,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  revoked_at INTEGER
) WITHOUT ROWID;

CREATE TABLE heartbeats (
  id                 BLOB NOT NULL PRIMARY KEY,
  user_id            BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  time               INTEGER NOT NULL,
  received_at        INTEGER NOT NULL,
  entity_id          INTEGER NOT NULL REFERENCES strings(id),
  kind               INTEGER NOT NULL,
  category           INTEGER NOT NULL,
  project_id         INTEGER REFERENCES strings(id),
  branch_id          INTEGER REFERENCES strings(id),
  language_id        INTEGER REFERENCES strings(id),
  editor_id          INTEGER REFERENCES strings(id),
  os_id              INTEGER REFERENCES strings(id),
  machine_id         INTEGER REFERENCES strings(id),
  plugin_id          INTEGER REFERENCES strings(id),
  is_write           INTEGER NOT NULL,
  lines              INTEGER,
  lineno             INTEGER,
  cursorpos          INTEGER,
  line_additions     INTEGER,
  line_deletions     INTEGER,
  project_root_count INTEGER,
  dependencies       TEXT,
  ai_line_changes    INTEGER,
  human_line_changes INTEGER,
  ai_meta            TEXT,
  dedup_hash         INTEGER NOT NULL
);

CREATE UNIQUE INDEX hb_dedup ON heartbeats(user_id, dedup_hash);
CREATE INDEX hb_time ON heartbeats(user_id, time);

CREATE TABLE dirty_days (
  user_id    BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  local_date TEXT NOT NULL,
  marked_at  INTEGER NOT NULL,
  PRIMARY KEY (user_id, local_date)
) WITHOUT ROWID;

CREATE TABLE teams (
  id         BLOB PRIMARY KEY,
  name       TEXT NOT NULL,
  owner_id   BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE team_members (
  team_id   BLOB NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  user_id   BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role      TEXT NOT NULL,
  joined_at INTEGER NOT NULL,
  PRIMARY KEY (team_id, user_id)
) WITHOUT ROWID;

CREATE INDEX api_keys_user ON api_keys(user_id);
CREATE INDEX sessions_user ON sessions(user_id);
CREATE INDEX teams_owner ON teams(owner_id);
CREATE INDEX team_members_user ON team_members(user_id);
"#;
```

`WITHOUT ROWID` стоит там, где строки узкие и первичный ключ действительно используется для поиска. У `strings` его нет намеренно: там PK и есть rowid, автоинкремент нужен для выдачи номеров.

**Почему `heartbeats` — обычная rowid-таблица, вопреки спеке §5.** WITHOUT ROWID окупается на узких строках, к которым ходят по первичному ключу. Здесь не выполнено ни одно из условий: `dependencies` и `ai_meta` — TEXT произвольной длины, и в WITHOUT ROWID их overflow-цепочки лягут внутрь того самого B-дерева, по которому идут сканы диапазона; а по `id` не ищет никто — все запросы идут через `(user_id, time)`. Вдобавок каждая запись `hb_time` тащила бы 16-байтовый blob первичного ключа вместо короткого rowid. `id` остаётся `NOT NULL PRIMARY KEY`: `NOT NULL` тут выписан явно, потому что в rowid-таблице `PRIMARY KEY` на не-INTEGER колонке его не подразумевает — давняя особенность SQLite, сохранённая ради совместимости.

**Почему на внешние ключи заведены индексы.** При `foreign_keys=ON` каждое `DELETE FROM users` заставляет SQLite искать детей во всех ссылающихся таблицах; без индекса это полный скан. У `heartbeats` и `dirty_days` проблемы нет — там `user_id` уже левый префикс `hb_dedup` и первичного ключа соответственно. У остальных четырёх колонок индекса не было.

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p wakode-store`
Expected: PASS, восемь тестов.

- [ ] **Step 5: Коммит**

```bash
git add crates/wakode-store
git commit -m "feat(store): схема волны 0"
```

---

### Task 4: Кодек доменных типов

**Files:**
- Modify: `crates/wakode-core/src/domain.rs` (явные дискриминанты `EntityKind`)
- Create: `crates/wakode-store/src/codec.rs`
- Modify: `crates/wakode-store/src/lib.rs`

**Interfaces:**
- Consumes: `wakode_core::{Category, EntityKind, Sid}`.
- Produces: `uuid_to_blob(Uuid) -> [u8; 16]`, `blob_to_uuid(&[u8]) -> StoreResult<Uuid>`, `sid_to_i64(Sid) -> i64`, `i64_to_sid(i64) -> StoreResult<Sid>`, `kind_to_i64(EntityKind) -> i64`, `i64_to_kind(i64) -> StoreResult<EntityKind>`, `category_to_i64(Category) -> i64`, `i64_to_category(i64) -> StoreResult<Category>`.

**Почему эта задача существует.** `Category` в core уже имеет `#[repr(u8)]` с явными дискриминантами и контракт-тест — это делалось ровно потому, что число уходит в базу. `EntityKind` уходит в базу точно так же (колонка `kind INTEGER NOT NULL`), но его дискриминанты сейчас неявные: они равны порядку объявления, и перестановка вариантов молча переименует уже записанные данные. Ту же дыру закрываем тем же способом.

Отдельно: ревью `wakode-core` оставило прямое указание — **целое из базы отображать в вариант явным `match`, никогда через serde**. Выведенный serde-визитор принимает числовые индексы вариантов, а они равны позициям в объявлении, а не дискриминантам.

- [ ] **Step 1: Написать падающий тест кодека**

`crates/wakode-store/src/codec.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Контракт хранения. Числа тут — то, что уже лежит в чужих базах.
    /// Менять их нельзя; можно только дописывать новые варианты в конец.
    const KIND_CONTRACT: &[(EntityKind, i64)] = &[
        (EntityKind::File, 0),
        (EntityKind::App, 1),
        (EntityKind::Url, 2),
        (EntityKind::Domain, 3),
    ];

    #[test]
    fn entity_kind_round_trips_through_its_pinned_number() {
        for (kind, number) in KIND_CONTRACT {
            assert_eq!(kind_to_i64(*kind), *number, "{kind:?}");
            assert_eq!(i64_to_kind(*number).unwrap(), *kind, "{number}");
        }
    }

    #[test]
    fn unknown_kind_number_is_reported_not_guessed() {
        assert!(i64_to_kind(99).is_err());
        assert!(i64_to_kind(-1).is_err());
    }

    #[test]
    fn category_round_trips_and_unknown_survives() {
        // Перечислены все двадцать два варианта, а не образцовая горстка:
        // `match` в `i64_to_category` — независимая копия дискриминантов из
        // core, и перепутанные соседние номера в ней при выборочной проверке
        // не поймает никто. Список ещё и заставляет дописать сюда строку при
        // добавлении варианта, что и требуется.
        for category in [
            Category::Unknown,
            Category::Advising,
            Category::AiCoding,
            Category::Browsing,
            Category::Building,
            Category::CodeReviewing,
            Category::Coding,
            Category::Communicating,
            Category::Debugging,
            Category::Designing,
            Category::Indexing,
            Category::Learning,
            Category::ManualTesting,
            Category::Meeting,
            Category::Notes,
            Category::Planning,
            Category::Researching,
            Category::RunningTests,
            Category::Supporting,
            Category::Translating,
            Category::WritingDocs,
            Category::WritingTests,
        ] {
            let number = category_to_i64(category);
            assert_eq!(i64_to_category(number).unwrap(), category);
        }
        assert_eq!(category_to_i64(Category::Unknown), 0);
        assert_eq!(category_to_i64(Category::Coding), 6);
        assert_eq!(category_to_i64(Category::WritingTests), 21);
    }

    #[test]
    fn unknown_category_number_maps_to_unknown_not_an_error() {
        // Тут поведение сознательно отличается от `EntityKind`: категорию мог
        // прислать более новый плагин, и терять из-за неё всю отметку нельзя.
        // Вид сущности приходит из закрытого списка нашего же кода.
        assert_eq!(i64_to_category(99).unwrap(), Category::Unknown);
    }

    #[test]
    fn uuid_round_trips_through_sixteen_bytes() {
        let id = uuid::Uuid::now_v7();
        let blob = uuid_to_blob(id);
        assert_eq!(blob.len(), 16);
        assert_eq!(blob_to_uuid(&blob).unwrap(), id);
    }

    #[test]
    fn wrong_length_blob_is_rejected() {
        assert!(blob_to_uuid(&[0u8; 15]).is_err());
        assert!(blob_to_uuid(&[]).is_err());
    }

    #[test]
    fn sid_round_trips_and_negative_is_rejected() {
        assert_eq!(i64_to_sid(sid_to_i64(Sid(4_000_000_000))).unwrap(), Sid(4_000_000_000));
        assert!(i64_to_sid(-1).is_err(), "номер строки не бывает отрицательным");
        assert!(
            i64_to_sid(i64::from(u32::MAX) + 1).is_err(),
            "Sid — u32, значение шире него потерялось бы молча"
        );
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p wakode-store`
Expected: FAIL — модуля `codec` нет.

- [ ] **Step 3: Закрепить дискриминанты `EntityKind` в core**

`crates/wakode-core/src/domain.rs` — приведи `EntityKind` к тому же виду, что уже сделан для `Category`:

```rust
/// Что за сущность правил в редакторе.
///
/// Числа уходят в колонку `kind` таблицы `heartbeats`, поэтому дискриминанты
/// заданы явно: перестановка вариантов без этого молча переименовала бы уже
/// записанные данные. Новые варианты дописываются в конец, существующие
/// не перенумеровываются.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum EntityKind {
    #[default]
    File = 0,
    App = 1,
    Url = 2,
    Domain = 3,
}
```

И добавь в тесты `domain.rs` контракт по образцу `CATEGORY_CONTRACT`:

```rust
    #[test]
    fn entity_kind_discriminants_are_pinned() {
        const CONTRACT: &[(EntityKind, u8)] = &[
            (EntityKind::File, 0),
            (EntityKind::App, 1),
            (EntityKind::Url, 2),
            (EntityKind::Domain, 3),
        ];
        for (kind, number) in CONTRACT {
            assert_eq!(*kind as u8, *number, "{kind:?}");
        }
        assert_eq!(CONTRACT.len(), 4, "новый вариант — новая строка контракта");
    }
```

- [ ] **Step 4: Реализовать кодек**

`crates/wakode-store/src/codec.rs`:

```rust
use uuid::Uuid;
use wakode_core::{Category, EntityKind, Sid};

use crate::error::{StoreError, StoreResult};

pub fn uuid_to_blob(id: Uuid) -> [u8; 16] {
    *id.as_bytes()
}

pub fn blob_to_uuid(blob: &[u8]) -> StoreResult<Uuid> {
    let bytes: [u8; 16] = blob
        .try_into()
        .map_err(|_| StoreError::Corrupt(format!("UUID из {} байт вместо 16", blob.len())))?;
    Ok(Uuid::from_bytes(bytes))
}

pub fn sid_to_i64(sid: Sid) -> i64 {
    i64::from(sid.0)
}

pub fn i64_to_sid(value: i64) -> StoreResult<Sid> {
    u32::try_from(value)
        .map(Sid)
        .map_err(|_| StoreError::OutOfRange("номер строки не помещается в u32"))
}

pub fn kind_to_i64(kind: EntityKind) -> i64 {
    kind as u8 as i64
}

/// Явный `match` вместо serde: выведенный визитор идентификаторов принимает
/// числовые индексы вариантов, а они равны позициям в объявлении, а не
/// дискриминантам. Совпадение сегодня и расхождение завтра.
pub fn i64_to_kind(value: i64) -> StoreResult<EntityKind> {
    match value {
        0 => Ok(EntityKind::File),
        1 => Ok(EntityKind::App),
        2 => Ok(EntityKind::Url),
        3 => Ok(EntityKind::Domain),
        other => Err(StoreError::Corrupt(format!("неизвестный вид сущности: {other}"))),
    }
}

pub fn category_to_i64(category: Category) -> i64 {
    category as u8 as i64
}

/// В отличие от вида сущности, неизвестная категория — не порча данных, а
/// более новый плагин. Отметку из-за неё терять нельзя.
pub fn i64_to_category(value: i64) -> StoreResult<Category> {
    Ok(match value {
        0 => Category::Unknown,
        1 => Category::Advising,
        2 => Category::AiCoding,
        3 => Category::Browsing,
        4 => Category::Building,
        5 => Category::CodeReviewing,
        6 => Category::Coding,
        7 => Category::Communicating,
        8 => Category::Debugging,
        9 => Category::Designing,
        10 => Category::Indexing,
        11 => Category::Learning,
        12 => Category::ManualTesting,
        13 => Category::Meeting,
        14 => Category::Notes,
        15 => Category::Planning,
        16 => Category::Researching,
        17 => Category::RunningTests,
        18 => Category::Supporting,
        19 => Category::Translating,
        20 => Category::WritingDocs,
        21 => Category::WritingTests,
        _ => Category::Unknown,
    })
}
```

- [ ] **Step 5: Подключить модуль и прогнать оба крейта**

Добавь `pub mod codec;` в `lib.rs`.

Run: `cargo test --workspace`
Expected: PASS. В `wakode-core` появился один новый тест, в `wakode-store` — семь.

- [ ] **Step 6: Коммит**

```bash
git add crates
git commit -m "feat(store): кодек доменных типов и закрепление дискриминантов EntityKind"
```

---

### Task 5: Детерминированный хеш дедупликации

**Files:**
- Create: `crates/wakode-store/src/dedup.rs`
- Modify: `crates/wakode-store/src/lib.rs`

**Interfaces:**
- Consumes: `codec` из задачи 4, `wakode_core::{Attrs, Micros}`.
- Produces: `dedup_hash(user: Uuid, time: Micros, attrs: &Attrs, is_write: bool) -> i64`.

**Ловушка, из-за которой задача отдельная.** `std::collections::hash_map::DefaultHasher` **не гарантирует стабильность между релизами Rust** — это записано в его собственной документации. Хеш, посчитанный им, уедет при обновлении тулчейна, уникальный индекс перестанет ловить повторы, и база тихо наполнится дублями. Поэтому алгоритм берём явный и зафиксированный: `xxh3_64`.

Алгоритм и порядок скармливаемых полей — **часть формата хранения**. Менять их нельзя так же, как нельзя менять применённую миграцию.

- [ ] **Step 1: Написать падающий тест**

`crates/wakode-store/src/dedup.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wakode_core::{Category, EntityKind, Sid};

    fn attrs() -> Attrs {
        Attrs {
            entity: Sid(1),
            kind: EntityKind::File,
            category: Category::Coding,
            project: Some(Sid(2)),
            branch: Some(Sid(3)),
            language: Some(Sid(4)),
            editor: Some(Sid(5)),
            os: Some(Sid(6)),
            machine: Some(Sid(7)),
        }
    }

    #[test]
    fn same_input_gives_the_same_hash() {
        let user = Uuid::now_v7();
        let time = Micros::from_secs(1_755_000_000);
        assert_eq!(
            dedup_hash(user, time, &attrs(), true),
            dedup_hash(user, time, &attrs(), true)
        );
    }

    #[test]
    fn hash_is_pinned_to_an_exact_number() {
        // Значение зафиксировано намеренно: если оно изменится, у всех
        // существующих баз перестанет работать дедупликация. Тест обязан
        // упасть при подмене алгоритма или порядка полей.
        let user = Uuid::from_bytes([7; 16]);
        let time = Micros::from_secs(1_700_000_000);
        assert_eq!(dedup_hash(user, time, &attrs(), true), PINNED);
    }

    #[test]
    fn every_field_changes_the_hash() {
        let user = Uuid::now_v7();
        let other_user = Uuid::now_v7();
        let time = Micros::from_secs(1_755_000_000);
        let base = dedup_hash(user, time, &attrs(), true);

        assert_ne!(base, dedup_hash(other_user, time, &attrs(), true), "пользователь");
        assert_ne!(base, dedup_hash(user, Micros::from_secs(1), &attrs(), true), "время");
        assert_ne!(base, dedup_hash(user, time, &attrs(), false), "признак записи");

        let mut a = attrs();
        a.entity = Sid(99);
        assert_ne!(base, dedup_hash(user, time, &a, true), "сущность");

        let mut a = attrs();
        a.kind = EntityKind::App;
        assert_ne!(base, dedup_hash(user, time, &a, true), "вид");

        let mut a = attrs();
        a.category = Category::Debugging;
        assert_ne!(base, dedup_hash(user, time, &a, true), "категория");

        let mut a = attrs();
        a.project = Some(Sid(99));
        assert_ne!(base, dedup_hash(user, time, &a, true), "проект");

        let mut a = attrs();
        a.branch = Some(Sid(99));
        assert_ne!(base, dedup_hash(user, time, &a, true), "ветка");
    }

    #[test]
    fn absent_and_zero_are_different() {
        // `None` и `Some(Sid(0))` обязаны разойтись: иначе отметка без проекта
        // склеится с отметкой, у которой проект под номером ноль.
        let user = Uuid::now_v7();
        let time = Micros::from_secs(1);

        let mut none = attrs();
        none.project = None;
        let mut zero = attrs();
        zero.project = Some(Sid(0));

        assert_ne!(
            dedup_hash(user, time, &none, true),
            dedup_hash(user, time, &zero, true)
        );

        // То же самое для ветки: механизм у неё общий с проектом, значит и
        // сломать его можно одной правкой на оба поля сразу.
        let mut none = attrs();
        none.branch = None;
        let mut zero = attrs();
        zero.branch = Some(Sid(0));

        assert_ne!(
            dedup_hash(user, time, &none, true),
            dedup_hash(user, time, &zero, true)
        );
    }

    #[test]
    fn client_environment_stays_out_of_the_hash() {
        // Язык, редактор, ОС и машина в хеш не идут: одну и ту же работу cli
        // может дослать из другой сборки плагина, и отметка не должна из-за
        // этого стать «новой». Инвариант проверяется тестом, а не только
        // комментарием: случайно скормить лишнее поле — однострочная правка,
        // после которой перестанут узнаваться все уже записанные отметки.
        let user = Uuid::now_v7();
        let time = Micros::from_secs(1);
        let base = dedup_hash(user, time, &attrs(), true);

        for mutate in [
            (|a: &mut Attrs| a.language = Some(Sid(777))) as fn(&mut Attrs),
            |a: &mut Attrs| a.editor = Some(Sid(777)),
            |a: &mut Attrs| a.os = Some(Sid(777)),
            |a: &mut Attrs| a.machine = Some(Sid(777)),
        ] {
            let mut changed = attrs();
            mutate(&mut changed);
            assert_eq!(dedup_hash(user, time, &changed, true), base);
        }
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p wakode-store`
Expected: FAIL — `dedup_hash` не существует, `PINNED` не определён.

- [ ] **Step 3: Реализовать хеш**

`crates/wakode-store/src/dedup.rs`:

```rust
use uuid::Uuid;
use wakode_core::{Attrs, Micros, Sid};
use xxhash_rust::xxh3::Xxh3;

use crate::codec::{category_to_i64, kind_to_i64, sid_to_i64};

/// Хеш содержимого отметки для уникального индекса `hb_dedup`.
///
/// Алгоритм и порядок полей — часть формата хранения. Их изменение ломает
/// дедупликацию у всех существующих баз так же, как правка применённой
/// миграции ломает схему. `DefaultHasher` из стандартной библиотеки тут
/// неприменим: он прямо документирован как нестабильный между релизами Rust.
///
/// Возвращается `i64`, потому что колонка в SQLite целочисленная и знаковая.
/// Верхний бит переносится как есть — это перетолкование битов, не усечение.
pub fn dedup_hash(user: Uuid, time: Micros, attrs: &Attrs, is_write: bool) -> i64 {
    let mut h = Xxh3::new();

    h.update(user.as_bytes());
    h.update(&time.get().to_le_bytes());
    h.update(&sid_to_i64(attrs.entity).to_le_bytes());
    h.update(&kind_to_i64(attrs.kind).to_le_bytes());
    h.update(&category_to_i64(attrs.category).to_le_bytes());
    feed_optional(&mut h, attrs.project);
    feed_optional(&mut h, attrs.branch);
    h.update(&[u8::from(is_write)]);

    h.digest() as i64
}

/// Отсутствие значения кодируется отдельным маркером, а не нулём: иначе
/// «проекта нет» и «проект под номером ноль» дали бы один хеш.
fn feed_optional(h: &mut Xxh3, value: Option<Sid>) {
    match value {
        Some(sid) => {
            h.update(&[1]);
            h.update(&sid_to_i64(sid).to_le_bytes());
        }
        None => h.update(&[0]),
    }
}
```

Состав полей взят из спеки §5: пользователь, время, сущность, вид, категория, проект, ветка, признак записи. Язык, редактор, ОС и машина в хеш **не входят** — они выводятся из user-agent и не различают события, а их попадание в хеш заставило бы одну и ту же отметку с двух версий плагина считаться двумя разными.

- [ ] **Step 4: Получить и вписать закреплённое значение**

Запусти тесты, возьми фактическое значение из сообщения о падении `hash_is_pinned_to_an_exact_number` и впиши его в константу рядом с тестами:

```rust
    /// Значение, посчитанное текущим алгоритмом. Проверено вручную один раз;
    /// дальше тест сторожит, чтобы оно не поменялось.
    const PINNED: i64 = /* вставить фактическое */;
```

Это единственное место в плане, где число берётся из прогона, а не выводится: смысл теста именно в том, чтобы зафиксировать выход конкретного алгоритма.

- [ ] **Step 5: Запустить тесты**

Run: `cargo test -p wakode-store`
Expected: PASS, пять новых тестов.

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-store
git commit -m "feat(store): детерминированный хеш дедупликации отметок"
```

---

### Task 6: Интернирование строк

**Files:**
- Create: `crates/wakode-store/src/interner.rs`
- Modify: `crates/wakode-store/src/lib.rs`
- Modify: `crates/wakode-store/tests/repository.rs`

**Interfaces:**
- Consumes: `codec::i64_to_sid`, `StoreResult`.
- Produces: `Interner` с методами `load(conn: &Connection) -> StoreResult<Interner>`, `resolve(&self, sid: Sid) -> Option<Arc<str>>`, `lookup(&self, value: &str) -> Option<Sid>`, `intern_batch(&self, conn: &Connection, values: &[&str]) -> StoreResult<Vec<Sid>>`.

`intern_batch` вызывается **только из пишущей задачи** — она единственная имеет право вставлять. `resolve` и `lookup` зовут читатели.

- [ ] **Step 1: Написать падающие тесты**

Добавь в `crates/wakode-store/tests/repository.rs`:

```rust
use wakode_store::Interner;

#[test]
fn interning_the_same_value_twice_gives_the_same_number() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let first = interner.intern_batch(&conn, &["src/main.rs"]).unwrap();
    let second = interner.intern_batch(&conn, &["src/main.rs"]).unwrap();

    assert_eq!(first, second);
}

#[test]
fn interned_value_resolves_back_to_the_original_string() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let ids = interner.intern_batch(&conn, &["wakode", "rust"]).unwrap();

    assert_eq!(&*interner.resolve(ids[0]).unwrap(), "wakode");
    assert_eq!(&*interner.resolve(ids[1]).unwrap(), "rust");
    assert_eq!(interner.lookup("rust"), Some(ids[1]));
    assert_eq!(interner.lookup("не интернировали"), None);
}

#[test]
fn a_batch_with_repeats_inside_it_stays_consistent() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let ids = interner
        .intern_batch(&conn, &["a", "b", "a", "b", "a"])
        .unwrap();

    assert_eq!(ids.len(), 5);
    assert_eq!(ids[0], ids[2]);
    assert_eq!(ids[0], ids[4]);
    assert_eq!(ids[1], ids[3]);
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn dictionary_survives_reopening_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wakode.db");

    let sid = {
        let mut conn = wakode_store::open(&path).unwrap();
        migrate(&mut conn).unwrap();
        let interner = Interner::load(&conn).unwrap();
        interner.intern_batch(&conn, &["постоянная строка"]).unwrap()[0]
    };

    let conn = wakode_store::open(&path).unwrap();
    let interner = Interner::load(&conn).unwrap();

    assert_eq!(&*interner.resolve(sid).unwrap(), "постоянная строка");
    assert_eq!(interner.lookup("постоянная строка"), Some(sid));
}

#[test]
fn intern_batch_commits_before_it_returns() {
    // Доказательство того, что коммит произошёл **внутри** `intern_batch`, а
    // не когда-нибудь потом: второе соединение — независимый наблюдатель, и
    // незакоммиченную запись первого оно увидеть не может в принципе.
    // Переоткрытие базы этого не показывает: там коммит мог случиться и на
    // закрытии соединения.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wakode.db");

    let mut writer = wakode_store::open(&path).unwrap();
    migrate(&mut writer).unwrap();
    let interner = Interner::load(&writer).unwrap();

    let sid = interner.intern_batch(&writer, &["видно снаружи"]).unwrap()[0];

    let observer = wakode_store::open(&path).unwrap();
    let seen = Interner::load(&observer).unwrap();

    assert_eq!(seen.lookup("видно снаружи"), Some(sid));
    assert_eq!(&*seen.resolve(sid).unwrap(), "видно снаружи");
}

#[test]
fn interning_inside_an_open_transaction_is_refused() {
    // Контракт метода — «зовётся вне открытой транзакции». Нарушение обязано
    // быть ошибкой, а не тихой вложенностью: словарь, записавший в память
    // номера из чужой транзакции, переживёт её откат и начнёт врать.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let tx = conn.transaction().unwrap();
    assert!(interner.intern_batch(&tx, &["внутри транзакции"]).is_err());
    drop(tx);

    assert_eq!(interner.lookup("внутри транзакции"), None);
}
```

В последнем тесте `&tx` подставляется в параметр типа `&Connection` через разыменование: `Transaction` реализует `Deref<Target = Connection>`. Отдельного импорта не нужно.

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p wakode-store --test repository`
Expected: FAIL — типа `Interner` нет.

- [ ] **Step 3: Реализовать словарь**

`crates/wakode-store/src/interner.rs`:

```rust
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rusqlite::Connection;
use wakode_core::Sid;

use crate::codec::{i64_to_sid, sid_to_i64};
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
    /// **Зовётся вне открытой транзакции.** Метод открывает свою и
    /// коммитит её сам: словарь обязан быть долговечнее любой операции,
    /// которая им пользуется.
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
        // запросом: батч отметок приносит имя проекта по разу на отметку, и
        // `DO UPDATE` — это настоящая перезапись строки, а не холостой ход.
        // Заодно карта держит по одной `Arc` на значение, чтобы обе карты
        // словаря делили одну копию текста.
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
                fresh.insert(*value, (Arc::from(*value), sid));
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

        let _ = sid_to_i64;
        Ok(out)
    }
}
```

`ON CONFLICT ... DO UPDATE SET value = value` вместо `DO NOTHING` — потому что `RETURNING` при `DO NOTHING` не отдаёт строку, и получить номер уже существующего значения одним запросом не выйдет. Присваивание самому себе — стандартный приём, чтобы конфликт считался обновлением.

`unchecked_transaction` вместо `transaction` — потому что второй требует `&mut Connection`, а интернер держит соединение по `&`. «Unchecked» здесь значит лишь то, что компилятор не проверяет отсутствие другой открытой транзакции на этом же соединении; за это отвечает контракт метода, записанный в его документации.

- [ ] **Step 4: Убрать заглушку и подключить модуль**

Строка `let _ = sid_to_i64;` в коде выше — временная, чтобы импорт не давал предупреждения на промежуточном шаге. Удали её вместе с `sid_to_i64` из списка импортов: в финальном виде функция тут не нужна.

Добавь в `lib.rs`: `pub mod interner;` и `pub use interner::Interner;`.

- [ ] **Step 5: Запустить тесты**

Run: `cargo test -p wakode-store`
Expected: PASS, шесть новых тестов. Предупреждений нет.

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-store
git commit -m "feat(store): интернирование строк со словарём в памяти"
```

---

### Task 7: Пользователи

**Files:**
- Create: `crates/wakode-store/src/users.rs`
- Modify: `crates/wakode-store/src/lib.rs`
- Modify: `crates/wakode-store/tests/repository.rs`

**Interfaces:**
- Consumes: `codec`, `StoreResult`.
- Produces: `NewUser` (входная структура), `User` (запись из базы), `insert_user(conn, &NewUser) -> StoreResult<User>`, `find_user_by_login(conn, &str) -> StoreResult<Option<User>>`, `find_user_by_id(conn, Uuid) -> StoreResult<Option<User>>`.

Пользователь нужен раньше отметок: у `heartbeats.user_id` внешний ключ, а `foreign_keys` мы включили — вставка отметки для несуществующего пользователя обязана падать, и тест на это будет в задаче 8.

- [ ] **Step 1: Написать падающие тесты**

Добавь в `tests/repository.rs`:

```rust
use chrono_tz::Tz;
use wakode_store::{find_user_by_id, find_user_by_login, insert_user, NewUser};

fn a_user(login: &str) -> NewUser {
    NewUser {
        login: login.to_owned(),
        email: None,
        password_hash: "непрозрачные байты из плана 3".to_owned(),
        display_name: None,
        timezone: "Europe/Moscow".parse().unwrap(),
        timeout_secs: 900,
        is_admin: false,
    }
}

#[test]
fn inserted_user_is_found_by_login() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    let created = insert_user(&conn, &a_user("swrneko")).unwrap();
    let found = find_user_by_login(&conn, "swrneko").unwrap().unwrap();

    assert_eq!(found.id, created.id);
    assert_eq!(found.login, "swrneko");
    assert_eq!(found.timezone, Tz::Europe__Moscow);
    assert_eq!(found.timeout_secs, 900);
    assert!(!found.is_admin);
}

#[test]
fn every_field_survives_the_round_trip() {
    // Проверяются **все** поля, а не показательные. Отображение колонок —
    // ровно то место, где индекс `row.get(N)` съезжает на единицу между
    // двумя колонками одного типа: ни компилятор, ни тест по логину такого
    // не заметят. Необязательные поля заполнены намеренно: `None` в них
    // прошёл бы и при полностью потерянной колонке.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    let new = NewUser {
        login: "полный".to_owned(),
        email: Some("почта@пример.рф".to_owned()),
        password_hash: "непрозрачные байты из плана 3".to_owned(),
        display_name: Some("Отображаемое имя".to_owned()),
        timezone: "America/St_Johns".parse().unwrap(),
        timeout_secs: 1800,
        is_admin: true,
    };

    let created = insert_user(&conn, &new).unwrap();
    let found = find_user_by_id(&conn, created.id).unwrap().unwrap();

    assert_eq!(found.id, created.id);
    assert_eq!(found.login, "полный");
    assert_eq!(found.email.as_deref(), Some("почта@пример.рф"));
    assert_eq!(found.password_hash, "непрозрачные байты из плана 3");
    assert_eq!(found.display_name.as_deref(), Some("Отображаемое имя"));
    assert_eq!(found.timezone, Tz::America__St_Johns);
    assert_eq!(found.timeout_secs, 1800);
    assert!(found.is_admin);
    assert_eq!(found.created_at, created.created_at);
    assert_eq!(found.updated_at, created.updated_at);
}

#[test]
fn missing_user_is_none_not_an_error() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    assert!(find_user_by_login(&conn, "нет такого").unwrap().is_none());
    assert!(find_user_by_id(&conn, uuid::Uuid::now_v7()).unwrap().is_none());
}

#[test]
fn duplicate_login_is_refused() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    insert_user(&conn, &a_user("swrneko")).unwrap();
    assert!(insert_user(&conn, &a_user("swrneko")).is_err());
}

#[test]
fn timezone_survives_the_round_trip() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    let mut user = a_user("havana");
    user.timezone = "America/Havana".parse().unwrap();
    let created = insert_user(&conn, &user).unwrap();

    let found = find_user_by_id(&conn, created.id).unwrap().unwrap();
    assert_eq!(found.timezone, Tz::America__Havana);
}
```

Добавь `chrono-tz` в зависимости крейта: `cargo add chrono-tz -p wakode-store` (в манифесте сошлись на `{ workspace = true }`).

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p wakode-store --test repository`
Expected: FAIL — `NewUser` и функций нет.

- [ ] **Step 3: Реализовать**

`crates/wakode-store/src/users.rs`:

```rust
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
```

Время берётся из `crate::clock::now` — это его первый вызов, поэтому сними с модуля `clock` пометку `#[allow(dead_code)]`, поставленную в задаче 1.

Таймзона хранится строкой IANA, а не числом: набор зон меняется с обновлением tzdata, и числовой идентификатор был бы привязан к порядку в конкретной версии.

- [ ] **Step 4: Подключить и прогнать**

В `lib.rs`: `pub mod users;` и `pub use users::{find_user_by_id, find_user_by_login, insert_user, NewUser, User};`.

Run: `cargo test -p wakode-store`
Expected: PASS, четыре новых теста.

- [ ] **Step 5: Коммит**

```bash
git add crates/wakode-store
git commit -m "feat(store): пользователи"
```

---

### Task 8: Вставка отметок с дедупликацией

**Files:**
- Create: `crates/wakode-store/src/heartbeats.rs`
- Create: `crates/wakode-store/src/dirty.rs`
- Modify: `crates/wakode-store/src/lib.rs`
- Modify: `crates/wakode-store/tests/repository.rs`

**Interfaces:**
- Consumes: `Interner`, `dedup_hash`, `codec`, пользователи из задачи 7.
- Produces: `IncomingHeartbeat` (сырая отметка со строками), `Outcome` (`Inserted` | `Duplicate`), `InsertReport { outcomes: Vec<Outcome> }` с методами `inserted()` и `duplicates()`, `insert_heartbeats(conn: &mut Connection, interner: &Interner, user: Uuid, batch: &[IncomingHeartbeat], tz: Tz) -> StoreResult<InsertReport>`, `mark_dirty(tx, user, days, now) -> StoreResult<()>`, `dirty_days_for(conn, user) -> StoreResult<Vec<NaiveDate>>`.

**Ключевое требование спеки §6:** успех отдаётся только после коммита транзакции. wakatime-cli, получив успех, удаляет отметки из своей очереди — ответ до коммита плюс падение равен безвозвратной потере. Поэтому вставка отметок и пометка грязных дней происходят **в одной транзакции**, и ответ уходит только после её коммита.

Интернирование строк в эту транзакцию **не входит** и делается до неё, своей собственной. Словарь монотонен — попавшая в него строка оттуда не уходит, — а его копия в памяти отката не знает: общий с отметками откат вынул бы строки из базы, оставив номера в памяти, и следующая отметка с таким номером упёрлась бы во внешний ключ. Осиротевшая строка в `strings`, на которую в итоге никто не сослался, стоит нескольких байт и будет переиспользована при следующей же попытке. Это единственное место плана, где две операции сознательно разведены по разным транзакциям.

**Почему отчёт пер-элементный, а не два счётчика.** Спека §6 требует пер-элементных статусов в ответе bulk-эндпоинта. Валидацию делает план 3 и до хранилища доводит только годные отметки, но собрать ответ он сможет лишь зная, что случилось с каждой позицией. Пара `(inserted, duplicates)` этого не даёт: по ней нельзя восстановить, какая именно отметка оказалась повтором. `outcomes` выровнен с входом по индексу.

Таймзона — параметр, а не поле пользователя, читаемое изнутри: хранилище не должно ходить в таблицу `users` ради каждой вставки, а вызывающий её уже знает.

- [ ] **Step 1: Написать падающие тесты**

Добавь в `tests/repository.rs`:

```rust
use wakode_core::{Category, EntityKind, Micros};
use wakode_store::{insert_heartbeats, IncomingHeartbeat, Interner};

fn incoming(time_secs: i64, entity: &str, project: Option<&str>) -> IncomingHeartbeat {
    IncomingHeartbeat {
        time: Micros::from_secs(time_secs),
        entity: entity.to_owned(),
        kind: EntityKind::File,
        category: Category::Coding,
        project: project.map(str::to_owned),
        branch: None,
        language: None,
        editor: None,
        os: None,
        machine: None,
        plugin: None,
        is_write: false,
        lines: None,
        lineno: None,
        cursorpos: None,
        line_additions: None,
        line_deletions: None,
        project_root_count: None,
        dependencies: None,
        ai_line_changes: None,
        human_line_changes: None,
        ai_meta: None,
    }
}

#[test]
fn heartbeats_are_stored_and_counted() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let batch = [
        incoming(1_755_000_000, "src/main.rs", Some("wakode")),
        incoming(1_755_000_060, "src/lib.rs", Some("wakode")),
    ];
    let report = insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();

    assert_eq!(report.inserted(), 2);
    assert_eq!(report.duplicates(), 0);
    assert_eq!(report.outcomes, vec![Outcome::Inserted, Outcome::Inserted]);
}

#[test]
fn report_says_which_position_was_the_duplicate() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let first = [incoming(1_755_000_000, "src/main.rs", None)];
    insert_heartbeats(&mut conn, &interner, user.id, &first, user.timezone).unwrap();

    // Вторая отметка новая, первая — повтор. План 3 обязан уметь отличить их
    // по позиции, чтобы собрать пер-элементный ответ bulk-эндпоинта.
    let second = [
        incoming(1_755_000_000, "src/main.rs", None),
        incoming(1_755_000_060, "src/lib.rs", None),
    ];
    let report = insert_heartbeats(&mut conn, &interner, user.id, &second, user.timezone).unwrap();

    assert_eq!(report.outcomes, vec![Outcome::Duplicate, Outcome::Inserted]);
}

#[test]
fn resending_the_same_batch_inserts_nothing_new() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let batch = [incoming(1_755_000_000, "src/main.rs", Some("wakode"))];

    let first = insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();
    let second = insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();

    assert_eq!(first.inserted(), 1);
    assert_eq!(second.inserted(), 0);
    assert_eq!(second.duplicates(), 1, "повторная доставка очереди cli — норма, не ошибка");
}

#[test]
fn the_same_heartbeat_from_two_users_is_not_a_duplicate() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let one = insert_user(&conn, &a_user("one")).unwrap();
    let two = insert_user(&conn, &a_user("two")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let batch = [incoming(1_755_000_000, "src/main.rs", Some("wakode"))];

    let one_report = insert_heartbeats(&mut conn, &interner, one.id, &batch, one.timezone).unwrap();
    let two_report = insert_heartbeats(&mut conn, &interner, two.id, &batch, two.timezone).unwrap();

    assert_eq!(one_report.inserted(), 1);
    assert_eq!(two_report.inserted(), 1);
}

#[test]
fn heartbeat_for_a_missing_user_is_refused() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let ghost = uuid::Uuid::now_v7();
    let batch = [incoming(1_755_000_000, "src/main.rs", None)];
    assert!(
        insert_heartbeats(&mut conn, &interner, ghost, &batch, chrono_tz::UTC).is_err(),
        "внешний ключ должен сработать: без него отметки повиснут в никуда"
    );

    // Атомарность этот тест НЕ доказывает: внешний ключ срабатывает на первой
    // же вставке, то есть до `mark_dirty`, и пустой список дней получился бы
    // и вовсе без транзакции. Прямая проверка отката появится в задаче 9,
    // когда отметки можно будет прочитать.
    assert!(dirty_days_for(&conn, ghost).unwrap().is_empty());
}

#[test]
fn an_empty_batch_touches_nothing() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let report = insert_heartbeats(&mut conn, &interner, user.id, &[], user.timezone).unwrap();

    assert!(report.outcomes.is_empty());
    assert!(dirty_days_for(&conn, user.id).unwrap().is_empty());
}

#[test]
fn the_marked_day_is_local_not_utc() {
    // 1 755 036 000 — 2025-08-12T22:00:00Z. В Москве это уже 13 августа, в
    // UTC — ещё 12-е. Момент выбран именно так: на любом времени внутри
    // суток обеих зон реализация, считающая день по UTC, прошла бы тест
    // незамеченной, а ключ пометки обязан совпадать с ключом будущей сводки.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let moscow = insert_user(&conn, &a_user("москвич")).unwrap();
    let mut greenwich = a_user("гринвич");
    greenwich.timezone = chrono_tz::UTC;
    let greenwich = insert_user(&conn, &greenwich).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let batch = [incoming(1_755_036_000, "src/main.rs", None)];
    insert_heartbeats(&mut conn, &interner, moscow.id, &batch, moscow.timezone).unwrap();
    insert_heartbeats(&mut conn, &interner, greenwich.id, &batch, greenwich.timezone).unwrap();

    assert_eq!(
        dirty_days_for(&conn, moscow.id).unwrap(),
        vec![NaiveDate::from_ymd_opt(2025, 8, 13).unwrap()]
    );
    assert_eq!(
        dirty_days_for(&conn, greenwich.id).unwrap(),
        vec![NaiveDate::from_ymd_opt(2025, 8, 12).unwrap()]
    );
}

#[test]
fn a_duplicate_does_not_widen_the_marked_days() {
    // Дни помечаются по вставленному, а не по всему батчу. Прямо проверить
    // это нечем: снятие пометки — работа волны 1, а без него «день уже
    // пересчитали, пришёл повтор» не воспроизвести. Проверяем то, что
    // наблюдаемо: повтор не добавляет дней сверх уже помеченных.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let batch = [incoming(1_755_000_000, "src/main.rs", None)];
    insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();
    let after_first = dirty_days_for(&conn, user.id).unwrap();

    let report =
        insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();

    assert_eq!(report.duplicates(), 1);
    assert_eq!(dirty_days_for(&conn, user.id).unwrap(), after_first);
}
```

Тесту нужны `use chrono::NaiveDate;` и `use wakode_store::dirty_days_for;` в шапке файла.

Сырого SQL в этих тестах нет и не должно появиться: трёх мест, разрешённых глобальными ограничениями, по-прежнему ровно три, и все три проверяют схему.

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p wakode-store --test repository`
Expected: FAIL — `IncomingHeartbeat` и `insert_heartbeats` не существуют.

- [ ] **Step 3: Реализовать пометку грязных дней**

`crates/wakode-store/src/dirty.rs`:

```rust
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
pub(crate) fn affected_days(times: impl IntoIterator<Item = Micros>, tz: Tz) -> BTreeSet<NaiveDate> {
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
```

Добавь `chrono` в зависимости крейта: `cargo add chrono -p wakode-store --no-default-features --features std`.

- [ ] **Step 4: Реализовать вставку отметок**

`crates/wakode-store/src/heartbeats.rs`:

```rust
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
/// успех сообщается только после коммита, потому что cli, получив успех,
/// стирает отметки из своей очереди.
///
/// Интернирование строк в эту транзакцию **не входит** — оно делается до
/// неё и коммитит свою. Словарь монотонен, а его копия в памяти про откат
/// не знает: общий откат вынул бы строки из базы, оставив номера в памяти.
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
            // Осторожно при правках схемы: `OR IGNORE` глушит не только
            // конфликт уникальности, но и `NOT NULL` с `CHECK`. Колонка или
            // ограничение, добавленные будущей миграцией, превратят потерю
            // отметки в тихий `Duplicate`, и наружу это не всплывёт никак.
            outcomes.push(if affected == 1 {
                Outcome::Inserted
            } else {
                Outcome::Duplicate
            });
        }
    }

    // Дни только от реально вставленных отметок. Повторная доставка очереди
    // cli — штатный сценарий, и если помечать по всему батчу, каждый такой
    // повтор будет заново пачкать уже пересчитанные дни и гонять пересчёт
    // сводок вхолостую. Ничего не вставили — ничего и не изменилось.
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
```

- [ ] **Step 5: Подключить и прогнать**

В `lib.rs`: `pub mod dirty;`, `pub mod heartbeats;`, `pub use dirty::dirty_days_for;`, `pub use heartbeats::{insert_heartbeats, IncomingHeartbeat, InsertReport, Outcome};`.

Run: `cargo test -p wakode-store`
Expected: PASS, девять новых тестов.

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-store
git commit -m "feat(store): вставка отметок с дедупликацией и пометкой дней"
```

---

### Task 9: Чтение диапазона отметок

**Files:**
- Modify: `crates/wakode-store/src/heartbeats.rs`
- Modify: `crates/wakode-store/tests/repository.rs`

**Interfaces:**
- Consumes: вставка из задачи 8, `codec`.
- Produces: `load_heartbeats(conn: &Connection, user: Uuid, from: Micros, to: Micros) -> StoreResult<Vec<wakode_core::Heartbeat>>`.

Возвращается ровно тот тип, который потребляет `wakode_core::build_intervals`. Границы — полуинтервал `[from, to)`, как их отдаёт `wakode_core::heartbeat_window`.

**Два долга задачи 8, которые закрываются здесь и нигде больше.** До этой задачи отметку нельзя было прочитать, поэтому две вещи остались недоказанными:

1. **Соответствие полей.** Тесты задачи 8 оставляли все необязательные поля пустыми, кроме проекта. При пустом поле курсор разбора не двигается — значит перестановка соседних полей в разборе (проект получает номер ветки) не ловилась ничем, и то же верно для десяти числовых и текстовых колонок, которые всюду были `None`. Тест `every_field_survives_the_round_trip` ниже обязателен: **все** поля заполнены **различимыми** значениями, и каждое проверяется после чтения. Без него порядок 26 параметров `INSERT` держится только на внимательности.
2. **Атомарность отката.** Тест на несуществующего пользователя в задаче 8 доказывал не то, что заявлял: внешний ключ срабатывает на первой же вставке, до `mark_dirty`, и пустой список дней получился бы и вовсе без транзакции. Тест `a_failed_batch_leaves_nothing_behind` ниже проверяет настоящий инвариант: после отказа число отметок не изменилось.

- [ ] **Step 1: Написать падающие тесты**

Добавь в `tests/repository.rs`:

```rust
use wakode_store::load_heartbeats;

#[test]
fn loaded_heartbeats_come_back_as_core_types() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let batch = [incoming(1_000, "src/main.rs", Some("wakode"))];
    insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();

    let loaded = load_heartbeats(
        &conn,
        user.id,
        Micros::from_secs(0),
        Micros::from_secs(2_000),
    )
    .unwrap();

    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].time, Micros::from_secs(1_000));
    assert_eq!(loaded[0].attrs.category, Category::Coding);
    assert_eq!(loaded[0].attrs.kind, EntityKind::File);
    assert!(loaded[0].attrs.project.is_some());
    assert_eq!(
        interner.resolve(loaded[0].attrs.entity).unwrap().as_ref(),
        "src/main.rs"
    );
}

#[test]
fn range_is_half_open_and_sorted() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    // Вставляем не по порядку — чтение обязано отдать по возрастанию времени.
    let batch = [
        incoming(300, "c.rs", None),
        incoming(100, "a.rs", None),
        incoming(200, "b.rs", None),
    ];
    insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();

    let loaded = load_heartbeats(
        &conn,
        user.id,
        Micros::from_secs(100),
        Micros::from_secs(300),
    )
    .unwrap();

    let times: Vec<i64> = loaded.iter().map(|hb| hb.time.get()).collect();
    assert_eq!(
        times,
        vec![Micros::from_secs(100).get(), Micros::from_secs(200).get()],
        "нижняя граница включена, верхняя — нет"
    );
}

#[test]
fn one_user_never_sees_another_users_heartbeats() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let one = insert_user(&conn, &a_user("one")).unwrap();
    let two = insert_user(&conn, &a_user("two")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    insert_heartbeats(&mut conn, &interner, one.id, &[incoming(100, "a.rs", None)], one.timezone).unwrap();
    insert_heartbeats(&mut conn, &interner, two.id, &[incoming(100, "b.rs", None)], two.timezone).unwrap();

    let loaded = load_heartbeats(&conn, one.id, Micros::from_secs(0), Micros::from_secs(1_000)).unwrap();

    assert_eq!(loaded.len(), 1);
    assert_eq!(interner.resolve(loaded[0].attrs.entity).unwrap().as_ref(), "a.rs");
}

#[test]
fn empty_range_gives_an_empty_vector_not_an_error() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let loaded = load_heartbeats(&conn, user.id, Micros::from_secs(0), Micros::from_secs(1)).unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn every_attribute_survives_the_round_trip() {
    // Долг задачи 8. Все необязательные поля заполнены **различимыми**
    // значениями: только так ловится перестановка соседних полей при
    // разборе. Пустое поле не двигает курсор, поэтому на `None` подмена
    // проекта веткой выглядит точно так же, как её отсутствие.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let full = IncomingHeartbeat {
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
        is_write: true,
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
    };

    insert_heartbeats(&mut conn, &interner, user.id, &[full], user.timezone).unwrap();
    let loaded = load_heartbeats(
        &conn,
        user.id,
        Micros::from_secs(1_755_000_000),
        Micros::from_secs(1_755_000_001),
    )
    .unwrap();

    let attrs = loaded[0].attrs;
    let text = |sid| interner.resolve(sid).unwrap().to_string();

    assert_eq!(loaded[0].time, Micros::from_secs(1_755_000_000));
    assert_eq!(attrs.kind, EntityKind::App);
    assert_eq!(attrs.category, Category::Debugging);
    assert_eq!(text(attrs.entity), "сущность");
    assert_eq!(text(attrs.project.unwrap()), "проект");
    assert_eq!(text(attrs.branch.unwrap()), "ветка");
    assert_eq!(text(attrs.language.unwrap()), "язык");
    assert_eq!(text(attrs.editor.unwrap()), "редактор");
    assert_eq!(text(attrs.os.unwrap()), "ос");
    assert_eq!(text(attrs.machine.unwrap()), "машина");
}

#[test]
fn a_refused_batch_stores_no_heartbeats_at_all() {
    // Долг задачи 8, закрытый ровно настолько, насколько он закрываем.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let ghost = uuid::Uuid::now_v7();
    let doomed = [incoming(200, "b.rs", None), incoming(300, "c.rs", None)];
    assert!(insert_heartbeats(&mut conn, &interner, ghost, &doomed, chrono_tz::UTC).is_err());

    let loaded =
        load_heartbeats(&conn, ghost, Micros::from_secs(0), Micros::from_secs(1_000)).unwrap();
    assert!(loaded.is_empty());
}
```

Первому тесту нужны `IncomingHeartbeat`, `EntityKind` и `Category` в импортах. Поле `category` в нём — `Category::Debugging`: категория, отличная и от `Coding` из хелпера `incoming`, и от `Unknown`, чтобы утверждение не прошло по совпадению с любым из дефолтов.

**Про второй тест — честно о его пределах.** Он проверяет, что после отказа отметок не осталось, но атомарности связки «вставка + `mark_dirty`» не доказывает и доказать не может. Все отметки батча идут одному пользователю, поэтому внешний ключ либо отвергает их все на первой же вставке, либо не отвергает ни одной; сценария «половина вставилась, потом упало» текущая схема не допускает вовсе. Настоящая проверка потребовала бы подмены соединения ради сбоя ровно между вставкой и `mark_dirty` — инфраструктуры, несоразмерной риску. Инвариант держится формой кода: обе операции идут по одному `tx`, и `commit` один.

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p wakode-store --test repository`
Expected: FAIL — `load_heartbeats` не существует.

- [ ] **Step 3: Реализовать чтение**

Добавь в `crates/wakode-store/src/heartbeats.rs`:

```rust
use crate::codec::{i64_to_category, i64_to_kind, i64_to_sid};
use wakode_core::{Attrs, Heartbeat};

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
```

- [ ] **Step 4: Прогнать**

В `lib.rs` добавь `load_heartbeats` в реэкспорт.

Run: `cargo test -p wakode-store`
Expected: PASS, шесть новых тестов.

- [ ] **Step 5: Коммит**

```bash
git add crates/wakode-store
git commit -m "feat(store): чтение диапазона отметок"
```

---

### Task 10: Ключи и сессии

**Files:**
- Create: `crates/wakode-store/src/keys.rs`
- Create: `crates/wakode-store/src/sessions.rs`
- Modify: `crates/wakode-store/src/lib.rs`
- Modify: `crates/wakode-store/tests/repository.rs`

**Interfaces:**
- Consumes: пользователи из задачи 7.
- Produces: `insert_api_key`, `find_key_by_lookup`, `revoke_key`, `touch_key_used`, `insert_session`, `find_session_by_token_hash`, `revoke_session` и структуры `NewApiKey`/`ApiKey`/`NewSession`/`Session`.

**Криптографии тут нет.** `key_encrypted` и `key_lookup` — непрозрачные `Vec<u8>`, которые считает план 3. Хранилище отвечает ровно за одно: найти ключ по `key_lookup` **за один запрос** (по зашифрованному значению искать нельзя, поэтому детерминированный отпечаток лежит отдельной колонкой с уникальным индексом).

- [ ] **Step 1: Написать падающие тесты**

Добавь в `tests/repository.rs`:

```rust
use wakode_store::{
    find_key_by_lookup, find_session_by_token_hash, insert_api_key, insert_session,
    revoke_key, NewApiKey, NewSession,
};

#[test]
fn api_key_is_found_by_its_lookup_fingerprint() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let created = insert_api_key(
        &conn,
        &NewApiKey {
            user_id: user.id,
            name: "рабочий ноутбук".to_owned(),
            key_encrypted: vec![1, 2, 3],
            key_lookup: vec![9, 9, 9],
        },
    )
    .unwrap();

    let found = find_key_by_lookup(&conn, &[9, 9, 9]).unwrap().unwrap();
    assert_eq!(found.id, created.id);
    assert_eq!(found.user_id, user.id);
    assert_eq!(found.key_encrypted, vec![1, 2, 3]);
    assert!(found.revoked_at.is_none());
}

#[test]
fn revoked_key_is_still_found_but_marked() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let created = insert_api_key(
        &conn,
        &NewApiKey {
            user_id: user.id,
            name: "старый".to_owned(),
            key_encrypted: vec![1],
            key_lookup: vec![2],
        },
    )
    .unwrap();

    revoke_key(&conn, created.id).unwrap();

    // Отозванный ключ обязан находиться: иначе слой аутентификации не сможет
    // отличить «ключа никогда не было» от «ключ отозван», а это разные ответы.
    let found = find_key_by_lookup(&conn, &[2]).unwrap().unwrap();
    assert!(found.revoked_at.is_some());
}

#[test]
fn unknown_lookup_is_none() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    assert!(find_key_by_lookup(&conn, &[0, 0, 0]).unwrap().is_none());
}

#[test]
fn duplicate_lookup_is_refused() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let key = |name: &str| NewApiKey {
        user_id: user.id,
        name: name.to_owned(),
        key_encrypted: vec![1],
        key_lookup: vec![7],
    };

    insert_api_key(&conn, &key("первый")).unwrap();
    assert!(insert_api_key(&conn, &key("второй")).is_err());
}

#[test]
fn session_round_trips_by_token_hash() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let created = insert_session(
        &conn,
        &NewSession {
            user_id: user.id,
            token_hash: vec![4, 2],
            user_agent: Some("Firefox".to_owned()),
            expires_at: Micros::from_secs(2_000_000_000),
        },
    )
    .unwrap();

    let found = find_session_by_token_hash(&conn, &[4, 2]).unwrap().unwrap();
    assert_eq!(found.id, created.id);
    assert_eq!(found.user_id, user.id);
    assert_eq!(found.expires_at, Micros::from_secs(2_000_000_000));
}

#[test]
fn deleting_a_user_takes_their_keys_and_sessions_with_them() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    insert_api_key(&conn, &NewApiKey {
        user_id: user.id,
        name: "ключ".to_owned(),
        key_encrypted: vec![1],
        key_lookup: vec![1],
    }).unwrap();
    insert_session(&conn, &NewSession {
        user_id: user.id,
        token_hash: vec![1],
        user_agent: None,
        expires_at: Micros::from_secs(1),
    }).unwrap();

    conn.execute(
        "DELETE FROM users WHERE id = ?1",
        [wakode_store::codec::uuid_to_blob(user.id)],
    )
    .unwrap();

    assert!(find_key_by_lookup(&conn, &[1]).unwrap().is_none());
    assert!(find_session_by_token_hash(&conn, &[1]).unwrap().is_none());
}
```

Последний тест проверяет, что `ON DELETE CASCADE` действительно работает — а он работает только при включённой прагме `foreign_keys`, которую мы ставим в `conn.rs`. Тест сторожит связку этих двух вещей.

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p wakode-store --test repository`
Expected: FAIL — типов и функций нет.

- [ ] **Step 3: Реализовать ключи**

`crates/wakode-store/src/keys.rs`:

```rust
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;
use wakode_core::Micros;

use crate::codec::{blob_to_uuid, uuid_to_blob};
use crate::error::StoreResult;
use crate::clock;

/// Новый API-ключ.
///
/// `key_encrypted` — значение ключа под мастер-ключом, чтобы показать его в
/// настройках. `key_lookup` — детерминированный отпечаток того же значения:
/// по зашифрованному искать нельзя, а аутентификация обязана найти ключ за
/// один запрос. Оба считает план 3; сюда приезжают готовые байты.
#[derive(Debug, Clone)]
pub struct NewApiKey {
    pub user_id: Uuid,
    pub name: String,
    pub key_encrypted: Vec<u8>,
    pub key_lookup: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ApiKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub key_encrypted: Vec<u8>,
    pub created_at: Micros,
    pub last_used_at: Option<Micros>,
    pub revoked_at: Option<Micros>,
}

pub fn insert_api_key(conn: &Connection, new: &NewApiKey) -> StoreResult<ApiKey> {
    let id = Uuid::now_v7();
    let now = clock::now();

    conn.execute(
        "INSERT INTO api_keys
           (id, user_id, name, key_encrypted, key_lookup, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            uuid_to_blob(id),
            uuid_to_blob(new.user_id),
            new.name,
            new.key_encrypted,
            new.key_lookup,
            now.get(),
        ],
    )?;

    Ok(ApiKey {
        id,
        user_id: new.user_id,
        name: new.name.clone(),
        key_encrypted: new.key_encrypted.clone(),
        created_at: now,
        last_used_at: None,
        revoked_at: None,
    })
}

/// Найти ключ по отпечатку.
///
/// Отозванные ключи тоже находятся: слой аутентификации должен различать
/// «такого ключа не было» и «ключ отозван» — это разные ответы пользователю.
pub fn find_key_by_lookup(conn: &Connection, lookup: &[u8]) -> StoreResult<Option<ApiKey>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, user_id, name, key_encrypted, created_at, last_used_at, revoked_at
         FROM api_keys WHERE key_lookup = ?1",
    )?;

    let row = stmt
        .query_row([lookup], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .optional()?;

    let Some((id, user_id, name, key_encrypted, created, used, revoked)) = row else {
        return Ok(None);
    };

    Ok(Some(ApiKey {
        id: blob_to_uuid(&id)?,
        user_id: blob_to_uuid(&user_id)?,
        name,
        key_encrypted,
        created_at: Micros::new(created),
        last_used_at: used.map(Micros::new),
        revoked_at: revoked.map(Micros::new),
    }))
}

pub fn revoke_key(conn: &Connection, id: Uuid) -> StoreResult<()> {
    conn.execute(
        "UPDATE api_keys SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        rusqlite::params![uuid_to_blob(id), clock::now().get()],
    )?;
    Ok(())
}

pub fn touch_key_used(conn: &Connection, id: Uuid) -> StoreResult<()> {
    conn.execute(
        "UPDATE api_keys SET last_used_at = ?2 WHERE id = ?1",
        rusqlite::params![uuid_to_blob(id), clock::now().get()],
    )?;
    Ok(())
}
```

- [ ] **Step 4: Реализовать сессии**

`crates/wakode-store/src/sessions.rs` — по той же форме, с полями `id`, `user_id`, `token_hash`, `user_agent`, `created_at`, `expires_at`, `revoked_at`:

```rust
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;
use wakode_core::Micros;

use crate::codec::{blob_to_uuid, uuid_to_blob};
use crate::error::StoreResult;
use crate::clock;

#[derive(Debug, Clone)]
pub struct NewSession {
    pub user_id: Uuid,
    pub token_hash: Vec<u8>,
    pub user_agent: Option<String>,
    pub expires_at: Micros,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: Uuid,
    pub user_id: Uuid,
    pub user_agent: Option<String>,
    pub created_at: Micros,
    pub expires_at: Micros,
    pub revoked_at: Option<Micros>,
}

pub fn insert_session(conn: &Connection, new: &NewSession) -> StoreResult<Session> {
    let id = Uuid::now_v7();
    let now = clock::now();

    conn.execute(
        "INSERT INTO sessions
           (id, user_id, token_hash, user_agent, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            uuid_to_blob(id),
            uuid_to_blob(new.user_id),
            new.token_hash,
            new.user_agent,
            now.get(),
            new.expires_at.get(),
        ],
    )?;

    Ok(Session {
        id,
        user_id: new.user_id,
        user_agent: new.user_agent.clone(),
        created_at: now,
        expires_at: new.expires_at,
        revoked_at: None,
    })
}

pub fn find_session_by_token_hash(
    conn: &Connection,
    token_hash: &[u8],
) -> StoreResult<Option<Session>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, user_id, user_agent, created_at, expires_at, revoked_at
         FROM sessions WHERE token_hash = ?1",
    )?;

    let row = stmt
        .query_row([token_hash], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })
        .optional()?;

    let Some((id, user_id, user_agent, created, expires, revoked)) = row else {
        return Ok(None);
    };

    Ok(Some(Session {
        id: blob_to_uuid(&id)?,
        user_id: blob_to_uuid(&user_id)?,
        user_agent,
        created_at: Micros::new(created),
        expires_at: Micros::new(expires),
        revoked_at: revoked.map(Micros::new),
    }))
}

pub fn revoke_session(conn: &Connection, id: Uuid) -> StoreResult<()> {
    conn.execute(
        "UPDATE sessions SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        rusqlite::params![uuid_to_blob(id), clock::now().get()],
    )?;
    Ok(())
}
```

- [ ] **Step 5: Подключить и прогнать**

В `lib.rs`: `pub mod keys;`, `pub mod sessions;`, `pub use keys::{find_key_by_lookup, insert_api_key, revoke_key, touch_key_used, ApiKey, NewApiKey};`, `pub use sessions::{find_session_by_token_hash, insert_session, revoke_session, NewSession, Session};`.

Run: `cargo test -p wakode-store`
Expected: PASS, шесть новых тестов.

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-store
git commit -m "feat(store): API-ключи и сессии"
```

---

### Task 11: Единственная пишущая задача

**Files:**
- Create: `crates/wakode-store/src/writer.rs`
- Modify: `crates/wakode-store/src/lib.rs`
- Modify: `crates/wakode-store/tests/repository.rs`

**Interfaces:**
- Consumes: `insert_heartbeats` из задачи 8, `Interner`.
- Produces: `WriteHandle` с методом `insert_heartbeats(&self, user, batch, tz) -> StoreResult<InsertReport>` (асинхронный), `spawn_writer(conn: Connection, interner: Arc<Interner>, capacity: usize) -> WriteHandle`.

**Зачем.** SQLite допускает одного писателя. Если каждый HTTP-обработчик пишет сам, они дерутся за блокировку и получают `SQLITE_BUSY`. Единственная пишущая задача убирает борьбу целиком и попутно даёт групповую фиксацию.

**Переполнение канала — это замысел, а не деградация** (спека §8). При заполненной очереди возвращается `StoreError::WriteQueueFull`, из которого HTTP-слой сделает `503` с `Retry-After`. wakatime-cli сложит отметки в свою очередь и дошлёт позже. Буферизация в памяти вместо отказа привела бы к потере при падении процесса.

- [ ] **Step 1: Написать падающие тесты**

Добавь в `tests/repository.rs`:

```rust
use std::sync::Arc;
use wakode_store::{spawn_writer, StoreError};

#[tokio::test]
async fn writer_commits_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wakode.db");
    let mut conn = wakode_store::open(&path).unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Arc::new(Interner::load(&conn).unwrap());

    let handle = spawn_writer(conn, Arc::clone(&interner), 8);

    let report = handle
        .insert_heartbeats(user.id, vec![incoming(1_000, "src/main.rs", None)], user.timezone)
        .await
        .unwrap();
    assert_eq!(report.inserted(), 1);

    // Читаем отдельным соединением: успех обязан означать, что транзакция
    // уже закоммичена, иначе cli сотрёт отметки из своей очереди зря.
    let read = wakode_store::open(&path).unwrap();
    let loaded = load_heartbeats(&read, user.id, Micros::from_secs(0), Micros::from_secs(9_999)).unwrap();
    assert_eq!(loaded.len(), 1);
}

#[tokio::test]
async fn a_full_queue_refuses_instead_of_buffering() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wakode.db");
    let mut conn = wakode_store::open(&path).unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Arc::new(Interner::load(&conn).unwrap());

    // Канал на одну заявку: заполнить его тривиально.
    let handle = spawn_writer(conn, interner, 1);

    let mut refused = 0;
    let mut tasks = Vec::new();
    for i in 0..64 {
        let handle = handle.clone();
        let tz = user.timezone;
        let id = user.id;
        tasks.push(tokio::spawn(async move {
            handle
                .insert_heartbeats(id, vec![incoming(1_000 + i, "f.rs", None)], tz)
                .await
        }));
    }
    for task in tasks {
        if let Err(StoreError::WriteQueueFull) = task.await.unwrap() {
            refused += 1;
        }
    }

    assert!(
        refused > 0,
        "при канале на одну заявку и 64 одновременных записях отказы обязаны появиться: \
         молчаливая буферизация здесь означала бы потерю при падении процесса"
    );
}

#[tokio::test]
async fn writer_survives_a_failing_batch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wakode.db");
    let mut conn = wakode_store::open(&path).unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Arc::new(Interner::load(&conn).unwrap());

    let handle = spawn_writer(conn, interner, 8);

    // Несуществующий пользователь — внешний ключ не пустит.
    let failed = handle
        .insert_heartbeats(uuid::Uuid::now_v7(), vec![incoming(1, "f.rs", None)], user.timezone)
        .await;
    assert!(failed.is_err());

    // Задача обязана остаться живой: одна битая заявка не должна уносить
    // с собой запись для всех остальных.
    let ok = handle
        .insert_heartbeats(user.id, vec![incoming(2, "f.rs", None)], user.timezone)
        .await
        .unwrap();
    assert_eq!(ok.inserted, 1);
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p wakode-store --test repository`
Expected: FAIL — `spawn_writer` не существует.

- [ ] **Step 3: Реализовать пишущую задачу**

`crates/wakode-store/src/writer.rs`:

```rust
use std::sync::Arc;

use chrono_tz::Tz;
use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::error::{StoreError, StoreResult};
use crate::heartbeats::{insert_heartbeats, IncomingHeartbeat, InsertReport};
use crate::interner::Interner;

/// Заявка на запись и канал для ответа.
struct WriteJob {
    user: Uuid,
    batch: Vec<IncomingHeartbeat>,
    tz: Tz,
    reply: oneshot::Sender<StoreResult<InsertReport>>,
}

/// Ручка к пишущей задаче. Клонируется свободно, все копии шлют в один канал.
///
/// `Debug` выводится, хотя `WriteJob` его не реализует: `mpsc::Sender<T>`
/// реализует `Debug` без требования `T: Debug`.
#[derive(Debug, Clone)]
pub struct WriteHandle {
    tx: mpsc::Sender<WriteJob>,
}

/// Поднять пишущую задачу. Соединение переезжает к ней насовсем: больше
/// никто в базу не пишет.
pub fn spawn_writer(
    mut conn: Connection,
    interner: Arc<Interner>,
    capacity: usize,
) -> WriteHandle {
    let (tx, mut rx) = mpsc::channel::<WriteJob>(capacity);

    // Отдельный поток, а не задача tokio: работа тут блокирующая, и держать
    // её на исполнителе асинхронных задач нельзя.
    std::thread::spawn(move || {
        while let Some(job) = rx.blocking_recv() {
            let result = insert_heartbeats(&mut conn, &interner, job.user, &job.batch, job.tz);
            // Ответ уходит только после того, как транзакция закоммичена
            // внутри insert_heartbeats. Отправитель мог уйти — это не наша
            // беда, запись уже состоялась.
            let _ = job.reply.send(result);
        }
    });

    WriteHandle { tx }
}

impl WriteHandle {
    /// Записать батч. Ждёт подтверждения коммита.
    pub async fn insert_heartbeats(
        &self,
        user: Uuid,
        batch: Vec<IncomingHeartbeat>,
        tz: Tz,
    ) -> StoreResult<InsertReport> {
        let (reply, wait) = oneshot::channel();
        let job = WriteJob { user, batch, tz, reply };

        // try_send, а не send: ждать места в очереди значило бы копить
        // запросы в памяти. Отказ здесь превращается в 503 с Retry-After,
        // и cli дошлёт отметки из собственной очереди.
        self.tx.try_send(job).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => StoreError::WriteQueueFull,
            mpsc::error::TrySendError::Closed(_) => StoreError::WriterGone,
        })?;

        wait.await.map_err(|_| StoreError::WriterGone)?
    }
}
```

- [ ] **Step 4: Подключить и прогнать**

В `lib.rs`: `pub mod writer;` и `pub use writer::{spawn_writer, WriteHandle};`.

Run: `cargo test -p wakode-store`
Expected: PASS, три новых теста.

- [ ] **Step 5: Коммит**

```bash
git add crates/wakode-store
git commit -m "feat(store): единственная пишущая задача с отказом при переполнении"
```

---

### Task 12: Репозиторный трейт и бэкап

**Files:**
- Create: `crates/wakode-store/src/repo.rs`
- Modify: `crates/wakode-store/src/lib.rs`
- Modify: `crates/wakode-store/tests/repository.rs`

**Interfaces:**
- Consumes: всё предыдущее.
- Produces: трейты `HeartbeatRepo`, `UserRepo`, `KeyRepo`, `SessionRepo`; тип `SqliteStore`, реализующий их все; `SqliteStore::open(path, capacity) -> StoreResult<Self>`; `SqliteStore::backup(&self, dest: &Path) -> StoreResult<()>`.

**Почему трейты асинхронные.** Обещание из спеки — «Postgres добавляется позже без переписывания логики». Драйвер Postgres асинхронный нативно; синхронный трейт пришлось бы ломать при переезде, и обещание оказалось бы пустым. Внутри реализация синхронная: записи уходят в пишущую задачу, чтения — через `spawn_blocking`.

- [ ] **Step 1: Написать падающие тесты**

Добавь в `tests/repository.rs`:

```rust
use std::path::Path;
use wakode_store::{HeartbeatRepo, SqliteStore, UserRepo};

#[tokio::test]
async fn store_goes_through_the_trait_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap();

    let user = store.create_user(a_user("swrneko")).await.unwrap();

    let report = store
        .record_heartbeats(user.id, vec![incoming(1_000, "src/main.rs", Some("wakode"))], user.timezone)
        .await
        .unwrap();
    assert_eq!(report.inserted(), 1);

    let loaded = store
        .heartbeats_in_range(user.id, Micros::from_secs(0), Micros::from_secs(9_999))
        .await
        .unwrap();
    assert_eq!(loaded.len(), 1);

    let found = store.user_by_login("swrneko").await.unwrap().unwrap();
    assert_eq!(found.id, user.id);
}

#[tokio::test]
async fn backup_produces_a_readable_copy() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap();
    let user = store.create_user(a_user("swrneko")).await.unwrap();
    store
        .record_heartbeats(user.id, vec![incoming(1_000, "f.rs", None)], user.timezone)
        .await
        .unwrap();

    let dest = dir.path().join("backup.db");
    store.backup(&dest).await.unwrap();

    // Копия обязана открываться и содержать те же данные — снимок делается
    // на живой базе, поэтому проверяем именно консистентность, а не факт
    // существования файла.
    let copy = wakode_store::open(&dest).unwrap();
    let loaded = load_heartbeats(&copy, user.id, Micros::from_secs(0), Micros::from_secs(9_999)).unwrap();
    assert_eq!(loaded.len(), 1);
}

#[tokio::test]
async fn opening_the_store_applies_migrations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wakode.db");

    let _store = SqliteStore::open(&path, 16).unwrap();

    let conn = wakode_store::open(&path).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 1);
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p wakode-store --test repository`
Expected: FAIL — трейтов и `SqliteStore` нет.

- [ ] **Step 3: Определить трейты**

`crates/wakode-store/src/repo.rs`:

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono_tz::Tz;
use uuid::Uuid;
use wakode_core::{Heartbeat, Micros};

use crate::error::StoreResult;
use crate::heartbeats::{IncomingHeartbeat, InsertReport};
use crate::interner::Interner;
use crate::keys::{ApiKey, NewApiKey};
use crate::sessions::{NewSession, Session};
use crate::users::{NewUser, User};
use crate::writer::{spawn_writer, WriteHandle};

/// Отметки: запись и чтение диапазона.
///
/// Трейт асинхронный, хотя SQLite синхронен: это цена обещания, что
/// Postgres добавляется реализацией трейта, а не переписыванием вызывающих.
pub trait HeartbeatRepo: Send + Sync {
    fn record_heartbeats(
        &self,
        user: Uuid,
        batch: Vec<IncomingHeartbeat>,
        tz: Tz,
    ) -> impl std::future::Future<Output = StoreResult<InsertReport>> + Send;

    fn heartbeats_in_range(
        &self,
        user: Uuid,
        from: Micros,
        to: Micros,
    ) -> impl std::future::Future<Output = StoreResult<Vec<Heartbeat>>> + Send;

    /// Развернуть номер строки обратно в текст. Словарь в памяти, поэтому
    /// метод синхронный — асинхронность тут была бы ложью.
    fn resolve(&self, sid: wakode_core::Sid) -> Option<Arc<str>>;
}

pub trait UserRepo: Send + Sync {
    fn create_user(&self, new: NewUser) -> impl std::future::Future<Output = StoreResult<User>> + Send;
    fn user_by_login(&self, login: &str) -> impl std::future::Future<Output = StoreResult<Option<User>>> + Send;
    fn user_by_id(&self, id: Uuid) -> impl std::future::Future<Output = StoreResult<Option<User>>> + Send;
}

pub trait KeyRepo: Send + Sync {
    fn create_key(&self, new: NewApiKey) -> impl std::future::Future<Output = StoreResult<ApiKey>> + Send;
    fn key_by_lookup(&self, lookup: Vec<u8>) -> impl std::future::Future<Output = StoreResult<Option<ApiKey>>> + Send;
    fn revoke_key(&self, id: Uuid) -> impl std::future::Future<Output = StoreResult<()>> + Send;
}

pub trait SessionRepo: Send + Sync {
    fn create_session(&self, new: NewSession) -> impl std::future::Future<Output = StoreResult<Session>> + Send;
    fn session_by_token_hash(&self, hash: Vec<u8>) -> impl std::future::Future<Output = StoreResult<Option<Session>>> + Send;
    fn revoke_session(&self, id: Uuid) -> impl std::future::Future<Output = StoreResult<()>> + Send;
}
```

`impl Future` в позиции возврата у метода трейта (RPITIT) стабилен с Rust 1.75 — `async-trait` не нужен, и подключать его не надо: он даёт лишнюю зависимость и `Box`-аллокацию на каждый вызов. Реализации при этом пишутся обычным `async fn`: он подходит под `-> impl Future + Send`, пока захваченные значения `Send`.

`Vec<u8>` в `key_by_lookup` и `session_by_token_hash` вместо `&[u8]` — не небрежность: значение переезжает в `spawn_blocking` и должно быть владеемым, а заимствование в асинхронной сигнатуре потребовало бы времени жизни, которое переживёт вызов.

- [ ] **Step 4: Реализовать `SqliteStore`**

Дальше в `repo.rs`:

```rust
/// Хранилище на SQLite.
///
/// Пишущая задача владеет своим соединением. Читатели открывают собственные:
/// в WAL-режиме они не мешают ни писателю, ни друг другу, поэтому гонять
/// чтения через ту же очередь было бы искусственным узким местом.
#[derive(Debug, Clone)]
pub struct SqliteStore {
    path: PathBuf,
    writer: WriteHandle,
    interner: Arc<Interner>,
}

impl SqliteStore {
    pub fn open(path: &Path, write_queue: usize) -> StoreResult<Self> {
        let mut conn = crate::open(path)?;
        crate::migrate(&mut conn)?;

        let interner = Arc::new(Interner::load(&conn)?);
        let writer = spawn_writer(conn, Arc::clone(&interner), write_queue);

        Ok(Self {
            path: path.to_path_buf(),
            writer,
            interner,
        })
    }

    /// Соединение для чтения. Открывается на операцию: SQLite открывает файл
    /// дёшево, а пул понадобится только если это измерят как узкое место.
    fn read_conn(&self) -> StoreResult<rusqlite::Connection> {
        crate::open(&self.path)
    }

    /// Консистентный снимок живой базы.
    pub async fn backup(&self, dest: &Path) -> StoreResult<()> {
        let path = self.path.clone();
        let dest = dest.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let conn = crate::open(&path)?;
            // VACUUM INTO делает снимок без остановки записи и попутно
            // дефрагментирует файл — в отличие от копирования файла руками,
            // которое на живой базе даёт битую копию.
            conn.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])?;
            Ok(())
        })
        .await
        .map_err(|_| crate::StoreError::TaskPanicked)?
    }
}

/// Выполнить блокирующую работу над свежим соединением для чтения.
///
/// `JoinError` тут значит ровно одно: замыкание паникнуло (отменять эти
/// задачи некому). Поэтому `TaskPanicked`, а не `WriterGone` — пишущая
/// задача к чтениям отношения не имеет, и путать эти два состояния значит
/// врать в логах.
async fn read_blocking<T, F>(store: &SqliteStore, work: F) -> StoreResult<T>
where
    T: Send + 'static,
    F: FnOnce(rusqlite::Connection) -> StoreResult<T> + Send + 'static,
{
    let conn = store.read_conn()?;
    tokio::task::spawn_blocking(move || work(conn))
        .await
        .map_err(|_| crate::StoreError::TaskPanicked)?
}

impl HeartbeatRepo for SqliteStore {
    async fn record_heartbeats(
        &self,
        user: Uuid,
        batch: Vec<IncomingHeartbeat>,
        tz: Tz,
    ) -> StoreResult<InsertReport> {
        self.writer.insert_heartbeats(user, batch, tz).await
    }

    async fn heartbeats_in_range(
        &self,
        user: Uuid,
        from: Micros,
        to: Micros,
    ) -> StoreResult<Vec<Heartbeat>> {
        read_blocking(self, move |conn| {
            crate::load_heartbeats(&conn, user, from, to)
        })
        .await
    }

    fn resolve(&self, sid: wakode_core::Sid) -> Option<Arc<str>> {
        self.interner.resolve(sid)
    }
}
```

- [ ] **Step 5: Реализовать оставшиеся три трейта**

Пользователи, ключи и сессии идут **мимо** пишущей задачи: записи там редкие и одиночные, а очередь существует ради потока отметок. Логин или выдача ключа, застрявшие за батчем чужих отметок, — это цена без выгоды.

Дальше в `repo.rs`:

```rust
impl UserRepo for SqliteStore {
    async fn create_user(&self, new: NewUser) -> StoreResult<User> {
        read_blocking(self, move |conn| crate::insert_user(&conn, &new)).await
    }

    async fn user_by_login(&self, login: &str) -> StoreResult<Option<User>> {
        // Строка копируется: замыкание переезжает в другой поток и пережить
        // заимствование не может.
        let login = login.to_owned();
        read_blocking(self, move |conn| crate::find_user_by_login(&conn, &login)).await
    }

    async fn user_by_id(&self, id: Uuid) -> StoreResult<Option<User>> {
        read_blocking(self, move |conn| crate::find_user_by_id(&conn, id)).await
    }
}

impl KeyRepo for SqliteStore {
    async fn create_key(&self, new: NewApiKey) -> StoreResult<ApiKey> {
        read_blocking(self, move |conn| crate::insert_api_key(&conn, &new)).await
    }

    async fn key_by_lookup(&self, lookup: Vec<u8>) -> StoreResult<Option<ApiKey>> {
        read_blocking(self, move |conn| crate::find_key_by_lookup(&conn, &lookup)).await
    }

    async fn revoke_key(&self, id: Uuid) -> StoreResult<()> {
        read_blocking(self, move |conn| crate::revoke_key(&conn, id)).await
    }
}

impl SessionRepo for SqliteStore {
    async fn create_session(&self, new: NewSession) -> StoreResult<Session> {
        read_blocking(self, move |conn| crate::insert_session(&conn, &new)).await
    }

    async fn session_by_token_hash(&self, hash: Vec<u8>) -> StoreResult<Option<Session>> {
        read_blocking(self, move |conn| {
            crate::find_session_by_token_hash(&conn, &hash)
        })
        .await
    }

    async fn revoke_session(&self, id: Uuid) -> StoreResult<()> {
        read_blocking(self, move |conn| crate::revoke_session(&conn, id)).await
    }
}
```

`touch_key_used` в трейт не выведен намеренно: отметка последнего использования ключа — забота слоя аутентификации, и он вызовет функцию напрямую. Трейт описывает то, что придётся повторить в реализации на Postgres; лишний метод там — лишняя работа без причины.

Для `spawn_blocking` нужна фича `rt` у tokio; многопоточный исполнитель для `#[tokio::test]` уже добавлен в dev-зависимостях задачи 1.

Run: `cargo test -p wakode-store --test repository`
Expected: PASS, три теста задачи 12.

- [ ] **Step 6: Подключить и прогнать весь workspace**

В `lib.rs`: `pub mod repo;` и `pub use repo::{HeartbeatRepo, KeyRepo, SessionRepo, SqliteStore, UserRepo};`.

Run: `cargo test --workspace`
Expected: PASS. Предупреждений нет ни на этапе компиляции, ни в выводе тестов.

- [ ] **Step 7: Коммит**

```bash
git add crates/wakode-store
git commit -m "feat(store): репозиторные трейты и бэкап через VACUUM INTO"
```

---

## Что этот план сознательно не делает

- **Криптографии нет.** Argon2, шифрование ключа мастер-ключом и HMAC для `key_lookup` — план 3. Хранилище пишет непрозрачные байты.
- **Валидации домена нет.** Отсечка отметок из будущего (спека §8) — HTTP-слой.
- **Разбора user-agent нет.** Редактор, ОС и плагин приезжают сюда уже разобранными строками.
- **Кэша агрегатов нет.** `dirty_days` заполняется, но никто её пока не читает: это задел, и спека честно называет его заделом.
- **Пула соединений для чтения нет.** Соединение открывается на операцию. Пул (`deadpool-sqlite` или свой) вводится, когда это измерят как узкое место, а не заранее.
- **Таблиц волны 1 нет** — см. раздел отклонений.

## Открытые вопросы, унаследованные от плана 1

Ревью `wakode-core` оставило пять задач следующим планам. Из них к плану 2 относятся:

1. **Категория из базы читается явным `match`, никогда через serde.** Закрыто задачей 4 — записано и в код, и в тест.
2. Остальные четыре (`"unknown"` → `null` на проводе, `Micros` как дробные секунды, `dependencies` списком, пустые дни в `summaries`) — обязанности плана 3.
