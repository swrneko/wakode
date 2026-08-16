# wakode-core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Построить чистый вычислительный крейт `wakode-core`: превращение потока heartbeat'ов в интервалы времени, агрегация по измерениям и нарезка по локальным дням пользователя.

**Architecture:** Крейт не имеет доступа к БД, сети и файловой системе — только чистые функции над векторами структур. Строковые атрибуты представлены числовыми идентификаторами (`Sid`), потому что разрешением занимается слой хранения. Такая изоляция существует ради property-тестов: они должны прогонять сотни тысяч сгенерированных сценариев в памяти за секунды.

**Tech Stack:** Rust (edition 2024), `chrono` + `chrono-tz` для работы с локальными днями, `proptest` для property-тестов.

## Global Constraints

- Спека: `docs/superpowers/specs/2026-08-15-wakode-design.md`.
- Крейт `wakode-core` **не имеет права** зависеть от `sqlx`, `axum`, `tokio`, `reqwest` и любых I/O-библиотек. Единственные зависимости — `chrono`, `chrono-tz`, `serde` (только derive для доменных типов), `proptest` в dev.
- Время внутри крейта — всегда микросекунды с epoch UTC, тип `Micros(i64)`. `f64` не является представлением времени ни в одном типе и ни в одной сигнатуре расчётов; единственное исключение — конверсионные методы `Micros::from_secs_f64` / `Micros::as_secs_f64`, которые живут на самом newtype, чтобы округление было определено в одном месте, а не дублировалось на границе HTTP-слоя.
- Таймаут по умолчанию — 900 секунд (совпадает с WakaTime).
- Хвостовая добавка (`tail_padding`) — явный параметр конфигурации, а не константа. Её значение неизвестно и калибруется позже по живому аккаунту WakaTime; по умолчанию 0.
- Инвариант конфигурации: `tail_padding <= timeout`. Без него интервалы разных сессий могут пересечься.
- Все публичные типы выводят `Debug`; все типы-значения — `Clone, Copy, PartialEq, Eq`.
- Каждая задача заканчивается коммитом. Сообщения коммитов на русском, без упоминаний ИИ-ассистентов.

## Файловая структура

```
Cargo.toml                       # workspace
crates/wakode-core/
  Cargo.toml
  src/
    lib.rs                       # реэкспорт публичного API
    time.rs                      # Micros и арифметика времени
    domain.rs                    # Sid, EntityKind, Category, Attrs, Heartbeat
    config.rs                    # DurationConfig с валидацией
    intervals.rs                 # build_intervals — движок длительностей
    aggregate.rs                 # aggregate_by, grand_total, percent
    calendar.rs                  # локальные дни, границы, нарезка
  tests/
    properties.rs                # property-тесты движка
```

Разбиение по ответственности, а не по слоям: `intervals.rs` знает только про склейку отметок, `calendar.rs` — только про таймзоны, `aggregate.rs` — только про суммирование. Ни один из трёх не знает про два других.

---

### Task 1: Workspace и тип времени

**Files:**
- Create: `Cargo.toml`
- Create: `crates/wakode-core/Cargo.toml`
- Create: `crates/wakode-core/src/lib.rs`
- Create: `crates/wakode-core/src/time.rs`

**Interfaces:**
- Consumes: ничего.
- Produces: `Micros` с методами `new(i64) -> Micros`, `from_secs(i64) -> Micros`, `from_secs_f64(f64) -> Micros`, `get(self) -> i64`, `as_secs_f64(self) -> f64`, `saturating_add(self, Micros) -> Micros`, `saturating_sub(self, Micros) -> Micros`. Константа `Micros::ZERO`.

- [ ] **Step 1: Создать workspace**

`Cargo.toml` в корне:

```toml
[workspace]
resolver = "3"
members = ["crates/wakode-core"]

[workspace.package]
edition = "2024"
license = "AGPL-3.0-or-later"

[workspace.dependencies]
chrono = { version = "0.4", default-features = false, features = ["std", "clock"] }
chrono-tz = "0.10"
serde = { version = "1", features = ["derive"] }
proptest = "1"
```

`crates/wakode-core/Cargo.toml`:

```toml
[package]
name = "wakode-core"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
chrono.workspace = true
chrono-tz.workspace = true
serde.workspace = true

[dev-dependencies]
proptest.workspace = true
```

- [ ] **Step 2: Написать падающий тест**

`crates/wakode-core/src/time.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_secs_converts_to_microseconds() {
        assert_eq!(Micros::from_secs(90).get(), 90_000_000);
    }

    #[test]
    fn from_secs_f64_rounds_to_nearest_microsecond() {
        // WakaTime присылает время как float-секунды; округляем, а не отбрасываем
        assert_eq!(Micros::from_secs_f64(1.0000005).get(), 1_000_001);
        assert_eq!(Micros::from_secs_f64(1.0000004).get(), 1_000_000);
    }

    #[test]
    fn saturating_sub_does_not_overflow() {
        let a = Micros::new(i64::MIN);
        assert_eq!(a.saturating_sub(Micros::from_secs(1)).get(), i64::MIN);
    }
}
```

`crates/wakode-core/src/lib.rs`:

```rust
pub mod time;

pub use time::Micros;
```

- [ ] **Step 3: Запустить тест и убедиться, что он падает**

Run: `cargo test -p wakode-core`
Expected: FAIL, `cannot find type Micros in this scope`

- [ ] **Step 4: Реализовать `Micros`**

В начало `crates/wakode-core/src/time.rs`, перед блоком тестов:

```rust
use serde::{Deserialize, Serialize};

/// Момент времени в микросекундах от Unix epoch, всегда UTC.
///
/// Внутри крейта время никогда не представляется числами с плавающей точкой:
/// они появляются только на границе HTTP-слоя, где так велит протокол WakaTime.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[derive(Serialize, Deserialize)]
pub struct Micros(i64);

impl Micros {
    pub const ZERO: Micros = Micros(0);

    pub const fn new(micros: i64) -> Self {
        Micros(micros)
    }

    pub const fn from_secs(secs: i64) -> Self {
        Micros(secs.saturating_mul(1_000_000))
    }

    /// Преобразование из float-секунд, как их присылает wakatime-cli.
    pub fn from_secs_f64(secs: f64) -> Self {
        Micros((secs * 1_000_000.0).round() as i64)
    }

    pub const fn get(self) -> i64 {
        self.0
    }

    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }

    pub const fn saturating_add(self, other: Micros) -> Self {
        Micros(self.0.saturating_add(other.0))
    }

    pub const fn saturating_sub(self, other: Micros) -> Self {
        Micros(self.0.saturating_sub(other.0))
    }
}
```

