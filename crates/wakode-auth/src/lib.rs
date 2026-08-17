//! Криптография wakode: чистые функции над байтами.
//!
//! Крейт не обращается ни к базе, ни к сети, ни к файлам, ни к часам.
//! Граница держится списком зависимостей: если здесь появится `rusqlite`
//! или `axum`, значит криптография перестала быть отдельной и проверять
//! её изоляцию станет нечем.

pub mod error;
pub mod master_key;

pub use error::{AuthError, AuthResult};
pub use master_key::MasterKey;

/// Заглушка вместо секрета в `Debug`.
pub(crate) const REDACTED: &str = "<скрыт>";
