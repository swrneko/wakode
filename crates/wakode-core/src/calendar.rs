use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;

use crate::{Interval, Micros};

/// Ширина окна поиска первого существующего момента суток, в минутах.
const MINUTES_IN_DAY: i64 = 24 * 60;

/// Календарное окно, за которое календарь отвечает: `0001-01-01T00:00:00Z` и
/// последняя микросекунда `9999-12-31`.
///
/// `Micros` шире любого календаря: `i64::MAX` микросекунд — это примерно
/// 292 277 год, а `chrono` перестаёт считать раньше, да ещё и паникует, когда
/// смещение зоны выталкивает локальное время за край. Поэтому время вне окна
/// не отвергается и не роняет вычисление, а насыщается к краю — ровно так же,
/// как насыщается вся остальная арифметика времени в крейте. Запас до края
/// `chrono` (примерно 262 000 лет) с обеих сторон покрывает любое смещение.
const CALENDAR_MIN: i64 = -62_135_596_800_000_000;
const CALENDAR_MAX: i64 = 253_402_300_799_999_999;

/// Первая и последняя даты календарного окна.
fn first_day() -> NaiveDate {
    NaiveDate::from_ymd_opt(1, 1, 1).expect("0001-01-01 — валидная дата")
}

fn last_day() -> NaiveDate {
    NaiveDate::from_ymd_opt(9999, 12, 31).expect("9999-12-31 — валидная дата")
}

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
    let minute = (1..=MINUTES_IN_DAY)
        .filter_map(|minute| midnight.checked_add_signed(TimeDelta::minutes(minute)))
        .find(|probe| tz.from_local_datetime(probe).earliest().is_some())?;
    // Дыра не обязана кончаться на границе минуты: отказываясь от солнечного
    // времени, зоны сдвигались на секунды (Монровия, 1972 — на 44 м 30 с), и
    // поминутный шаг проскочил бы начало суток на полминуты. Отступаем назад
    // посекундно, пока моменты ещё существуют; дальше 59 секунд отступать
    // некуда — там уже проверенная несуществующая минута.
    let first = (1..60)
        .filter_map(|second| minute.checked_sub_signed(TimeDelta::seconds(second)))
        .take_while(|probe| tz.from_local_datetime(probe).earliest().is_some())
        .last()
        .unwrap_or(minute);
    tz.from_local_datetime(&first).earliest()
}

/// Конец локальных суток: начало следующей даты.
///
/// За последним днём окна конец насыщается до `i64::MAX` — всё время после
/// него числится за `9999-12-31`. Это же насыщение делает нарезку конечной:
/// конец суток гарантированно обгоняет любой курсор.
fn local_day_end(date: NaiveDate, tz: Tz) -> Micros {
    match date.succ_opt() {
        Some(next) if next <= last_day() => local_day_start(next, tz),
        _ => Micros::new(i64::MAX),
    }
}

/// Границы локальных суток в UTC-микросекундах: `[начало, конец)`.
///
/// Конец берётся как начало следующей даты, а не как «начало плюс 24 часа»:
/// в дни перевода часов сутки длятся 23 или 25 часов.
///
/// Крайние дни календарного окна вбирают в себя всё время за его пределами:
/// сутки `0001-01-01` начинаются в `i64::MIN`, сутки `9999-12-31` кончаются в
/// `i64::MAX`.
pub fn local_day_bounds(date: NaiveDate, tz: Tz) -> (Micros, Micros) {
    let start = if date <= first_day() {
        Micros::new(i64::MIN)
    } else {
        local_day_start(date, tz)
    };
    (start, local_day_end(date, tz))
}

/// Локальная дата, которой принадлежит момент времени.
///
/// Время за пределами календарного окна насыщается к его краю: даты у такого
/// времени нет, но и права уронить вычисление у него тоже нет — `Micros`
/// принимает любой `i64`, а движок длительностей выше по потоку насыщает, а не
/// отвергает крайние значения. Результат всегда лежит внутри окна: за его
/// краем начинаются годы из пяти цифр, которые ниже по потоку не переживёт ни
/// один формат даты.
pub fn local_date_of(t: Micros, tz: Tz) -> NaiveDate {
    DateTime::<Utc>::from_timestamp_micros(t.get().clamp(CALENDAR_MIN, CALENDAR_MAX))
        .expect("календарное окно заведомо представимо")
        .with_timezone(&tz)
        .date_naive()
        .clamp(first_day(), last_day())
}

/// Сутки, внутри которых лежит момент, и их конец.
///
/// Дата ищется не по `local_date_of`, а по вложенности: `local_date_of` даёт
/// лишь нижнюю оценку. Когда часы переводят назад **после** локальной полуночи
/// и отматывают её обратно — так делали ньюфаундлендские зоны до 2011 года,
/// заканчивая летнее время в местные 00:01, — заново прожитый конец вчерашней
/// даты физически лежит уже внутри новых суток, но `local_date_of` для него
/// всё ещё вчерашний. Поэтому подсказка подтягивается вперёд, пока конец суток
/// не обгонит момент.
///
/// Возвращаемый конец строго позже `t`: сутки за краем календарного окна
/// насыщены до `i64::MAX`, поэтому продвижение всегда упирается в границу и
/// цикл конечен.
fn containing_day(t: Micros, hint: NaiveDate, tz: Tz) -> (NaiveDate, Micros) {
    let mut date = hint;
    loop {
        let day_end = local_day_end(date, tz);
        if day_end > t {
            return (date, day_end);
        }
        match date.succ_opt() {
            Some(next) => date = next,
            None => return (date, Micros::new(i64::MAX)),
        }
    }
}