- [ ] **Step 5: Запустить тесты**

Run: `cargo test -p wakode-core`
Expected: PASS, три теста

- [ ] **Step 6: Коммит**

```bash
git add Cargo.toml crates/wakode-core
git commit -m "feat(core): workspace и тип времени Micros"
```

---

### Task 2: Доменные типы

**Files:**
- Create: `crates/wakode-core/src/domain.rs`
- Modify: `crates/wakode-core/src/lib.rs`

**Interfaces:**
- Consumes: `Micros` из Task 1.
- Produces: `Sid(u32)`; перечисления `EntityKind { File, App, Url, Domain }` и `Category { Coding, Building, Debugging, Writing, Reviewing, Browsing, Communicating, Designing, Other }`; структура `Attrs` с полями `entity: Sid`, `kind: EntityKind`, `category: Category`, `project/branch/language/editor/os/machine: Option<Sid>`; структура `Heartbeat { time: Micros, attrs: Attrs }`.

- [ ] **Step 1: Написать падающий тест**

В конец `crates/wakode-core/src/domain.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Micros;

    fn attrs(project: u32) -> Attrs {
        Attrs {
            entity: Sid(1),
            kind: EntityKind::File,
            category: Category::Coding,
            project: Some(Sid(project)),
            branch: None,
            language: None,
            editor: None,
            os: None,
            machine: None,
        }
    }

    #[test]
    fn heartbeats_sort_by_time_then_attrs() {
        // Детерминированный порядок нужен движку интервалов: одинаковые входные
        // данные обязаны давать одинаковый результат независимо от порядка.
        let mut hbs = vec![
            Heartbeat { time: Micros::from_secs(10), attrs: attrs(2) },
            Heartbeat { time: Micros::from_secs(10), attrs: attrs(1) },
            Heartbeat { time: Micros::from_secs(5), attrs: attrs(9) },
        ];
        hbs.sort();

        assert_eq!(hbs[0].time, Micros::from_secs(5));
        assert_eq!(hbs[1].attrs.project, Some(Sid(1)));
        assert_eq!(hbs[2].attrs.project, Some(Sid(2)));
    }

    #[test]
    fn category_defaults_to_coding() {
        assert_eq!(Category::default(), Category::Coding);
    }
}
```

- [ ] **Step 2: Запустить тест и убедиться, что он падает**

Сначала добавить модуль в `crates/wakode-core/src/lib.rs`:

```rust
pub mod domain;
pub mod time;

pub use domain::{Attrs, Category, EntityKind, Heartbeat, Sid};
pub use time::Micros;
```

Run: `cargo test -p wakode-core`
Expected: FAIL, `cannot find type Attrs in this scope`

- [ ] **Step 3: Реализовать доменные типы**

В начало `crates/wakode-core/src/domain.rs`:

```rust
use serde::{Deserialize, Serialize};

use crate::Micros;

/// Идентификатор интернированной строки.
///
/// Крейт никогда не видит самих строк: путь к файлу, проект, ветка и язык
/// повторяются в потоке миллионы раз, поэтому слой хранения держит словарь,
/// а сюда передаёт только номера. Группировка по числам и быстрее, и проще.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[derive(Serialize, Deserialize)]
pub struct Sid(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityKind {
    #[default]
    File,
    App,
    Url,
    Domain,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    #[default]
    Coding,
    Building,
    Debugging,
    Writing,
    Reviewing,
    Browsing,
    Communicating,
    Designing,
    Other,
}

/// Атрибуты отметки — всё, кроме времени.
///
/// Интервал наследует атрибуты более ранней из пары отметок, поэтому они
/// хранятся отдельным типом и копируются целиком.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[derive(Serialize, Deserialize)]
pub struct Attrs {
    pub entity: Sid,
    pub kind: EntityKind,
    pub category: Category,
    pub project: Option<Sid>,
    pub branch: Option<Sid>,
    pub language: Option<Sid>,
    pub editor: Option<Sid>,
    pub os: Option<Sid>,
    pub machine: Option<Sid>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[derive(Serialize, Deserialize)]
pub struct Heartbeat {
    pub time: Micros,
    pub attrs: Attrs,
}
```

Порядок полей в `Attrs` важен: производный `Ord` сравнивает поля сверху вниз, а `Heartbeat` сравнивается сначала по `time`, потом по `attrs` — именно потому, что `time` объявлено первым.

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p wakode-core`
Expected: PASS, пять тестов

- [ ] **Step 5: Коммит**

```bash
git add crates/wakode-core
git commit -m "feat(core): доменные типы heartbeat"
```

---

### Task 3: Конфигурация движка длительностей

**Files:**
- Create: `crates/wakode-core/src/config.rs`
- Modify: `crates/wakode-core/src/lib.rs`

**Interfaces:**
- Consumes: `Micros`.
- Produces: `DurationConfig` с конструктором `new(timeout: Micros, tail_padding: Micros) -> Result<DurationConfig, ConfigError>`, геттерами `timeout()` и `tail_padding()`, реализацией `Default`; перечисление `ConfigError { NonPositiveTimeout, NegativePadding, PaddingExceedsTimeout }`.

- [ ] **Step 1: Написать падающий тест**

В конец `crates/wakode-core/src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timeout_matches_wakatime() {
        assert_eq!(DurationConfig::default().timeout(), Micros::from_secs(900));
    }

    #[test]
    fn default_tail_padding_is_zero() {
        // Настоящее значение WakaTime неизвестно и калибруется отдельно;
        // до тех пор ноль — единственный честный вариант.
        assert_eq!(DurationConfig::default().tail_padding(), Micros::ZERO);
    }

    #[test]
    fn rejects_padding_larger_than_timeout() {
        // Иначе хвост одной сессии наедет на начало следующей.
        let err = DurationConfig::new(Micros::from_secs(60), Micros::from_secs(61));
        assert_eq!(err, Err(ConfigError::PaddingExceedsTimeout));
    }

    #[test]
    fn rejects_non_positive_timeout() {
        assert_eq!(
            DurationConfig::new(Micros::ZERO, Micros::ZERO),
            Err(ConfigError::NonPositiveTimeout)
        );
    }

    #[test]
    fn rejects_negative_padding() {
        assert_eq!(
            DurationConfig::new(Micros::from_secs(60), Micros::new(-1)),
            Err(ConfigError::NegativePadding)
        );
    }

    #[test]
    fn accepts_valid_configuration() {
        let cfg = DurationConfig::new(Micros::from_secs(300), Micros::from_secs(30)).unwrap();
        assert_eq!(cfg.timeout(), Micros::from_secs(300));
        assert_eq!(cfg.tail_padding(), Micros::from_secs(30));
    }
}
```

- [ ] **Step 2: Запустить тест и убедиться, что он падает**

Добавить в `crates/wakode-core/src/lib.rs`:

```rust
pub mod config;
pub mod domain;
pub mod time;

