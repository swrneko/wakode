use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("файл конфигурации не найден: {path}")]
    Missing { path: PathBuf },

    #[error("не удалось прочитать {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("не удалось разобрать {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("переменная {name} содержит {value:?}, а ожидалось число")]
    NotANumber { name: &'static str, value: String },

    #[error("переменная {name} содержит {value:?}, а ожидалось true или false")]
    NotABool { name: &'static str, value: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    pub durations: DurationsConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub listen: String,
    pub public_url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub path: PathBuf,
    pub write_queue: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    pub registration: bool,
    pub session_ttl_days: i64,
    pub setup_from_any_address: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DurationsConfig {
    pub timeout_secs: i64,
    /// Добавка последней отметке сессии.
    ///
    /// Ноль до калибровки по живому аккаунту WakaTime: величина нигде не
    /// задокументирована. Пока здесь ноль, wakode недосчитывает хвост
    /// каждой сессии — работать это не мешает, совпасть цифрам мешает.
    pub tail_padding_secs: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            database: DatabaseConfig::default(),
            auth: AuthConfig::default(),
            durations: DurationsConfig::default(),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            // Петлевой адрес: инстанс, выставленный наружу, должен быть
            // выставлен осознанно, а не по умолчанию.
            listen: "127.0.0.1:9000".to_owned(),
            public_url: "http://localhost:9000".to_owned(),
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("./wakode.db"),
            write_queue: 256,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            registration: false,
            session_ttl_days: 30,
            setup_from_any_address: false,
        }
    }
}

impl Default for DurationsConfig {
    fn default() -> Self {
        Self {
            timeout_secs: wakode_core::DEFAULT_TIMEOUT_SECS,
            tail_padding_secs: 0,
        }
    }
}

pub const DEFAULT_CONFIG_PATH: &str = "./wakode.toml";

impl Config {
    /// Прочитать конфигурацию.
    ///
    /// `explicit` — путь, названный флагом `--config`. Если он задан, а
    /// файла нет, это ошибка: пользователь сказал, где смотреть. Если не
    /// задан и умолчания на диске нет — берутся значения по умолчанию плюс
    /// окружение, что и нужно в контейнере.
    ///
    /// Пока не вызывается из `main`: последовательность старта — задача 7.
    #[allow(dead_code)]
    pub fn load(explicit: Option<&Path>) -> Result<Self, ConfigError> {
        let default_path = PathBuf::from(DEFAULT_CONFIG_PATH);
        Self::load_from(explicit, &default_path, |name| std::env::var(name).ok())
    }

    /// То же, но с явной читалкой окружения — так тестируется без
    /// `std::env::set_var`, который в параллельных тестах течёт между ними.
    pub fn load_from(
        explicit: Option<&Path>,
        default_path: &Path,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let path = explicit.unwrap_or(default_path);

        let mut config = if path.exists() {
            let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
                path: path.to_path_buf(),
                source,
            })?;
            toml::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            })?
        } else if explicit.is_some() {
            return Err(ConfigError::Missing {
                path: path.to_path_buf(),
            });
        } else {
            Config::default()
        };

        config.apply_env(&env)?;
        Ok(config)
    }

    fn apply_env(&mut self, env: &impl Fn(&str) -> Option<String>) -> Result<(), ConfigError> {
        if let Some(value) = env("WAKODE_SERVER_LISTEN") {
            self.server.listen = value;
        }
        if let Some(value) = env("WAKODE_SERVER_PUBLIC_URL") {
            self.server.public_url = value;
        }
        if let Some(value) = env("WAKODE_DATABASE_PATH") {
            self.database.path = PathBuf::from(value);
        }
        if let Some(value) = env("WAKODE_DATABASE_WRITE_QUEUE") {
            self.database.write_queue = parse_number("WAKODE_DATABASE_WRITE_QUEUE", &value)? as usize;
        }
        if let Some(value) = env("WAKODE_AUTH_REGISTRATION") {
            self.auth.registration = parse_bool("WAKODE_AUTH_REGISTRATION", &value)?;
        }
        if let Some(value) = env("WAKODE_AUTH_SESSION_TTL_DAYS") {
            self.auth.session_ttl_days = parse_number("WAKODE_AUTH_SESSION_TTL_DAYS", &value)?;
        }
        if let Some(value) = env("WAKODE_AUTH_SETUP_FROM_ANY_ADDRESS") {
            self.auth.setup_from_any_address =
                parse_bool("WAKODE_AUTH_SETUP_FROM_ANY_ADDRESS", &value)?;
        }
        if let Some(value) = env("WAKODE_DURATIONS_TIMEOUT_SECS") {
            self.durations.timeout_secs = parse_number("WAKODE_DURATIONS_TIMEOUT_SECS", &value)?;
        }
        if let Some(value) = env("WAKODE_DURATIONS_TAIL_PADDING_SECS") {
            self.durations.tail_padding_secs =
                parse_number("WAKODE_DURATIONS_TAIL_PADDING_SECS", &value)?;
        }
        Ok(())
    }
}

