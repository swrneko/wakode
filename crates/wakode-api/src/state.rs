use wakode_auth::{MasterKey, SetupToken};
use wakode_store::SqliteStore;

/// Состояние приложения.
///
/// Держит шесть полей конфигурации, а не весь `Config`: слою HTTP не нужны
/// ни адрес прослушивания, ни путь к базе, а узкое состояние — ещё и
/// защита от того, что в конфиг со временем приедет что-то чувствительное.
#[derive(Clone)]
pub struct AppState {
    pub store: SqliteStore,
    pub master_key: Option<MasterKey>,
    pub registration: bool,
    pub session_ttl_days: i64,
    pub setup_from_any_address: bool,
    pub default_timeout_secs: i64,
    /// Токен первичной настройки.
    ///
    /// Выдаётся вызывающим, только если на старте процесса пользователей
    /// не было; после появления администратора в этом же процессе значение
    /// само не очищается — эндпоинт закрывает не оно, а проверка
    /// `user_count() > 0` в `setup`. Само значение живёт до перезапуска.
    ///
    /// `None` — закрытая сторона: инстанс без токена настройку по токену
    /// не пускает вовсе. Поэтому поле и не входит в `AppSettings`:
    /// умолчание должно получаться само, а не задаваться каждым
    /// вызывающим.
    pub setup_token: Option<SetupToken>,
}

/// Настройки, которые HTTP-слой берёт из конфига.
///
/// Отдельной структурой, а не пятью позиционными аргументами `new`.
/// Причина конкретная: `registration` и `setup_from_any_address` — два
/// соседних `bool`, и перестановку их местами компилятор не поймает
/// никогда. Цена такой перестановки у владельца, включившего регистрацию:
/// экран первичной настройки открыт всему интернету, пока в базе нет
/// пользователей. Именной инициализации полей для этой ошибки нужно уже
/// написать неверное имя — а это видно и глазами, и на ревью.
///
/// Проводку из конфига делает `app_settings` в бинаре `wakode`, и там же
/// она покрыта тестом с несовпадающими значениями флагов.
#[derive(Debug, Clone, Copy)]
pub struct AppSettings {
    pub registration: bool,
    pub session_ttl_days: i64,
    pub setup_from_any_address: bool,
    /// Тайм-аут сессии кодирования для заводимых пользователей, в секундах.
    ///
    /// Берётся из `[durations]`, а не из `wakode_core::DEFAULT_TIMEOUT_SECS`.
    /// Пока константа была прошита в обеих дверях создания пользователя,
    /// секция конфига не читалась вообще: владелец писал
    /// `timeout_secs = 300`, перезапускал, заводил пользователя — и в базе
    /// оказывалось 900, без единого слова куда-либо. Исправить строку в
    /// 3a было нечем.
    pub default_timeout_secs: i64,
}

impl AppState {
    pub fn new(store: SqliteStore, master_key: Option<MasterKey>, settings: AppSettings) -> Self {
        Self {
            store,
            master_key,
            registration: settings.registration,
            session_ttl_days: settings.session_ttl_days,
            setup_from_any_address: settings.setup_from_any_address,
            default_timeout_secs: settings.default_timeout_secs,
            setup_token: None,
        }
    }

    /// Выдать состоянию токен первичной настройки.
    ///
    /// Отдельным методом, а не четвёртым аргументом `new`: токен есть
    /// ровно у одного вызывающего из дюжины — у `serve` в бинаре, — и
    /// умолчание `None` закрытое. Четвёртый аргумент заставил бы одиннадцать
    /// вызовов писать `None` и ничего бы этим не доказал.
    #[must_use]
    pub fn with_setup_token(mut self, token: Option<SetupToken>) -> Self {
        self.setup_token = token;
        self
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("store", &self.store)
            .field("master_key", &self.master_key.is_some())
            .field("registration", &self.registration)
            .field("session_ttl_days", &self.session_ttl_days)
            .field("setup_from_any_address", &self.setup_from_any_address)
            .field("default_timeout_secs", &self.default_timeout_secs)
            .field("setup_token", &self.setup_token.is_some())
            .finish()
    }
}