pub use config::{ConfigError, DurationConfig};
pub use domain::{Attrs, Category, EntityKind, Heartbeat, Sid};
pub use time::Micros;
```

Run: `cargo test -p wakode-core`
Expected: FAIL, `cannot find type DurationConfig in this scope`

- [ ] **Step 3: Реализовать конфигурацию**

В начало `crates/wakode-core/src/config.rs`:

```rust
use crate::Micros;

pub const DEFAULT_TIMEOUT_SECS: i64 = 900;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigError {
    NonPositiveTimeout,
    NegativePadding,
    PaddingExceedsTimeout,
}

/// Параметры склейки отметок в интервалы.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DurationConfig {
    timeout: Micros,
    tail_padding: Micros,
}

impl DurationConfig {
    /// Инвариант `tail_padding <= timeout` держит интервалы непересекающимися:
    /// хвост последней отметки сессии не может дотянуться до следующей сессии,
    /// потому что та начинается позже, чем через `timeout`.
    pub fn new(timeout: Micros, tail_padding: Micros) -> Result<Self, ConfigError> {
        if timeout.get() <= 0 {
            return Err(ConfigError::NonPositiveTimeout);
        }
        if tail_padding.get() < 0 {
            return Err(ConfigError::NegativePadding);
        }
        if tail_padding > timeout {
            return Err(ConfigError::PaddingExceedsTimeout);
        }
        Ok(Self { timeout, tail_padding })
    }

    pub fn timeout(self) -> Micros {
        self.timeout
    }

    pub fn tail_padding(self) -> Micros {
        self.tail_padding
    }
}

impl Default for DurationConfig {
    fn default() -> Self {
        Self {
            timeout: Micros::from_secs(DEFAULT_TIMEOUT_SECS),
            tail_padding: Micros::ZERO,
        }
    }
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p wakode-core`
Expected: PASS, одиннадцать тестов

- [ ] **Step 5: Коммит**

```bash
git add crates/wakode-core
git commit -m "feat(core): конфигурация движка длительностей"
```

---

### Task 4: Движок интервалов — склейка в пределах таймаута

**Files:**
- Create: `crates/wakode-core/src/intervals.rs`
- Modify: `crates/wakode-core/src/lib.rs`

**Interfaces:**
- Consumes: `Heartbeat`, `Attrs`, `Micros`, `DurationConfig`.
- Produces: `Interval { start: Micros, end: Micros, attrs: Attrs }` с методом `duration(self) -> Micros`; функция `build_intervals(heartbeats: &[Heartbeat], _cfg: DurationConfig) -> Vec<Interval>`.

Параметр конфигурации в этой задаче ещё не используется — отсюда подчёркивание в имени. Разрыв сессии по таймауту появится в Task 5 вместе с тестами, которые его требуют.

- [ ] **Step 1: Написать падающий тест**

В конец `crates/wakode-core/src/intervals.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Category, EntityKind, Sid};

    fn attrs(project: u32) -> Attrs {
        Attrs {
            entity: Sid(project),
            kind: EntityKind::File,
            category: Category::Coding,
            project: Some(Sid(project)),
            branch: None,
            language: None,
            editor: None,
            os: None,
            machine: None,
        }
    }

    fn hb(secs: i64, project: u32) -> Heartbeat {
        Heartbeat { time: Micros::from_secs(secs), attrs: attrs(project) }
    }

    #[test]
    fn empty_input_produces_no_intervals() {
        assert!(build_intervals(&[], DurationConfig::default()).is_empty());
    }

    #[test]
    fn adjacent_heartbeats_within_timeout_form_one_interval() {
        let cfg = DurationConfig::default();
        let intervals = build_intervals(&[hb(0, 1), hb(60, 1)], cfg);

        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0].start, Micros::from_secs(0));
        assert_eq!(intervals[0].end, Micros::from_secs(60));
        assert_eq!(intervals[0].duration(), Micros::from_secs(60));
    }

    #[test]
    fn interval_inherits_attributes_of_the_earlier_heartbeat() {
        // Промежуток между отметками — это время, проведённое в том, что было
        // открыто раньше, а не в том, куда пользователь только что перешёл.
        let cfg = DurationConfig::default();
        let intervals = build_intervals(&[hb(0, 1), hb(60, 2)], cfg);

        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0].attrs.project, Some(Sid(1)));
    }

    #[test]
    fn three_heartbeats_form_two_intervals() {
        let cfg = DurationConfig::default();
        let intervals = build_intervals(&[hb(0, 1), hb(60, 1), hb(120, 1)], cfg);

        assert_eq!(intervals.len(), 2);
        assert_eq!(intervals[1].start, Micros::from_secs(60));
        assert_eq!(intervals[1].end, Micros::from_secs(120));
    }
}
```

- [ ] **Step 2: Запустить тест и убедиться, что он падает**

Добавить в `crates/wakode-core/src/lib.rs`:

```rust
pub mod config;
pub mod domain;
pub mod intervals;
pub mod time;

