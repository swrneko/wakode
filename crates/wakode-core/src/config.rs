use std::fmt;

use crate::Micros;

/// Таймаут по умолчанию в секундах — тот же, что у WakaTime.
///
/// Значение публично не только ради `Default`: совместимый эндпоинт
/// `all_time_since_today` обязан вернуть поле `timeout`, и брать его неоткуда,
/// кроме как отсюда.
pub const DEFAULT_TIMEOUT_SECS: i64 = 900;

/// Чем плоха отвергнутая конфигурация.
///
/// Значения приходят из пользовательского TOML, поэтому у ошибки есть `Display`
/// с человеческим текстом и `std::error::Error` — слой загрузки поднимает её
/// через `?` и печатает как есть.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigError {
    NonPositiveTimeout,
    NegativePadding,
    PaddingExceedsTimeout,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            ConfigError::NonPositiveTimeout => "таймаут должен быть положительным",
            ConfigError::NegativePadding => "хвостовая добавка не может быть отрицательной",
            ConfigError::PaddingExceedsTimeout => "хвостовая добавка не может превышать таймаут",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ConfigError {}

/// Параметры склейки отметок в интервалы.
///
/// # Почему тип сериализуется, но не десериализуется
///
/// `Serialize` нужен совместимому эндпоинту `all_time_since_today`, который
/// обязан вернуть поле `timeout`. Обратной реализации у типа нет намеренно, и
/// добавлять её **нельзя**: производный `Deserialize` собирает структуру
/// поле за полем, минуя [`DurationConfig::new`], — то есть молча обходит
/// проверку `tail_padding <= timeout`. Инвариант перестал бы существовать
/// ровно там, где значения приходят снаружи и доверять им нельзя больше всего.
///
/// Конфигурацию из TOML или JSON следует читать в свой тип-сырец и пропускать
/// через `new()`, а ошибку показывать пользователю: у [`ConfigError`] для этого
/// есть `Display`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[derive(serde::Serialize)]
pub struct DurationConfig {
    timeout: Micros,
    tail_padding: Micros,
}

impl DurationConfig {
    /// Инвариант `tail_padding <= timeout` держит интервалы непересекающимися:
    /// хвост последней отметки сессии не может дотянуться до следующей сессии,
    /// потому что та начинается позже, чем через `timeout`.
    pub fn new(timeout: Micros, tail_padding: Micros) -> Result<Self, ConfigError> {
        if timeout.get() <= 0 {
            return Err(ConfigError::NonPositiveTimeout);
        }
        if tail_padding.get() < 0 {
            return Err(ConfigError::NegativePadding);
        }
        if tail_padding > timeout {
            return Err(ConfigError::PaddingExceedsTimeout);
        }
        Ok(Self { timeout, tail_padding })
    }

    pub fn timeout(self) -> Micros {
        self.timeout
    }

    pub fn tail_padding(self) -> Micros {
        self.tail_padding
    }
}

impl Default for DurationConfig {
    fn default() -> Self {
        Self {
            timeout: Micros::from_secs(DEFAULT_TIMEOUT_SECS),
            tail_padding: Micros::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timeout_matches_wakatime() {
        assert_eq!(DurationConfig::default().timeout(), Micros::from_secs(900));
    }

    #[test]
    fn default_tail_padding_is_zero() {
        // Настоящее значение WakaTime неизвестно и калибруется отдельно;
        // до тех пор ноль — единственный честный вариант.
        assert_eq!(DurationConfig::default().tail_padding(), Micros::ZERO);
    }

    #[test]
    fn rejects_padding_larger_than_timeout() {
        // Иначе хвост одной сессии наедет на начало следующей.
        let err = DurationConfig::new(Micros::from_secs(60), Micros::from_secs(61));
        assert_eq!(err, Err(ConfigError::PaddingExceedsTimeout));
    }

    #[test]
    fn rejects_non_positive_timeout() {
        assert_eq!(
            DurationConfig::new(Micros::ZERO, Micros::ZERO),
            Err(ConfigError::NonPositiveTimeout)
        );
    }

    #[test]
    fn rejects_negative_padding() {
        assert_eq!(
            DurationConfig::new(Micros::from_secs(60), Micros::new(-1)),
            Err(ConfigError::NegativePadding)
        );
    }

    #[test]
    fn accepts_padding_equal_to_timeout() {
        // Граница инварианта tail_padding <= timeout: равенство допустимо,
        // строго больше — нет (см. rejects_padding_larger_than_timeout).
        let cfg = DurationConfig::new(Micros::from_secs(60), Micros::from_secs(60)).unwrap();
        assert_eq!(cfg.tail_padding(), cfg.timeout());
    }

    #[test]
    fn every_error_explains_itself_to_the_user() {
        // Конфигурацию читает человек из TOML, и «NonPositiveTimeout» в консоли
        // ему не поможет. Display нужен не для красоты, а чтобы слой загрузки
        // мог поднять ошибку через `?` и напечатать её как есть.
        assert_eq!(
            ConfigError::NonPositiveTimeout.to_string(),
            "таймаут должен быть положительным"
        );
        assert_eq!(
            ConfigError::NegativePadding.to_string(),
            "хвостовая добавка не может быть отрицательной"
        );
        assert_eq!(
            ConfigError::PaddingExceedsTimeout.to_string(),
            "хвостовая добавка не может превышать таймаут"
        );
    }

    #[test]
    fn duration_config_serializes_for_the_compat_payload() {
        // `all_time_since_today` обязан вернуть поле `timeout`. Обратной
        // операции у типа сознательно нет — см. документацию DurationConfig.
        let json = serde_json::to_value(DurationConfig::default()).unwrap();

        assert_eq!(json, serde_json::json!({ "timeout": 900_000_000i64, "tail_padding": 0 }));
    }

    #[test]
    fn config_error_is_a_std_error() {
        // Признак пригодности для `?` в функции, возвращающей Box<dyn Error>.
        let boxed: Box<dyn std::error::Error> = Box::new(ConfigError::PaddingExceedsTimeout);
        assert_eq!(boxed.to_string(), "хвостовая добавка не может превышать таймаут");
    }

    #[test]
    fn accepts_valid_configuration() {
        let cfg = DurationConfig::new(Micros::from_secs(300), Micros::from_secs(30)).unwrap();
        assert_eq!(cfg.timeout(), Micros::from_secs(300));
        assert_eq!(cfg.tail_padding(), Micros::from_secs(30));
    }
}
