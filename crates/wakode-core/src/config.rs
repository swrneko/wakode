use crate::Micros;

pub const DEFAULT_TIMEOUT_SECS: i64 = 900;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigError {
    NonPositiveTimeout,
    NegativePadding,
    PaddingExceedsTimeout,
}

/// Параметры склейки отметок в интервалы.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
    fn accepts_valid_configuration() {
        let cfg = DurationConfig::new(Micros::from_secs(300), Micros::from_secs(30)).unwrap();
        assert_eq!(cfg.timeout(), Micros::from_secs(300));
        assert_eq!(cfg.tail_padding(), Micros::from_secs(30));
    }
}