pub use config::{ConfigError, DurationConfig};
pub use domain::{Attrs, Category, EntityKind, Heartbeat, Sid};
pub use intervals::{build_intervals, Interval};
pub use time::Micros;
```

Run: `cargo test -p wakode-core`
Expected: FAIL, `cannot find function build_intervals in this scope`

- [ ] **Step 3: Реализовать минимальную склейку**

В начало `crates/wakode-core/src/intervals.rs`:

```rust
use serde::{Deserialize, Serialize};

use crate::{Attrs, DurationConfig, Heartbeat, Micros};

/// Отрезок времени с атрибутами отметки, которой он принадлежит.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[derive(Serialize, Deserialize)]
pub struct Interval {
    pub start: Micros,
    pub end: Micros,
    pub attrs: Attrs,
}

impl Interval {
    pub fn duration(self) -> Micros {
        self.end.saturating_sub(self.start)
    }
}

/// Превращает поток отметок в непересекающиеся интервалы.
///
/// Каждая пара соседних по времени отметок даёт интервал; интервал наследует
/// атрибуты более ранней из пары.
pub fn build_intervals(heartbeats: &[Heartbeat], _cfg: DurationConfig) -> Vec<Interval> {
    if heartbeats.is_empty() {
        return Vec::new();
    }

    let mut sorted = heartbeats.to_vec();
    sorted.sort_unstable();

    let mut out = Vec::with_capacity(sorted.len());
    for (i, hb) in sorted.iter().enumerate() {
        let Some(next) = sorted.get(i + 1) else { continue };
        if next.time > hb.time {
            out.push(Interval { start: hb.time, end: next.time, attrs: hb.attrs });
        }
    }
    out
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p wakode-core`
Expected: PASS, пятнадцать тестов

- [ ] **Step 5: Коммит**

```bash
git add crates/wakode-core
git commit -m "feat(core): склейка соседних отметок в интервалы"
```

---

### Task 5: Разрыв сессии по таймауту

**Files:**
- Modify: `crates/wakode-core/src/intervals.rs`

**Interfaces:**
- Consumes: всё из Task 4.
- Produces: изменений в сигнатурах нет, меняется поведение `build_intervals`.

- [ ] **Step 1: Написать падающий тест**

Добавить в модуль тестов `crates/wakode-core/src/intervals.rs`:

```rust
    #[test]
    fn gap_longer_than_timeout_breaks_the_session() {
        // Пауза длиннее таймаута не засчитывается никому: пользователь ушёл.
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::ZERO).unwrap();
        let intervals = build_intervals(&[hb(0, 1), hb(901, 1)], cfg);

        assert!(intervals.is_empty(), "пауза в 901 секунду не должна давать интервал");
    }

    #[test]
    fn gap_exactly_equal_to_timeout_is_still_counted() {
        // Граница включительная: ровно таймаут — ещё та же сессия.
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::ZERO).unwrap();
        let intervals = build_intervals(&[hb(0, 1), hb(900, 1)], cfg);

        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0].duration(), Micros::from_secs(900));
    }

    #[test]
    fn two_sessions_separated_by_a_long_pause() {
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::ZERO).unwrap();
        let intervals = build_intervals(
            &[hb(0, 1), hb(60, 1), hb(5000, 1), hb(5060, 1)],
            cfg,
        );

        assert_eq!(intervals.len(), 2);
        assert_eq!(intervals[0].duration(), Micros::from_secs(60));
        assert_eq!(intervals[1].start, Micros::from_secs(5000));
        assert_eq!(intervals[1].duration(), Micros::from_secs(60));
    }
```

- [ ] **Step 2: Запустить тесты и убедиться, что они падают**

Run: `cargo test -p wakode-core`
Expected: FAIL на `gap_longer_than_timeout_breaks_the_session` — пауза в 901 секунду пока даёт интервал, потому что таймаут ещё не учитывается

- [ ] **Step 3: Реализовать разрыв сессии**

В `crates/wakode-core/src/intervals.rs` переименовать параметр `_cfg` в `cfg` и добавить проверку внутрь цикла:

```rust
pub fn build_intervals(heartbeats: &[Heartbeat], cfg: DurationConfig) -> Vec<Interval> {
    if heartbeats.is_empty() {
        return Vec::new();
    }

    let mut sorted = heartbeats.to_vec();
    sorted.sort_unstable();

    let mut out = Vec::with_capacity(sorted.len());
    for (i, hb) in sorted.iter().enumerate() {
        let Some(next) = sorted.get(i + 1) else { continue };
        // Пауза длиннее таймаута означает, что пользователь ушёл: это время не
        // засчитывается никому. Граница включительная — ровно таймаут ещё считается.
        if next.time.saturating_sub(hb.time) > cfg.timeout() {
            continue;
        }
        if next.time > hb.time {
            out.push(Interval { start: hb.time, end: next.time, attrs: hb.attrs });
        }
    }
    out
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p wakode-core`
Expected: PASS, восемнадцать тестов

- [ ] **Step 5: Коммит**

```bash
git add crates/wakode-core
git commit -m "feat(core): разрыв сессии по таймауту"
```

---

### Task 6: Хвостовая добавка последней отметке сессии

**Files:**
- Modify: `crates/wakode-core/src/intervals.rs`

**Interfaces:**
- Consumes: всё из Task 4.
- Produces: изменений в сигнатурах нет, меняется поведение `build_intervals`.

- [ ] **Step 1: Написать падающий тест**

Добавить в модуль тестов `crates/wakode-core/src/intervals.rs`:

```rust
    #[test]
    fn last_heartbeat_of_a_session_gets_tail_padding() {
        // У последней отметки сессии нет пары, поэтому ей начисляется добавка.
        // Величина, которую использует WakaTime, неизвестна — здесь она задана явно.
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::from_secs(120)).unwrap();
        let intervals = build_intervals(&[hb(0, 1), hb(60, 1)], cfg);

        assert_eq!(intervals.len(), 2);
        assert_eq!(intervals[1].start, Micros::from_secs(60));
        assert_eq!(intervals[1].end, Micros::from_secs(180));
        assert_eq!(intervals[1].attrs.project, Some(Sid(1)));
    }

    #[test]
    fn each_session_gets_its_own_tail_padding() {
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::from_secs(60)).unwrap();
        let intervals = build_intervals(&[hb(0, 1), hb(5000, 2)], cfg);

        assert_eq!(intervals.len(), 2);
        assert_eq!(intervals[0].end, Micros::from_secs(60));
        assert_eq!(intervals[1].end, Micros::from_secs(5060));
    }

    #[test]
    fn zero_padding_produces_no_tail_interval() {
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::ZERO).unwrap();
        let intervals = build_intervals(&[hb(0, 1), hb(60, 1)], cfg);

        assert_eq!(intervals.len(), 1);
    }

    #[test]
    fn single_heartbeat_produces_only_padding() {
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::from_secs(30)).unwrap();
        let intervals = build_intervals(&[hb(100, 7)], cfg);

        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0].start, Micros::from_secs(100));
        assert_eq!(intervals[0].end, Micros::from_secs(130));
    }
