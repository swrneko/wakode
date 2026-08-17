//! Криптография wakode: чистые функции над байтами.
//!
//! Крейт не обращается ни к базе, ни к сети, ни к файлам, ни к часам.
//! Граница держится списком зависимостей: если здесь появится `rusqlite`
//! или `axum`, значит криптография перестала быть отдельной и проверять
//! её изоляцию станет нечем.

pub mod api_key;
pub mod error;
pub mod master_key;
pub mod password;
pub mod session;

pub use api_key::{ApiKeyValue, EncryptedKey};
pub use error::{AuthError, AuthResult};
pub use master_key::MasterKey;
pub use password::{hash_password, verify_password};
pub use session::SessionToken;

/// Заглушка вместо секрета в `Debug`.
pub(crate) const REDACTED: &str = "<скрыт>";
