use wakode_auth::MasterKey;
use wakode_store::SqliteStore;

/// Состояние приложения.
///
/// Держит четыре поля конфигурации, а не весь `Config`: слою HTTP не нужны
/// ни адрес прослушивания, ни путь к базе, а узкое состояние — ещё и
/// защита от того, что в конфиг со временем приедет что-то чувствительное.
#[derive(Clone)]
pub struct AppState {
    pub store: SqliteStore,
    pub master_key: Option<MasterKey>,
    pub registration: bool,
    pub session_ttl_days: i64,
    pub setup_from_any_address: bool,
}

impl AppState {
    pub fn new(
        store: SqliteStore,
        master_key: Option<MasterKey>,
        registration: bool,
        session_ttl_days: i64,
        setup_from_any_address: bool,
    ) -> Self {
        Self {
            store,
            master_key,
            registration,
            session_ttl_days,
            setup_from_any_address,
        }
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
            .finish()
    }
}
