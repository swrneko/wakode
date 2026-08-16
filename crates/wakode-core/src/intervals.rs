use serde::{Deserialize, Serialize};

use crate::{Attrs, DurationConfig, Heartbeat, Micros};

/// Отрезок времени с атрибутами отметки, которой он принадлежит.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[derive(Serialize, Deserialize)]
pub struct Interval {
    pub start: Micros,
    pub end: Micros,
    pub attrs: Attrs,
}

impl Interval {
    pub fn duration(self) -> Micros {
        self.end.saturating_sub(self.start)
    }
}

/// Превращает поток отметок в непересекающиеся интервалы.
///
/// Каждая пара соседних по времени отметок даёт интервал, если разрыв между
/// ними не превышает `cfg.timeout()`; интервал наследует атрибуты более
/// ранней из пары. Разрыв длиннее таймаута обрывает сессию — время паузы не
/// засчитывается никому. Последняя отметка сессии (та, у которой нет пары в
/// пределах таймаута) получает хвостовую добавку `cfg.tail_padding()` — при
/// нулевой добавке интервал не порождается, так же как без неё.
pub fn build_intervals(heartbeats: &[Heartbeat], cfg: DurationConfig) -> Vec<Interval> {
    if heartbeats.is_empty() {
        return Vec::new();
    }

    let mut sorted = heartbeats.to_vec();
    sorted.sort_unstable();
    // Дубликаты не удаляются отдельно: после сортировки полностью одинаковая
    // отметка стоит рядом со своей копией, даёт с ней интервал нулевой длины,
    // и его отсекает guard `end > hb.time` ниже — тот же механизм, что убирает
    // любой другой нулевой интервал.

    let mut out = Vec::with_capacity(sorted.len());
    for (i, hb) in sorted.iter().enumerate() {
        let end = match sorted.get(i + 1) {
            // Следующая отметка в пределах таймаута — интервал тянется до неё.
            // Граница включительная — ровно таймаут ещё та же сессия.
            Some(next) if next.time.saturating_sub(hb.time) <= cfg.timeout() => next.time,
            // Иначе это последняя отметка сессии: ей начисляется хвостовая добавка.
            _ => hb.time.saturating_add(cfg.tail_padding()),
        };
        if end > hb.time {
            out.push(Interval { start: hb.time, end, attrs: hb.attrs });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Category, EntityKind, Sid};

    fn attrs(project: u32) -> Attrs {
        Attrs {
            entity: Sid(project),
            kind: EntityKind::File,
            category: Category::Coding,
            project: Some(Sid(project)),
            branch: None,
            language: None,
            editor: None,
            os: None,
            machine: None,
        }
    }

    fn hb(secs: i64, project: u32) -> Heartbeat {
        Heartbeat { time: Micros::from_secs(secs), attrs: attrs(project) }
    }

    #[test]
    fn empty_input_produces_no_intervals() {
        assert!(build_intervals(&[], DurationConfig::default()).is_empty());
    }

    #[test]
    fn single_heartbeat_has_no_partner_and_produces_no_intervals() {
        // Последней отметке пары нет — она не порождает интервал в этой задаче.
        assert!(build_intervals(&[hb(0, 1)], DurationConfig::default()).is_empty());
    }

    #[test]
    fn adjacent_heartbeats_within_timeout_form_one_interval() {
        let cfg = DurationConfig::default();
        let intervals = build_intervals(&[hb(0, 1), hb(60, 1)], cfg);

        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0].start, Micros::from_secs(0));
        assert_eq!(intervals[0].end, Micros::from_secs(60));
        assert_eq!(intervals[0].duration(), Micros::from_secs(60));
    }

    #[test]
    fn interval_inherits_attributes_of_the_earlier_heartbeat() {
        // Промежуток между отметками — это время, проведённое в том, что было
        // открыто раньше, а не в том, куда пользователь только что перешёл.
        let cfg = DurationConfig::default();
        let intervals = build_intervals(&[hb(0, 1), hb(60, 2)], cfg);

        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0].attrs.project, Some(Sid(1)));
    }

    #[test]
    fn three_heartbeats_form_two_intervals() {
        let cfg = DurationConfig::default();
        let intervals = build_intervals(&[hb(0, 1), hb(60, 1), hb(120, 1)], cfg);

        assert_eq!(intervals.len(), 2);
        assert_eq!(intervals[1].start, Micros::from_secs(60));
        assert_eq!(intervals[1].end, Micros::from_secs(120));
    }

    #[test]
    fn gap_longer_than_timeout_breaks_the_session() {
        // Пауза длиннее таймаута не засчитывается никому: пользователь ушёл.
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::ZERO).unwrap();
        let intervals = build_intervals(&[hb(0, 1), hb(901, 1)], cfg);

        assert!(intervals.is_empty(), "пауза в 901 секунду не должна давать интервал");
    }

    #[test]
    fn gap_exactly_equal_to_timeout_is_still_counted() {
        // Граница включительная: ровно таймаут — ещё та же сессия.
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::ZERO).unwrap();
        let intervals = build_intervals(&[hb(0, 1), hb(900, 1)], cfg);

        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0].duration(), Micros::from_secs(900));
    }

    #[test]
    fn two_sessions_separated_by_a_long_pause() {
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::ZERO).unwrap();
        let intervals = build_intervals(
            &[hb(0, 1), hb(60, 1), hb(5000, 1), hb(5060, 1)],
            cfg,
        );

        assert_eq!(intervals.len(), 2);
        assert_eq!(intervals[0].duration(), Micros::from_secs(60));
        assert_eq!(intervals[1].start, Micros::from_secs(5000));
        assert_eq!(intervals[1].duration(), Micros::from_secs(60));
    }

    #[test]
    fn last_heartbeat_of_a_session_gets_tail_padding() {
        // У последней отметки сессии нет пары, поэтому ей начисляется добавка.
        // Величина, которую использует WakaTime, неизвестна — здесь она задана явно.
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::from_secs(120)).unwrap();
        let intervals = build_intervals(&[hb(0, 1), hb(60, 1)], cfg);

        assert_eq!(intervals.len(), 2);
        assert_eq!(intervals[1].start, Micros::from_secs(60));
        assert_eq!(intervals[1].end, Micros::from_secs(180));
        assert_eq!(intervals[1].attrs.project, Some(Sid(1)));
    }

    #[test]
    fn each_session_gets_its_own_tail_padding() {
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::from_secs(60)).unwrap();
        let intervals = build_intervals(&[hb(0, 1), hb(5000, 2)], cfg);

        assert_eq!(intervals.len(), 2);
        assert_eq!(intervals[0].end, Micros::from_secs(60));
        assert_eq!(intervals[1].end, Micros::from_secs(5060));
    }

    #[test]
    fn zero_padding_produces_no_tail_interval() {
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::ZERO).unwrap();
        let intervals = build_intervals(&[hb(0, 1), hb(60, 1)], cfg);

        assert_eq!(intervals.len(), 1);
    }

    #[test]
    fn single_heartbeat_produces_only_padding() {
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::from_secs(30)).unwrap();
        let intervals = build_intervals(&[hb(100, 7)], cfg);

        assert_eq!(intervals.len(), 1);
        assert_eq!(intervals[0].start, Micros::from_secs(100));
        assert_eq!(intervals[0].end, Micros::from_secs(130));
    }

    #[test]
    fn input_order_does_not_affect_the_result() {
        let cfg = DurationConfig::default();
        let ordered = build_intervals(&[hb(0, 1), hb(60, 1), hb(120, 1)], cfg);
        let shuffled = build_intervals(&[hb(120, 1), hb(0, 1), hb(60, 1)], cfg);

        assert_eq!(ordered, shuffled);
    }

    #[test]
    fn duplicate_heartbeats_do_not_inflate_totals() {
        // Полный дубликат — это повтор доставки, а не новая активность.
        let cfg = DurationConfig::default();
        let clean = build_intervals(&[hb(0, 1), hb(60, 1)], cfg);
        let duplicated = build_intervals(&[hb(0, 1), hb(0, 1), hb(60, 1), hb(60, 1)], cfg);

        assert_eq!(clean, duplicated);
    }

    #[test]
    fn simultaneous_heartbeats_with_different_attributes_produce_no_zero_intervals() {
        let cfg = DurationConfig::new(Micros::from_secs(900), Micros::ZERO).unwrap();
        let intervals = build_intervals(&[hb(0, 1), hb(0, 2), hb(60, 1)], cfg);

        assert!(
            intervals.iter().all(|iv| iv.duration().get() > 0),
            "интервалы нулевой длины не должны попадать в результат"
        );
    }
}
