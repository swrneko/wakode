use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;

use crate::{Interval, Micros};

/// Ширина окна поиска первого существующего момента суток, в минутах.
const MINUTES_IN_DAY: i64 = 24 * 60;

/// Момент начала локальных суток в UTC-микросекундах.
///
/// Обычно это локальная полночь, но она существует не всегда: перевод часов
/// вперёд выбрасывает из местного времени кусок, и в ряде стран (Чили, Куба,
/// Ливан) вместе с ним исчезает сама полночь. Тогда сутки начинаются с первого
/// существующего момента — момента перевода часов.
fn local_day_start(date: NaiveDate, tz: Tz) -> Micros {
    let midnight = date.and_hms_opt(0, 0, 0).expect("полночь всегда валидна");
    // `earliest` берёт раннее из двух вхождений, когда часы переводят назад и
    // полночь случается дважды. Выбор не косметический: сутки задаются
    // полуинтервалом `[начало, начало следующих суток)`, и если бы концом
    // суток служило позднее вхождение, повторившийся час попал бы и в эти
    // сутки, и в следующие — то есть был бы засчитан дважды.
    let start = match tz.from_local_datetime(&midnight).earliest() {
        Some(dt) => dt,
        // Поиск охватывает и полночь следующей даты, поэтому не находит ничего
        // только если местного времени нет двое суток подряд — такого в базе
        // IANA не бывает даже там, где сутки выпадали целиком (Самоа, 2011).
        None => first_existing_moment(midnight, tz).expect("в сутках есть хоть один момент"),
    };
    Micros::new(start.timestamp_micros())
}

/// Первый существующий момент суток, начало которых провалилось в дыру
/// местного времени. Поиск идёт вперёд от полуночи и ограничен сутками: за
/// ними уже начинается следующая дата, и продолжать бессмысленно.
fn first_existing_moment(midnight: NaiveDateTime, tz: Tz) -> Option<DateTime<Tz>> {
    (1..=MINUTES_IN_DAY)
        .filter_map(|minute| midnight.checked_add_signed(TimeDelta::minutes(minute)))
        .find_map(|probe| tz.from_local_datetime(&probe).earliest())
}

/// Границы локальных суток в UTC-микросекундах: `[начало, конец)`.
///
/// Конец берётся как начало следующей даты, а не как «начало плюс 24 часа»:
/// в дни перевода часов сутки длятся 23 или 25 часов.
pub fn local_day_bounds(date: NaiveDate, tz: Tz) -> (Micros, Micros) {
    let next = date.succ_opt().expect("дата не на краю календаря");
    (local_day_start(date, tz), local_day_start(next, tz))
}

/// Локальная дата, которой принадлежит момент времени.
///
/// Момент обязан лежать в календарном диапазоне chrono (около ±262 000 лет от
/// эпохи): у времени за его пределами локальной даты не существует вовсе, и
/// отсекать такие значения обязан слой ввода, а не календарь.
pub fn local_date_of(t: Micros, tz: Tz) -> NaiveDate {
    DateTime::<Utc>::from_timestamp_micros(t.get())
        .expect("время в календарном диапазоне")
        .with_timezone(&tz)
        .date_naive()
}