```

- [ ] **Step 2: Запустить тесты и убедиться, что они падают**

Run: `cargo test -p wakode-core`
Expected: FAIL — `assertion \`left == right\` failed: 1 != 2`, добавка пока не реализована

- [ ] **Step 3: Реализовать добавку**

Заменить тело цикла в `build_intervals` (`crates/wakode-core/src/intervals.rs`):

```rust
    let mut out = Vec::with_capacity(sorted.len());
    for (i, hb) in sorted.iter().enumerate() {
        let end = match sorted.get(i + 1) {
            // Следующая отметка в пределах таймаута — интервал тянется до неё.
            Some(next) if next.time.saturating_sub(hb.time) <= cfg.timeout() => next.time,
            // Иначе это последняя отметка сессии: ей начисляется хвостовая добавка.
            _ => hb.time.saturating_add(cfg.tail_padding()),
        };
        if end > hb.time {
            out.push(Interval { start: hb.time, end, attrs: hb.attrs });
        }
    }
    out
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p wakode-core`
Expected: PASS, двадцать два теста

- [ ] **Step 5: Коммит**

```bash
git add crates/wakode-core
git commit -m "feat(core): хвостовая добавка последней отметке сессии"
```

---

### Task 7: Устойчивость к порядку и дубликатам

**Files:**
- Modify: `crates/wakode-core/src/intervals.rs`

**Interfaces:**
- Consumes: всё из Task 6.
- Produces: изменений в сигнатурах нет.

Зачем это отдельная задача: wakatime-cli досылает накопленные оффлайн отметки **вне хронологического порядка** и может прислать один и тот же батч дважды, если не получил подтверждения. Обе ситуации штатные, и движок обязан давать на них тот же ответ, что и на чистый ввод.

- [ ] **Step 1: Написать падающий тест**

Добавить в модуль тестов `crates/wakode-core/src/intervals.rs`:

```rust
    #[test]
    fn input_order_does_not_affect_the_result() {
        let cfg = DurationConfig::default();
        let ordered = build_intervals(&[hb(0, 1), hb(60, 1), hb(120, 1)], cfg);
        let shuffled = build_intervals(&[hb(120, 1), hb(0, 1), hb(60, 1)], cfg);

        assert_eq!(ordered, shuffled);
    }

    #[test]
    fn duplicate_heartbeats_do_not_inflate_totals() {
        // Полный дубликат — это повтор доставки, а не новая активность.
        let cfg = DurationConfig::default();
        let clean = build_intervals(&[hb(0, 1), hb(60, 1)], cfg);
        let duplicated = build_intervals(&[hb(0, 1), hb(0, 1), hb(60, 1), hb(60, 1)], cfg);

        assert_eq!(clean, duplicated);
    }

    #[test]
    fn simultaneous_heartbeats_with_different_attributes_produce_no_zero_intervals() {
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::ZERO).unwrap();
        let intervals = build_intervals(&[hb(0, 1), hb(0, 2), hb(60, 1)], cfg);

        assert!(
            intervals.iter().all(|iv| iv.duration().get() > 0),
            "интервалы нулевой длины не должны попадать в результат"
        );
    }
```

- [ ] **Step 2: Запустить тесты**

Run: `cargo test -p wakode-core`
Expected: PASS, двадцать семь тестов — без единого изменения в коде.

Это не ошибка плана и не повод дописывать реализацию. Движок уже устойчив к дубликатам, и устойчивость эту несёт guard `end > hb.time` из задачи 6: два одинаковых timestamp дают интервал нулевой длины, а guard его отсекает. Задача 7 не добавляет поведения — она фиксирует тестами уже существующий контракт, который до сих пор держался на одном строгом сравнении и мог быть сломан незаметно.

Явная дедупликация (`sorted.dedup()`) здесь **не нужна и не добавляется**: доказано, что она не меняет результат `build_intervals` ни при каком входе — ни в середине сессии, ни на разрыве, ни в позиции хвостовой добавки, ни при нулевой, ни при ненулевой `tail_padding`. Строка, которая ничего не делает, но выглядит несущей, дороже своего отсутствия.

- [ ] **Step 3: Коммит**

```bash
git add crates/wakode-core
git commit -m "test(core): зафиксировать устойчивость движка к порядку и дубликатам"
```

---

### Task 8: Property-тесты движка

**Files:**
- Create: `crates/wakode-core/tests/properties.rs`

**Interfaces:**
- Consumes: публичный API `wakode-core`.
- Produces: ничего, только тесты.

- [ ] **Step 1: Написать генераторы и property-тесты**

`crates/wakode-core/tests/properties.rs`:

