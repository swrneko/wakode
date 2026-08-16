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
/// Каждая пара соседних по времени отметок даёт интервал; интервал наследует
/// атрибуты более ранней из пары. Разрыв сессии по таймауту и хвостовая
/// добавка последней отметке появятся позже — здесь склейка безусловная.
pub fn build_intervals(heartbeats: &[Heartbeat], _cfg: DurationConfig) -> Vec<Interval> {
    if heartbeats.is_empty() {
        return Vec::new();
    }

    let mut sorted = heartbeats.to_vec();
    sorted.sort_unstable();

    let mut out = Vec::with_capacity(sorted.len());
    for (i, hb) in sorted.iter().enumerate() {
        let Some(next) = sorted.get(i + 1) else { continue };
        if next.time > hb.time {
            out.push(Interval { start: hb.time, end: next.time, attrs: hb.attrs });
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
}
