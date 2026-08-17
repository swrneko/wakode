use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono_tz::Tz;
use uuid::Uuid;
use wakode_core::{Heartbeat, Micros};

use crate::error::StoreResult;
use crate::heartbeats::{IncomingHeartbeat, InsertReport};
use crate::interner::Interner;
use crate::keys::{ApiKey, NewApiKey};
use crate::sessions::{NewSession, Session};
use crate::users::{NewUser, User};
use crate::writer::{spawn_writer, WriteHandle};

/// Отметки: запись и чтение диапазона.
///
/// Трейт асинхронный, хотя SQLite синхронен: это цена обещания, что
/// Postgres добавляется реализацией трейта, а не переписыванием вызывающих.
pub trait HeartbeatRepo: Send + Sync {
    fn record_heartbeats(
        &self,
        user: Uuid,
        batch: Vec<IncomingHeartbeat>,
        tz: Tz,
    ) -> impl std::future::Future<Output = StoreResult<InsertReport>> + Send;

    fn heartbeats_in_range(
        &self,
        user: Uuid,
        from: Micros,
        to: Micros,
    ) -> impl std::future::Future<Output = StoreResult<Vec<Heartbeat>>> + Send;

    /// Развернуть номер строки обратно в текст. Словарь в памяти, поэтому
    /// метод синхронный — асинхронность тут была бы ложью.
    fn resolve(&self, sid: wakode_core::Sid) -> Option<Arc<str>>;
}

pub trait UserRepo: Send + Sync {
    fn create_user(&self, new: NewUser) -> impl std::future::Future<Output = StoreResult<User>> + Send;
    fn user_by_login(&self, login: &str) -> impl std::future::Future<Output = StoreResult<Option<User>>> + Send;
    fn user_by_id(&self, id: Uuid) -> impl std::future::Future<Output = StoreResult<Option<User>>> + Send;
}

pub trait KeyRepo: Send + Sync {
    fn create_key(&self, new: NewApiKey) -> impl std::future::Future<Output = StoreResult<ApiKey>> + Send;
    fn key_by_lookup(&self, lookup: Vec<u8>) -> impl std::future::Future<Output = StoreResult<Option<ApiKey>>> + Send;
    fn revoke_key(&self, id: Uuid) -> impl std::future::Future<Output = StoreResult<()>> + Send;
}

pub trait SessionRepo: Send + Sync {
    fn create_session(&self, new: NewSession) -> impl std::future::Future<Output = StoreResult<Session>> + Send;
    fn session_by_token_hash(&self, hash: Vec<u8>) -> impl std::future::Future<Output = StoreResult<Option<Session>>> + Send;
    fn revoke_session(&self, id: Uuid) -> impl std::future::Future<Output = StoreResult<()>> + Send;
}

/// Хранилище на SQLite.
///
/// Пишущая задача владеет своим соединением и держит поток отметок —
/// `record_heartbeats`. Всё остальное (пользователи, ключи, сессии, чтение
/// отметок) открывает собственное соединение через `on_own_connection` и
/// идёт мимо этой очереди: в WAL-режиме такие соединения не мешают ни
/// писателю, ни друг другу, а гонять их через ту же очередь было бы
/// искусственным узким местом.
#[derive(Debug, Clone)]
pub struct SqliteStore {
    path: PathBuf,
    writer: WriteHandle,
    interner: Arc<Interner>,
}

impl SqliteStore {
    /// Открыть (или создать) базу по пути и поднять пишущую задачу.
    ///
    /// `write_queue` — ёмкость канала пишущей задачи, обычно из конфига
    /// HTTP-слоя. `0` тут не запрещён проверкой, но и не безобиден:
    /// `spawn_writer` называет его правдоподобной опечаткой конфига, и канал
    /// нулевой ёмкости роняет процесс паникой изнутри `tokio::sync::mpsc`
    /// при первом же вызове.
    pub fn open(path: &Path, write_queue: usize) -> StoreResult<Self> {
        let mut conn = crate::open(path)?;
        crate::migrate(&mut conn)?;

        let interner = Arc::new(Interner::load(&conn)?);
        let writer = spawn_writer(conn, Arc::clone(&interner), write_queue);

        Ok(Self {
            path: path.to_path_buf(),
            writer,
            interner,
        })
    }

    /// Собственное соединение под одну операцию: SQLite открывает файл
    /// дёшево, а пул понадобится только если это измерят как узкое место.
    fn own_conn(&self) -> StoreResult<rusqlite::Connection> {
        crate::open(&self.path)
    }