```rust
use proptest::prelude::*;
use wakode_core::{
    build_intervals, Attrs, Category, DurationConfig, EntityKind, Heartbeat, Micros, Sid,
};

fn arb_attrs() -> impl Strategy<Value = Attrs> {
    // Небольшие диапазоны намеренно: так генератор чаще создаёт совпадения
    // и коллизии, а именно на них ломается склейка.
    (0u32..5, 0u32..3, 0u32..3).prop_map(|(entity, project, language)| Attrs {
        entity: Sid(entity),
        kind: EntityKind::File,
        category: Category::Coding,
        project: Some(Sid(project)),
        branch: None,
        language: Some(Sid(language)),
        editor: None,
        os: None,
        machine: None,
    })
}

fn arb_heartbeats() -> impl Strategy<Value = Vec<Heartbeat>> {
    prop::collection::vec(
        (0i64..100_000, arb_attrs())
            .prop_map(|(secs, attrs)| Heartbeat { time: Micros::from_secs(secs), attrs }),
        0..200,
    )
}

fn cfg() -> DurationConfig {
    DurationConfig::new(Micros::from_secs(900), Micros::from_secs(120)).unwrap()
}

proptest! {
    /// Ни один интервал не может быть длиннее таймаута: всё, что длиннее,
    /// означает, что пауза была засчитана как работа.
    #[test]
    fn no_interval_exceeds_timeout(hbs in arb_heartbeats()) {
        let cfg = cfg();
        for iv in build_intervals(&hbs, cfg) {
            prop_assert!(iv.duration() <= cfg.timeout());
        }
    }

    /// Интервалы одного пользователя не пересекаются — иначе одно и то же
    /// время засчитывается дважды и сумма по проектам разъезжается с итогом.
    #[test]
    fn intervals_never_overlap(hbs in arb_heartbeats()) {
        let intervals = build_intervals(&hbs, cfg());
        for pair in intervals.windows(2) {
            prop_assert!(pair[0].end <= pair[1].start);
        }
    }

    /// Порядок доставки не влияет на результат: оффлайн-очередь присылает
    /// отметки как попало.
    #[test]
    fn result_is_invariant_under_permutation(
        hbs in arb_heartbeats(),
        seed in any::<u64>(),
    ) {
        let mut shuffled = hbs.clone();
        // Детерминированная перестановка без внешних зависимостей.
        let n = shuffled.len();
        for i in 0..n {
            let j = ((seed as usize).wrapping_mul(i + 1).wrapping_add(i)) % n.max(1);
            shuffled.swap(i, j);
        }
        prop_assert_eq!(build_intervals(&hbs, cfg()), build_intervals(&shuffled, cfg()));
    }

    /// Повторная доставка того же батча не меняет ничего.
    #[test]
    fn result_is_invariant_under_duplication(hbs in arb_heartbeats()) {
        let mut doubled = hbs.clone();
        doubled.extend(hbs.iter().copied());
        prop_assert_eq!(build_intervals(&hbs, cfg()), build_intervals(&doubled, cfg()));
    }

    /// Добавление отметки не может уменьшить суммарное время.
    #[test]
    fn adding_a_heartbeat_never_reduces_total(
        hbs in arb_heartbeats(),
        extra in (0i64..100_000, arb_attrs())
            .prop_map(|(s, a)| Heartbeat { time: Micros::from_secs(s), attrs: a }),
    ) {
        let before: i64 = build_intervals(&hbs, cfg()).iter().map(|i| i.duration().get()).sum();
        let mut more = hbs.clone();
        more.push(extra);
        let after: i64 = build_intervals(&more, cfg()).iter().map(|i| i.duration().get()).sum();
        prop_assert!(after >= before);
    }
}
```

- [ ] **Step 2: Запустить property-тесты**

Run: `cargo test -p wakode-core --test properties`
Expected: PASS, пять property-тестов по 256 случаев каждый

Если `intervals_never_overlap` падает — проверить инвариант `tail_padding <= timeout` в `DurationConfig::new`: именно он гарантирует, что хвост сессии не дотягивается до следующей.

- [ ] **Step 3: Коммит**

```bash
git add crates/wakode-core/tests
git commit -m "test(core): property-тесты движка длительностей"
```

---

### Task 9: Агрегация

**Files:**
- Create: `crates/wakode-core/src/aggregate.rs`
- Modify: `crates/wakode-core/src/lib.rs`

**Interfaces:**
- Consumes: `Interval`, `Attrs`, `Micros`.
- Produces: `Bucket<K> { key: K, total: Micros }`; `aggregate_by<K, F>(intervals: &[Interval], key_of: F) -> Vec<Bucket<K>>` где `K: Copy + Eq + Hash + Ord`, `F: Fn(&Attrs) -> K`; `grand_total(intervals: &[Interval]) -> Micros`; `percent(part: Micros, whole: Micros) -> f64`.

- [ ] **Step 1: Написать падающий тест**

В конец `crates/wakode-core/src/aggregate.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attrs, Category, EntityKind, Sid};

    fn interval(start: i64, end: i64, project: u32) -> Interval {
        Interval {
            start: Micros::from_secs(start),
            end: Micros::from_secs(end),
            attrs: Attrs {
                entity: Sid(project),
                kind: EntityKind::File,
                category: Category::Coding,
                project: Some(Sid(project)),
                branch: None,
                language: None,
                editor: None,
                os: None,
                machine: None,
            },
        }
    }

    #[test]
    fn sums_intervals_per_key() {
        let intervals = [interval(0, 60, 1), interval(60, 120, 2), interval(120, 200, 1)];
        let buckets = aggregate_by(&intervals, |a| a.project);

        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].key, Some(Sid(1)));
        assert_eq!(buckets[0].total, Micros::from_secs(140));
        assert_eq!(buckets[1].key, Some(Sid(2)));
        assert_eq!(buckets[1].total, Micros::from_secs(60));
    }

    #[test]
    fn buckets_are_sorted_by_total_descending_then_by_key() {
        // Детерминированный порядок нужен снапшот-тестам совместимого слоя.
        let intervals = [interval(0, 60, 3), interval(60, 120, 1), interval(120, 180, 2)];
        let buckets = aggregate_by(&intervals, |a| a.project);

        let keys: Vec<_> = buckets.iter().map(|b| b.key).collect();
        assert_eq!(keys, vec![Some(Sid(1)), Some(Sid(2)), Some(Sid(3))]);
    }

    #[test]
    fn bucket_totals_sum_to_grand_total() {
        let intervals = [interval(0, 60, 1), interval(60, 120, 2), interval(120, 200, 1)];
        let sum: i64 = aggregate_by(&intervals, |a| a.project)
            .iter()
            .map(|b| b.total.get())
            .sum();

        assert_eq!(sum, grand_total(&intervals).get());
    }

    #[test]
    fn empty_input_produces_no_buckets() {
        let buckets = aggregate_by(&[], |a: &Attrs| a.project);
        assert!(buckets.is_empty());
        assert_eq!(grand_total(&[]), Micros::ZERO);
    }

    #[test]
    fn percent_of_zero_whole_is_zero() {
        assert_eq!(percent(Micros::from_secs(10), Micros::ZERO), 0.0);
    }

    #[test]
    fn percent_is_computed_against_the_whole() {
        assert_eq!(percent(Micros::from_secs(25), Micros::from_secs(100)), 25.0);
    }
}
```

