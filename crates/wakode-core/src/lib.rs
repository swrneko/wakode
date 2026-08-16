//! Чистое ядро wakode: поток отметок → интервалы → локальные дни → сводки.
//!
//! Крейт не ходит ни в базу, ни в сеть, ни в файловую систему — только чистые
//! функции над срезами структур. Строки он тоже не видит: проект, язык и
//! редактор приезжают сюда номерами [`Sid`], потому что их разрешением занят
//! слой хранения. Время внутри — всегда [`Micros`], микросекунды от эпохи UTC;
//! арифметика насыщающая.
//!
//! # Конвейер
//!
//! Отдельные функции задокументированы каждая сама по себе, но пользоваться
//! ими поодиночке почти никогда не нужно. Порядок такой:
//!
//! 1. [`heartbeat_window`] — за какой отрезок поднимать отметки из хранилища.
//!    Шире запрошенных суток на таймаут в обе стороны; без этого запаса
//!    сессия через полночь теряется молча.
//! 2. [`build_intervals`] — склейка отметок в непересекающиеся интервалы:
//!    пауза длиннее таймаута обрывает сессию, последняя отметка получает
//!    хвостовую добавку. Вход сортируется сам, дубликаты безвредны.
//! 3. [`split_by_local_day`] — нарезка интервалов по границам локальных суток
//!    пользователя. Ключ карты — авторитетный день; тот же день для отдельного
//!    момента даёт [`local_day_of`].
//! 4. [`aggregate_by`] / [`grand_total`] / [`percent`] — суммы по любому
//!    измерению и доли от целого.
//!
//! Инвариант, ради которого всё это разделено именно так: сумма по дням равна
//! сумме за период, а сумма по бакетам — общему итогу.
//!
//! ```
//! use chrono_tz::Tz;
//! use wakode_core::*;
//!
//! let tz: Tz = "Europe/Moscow".parse().unwrap();
//! let cfg = DurationConfig::default();
//! let attrs = Attrs {
//!     entity: Sid(1),
//!     kind: EntityKind::File,
//!     category: Category::Coding,
//!     project: Some(Sid(42)),
//!     branch: None,
//!     language: None,
//!     editor: None,
//!     os: None,
//!     machine: None,
//! };
//!
//! // Две отметки с шагом в минуту — минута работы над проектом 42.
//! let heartbeats = [
//!     Heartbeat { time: Micros::from_secs(1_755_000_000), attrs },
//!     Heartbeat { time: Micros::from_secs(1_755_000_060), attrs },
//! ];
//!
//! let intervals = build_intervals(&heartbeats, cfg);
//! for (day, pieces) in split_by_local_day(&intervals, tz) {
//!     let by_project = aggregate_by(&pieces, |a| a.project);
//!     let total = grand_total(&pieces);
//!
//!     assert_eq!(total, Micros::from_secs(60));
//!     assert_eq!(by_project[0].key, Some(Sid(42)));
//!     assert_eq!(percent(by_project[0].total, total), 100.0);
//!     let _ = day;
//! }
//!
//! assert_eq!(DEFAULT_TIMEOUT_SECS, 900);
//! ```

pub mod aggregate;
pub mod calendar;
pub mod config;
pub mod domain;
pub mod intervals;
pub mod time;

pub use aggregate::{aggregate_by, grand_total, percent, Bucket};
pub use calendar::{
    heartbeat_window, local_date_of, local_day_bounds, local_day_of, split_by_local_day,
};
pub use config::{ConfigError, DurationConfig, DEFAULT_TIMEOUT_SECS};
pub use domain::{Attrs, Category, EntityKind, Heartbeat, Sid};
pub use intervals::{build_intervals, Interval};
pub use time::Micros;
