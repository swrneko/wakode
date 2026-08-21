//! Эндпоинты, совместимые с WakaTime API.
//!
//! Форма ответов заморожена чужим протоколом: она не наша, менять её по
//! вкусу нельзя, и сверяется она с эталонами в `tests/fixtures/wakatime`
//! помощником из `tests/shape.rs`.

pub mod user;

pub use user::current;

/// Момент времени в том виде, в каком его отдаёт WakaTime.
///
/// Формат сверен с эталоном, а не взят из привычки: разные эндпоинты
/// WakaTime печатают время по-разному. В `current.json` времена вида
/// `2026-08-01T13:35:41Z` — секундная точность и `Z`, а не `+00:00`.
/// Разницу между ними сверка формы не поймает: обе строки для неё просто
/// `string`, — поэтому она закреплена тестом ниже.
///
/// **`None`, а не паника.** Календарь `chrono` кончается раньше, чем
/// `i64` микросекунд, и момента вне его не существует. Здесь это
/// недостижимо — время приезжает из нашей же базы, — но помощник общий, а
/// в задаче 3 `time` приезжает **от клиента**: `wakatime-cli` шлёт его
/// float-секундами, и абсурдное число превратилось бы в панику посреди
/// обработчика. Отвечать на это `null`, а не пятисоткой, — работа
/// вызывающего, и с `Option` она у него хотя бы есть.
pub(crate) fn rfc3339(t: wakode_core::Micros) -> Option<String> {
    Some(
        chrono::DateTime::from_timestamp_micros(t.get())?
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wakode_core::Micros;

    #[test]
    fn a_moment_is_printed_the_way_wakatime_prints_it() {
        // Дословно: `use_z = true` даёт `Z`, а не `+00:00`, и секунды
        // отсекают доли. И то, и другое — часть чужого контракта, а по
        // форме ответа их не видно.
        assert_eq!(
            rfc3339(Micros::from_secs(1_785_591_341)).as_deref(),
            Some("2026-08-01T13:35:41Z")
        );
    }

    #[test]
    fn the_fraction_of_a_second_does_not_leak_into_the_output() {
        // Отметки приходят с долями секунды, и WakaTime их не печатает.
        assert_eq!(
            rfc3339(Micros::new(1_785_591_341_500_000)).as_deref(),
            Some("2026-08-01T13:35:41Z")
        );
    }

    #[test]
    fn a_moment_outside_the_calendar_is_none_and_not_a_panic() {
        // Задача 3 отдаст сюда время от клиента: `from_secs_f64`
        // насыщается до `i64::MAX`, и на `expect` это было бы паникой
        // посреди обработчика.
        assert_eq!(rfc3339(Micros::new(i64::MAX)), None);
        assert_eq!(rfc3339(Micros::new(i64::MIN)), None);
    }
}
