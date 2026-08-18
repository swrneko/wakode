use thiserror::Error;
use wakode_auth::{ApiKeyValue, AuthError, EncryptedKey, MasterKey};
use wakode_store::{KeyRepo, SqliteStore, StoreError};

use crate::config::Config;

#[derive(Debug, Error)]
pub enum StartupError {
    #[error("переменная WAKODE_MASTER_KEY задана неверно: {0}")]
    MasterKeyInvalid(#[source] AuthError),

    #[error(
        "в базе есть выданные API-ключи, но WAKODE_MASTER_KEY не задана. \
         Без мастер-ключа расшифровать их нельзя, и сервер отвечал бы отказом \
         на каждый ключ — это выглядело бы как поломка хранилища"
    )]
    MasterKeyMissing,

    #[error(
        "WAKODE_MASTER_KEY не подходит к ключам в базе: пробный ключ не \
         расшифровался. Скорее всего задан мастер-ключ от другого инстанса"
    )]
    MasterKeyMismatch,

    #[error("ёмкость очереди записи должна быть больше нуля")]
    WriteQueueZero,

    #[error("хранилище: {0}")]
    Store(#[from] StoreError),
}

/// Поднятое состояние процесса.
#[derive(Debug)]
pub struct Startup {
    /// `expect`, а не `allow`: как только подкоманда `serve` (задача 14)
    /// начнёт поднимать на этом хранилище HTTP-слой, компилятор сообщит,
    /// что ожидание не оправдалось, и атрибут придётся снять. `allow`
    /// остался бы навсегда и глушил бы `dead_code` для всего, до чего
    /// дотянется.
    ///
    /// `cfg_attr(not(test))` — потому что собственные тесты этого модуля
    /// поле уже читают, и в тестовой сборке ожидание не оправдывается
    /// прямо сейчас. Без оговорки `cargo test` выдавал бы предупреждение
    /// «lint expectation is unfulfilled», то есть требование снять
    /// атрибут, снимать который ещё нельзя.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "HTTP-слой поднимает подкоманда serve, задача 14")
    )]
    pub store: SqliteStore,
    pub master_key: Option<MasterKey>,
    pub config: Config,
}