fn parse_number(name: &'static str, value: &str) -> Result<i64, ConfigError> {
    value.trim().parse().map_err(|_| ConfigError::NotANumber {
        name,
        value: value.to_owned(),
    })
}

fn parse_bool(name: &'static str, value: &str) -> Result<bool, ConfigError> {
    match value.trim() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        other => Err(ConfigError::NotABool {
            name,
            value: other.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_config(dir: &tempfile::TempDir, body: &str) -> std::path::PathBuf {
        let path = dir.path().join("wakode.toml");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn defaults_apply_when_the_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("нет-такого.toml");

        // Путь не задан вовсе — умолчания, без ошибки.
        let config = Config::load_from(None, &missing, |_| None).unwrap();

        assert_eq!(config.server.listen, "127.0.0.1:9000");
        assert_eq!(config.database.write_queue, 256);
        assert!(!config.auth.registration);
        assert!(!config.auth.setup_from_any_address);
        assert_eq!(config.durations.timeout_secs, 900);
        assert_eq!(config.durations.tail_padding_secs, 0);
    }

    #[test]
    fn an_explicitly_named_file_that_is_missing_is_an_error() {
        // Пользователь сказал, где смотреть. Промолчать здесь значило бы
        // поднять сервер не с той конфигурацией.
        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("нет-такого.toml");

        let err = Config::load_from(Some(&named), &named, |_| None).unwrap_err();
        assert!(matches!(err, ConfigError::Missing { .. }));
        assert!(format!("{err}").contains("нет-такого.toml"));
    }

    #[test]
    fn the_file_overrides_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            r#"
[server]
listen = "0.0.0.0:8080"

[durations]
timeout_secs = 1800
"#,
        );

        let config = Config::load_from(Some(&path), &path, |_| None).unwrap();

        assert_eq!(config.server.listen, "0.0.0.0:8080");
        assert_eq!(config.durations.timeout_secs, 1800);
        // Не упомянутые в файле поля остаются умолчаниями.
        assert_eq!(config.database.write_queue, 256);
    }

    #[test]
    fn the_environment_overrides_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "[server]\nlisten = \"0.0.0.0:8080\"\n");

        let config = Config::load_from(Some(&path), &path, |name| match name {
            "WAKODE_SERVER_LISTEN" => Some("127.0.0.1:1234".to_owned()),
            _ => None,
        })
        .unwrap();

        assert_eq!(config.server.listen, "127.0.0.1:1234");
    }

    #[test]
    fn a_numeric_field_from_the_environment_is_parsed_not_pasted() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "");

        let config = Config::load_from(Some(&path), &path, |name| match name {
            "WAKODE_DURATIONS_TIMEOUT_SECS" => Some("1200".to_owned()),
            _ => None,
        })
        .unwrap();

        assert_eq!(config.durations.timeout_secs, 1200);
    }

    #[test]
    fn a_broken_numeric_value_names_the_variable() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "");

        let err = Config::load_from(Some(&path), &path, |name| match name {
            "WAKODE_DURATIONS_TIMEOUT_SECS" => Some("много".to_owned()),
            _ => None,
        })
        .unwrap_err();

        assert!(format!("{err}").contains("WAKODE_DURATIONS_TIMEOUT_SECS"));
    }

    #[test]
    fn broken_toml_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "[server\nlisten =");

        let err = Config::load_from(Some(&path), &path, |_| None).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
        assert!(format!("{err}").contains("wakode.toml"));
    }

    #[test]
    fn the_config_carries_no_secrets() {
        // Мастер-ключ в конфиге отсутствует по построению: он живёт только
        // в `WAKODE_MASTER_KEY`. Файл с ключом рядом с базой означал бы,
        // что украденный бэкап содержит и шифротекст, и ключ к нему.
        //
        // Сторожит это сверка полного состава полей, а не поиск подстроки
        // «master». Поиск подстроки не поймал бы поле с любым другим
        // именем — а `Config` печатается производным `Debug`, и первая же
        // строка старта отправила бы такое поле в лог. Сверка состава
        // краснеет на **любом** новом поле и заставляет автора решить,
        // секрет это или нет. Если поле не секрет — просто допиши его сюда.
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "");
        let config = Config::load_from(Some(&path), &path, |_| None).unwrap();

        assert_eq!(
            format!("{config:?}"),
            "Config { \
             server: ServerConfig { listen: \"127.0.0.1:9000\", \
             public_url: \"http://localhost:9000\" }, \
             database: DatabaseConfig { path: \"./wakode.db\", write_queue: 256 }, \
             auth: AuthConfig { registration: false, session_ttl_days: 30, \
             setup_from_any_address: false }, \
             durations: DurationsConfig { timeout_secs: 900, tail_padding_secs: 0 } }"
        );
    }
}