- [ ] **Step 2: Запустить тест и убедиться, что он падает**

Добавить в `crates/wakode-core/src/lib.rs`:

```rust
pub mod aggregate;
pub mod config;
pub mod domain;
pub mod intervals;
pub mod time;

pub use aggregate::{aggregate_by, grand_total, percent, Bucket};
pub use config::{ConfigError, DurationConfig};
pub use domain::{Attrs, Category, EntityKind, Heartbeat, Sid};
pub use intervals::{build_intervals, Interval};
pub use time::Micros;
```

Run: `cargo test -p wakode-core`
Expected: FAIL, `cannot find function aggregate_by in this scope`

- [ ] **Step 3: Реализовать агрегацию**

В начало `crates/wakode-core/src/aggregate.rs`:

```rust
use std::collections::HashMap;
use std::hash::Hash;

use crate::{Interval, Micros};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bucket<K> {
    pub key: K,
    pub total: Micros,
}

/// Суммирует интервалы по произвольному признаку.
///
/// Измерение задаётся замыканием, а не перечислением: проекты, языки, редакторы
/// и категории различаются только тем, какое поле из атрибутов взять, и заводить
/// на это отдельный тип означало бы дублировать одну и ту же функцию восемь раз.
pub fn aggregate_by<K, F>(intervals: &[Interval], key_of: F) -> Vec<Bucket<K>>
where
    K: Copy + Eq + Hash + Ord,
    F: Fn(&crate::Attrs) -> K,
{
    let mut totals: HashMap<K, i64> = HashMap::new();
    for iv in intervals {
        *totals.entry(key_of(&iv.attrs)).or_insert(0) += iv.duration().get();
    }

    let mut out: Vec<Bucket<K>> = totals
        .into_iter()
        .map(|(key, total)| Bucket { key, total: Micros::new(total) })
        .collect();
    // Сначала по убыванию времени, при равенстве — по ключу: результат обязан
    // быть детерминированным, иначе снапшот-тесты совместимого слоя поплывут.
    out.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.key.cmp(&b.key)));
    out
}

pub fn grand_total(intervals: &[Interval]) -> Micros {
    Micros::new(intervals.iter().map(|iv| iv.duration().get()).sum())
}

pub fn percent(part: Micros, whole: Micros) -> f64 {
    if whole.get() == 0 {
        return 0.0;
    }
    part.get() as f64 * 100.0 / whole.get() as f64
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p wakode-core`
Expected: PASS, тридцать один тест плюс пять property-тестов

- [ ] **Step 5: Коммит**

```bash
git add crates/wakode-core
git commit -m "feat(core): агрегация интервалов по измерениям"
```

---

### Task 10: Локальные дни и таймзоны

**Files:**
- Create: `crates/wakode-core/src/calendar.rs`
- Modify: `crates/wakode-core/src/lib.rs`
- Modify: `crates/wakode-core/tests/properties.rs`

**Interfaces:**
- Consumes: `Interval`, `Micros`.
- Produces: `local_day_bounds(date: NaiveDate, tz: Tz) -> (Micros, Micros)`; `local_date_of(t: Micros, tz: Tz) -> NaiveDate`; `split_by_local_day(intervals: &[Interval], tz: Tz) -> BTreeMap<NaiveDate, Vec<Interval>>`.

- [ ] **Step 1: Написать падающий тест**

В конец `crates/wakode-core/src/calendar.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attrs, Category, EntityKind, Sid};
    use chrono_tz::Tz;

    fn attrs() -> Attrs {
        Attrs {
            entity: Sid(1),
            kind: EntityKind::File,
            category: Category::Coding,
            project: Some(Sid(1)),
            branch: None,
            language: None,
            editor: None,
            os: None,
            machine: None,
        }
    }

    fn at(iso: &str) -> Micros {
        Micros::new(
            iso.parse::<chrono::DateTime<chrono::Utc>>()
                .expect("валидная дата")
                .timestamp_micros(),
        )
    }

    #[test]
    fn day_bounds_respect_the_timezone() {
        let tz: Tz = "Europe/Moscow".parse().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        let (start, end) = local_day_bounds(date, tz);

        // Москва — UTC+3, значит локальная полночь это 21:00 предыдущего дня UTC.
        assert_eq!(start, at("2026-08-14T21:00:00Z"));
        assert_eq!(end, at("2026-08-15T21:00:00Z"));
    }

    #[test]
    fn day_bounds_handle_dst_shift() {
        // В Чили переход на летнее время происходит в полночь: 00:00 не существует,
        // сутки начинаются в 01:00 по местному времени.
        let tz: Tz = "America/Santiago".parse().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 9, 6).unwrap();
        let (start, end) = local_day_bounds(date, tz);

        assert!(start < end, "границы дня обязаны быть упорядочены даже при переходе");
        assert!(
            end.saturating_sub(start).get() < 25 * 3600 * 1_000_000,
            "сутки не могут быть длиннее 25 часов"
        );
    }

    #[test]
    fn interval_crossing_midnight_is_split() {
        let tz: Tz = "Europe/Moscow".parse().unwrap();
        let iv = Interval {
            start: at("2026-08-14T20:30:00Z"), // 23:30 по Москве
            end: at("2026-08-14T21:30:00Z"),   // 00:30 по Москве
            attrs: attrs(),
        };
        let days = split_by_local_day(&[iv], tz);

        assert_eq!(days.len(), 2);
        let d14 = NaiveDate::from_ymd_opt(2026, 8, 14).unwrap();
        let d15 = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        assert_eq!(days[&d14][0].duration(), Micros::from_secs(1800));
        assert_eq!(days[&d15][0].duration(), Micros::from_secs(1800));
    }

    #[test]
    fn splitting_preserves_total_duration() {
        let tz: Tz = "Europe/Moscow".parse().unwrap();
        let iv = Interval {
            start: at("2026-08-14T20:30:00Z"),
            end: at("2026-08-14T21:30:00Z"),
            attrs: attrs(),
        };
        let total: i64 = split_by_local_day(&[iv], tz)
            .values()
            .flatten()
            .map(|i| i.duration().get())
            .sum();

        assert_eq!(total, iv.duration().get());
    }
}
```

