//! Слой хранения wakode: SQLite за репозиторным трейтом.

pub(crate) mod clock;
pub mod codec;
pub mod conn;
pub mod dedup;
pub mod dirty;
pub mod error;
pub mod heartbeats;
pub mod interner;
pub mod migrate;
pub mod schema;
pub mod users;

pub use conn::{open, open_in_memory};
pub use dirty::dirty_days_for;
pub use error::{StoreError, StoreResult};
pub use heartbeats::{insert_heartbeats, IncomingHeartbeat, InsertReport, Outcome};
pub use interner::Interner;
pub use migrate::{migrate, schema_version};
pub use users::{find_user_by_id, find_user_by_login, insert_user, NewUser, User};