    /// Консистентный снимок живой базы.
    ///
    /// Отказывает, если по пути `dest` уже есть файл (`VACUUM INTO` не
    /// перезаписывает существующий файл): вызывающий получит
    /// `StoreError::Sqlite` с «output file already exists», а не тихую
    /// перезапись — ротацию имени бэкапа решает вызывающий, не эта функция.
    pub async fn backup(&self, dest: &Path) -> StoreResult<()> {
        let path = self.path.clone();
        let dest = dest.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let conn = crate::open(&path)?;
            // `to_str`, а не `to_string_lossy`: путь с не-UTF-8 байтами (на
            // Linux имена файлов — произвольные байты) молча подменился бы
            // символом замены, и VACUUM INTO создал бы файл с ДРУГИМ именем,
            // отрапортовав Ok(()) — бэкап потерялся бы, не сказав об этом.
            let dest_str = dest
                .to_str()
                .ok_or_else(|| rusqlite::Error::InvalidPath(dest.clone()))?;
            // VACUUM INTO делает снимок без остановки записи и попутно
            // дефрагментирует файл — в отличие от копирования файла руками,
            // которое на живой базе даёт битую копию.
            conn.execute("VACUUM INTO ?1", [dest_str])?;
            Ok(())
        })
        .await
        .map_err(|_| crate::StoreError::TaskPanicked)?
    }
}

/// Выполнить блокирующую работу над собственным соединением.
///
/// Имя намеренно не `read_blocking`: через этот помощник идут и записи —
/// пользователи, ключи и сессии, — а не только чтения. Они минуют пишущую
/// задачу осознанно (см. шаг 5), и параллельные писатели здесь разводятся
/// не очередью, а самим SQLite: WAL плюс `busy_timeout` из `conn.rs`.
/// Своего retry поверх этого нет и быть не должно.
///
/// `JoinError` тут значит ровно одно: замыкание паникнуло (отменять эти
/// задачи некому). Поэтому `TaskPanicked`, а не `WriterGone` — пишущая
/// задача к этим операциям отношения не имеет, и путать два состояния
/// значит врать в логах.
async fn on_own_connection<T, F>(store: &SqliteStore, work: F) -> StoreResult<T>
where
    T: Send + 'static,
    F: FnOnce(rusqlite::Connection) -> StoreResult<T> + Send + 'static,
{
    let conn = store.own_conn()?;
    tokio::task::spawn_blocking(move || work(conn))
        .await
        .map_err(|_| crate::StoreError::TaskPanicked)?
}

impl HeartbeatRepo for SqliteStore {
    async fn record_heartbeats(
        &self,
        user: Uuid,
        batch: Vec<IncomingHeartbeat>,
        tz: Tz,
    ) -> StoreResult<InsertReport> {
        self.writer.insert_heartbeats(user, batch, tz).await
    }

    async fn heartbeats_in_range(
        &self,
        user: Uuid,
        from: Micros,
        to: Micros,
    ) -> StoreResult<Vec<Heartbeat>> {
        on_own_connection(self, move |conn| {
            crate::load_heartbeats(&conn, user, from, to)
        })
        .await
    }

    fn resolve(&self, sid: wakode_core::Sid) -> Option<Arc<str>> {
        self.interner.resolve(sid)
    }
}

impl UserRepo for SqliteStore {
    async fn create_user(&self, new: NewUser) -> StoreResult<User> {
        on_own_connection(self, move |conn| crate::insert_user(&conn, &new)).await
    }

    async fn user_by_login(&self, login: &str) -> StoreResult<Option<User>> {
        // Строка копируется: замыкание переезжает в другой поток и пережить
        // заимствование не может.
        let login = login.to_owned();
        on_own_connection(self, move |conn| crate::find_user_by_login(&conn, &login)).await
    }

    async fn user_by_id(&self, id: Uuid) -> StoreResult<Option<User>> {
        on_own_connection(self, move |conn| crate::find_user_by_id(&conn, id)).await
    }
}

impl KeyRepo for SqliteStore {
    async fn create_key(&self, new: NewApiKey) -> StoreResult<ApiKey> {
        on_own_connection(self, move |conn| crate::insert_api_key(&conn, &new)).await
    }

    async fn key_by_lookup(&self, lookup: Vec<u8>) -> StoreResult<Option<ApiKey>> {
        on_own_connection(self, move |conn| crate::find_key_by_lookup(&conn, &lookup)).await
    }

    async fn revoke_key(&self, id: Uuid) -> StoreResult<()> {
        on_own_connection(self, move |conn| crate::revoke_key(&conn, id)).await
    }
}

impl SessionRepo for SqliteStore {
    async fn create_session(&self, new: NewSession) -> StoreResult<Session> {
        on_own_connection(self, move |conn| crate::insert_session(&conn, &new)).await
    }

    async fn session_by_token_hash(&self, hash: Vec<u8>) -> StoreResult<Option<Session>> {
        on_own_connection(self, move |conn| {
            crate::find_session_by_token_hash(&conn, &hash)
        })
        .await
    }

    async fn revoke_session(&self, id: Uuid) -> StoreResult<()> {
        on_own_connection(self, move |conn| crate::revoke_session(&conn, id)).await
    }
}
