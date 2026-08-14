use serde::{Deserialize, Serialize};

/// Момент времени в микросекундах от Unix epoch, всегда UTC.
///
/// Внутри крейта время никогда не представляется числами с плавающей точкой:
/// они появляются только на границе HTTP-слоя, где так велит протокол WakaTime.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[derive(Serialize, Deserialize)]
pub struct Micros(i64);

impl Micros {
    pub const ZERO: Micros = Micros(0);

    pub const fn new(micros: i64) -> Self {
        Micros(micros)
    }

    pub const fn from_secs(secs: i64) -> Self {
        Micros(secs.saturating_mul(1_000_000))
    }

    /// Преобразование из float-секунд, как их присылает wakatime-cli.
    pub fn from_secs_f64(secs: f64) -> Self {
        Micros((secs * 1_000_000.0).round() as i64)
    }

    pub const fn get(self) -> i64 {
        self.0
    }

    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }

    pub const fn saturating_add(self, other: Micros) -> Self {
        Micros(self.0.saturating_add(other.0))
    }

    pub const fn saturating_sub(self, other: Micros) -> Self {
        Micros(self.0.saturating_sub(other.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_secs_converts_to_microseconds() {
        assert_eq!(Micros::from_secs(90).get(), 90_000_000);
    }

    #[test]
    fn from_secs_f64_rounds_to_nearest_microsecond() {
        // WakaTime присылает время как float-секунды; округляем, а не отбрасываем
        assert_eq!(Micros::from_secs_f64(1.0000005).get(), 1_000_001);
        assert_eq!(Micros::from_secs_f64(1.0000004).get(), 1_000_000);
    }

    #[test]
    fn saturating_sub_does_not_overflow() {
        let a = Micros::new(i64::MIN);
        assert_eq!(a.saturating_sub(Micros::from_secs(1)).get(), i64::MIN);
    }
}