/// Разрезает интервалы по границам локальных суток.
///
/// Сессия с 23:50 до 00:30 обязана попасть в оба дня частями, иначе сумма за
/// день не сойдётся с суммой за неделю — классический источник расхождений.
/// Куски режутся по календарным границам, а не прибавлением 86 400 секунд: в
/// дни перевода часов сутки длятся 23 или 25 часов, и арифметика соврала бы.
pub fn split_by_local_day(intervals: &[Interval], tz: Tz) -> BTreeMap<NaiveDate, Vec<Interval>> {
    let mut out: BTreeMap<NaiveDate, Vec<Interval>> = BTreeMap::new();

    for iv in intervals {
        let mut cursor = iv.start;
        while cursor < iv.end {
            // Цикл конечен ровно потому, что конец суток берётся из календаря:
            // `day_end` — начало следующих суток, а значит строго позже любого
            // момента этих. Конец, посчитанный прибавкой 24 часов, в сутки
            // длиной 25 часов оказался бы позади курсора, и цикл завис бы.
            let date = local_date_of(cursor, tz);
            let (_, day_end) = local_day_bounds(date, tz);
            let piece_end = day_end.min(iv.end);
            out.entry(date)
                .or_default()
                .push(Interval { start: cursor, end: piece_end, attrs: iv.attrs });
            cursor = piece_end;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attrs, Category, EntityKind, Sid};

    fn attrs() -> Attrs {
        Attrs {
            entity: Sid(1),
            kind: EntityKind::File,
            category: Category::Coding,
            project: Some(Sid(1)),
            branch: None,
            language: None,
            editor: None,
            os: None,
            machine: None,
        }
    }

    fn interval(start: &str, end: &str) -> Interval {
        Interval { start: at(start), end: at(end), attrs: attrs() }
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("валидная дата")
    }

    fn at(iso: &str) -> Micros {
        Micros::new(
            iso.parse::<chrono::DateTime<chrono::Utc>>()
                .expect("валидная дата")
                .timestamp_micros(),
        )
    }

    #[test]
    fn day_bounds_respect_the_timezone() {
        let tz: Tz = "Europe/Moscow".parse().unwrap();
        let (start, end) = local_day_bounds(date(2026, 8, 15), tz);

        // Москва — UTC+3, значит локальная полночь это 21:00 предыдущего дня UTC.
        assert_eq!(start, at("2026-08-14T21:00:00Z"));
        assert_eq!(end, at("2026-08-15T21:00:00Z"));
    }

    #[test]
    fn day_starts_at_the_first_existing_moment_when_midnight_is_skipped() {
        // Чили переводит часы вперёд ровно в полночь: 2026-09-06 местное время
        // 00:00 не существует, сутки начинаются в 01:00 уже при UTC-03.
        let tz: Tz = "America/Santiago".parse().unwrap();
        let (start, end) = local_day_bounds(date(2026, 9, 6), tz);

        assert_eq!(start, at("2026-09-06T04:00:00Z"), "начало суток — момент перевода часов");
        assert_eq!(end, at("2026-09-07T03:00:00Z"));
        assert_eq!(end.saturating_sub(start), Micros::from_secs(23 * 3600), "сутки длятся 23 часа");
    }

    #[test]
    fn ambiguous_midnight_resolves_to_its_earliest_occurrence() {
        // 2026-11-01 Куба переводит часы назад в 01:00, поэтому местные 00:00
        // случаются дважды: в 04:00 UTC (ещё UTC-04) и в 05:00 UTC (уже
        // UTC-05). Началом суток обязано быть раннее вхождение — иначе
        // повторившийся час выпал бы из этих суток и попал в предыдущие.
        let tz: Tz = "America/Havana".parse().unwrap();
        let (start, end) = local_day_bounds(date(2026, 11, 1), tz);

        assert_eq!(start, at("2026-11-01T04:00:00Z"));
        assert_eq!(end, at("2026-11-02T05:00:00Z"));
        assert_eq!(end.saturating_sub(start), Micros::from_secs(25 * 3600), "сутки длятся 25 часов");
    }

    #[test]
    fn local_date_is_the_users_date_not_the_utc_one() {
        let tz: Tz = "Europe/Moscow".parse().unwrap();

        // 21:30 UTC — по Москве это уже полночь с половиной следующих суток.
        assert_eq!(local_date_of(at("2026-08-14T21:30:00Z"), tz), date(2026, 8, 15));
        assert_eq!(local_date_of(at("2026-08-14T20:30:00Z"), tz), date(2026, 8, 14));
    }

    #[test]
    fn both_passes_of_a_repeated_hour_belong_to_the_same_day() {
        // Местные 00:30 на Кубе 2026-11-01 проживаются дважды. Оба вхождения
        // обязаны лежать внутри одних и тех же суток — и по дате, и по
        // границам: иначе час работы либо потеряется, либо удвоится.
        let tz: Tz = "America/Havana".parse().unwrap();
        let (start, end) = local_day_bounds(date(2026, 11, 1), tz);
        let first_pass = at("2026-11-01T04:30:00Z");
        let second_pass = at("2026-11-01T05:30:00Z");

        assert_eq!(local_date_of(first_pass, tz), date(2026, 11, 1));
        assert_eq!(local_date_of(second_pass, tz), date(2026, 11, 1));
        assert!(start <= first_pass && second_pass < end);
        // А момент перед началом суток — уже предыдущая дата, без зазора.
        assert_eq!(local_date_of(start.saturating_sub(Micros::new(1)), tz), date(2026, 10, 31));
    }

    #[test]
    fn interval_crossing_midnight_is_split() {
        let tz: Tz = "Europe/Moscow".parse().unwrap();
        // 23:30 — 00:30 по Москве.
        let iv = interval("2026-08-14T20:30:00Z", "2026-08-14T21:30:00Z");
        let days = split_by_local_day(&[iv], tz);

        assert_eq!(days.len(), 2);
        assert_eq!(days[&date(2026, 8, 14)][0].duration(), Micros::from_secs(1800));
        assert_eq!(days[&date(2026, 8, 15)][0].duration(), Micros::from_secs(1800));
    }

    #[test]
    fn split_across_a_skipped_midnight_keeps_every_microsecond() {
        // Интервал проходит сквозь момент перевода часов в Чили: до него
        // местное время 23:30 пятого сентября, сразу после — 01:30 шестого.
        // Час работы обязан разделиться поровну между сутками, хотя локального
        // времени между 00:00 и 01:00 в этот день не существует.
        let tz: Tz = "America/Santiago".parse().unwrap();
        let iv = interval("2026-09-06T03:30:00Z", "2026-09-06T04:30:00Z");
        let days = split_by_local_day(&[iv], tz);

        assert_eq!(days.len(), 2);
        assert_eq!(days[&date(2026, 9, 5)][0].duration(), Micros::from_secs(1800));
        assert_eq!(days[&date(2026, 9, 6)][0].duration(), Micros::from_secs(1800));
    }

    #[test]
    fn split_uses_the_calendar_end_of_a_lengthened_day() {
        // Сутки 2026-11-01 на Кубе длятся 25 часов, поэтому кончаются в 05:00
        // UTC второго ноября, а не в 04:00, как насчитала бы прибавка суток к
        // началу дня. Интервал 04:30—05:30 обязан разделиться пополам.
        let tz: Tz = "America/Havana".parse().unwrap();
        let iv = interval("2026-11-02T04:30:00Z", "2026-11-02T05:30:00Z");
        let days = split_by_local_day(&[iv], tz);

        assert_eq!(days.len(), 2);
        assert_eq!(days[&date(2026, 11, 1)][0].duration(), Micros::from_secs(1800));
        assert_eq!(days[&date(2026, 11, 2)][0].duration(), Micros::from_secs(1800));
    }

    #[test]
    fn splitting_preserves_total_duration() {
        let tz: Tz = "Europe/Moscow".parse().unwrap();
        let iv = interval("2026-08-14T20:30:00Z", "2026-08-14T21:30:00Z");
        let total: i64 = split_by_local_day(&[iv], tz)
            .values()
            .flatten()
            .map(|piece| piece.duration().get())
            .sum();

        assert_eq!(total, iv.duration().get());
    }
}
