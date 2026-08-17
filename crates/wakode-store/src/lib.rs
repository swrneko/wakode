//! Слой хранения wakode: SQLite за репозиторным трейтом.

/// Заглушка вместо секрета в `Debug`.
///
/// Крейт намеренно не знает про криптографию и потому не может отличить
/// хеш пароля от логина сам. Поля, которые заведомо секретны, печатаются
/// этой заглушкой — одной на весь крейт, чтобы вывод был узнаваем и его
/// можно было искать по логам.
pub(crate) const REDACTED: &str = "<скрыт>";

pub(crate) mod clock;
pub mod codec;
pub mod conn;
pub mod dedup;
pub mod dirty;
pub mod error;
pub mod heartbeats;
pub mod interner;
pub mod keys;
pub mod migrate;
pub mod repo;
pub mod schema;
pub mod sessions;
pub mod users;
pub mod writer;

pub use conn::{open, open_in_memory};
pub use dirty::dirty_days_for;
pub use error::{StoreError, StoreResult};
pub use heartbeats::{insert_heartbeats, load_heartbeats, IncomingHeartbeat, InsertReport, Outcome};
pub use interner::Interner;
pub use keys::{find_key_by_lookup, insert_api_key, revoke_key, touch_key_used, ApiKey, NewApiKey};
pub use migrate::{migrate, schema_version};
pub use repo::{HeartbeatRepo, KeyRepo, SessionRepo, SqliteStore, UserRepo};
pub use sessions::{find_session_by_token_hash, insert_session, revoke_session, NewSession, Session};
pub use users::{find_user_by_id, find_user_by_login, insert_user, NewUser, User};
pub use writer::{spawn_writer, WriteHandle};