/// Разрезает интервалы по границам локальных суток.
///
/// Сессия с 23:50 до 00:30 обязана попасть в оба дня частями, иначе сумма за
/// день не сойдётся с суммой за неделю — классический источник расхождений.
/// Куски режутся по календарным границам, а не прибавлением 86 400 секунд: в
/// дни перевода часов сутки длятся 23 или 25 часов, и арифметика соврала бы.
///
/// Сутки — полуинтервал `[начало, начало следующих суток)`, поэтому у момента
/// всегда ровно одни сутки. Плата за это видна там, где часы отматывают за
/// полночь: заново прожитый конец вчерашней даты числится за новыми сутками,
/// потому что те уже начались, когда часы впервые показали новую дату. Любое
/// непрерывное разбиение обязано чем-то подобным заплатить; выбранное правило
/// — «сутки начинаются, когда часы впервые показали эту дату» — заодно ничего
/// не теряет на обычном повторе часа, не переходящем через полночь.
pub fn split_by_local_day(intervals: &[Interval], tz: Tz) -> BTreeMap<NaiveDate, Vec<Interval>> {
    let mut out: BTreeMap<NaiveDate, Vec<Interval>> = BTreeMap::new();

    for iv in intervals {
        let mut cursor = iv.start;
        let mut hint = local_date_of(cursor, tz);
        while cursor < iv.end {
            let (date, day_end) = containing_day(cursor, hint, tz);
            // `day_end > cursor` обеспечено `containing_day`, а `iv.end >
            // cursor` — условием цикла, поэтому курсор строго растёт.
            let piece_end = day_end.min(iv.end);
            out.entry(date)
                .or_default()
                .push(Interval { start: cursor, end: piece_end, attrs: iv.attrs });
            cursor = piece_end;
            hint = date;
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
    fn day_start_is_exact_when_the_gap_ends_mid_minute() {
        // Либерия отказалась от солнечного смещения −0:44:30 ровно в полночь
        // 7 января 1972 года: местного времени от 00:00 до 00:44:30 не
        // существует, и сутки начинаются на нецелой минуте. Поминутный поиск
        // проскочил бы начало суток на тридцать секунд.
        let tz: Tz = "Africa/Monrovia".parse().unwrap();
        let (start, _) = local_day_bounds(date(1972, 1, 7), tz);

        assert_eq!(start, at("1972-01-07T00:44:30Z"));
    }

    #[test]
    fn timestamps_outside_the_calendar_window_saturate_instead_of_panicking() {
        // `Micros` шире любого календаря, а движок длительностей крайние
        // значения насыщает, а не отвергает, — значит и календарь обязан их
        // пережить, а не уронить вычисление.
        let tz: Tz = "Europe/Moscow".parse().unwrap();
        let late = Interval {
            start: Micros::new(i64::MAX - 10),
            end: Micros::new(i64::MAX),
            attrs: attrs(),
        };
        let early = Interval {
            start: Micros::new(i64::MIN),
            end: Micros::new(i64::MIN + 10),
            attrs: attrs(),
        };

        for iv in [late, early] {
            let days = split_by_local_day(&[iv], tz);
            let counted: i64 = days.values().flatten().map(|piece| piece.duration().get()).sum();
            assert_eq!(days.len(), 1);
            assert_eq!(counted, 10, "время у края `i64` не должно теряться");
        }

        assert_eq!(local_date_of(Micros::new(i64::MAX), tz), last_day());
        assert_eq!(local_date_of(Micros::new(i64::MIN), tz), first_day());
    }

    #[test]
    fn calendar_window_constants_match_their_dates() {
        // Константы окна записаны числами, поэтому опечатка в них сдвинула бы
        // насыщение молча.
        assert_eq!(Micros::new(CALENDAR_MIN), at("0001-01-01T00:00:00Z"));
        assert_eq!(Micros::new(CALENDAR_MAX), at("9999-12-31T23:59:59.999999Z"));
        assert_eq!(local_date_of(Micros::new(CALENDAR_MIN), Tz::UTC), first_day());
        assert_eq!(local_date_of(Micros::new(CALENDAR_MAX), Tz::UTC), last_day());
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
    fn rollback_past_midnight_does_not_reverse_pieces() {
        // Ньюфаундлендские зоны до 2011 года заканчивали летнее время в местные
        // 00:01 и отматывали часы за полночь: 2009-11-01 в 03:01 UTC часы с
        // 00:01 первого ноября уходят на 23:01 тридцать первого октября. Дата
        // от `local_date_of` после этого снова вчерашняя, хотя сутки уже
        // сменились, и наивная нарезка выдаёт кусок с концом раньше начала.
        let tz: Tz = "America/Goose_Bay".parse().unwrap();
        let iv = interval("2009-11-01T03:30:00Z", "2009-11-01T04:30:00Z");
        let days = split_by_local_day(&[iv], tz);

        assert!(
            days.values().flatten().all(|piece| piece.start < piece.end),
            "кусок с концом раньше начала: {:?}",
            days
        );
        // Сутки первого ноября начались в 03:00 UTC — в момент, когда часы
        // впервые показали эту дату, — и весь час работы лежит внутри них.
        assert_eq!(days.len(), 1);
        assert_eq!(days[&date(2009, 11, 1)][0].duration(), Micros::from_secs(3600));
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
