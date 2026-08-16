//! Слой хранения wakode: SQLite за репозиторным трейтом.

#[allow(dead_code)]
pub(crate) mod clock;
pub mod codec;
pub mod conn;
pub mod error;
pub mod migrate;
pub mod schema;

pub use conn::{open, open_in_memory};
pub use error::{StoreError, StoreResult};
pub use migrate::{migrate, schema_version};
