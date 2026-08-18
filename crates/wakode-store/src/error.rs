use thiserror::Error;

/// Ошибки слоя хранения.
///
/// Тип намеренно не пробрасывает `rusqlite::Error` наружу как есть в тех
/// случаях, когда у ошибки есть доменный смысл: вызывающий не должен
/// разбирать коды SQLite, чтобы понять, что очередь записи переполнена.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("база данных: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("схема базы новее, чем понимает эта сборка: в файле версия {found}, поддерживается до {supported}")]
    SchemaTooNew { found: i32, supported: i32 },

    #[error("очередь записи переполнена, повторите позже")]
    WriteQueueFull,

    #[error("пишущая задача остановлена")]
    WriterGone,

    #[error("значение не помещается в тип: {0}")]
    OutOfRange(&'static str),

    #[error("повреждённые данные в базе: {0}")]
    Corrupt(String),

    #[error("фоновая задача упала")]
    TaskPanicked,

    #[error("логин пуст")]
    LoginEmpty,

    #[error("пользователь {0} уже есть")]
    LoginTaken(String),
}

pub type StoreResult<T> = Result<T, StoreError>;