/// Шесть шагов старта, они же порядок отказов.
///
/// Конфиг к этому моменту уже прочитан — это шаг 1, он живёт в `config.rs`,
/// потому что нужен и подкомандам CLI, которым сервер поднимать незачем.
pub async fn start(config: Config, master_key_raw: Option<String>) -> Result<Startup, StartupError> {
    // Шаг 2: мастер-ключ. Его отсутствие пока не ошибка — решает шаг 4.
    let master_key = match master_key_raw {
        Some(raw) => Some(MasterKey::from_base64(&raw).map_err(StartupError::MasterKeyInvalid)?),
        None => None,
    };

    // Своя ошибка вместо паники изнутри tokio: ноль в конфиге —
    // правдоподобная опечатка, и сообщение про mpsc её не объяснит.
    if config.database.write_queue == 0 {
        return Err(StartupError::WriteQueueZero);
    }

    // Шаг 3: база и миграции. `SqliteStore::open` делает и то, и другое.
    let store = SqliteStore::open(&config.database.path, config.database.write_queue)?;

    // Шаги 4 и 5: сверка мастер-ключа с тем, что уже лежит в базе.
    if let Some(key) = store.first_key().await? {
        match &master_key {
            None => return Err(StartupError::MasterKeyMissing),
            Some(master) => {
                let encrypted = EncryptedKey::from_bytes(key.key_encrypted);
                ApiKeyValue::decrypt(&encrypted, master)
                    .map_err(|_| StartupError::MasterKeyMismatch)?;
            }
        }
    }

    // Шаг 6 — поднятие сервера — делает вызывающий: подкомандам CLI сервер
    // не нужен, а последовательность проверок нужна им ровно та же.
    Ok(Startup {
        store,
        master_key,
        config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wakode_store::{KeyRepo, NewApiKey, UserRepo};

    fn a_config(dir: &tempfile::TempDir) -> Config {
        let mut config = Config::default();
        config.database.path = dir.path().join("wakode.db");
        config
    }

    async fn a_user(store: &SqliteStore) -> uuid::Uuid {
        store
            .create_user(wakode_store::NewUser {
                login: "swrneko".to_owned(),
                email: None,
                password_hash: "непрозрачно".to_owned(),
                display_name: None,
                timezone: "Europe/Moscow".parse().unwrap(),
                timeout_secs: 900,
                is_admin: true,
            })
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn an_empty_database_starts_without_a_master_key() {
        // Инстанс, где ключей ещё не выдавали, обязан подниматься: иначе
        // первый запуск требовал бы ключа, которым нечего шифровать.
        let dir = tempfile::tempdir().unwrap();
        let started = start(a_config(&dir), None).await.unwrap();

        assert!(started.master_key.is_none());
        assert_eq!(started.store.user_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn migrations_are_applied_on_start() {
        let dir = tempfile::tempdir().unwrap();
        let config = a_config(&dir);
        let path = config.database.path.clone();

        start(config, None).await.unwrap();

        let conn = wakode_store::open(&path).unwrap();
        assert_eq!(wakode_store::schema_version(&conn).unwrap(), 1);
    }

    #[tokio::test]
    async fn keys_without_a_master_key_refuse_to_start() {
        // Шаг 4. Без него сервер поднялся бы и отвечал 401 на все ключи, а
        // владелец искал бы поломку в хранилище.
        let dir = tempfile::tempdir().unwrap();
        let master = MasterKey::generate();

        {
            let started = start(a_config(&dir), Some(master.to_base64())).await.unwrap();
            let user = a_user(&started.store).await;
            let value = ApiKeyValue::generate();
            started
                .store
                .create_key(NewApiKey {
                    user_id: user,
                    name: "ключ".to_owned(),
                    key_encrypted: value.encrypt(&master).unwrap().as_bytes().to_vec(),
                    key_lookup: value.lookup(&master),
                })
                .await
                .unwrap();
        }

        let err = start(a_config(&dir), None).await.unwrap_err();
        assert!(matches!(err, StartupError::MasterKeyMissing));
    }

    #[tokio::test]
    async fn a_different_master_key_refuses_to_start() {
        // Шаг 5. Подменённый ключ иначе даёт ту же тихую поломку, только
        // конфигурация при этом выглядит правильной.
        let dir = tempfile::tempdir().unwrap();
        let master = MasterKey::generate();

        {
            let started = start(a_config(&dir), Some(master.to_base64())).await.unwrap();
            let user = a_user(&started.store).await;
            let value = ApiKeyValue::generate();
            started
                .store
                .create_key(NewApiKey {
                    user_id: user,
                    name: "ключ".to_owned(),
                    key_encrypted: value.encrypt(&master).unwrap().as_bytes().to_vec(),
                    key_lookup: value.lookup(&master),
                })
                .await
                .unwrap();
        }

        let other = MasterKey::generate();
        let err = start(a_config(&dir), Some(other.to_base64()))
            .await
            .unwrap_err();
        assert!(matches!(err, StartupError::MasterKeyMismatch));
    }

    #[tokio::test]
    async fn the_right_master_key_starts_fine() {
        // Зеркало предыдущего: без него «отказ всегда» прошёл бы обе
        // проверки на отказ и выглядел бы правильным.
        let dir = tempfile::tempdir().unwrap();
        let master = MasterKey::generate();

        {
            let started = start(a_config(&dir), Some(master.to_base64())).await.unwrap();
            let user = a_user(&started.store).await;
            let value = ApiKeyValue::generate();
            started
                .store
                .create_key(NewApiKey {
                    user_id: user,
                    name: "ключ".to_owned(),
                    key_encrypted: value.encrypt(&master).unwrap().as_bytes().to_vec(),
                    key_lookup: value.lookup(&master),
                })
                .await
                .unwrap();
        }

        let started = start(a_config(&dir), Some(master.to_base64())).await.unwrap();

        // `master_key.is_some()` здесь было бы тавтологией: ключ только что
        // передали на вход. Доказательная сила теста в том, что `start`
        // выше не вернул ошибку, — то есть ключ из базы прочитан и открыт.
        // Проверяем это прямо: ключ на месте и открывается тем же
        // мастер-ключом, которым его записали.
        let stored = started.store.first_key().await.unwrap().unwrap();
        let opened = ApiKeyValue::decrypt(
            &EncryptedKey::from_bytes(stored.key_encrypted),
            started.master_key.as_ref().unwrap(),
        )
        .expect("ключ не открылся тем мастер-ключом, с которым старт прошёл");
        assert_eq!(opened.to_string().len(), 36);
    }

    #[tokio::test]
    async fn a_malformed_master_key_names_the_variable() {
        let dir = tempfile::tempdir().unwrap();
        let err = start(a_config(&dir), Some("не base64!".to_owned()))
            .await
            .unwrap_err();

        assert!(matches!(err, StartupError::MasterKeyInvalid(_)));
        assert!(format!("{err}").contains("WAKODE_MASTER_KEY"));
    }

    #[tokio::test]
    async fn a_zero_write_queue_is_refused_before_it_panics() {
        // `tokio::sync::mpsc::channel(0)` роняет процесс паникой изнутри
        // tokio, и сообщение будет про его внутренности. Ноль в конфиге —
        // правдоподобная опечатка, и отвечать на неё надо своей ошибкой.
        let dir = tempfile::tempdir().unwrap();
        let mut config = a_config(&dir);
        config.database.write_queue = 0;

        let err = start(config, None).await.unwrap_err();
        assert!(matches!(err, StartupError::WriteQueueZero));
    }
}
