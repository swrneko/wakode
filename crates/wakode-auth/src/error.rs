use thiserror::Error;

/// Ошибки криптографического слоя.
///
/// Ни один вариант не несёт секрета: сообщение об ошибке уезжает в лог и
/// в ответ клиенту, и приложить к нему ключ значило бы отдать его даром.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("мастер-ключ не является корректным base64")]
    MasterKeyEncoding,

    #[error("мастер-ключ должен быть длиной 32 байта, получено {got}")]
    MasterKeyLength { got: usize },

    #[error("не удалось зашифровать значение")]
    Encrypt,

    #[error("не удалось расшифровать значение: неверный мастер-ключ или повреждённые данные")]
    Decrypt,

    #[error("хеш пароля повреждён")]
    PasswordHashMalformed,

    #[error("не удалось посчитать хеш пароля")]
    PasswordHashFailed,
}

pub type AuthResult<T> = Result<T, AuthError>;