- [ ] **Step 2: Запустить тест и убедиться, что он падает**

Добавить в `crates/wakode-core/src/lib.rs`:

```rust
pub mod aggregate;
pub mod calendar;
pub mod config;
pub mod domain;
pub mod intervals;
pub mod time;

pub use aggregate::{aggregate_by, grand_total, percent, Bucket};
pub use calendar::{local_date_of, local_day_bounds, split_by_local_day};
pub use config::{ConfigError, DurationConfig};
pub use domain::{Attrs, Category, EntityKind, Heartbeat, Sid};
pub use intervals::{build_intervals, Interval};
pub use time::Micros;
```

Run: `cargo test -p wakode-core`
Expected: FAIL, `cannot find function local_day_bounds in this scope`

- [ ] **Step 3: Реализовать календарь**

В начало `crates/wakode-core/src/calendar.rs`:

```rust
use std::collections::BTreeMap;

use chrono::{DateTime, Duration, LocalResult, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;

use crate::{Interval, Micros};

/// Момент локальной полуночи указанной даты, выраженный в UTC-микросекундах.
///
/// Полуночи может не существовать: в ряде стран переход на летнее время
/// происходит ровно в 00:00, и сутки начинаются в 01:00. В этом случае берётся
/// первый существующий момент этих суток.
fn local_midnight(date: NaiveDate, tz: Tz) -> Micros {
    let naive = date.and_hms_opt(0, 0, 0).expect("полночь всегда валидна");
    let dt: DateTime<Tz> = match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => dt,
        // Час повторяется (переход на зимнее время) — берём первое вхождение.
        LocalResult::Ambiguous(earliest, _) => earliest,
        LocalResult::None => {
            let mut probe = naive;
            loop {
                probe += Duration::minutes(1);
                if let Some(dt) = tz.from_local_datetime(&probe).earliest() {
                    break dt;
                }
            }
        }
    };
    Micros::new(dt.timestamp_micros())
}

/// Границы локальных суток в UTC-микросекундах: `[начало, конец)`.
pub fn local_day_bounds(date: NaiveDate, tz: Tz) -> (Micros, Micros) {
    let next = date.succ_opt().expect("дата не на границе диапазона");
    (local_midnight(date, tz), local_midnight(next, tz))
}

/// Локальная дата, которой принадлежит момент времени.
pub fn local_date_of(t: Micros, tz: Tz) -> NaiveDate {
    DateTime::<Utc>::from_timestamp_micros(t.get())
        .expect("время в допустимом диапазоне")
        .with_timezone(&tz)
        .date_naive()
}

/// Разрезает интервалы по границам локальных суток.
///
/// Сессия с 23:50 до 00:30 обязана попасть в оба дня частями, иначе сумма за
/// день не сойдётся с суммой за неделю — классический источник расхождений.
pub fn split_by_local_day(intervals: &[Interval], tz: Tz) -> BTreeMap<NaiveDate, Vec<Interval>> {
    let mut out: BTreeMap<NaiveDate, Vec<Interval>> = BTreeMap::new();

    for iv in intervals {
        let mut cursor = iv.start;
        while cursor < iv.end {
            let date = local_date_of(cursor, tz);
            let (_, day_end) = local_day_bounds(date, tz);
            let piece_end = day_end.min(iv.end);
            out.entry(date)
                .or_default()
                .push(Interval { start: cursor, end: piece_end, attrs: iv.attrs });
            cursor = piece_end;
        }
    }

    out
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p wakode-core`
Expected: PASS, тридцать пять тестов

- [ ] **Step 5: Добавить property-тест на сходимость суммы**

В конец блока `proptest!` в `crates/wakode-core/tests/properties.rs`:

```rust
    /// Сумма по дням равна сумме за весь период. Это тот самый инвариант,
    /// который ловит потерю времени на границе полуночи.
    #[test]
    fn daily_totals_sum_to_period_total(hbs in arb_heartbeats()) {
        let tz: chrono_tz::Tz = "Europe/Moscow".parse().unwrap();
        let intervals = build_intervals(&hbs, cfg());
        let whole: i64 = intervals.iter().map(|i| i.duration().get()).sum();
        let by_days: i64 = wakode_core::split_by_local_day(&intervals, tz)
            .values()
            .flatten()
            .map(|i| i.duration().get())
            .sum();

        prop_assert_eq!(whole, by_days);
    }
```

Интеграционные тесты в `tests/` — отдельный крейт: обычные зависимости `wakode-core` им недоступны, нужен свой импорт. Дописать в `[dev-dependencies]` файла `crates/wakode-core/Cargo.toml`:

```toml
chrono-tz.workspace = true
```

- [ ] **Step 6: Запустить property-тесты**

Run: `cargo test -p wakode-core --test properties`
Expected: PASS, шесть property-тестов

- [ ] **Step 7: Коммит**

```bash
git add crates/wakode-core
git commit -m "feat(core): локальные дни с учётом таймзон и переходов времени"
```

---

## Проверка результата плана

- [ ] `cargo test --workspace` — все тесты зелёные
- [ ] `cargo tree -p wakode-core | grep -E 'sqlx|axum|tokio|reqwest'` — пусто, изоляция крейта не нарушена

## Что осталось за пределами этого плана

- Хранилище, миграции, интернирование строк и дедупликация — план 2 (`wakode-store`).
- HTTP-слой, авторизация, совместимые эндпоинты — план 3.
- Фронт и упаковка в бинарь — план 4.
- Правила, ручные записи и перекрытия — волна 1, отдельная спека.
- Калибровка `tail_padding` по живому аккаунту WakaTime — до начала работ над совместимым слоем в плане 3.
