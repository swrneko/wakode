//! Слой хранения wakode: SQLite за репозиторным трейтом.

#[allow(dead_code)]
pub(crate) mod clock;
pub mod codec;
pub mod conn;
pub mod dedup;
pub mod error;
pub mod interner;
pub mod migrate;
pub mod schema;

pub use conn::{open, open_in_memory};
pub use error::{StoreError, StoreResult};
pub use interner::Interner;
pub use migrate::{migrate, schema_version};
