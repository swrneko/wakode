//! Слой хранения wakode: SQLite за репозиторным трейтом.

#[allow(dead_code)]
pub(crate) mod clock;
pub mod conn;
pub mod error;

pub use conn::{open, open_in_memory};
pub use error::{StoreError, StoreResult};
