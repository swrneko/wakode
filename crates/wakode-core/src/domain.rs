use serde::{Deserialize, Serialize};

use crate::Micros;

/// Идентификатор интернированной строки.
///
/// Крейт никогда не видит самих строк: путь к файлу, проект, ветка и язык
/// повторяются в потоке миллионы раз, поэтому слой хранения держит словарь,
/// а сюда передаёт только номера. Группировка по числам и быстрее, и проще.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[derive(Serialize, Deserialize)]
pub struct Sid(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityKind {
    #[default]
    File,
    App,
    Url,
    Domain,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    #[default]
    Coding,
    Building,
    Debugging,
    Writing,
    Reviewing,
    Browsing,
    Communicating,
    Designing,
    Other,
}

/// Атрибуты отметки — всё, кроме времени.
///
/// Интервал наследует атрибуты более ранней из пары отметок, поэтому они
/// хранятся отдельным типом и копируются целиком.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[derive(Serialize, Deserialize)]
pub struct Attrs {
    pub entity: Sid,
    pub kind: EntityKind,
    pub category: Category,
    pub project: Option<Sid>,
    pub branch: Option<Sid>,
    pub language: Option<Sid>,
    pub editor: Option<Sid>,
    pub os: Option<Sid>,
    pub machine: Option<Sid>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[derive(Serialize, Deserialize)]
pub struct Heartbeat {
    pub time: Micros,
    pub attrs: Attrs,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Micros;

    fn attrs(project: u32) -> Attrs {
        Attrs {
            entity: Sid(1),
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

    #[test]
    fn heartbeats_sort_by_time_then_attrs() {
        // Детерминированный порядок нужен движку интервалов: одинаковые входные
        // данные обязаны давать одинаковый результат независимо от порядка.
        let mut hbs = vec![
            Heartbeat { time: Micros::from_secs(10), attrs: attrs(2) },
            Heartbeat { time: Micros::from_secs(10), attrs: attrs(1) },
            Heartbeat { time: Micros::from_secs(5), attrs: attrs(9) },
        ];
        hbs.sort();

        assert_eq!(hbs[0].time, Micros::from_secs(5));
        assert_eq!(hbs[1].attrs.project, Some(Sid(1)));
        assert_eq!(hbs[2].attrs.project, Some(Sid(2)));
    }

    #[test]
    fn category_defaults_to_coding() {
        assert_eq!(Category::default(), Category::Coding);
    }
}
