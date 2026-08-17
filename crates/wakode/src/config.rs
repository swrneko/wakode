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
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    pub durations: DurationsConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: String,
    pub public_url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    pub path: PathBuf,
    pub write_queue: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    pub registration: bool,
    pub session_ttl_days: i64,
    pub setup_from_any_address: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
            // Путь приводится к абсолютному: пользователь запускает сервер
            // из-под systemd, где рабочий каталог не тот, что он думает, и
            // сообщение «файл не найден: wakode.toml» не говорит, где его
            // искали. Это и есть та неоднозначность, ради устранения
            // которой у явного пути вообще заведён отдельный отказ.
            return Err(ConfigError::Missing {
                path: std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()),
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
            // Разбор сразу в `usize`, а не `i64 as usize`: беззнаковый каст
            // превратил бы `-1` в `usize::MAX` молча, и очередь записи
            // завелась бы с абсурдной ёмкостью вместо внятного отказа.
            self.database.write_queue =
                parse_size("WAKODE_DATABASE_WRITE_QUEUE", &value)?;
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

fn parse_size(name: &'static str, value: &str) -> Result<usize, ConfigError> {
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
        // Значения заданы файлом, а не взяты из умолчаний: иначе правка
        // любого умолчания краснила бы сторожа секретов, автор получал бы
        // объяснение не про то, что он сделал, и рано или поздно перестал
        // бы его читать. Здесь у каждой красноты один адрес — состав.
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            r#"
[server]
listen = "l"
public_url = "u"

[database]
path = "d"
write_queue = 1

[auth]
registration = true
session_ttl_days = 2
setup_from_any_address = true

[durations]
timeout_secs = 3
tail_padding_secs = 4
"#,
        );
        let config = Config::load_from(Some(&path), &path, |_| None).unwrap();

        assert_eq!(
            format!("{config:?}"),
            "Config { \
             server: ServerConfig { listen: \"l\", public_url: \"u\" }, \
             database: DatabaseConfig { path: \"d\", write_queue: 1 }, \
             auth: AuthConfig { registration: true, session_ttl_days: 2, \
             setup_from_any_address: true }, \
             durations: DurationsConfig { timeout_secs: 3, tail_padding_secs: 4 } }"
        );
    }

    #[test]
    fn every_field_can_be_overridden_from_the_environment() {
        // Обещание «WAKODE_* перекрывают сверху» дано на все девять полей,
        // а доказывалось на два. Опечатка в имени переменной или молча
        // выпавшая ветка проявились бы не в тестах, а на живом инстансе:
        // WAKODE_AUTH_REGISTRATION=true не включил бы регистрацию, а
        // WAKODE_DATABASE_PATH не переехал бы на том.
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "");

        let config = Config::load_from(Some(&path), &path, |name| {
            Some(match name {
                "WAKODE_SERVER_LISTEN" => "0.0.0.0:1",
                "WAKODE_SERVER_PUBLIC_URL" => "https://пример.рф",
                "WAKODE_DATABASE_PATH" => "/данные/wakode.db",
                "WAKODE_DATABASE_WRITE_QUEUE" => "7",
                "WAKODE_AUTH_REGISTRATION" => "true",
                "WAKODE_AUTH_SESSION_TTL_DAYS" => "11",
                "WAKODE_AUTH_SETUP_FROM_ANY_ADDRESS" => "true",
                "WAKODE_DURATIONS_TIMEOUT_SECS" => "1200",
                "WAKODE_DURATIONS_TAIL_PADDING_SECS" => "13",
                other => panic!("незнакомое имя переменной: {other}"),
            }
            .to_owned())
        })
        .unwrap();

        assert_eq!(config.server.listen, "0.0.0.0:1");
        assert_eq!(config.server.public_url, "https://пример.рф");
        assert_eq!(config.database.path, PathBuf::from("/данные/wakode.db"));
        assert_eq!(config.database.write_queue, 7);
        assert!(config.auth.registration);
        assert_eq!(config.auth.session_ttl_days, 11);
        assert!(config.auth.setup_from_any_address);
        assert_eq!(config.durations.timeout_secs, 1200);
        assert_eq!(config.durations.tail_padding_secs, 13);
    }

    #[test]
    fn an_unknown_key_in_the_file_is_an_error() {
        // Опечатка в имени поля или секции иначе даёт запуск с умолчаниями
        // и ни слова в лог. Это хуже отсутствующего файла: там хотя бы есть
        // отказ. Ровно та же болезнь, ради которой у явного `--config`
        // заведён свой отказ.
        let dir = tempfile::tempdir().unwrap();

        let typo_in_field = write_config(&dir, "[server]\nlisen = \"0.0.0.0:80\"\n");
        assert!(matches!(
            Config::load_from(Some(&typo_in_field), &typo_in_field, |_| None),
            Err(ConfigError::Parse { .. })
        ));

        let typo_in_section = write_config(&dir, "[servr]\nlisten = \"0.0.0.0:80\"\n");
        assert!(matches!(
            Config::load_from(Some(&typo_in_section), &typo_in_section, |_| None),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn a_negative_queue_size_is_an_error_not_a_huge_number() {
        // `i64 as usize` превратил бы `-1` в `usize::MAX` молча, и очередь
        // записи завелась бы с абсурдной ёмкостью вместо отказа.
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "");

        let err = Config::load_from(Some(&path), &path, |name| {
            (name == "WAKODE_DATABASE_WRITE_QUEUE").then(|| "-1".to_owned())
        })
        .unwrap_err();

        assert!(matches!(err, ConfigError::NotANumber { .. }));
        assert!(format!("{err}").contains("WAKODE_DATABASE_WRITE_QUEUE"));
    }

    #[test]
    fn a_relative_missing_path_is_reported_absolutely() {
        // Под systemd рабочий каталог не тот, что думает админ, и сообщение
        // «файл не найден: wakode.toml» не говорит, где его искали.
        let relative = Path::new("нет-такого-конфига.toml");
        let err = Config::load_from(Some(relative), relative, |_| None).unwrap_err();

        let text = format!("{err}");
        assert!(
            text.contains(std::path::MAIN_SEPARATOR),
            "путь в сообщении не абсолютный: {text}"
        );
    }
}
