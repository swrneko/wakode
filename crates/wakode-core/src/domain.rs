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

/// Чем занят пользователь — категория из протокола WakaTime.
///
/// Список вариантов повторяет `pkg/heartbeat/category.go` из wakatime-cli.
/// Половина значений там состоит из двух слов (`code reviewing`, `writing
/// tests`), поэтому `rename_all` тут неприменим и каждый вариант назван явно:
/// строка в кавычках — это проволочный контракт с плагинами, а не деталь
/// оформления.
///
/// **Номера вариантов зафиксированы намеренно.** Категория хранится в БД
/// числом, поэтому номер — это данные, а не порядок объявления. Правило одно:
/// новый вариант получает следующий свободный номер и дописывается в конец;
/// существующие номера не переиспользуются и не сдвигаются никогда, даже если
/// вариант перестанет присылаться. Начальная нумерация раздана по алфавиту
/// проволочных строк — это разовое совпадение, а не инвариант.
///
/// Своей нумерации wakatime-cli доверять нельзя: там номера идут через `iota`,
/// и вставка `ai coding` третьим элементом уже сдвинула всё, что стояло после.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[derive(Serialize, Deserialize)]
#[repr(u8)]
pub enum Category {
    #[serde(rename = "advising")]
    Advising = 1,
    #[serde(rename = "ai coding")]
    AiCoding = 2,
    #[serde(rename = "browsing")]
    Browsing = 3,
    #[serde(rename = "building")]
    Building = 4,
    #[serde(rename = "code reviewing")]
    CodeReviewing = 5,
    #[default]
    #[serde(rename = "coding")]
    Coding = 6,
    #[serde(rename = "communicating")]
    Communicating = 7,
    #[serde(rename = "debugging")]
    Debugging = 8,
    #[serde(rename = "designing")]
    Designing = 9,
    #[serde(rename = "indexing")]
    Indexing = 10,
    #[serde(rename = "learning")]
    Learning = 11,
    #[serde(rename = "manual testing")]
    ManualTesting = 12,
    #[serde(rename = "meeting")]
    Meeting = 13,
    #[serde(rename = "notes")]
    Notes = 14,
    #[serde(rename = "planning")]
    Planning = 15,
    #[serde(rename = "researching")]
    Researching = 16,
    #[serde(rename = "running tests")]
    RunningTests = 17,
    #[serde(rename = "supporting")]
    Supporting = 18,
    #[serde(rename = "translating")]
    Translating = 19,
    #[serde(rename = "writing docs")]
    WritingDocs = 20,
    #[serde(rename = "writing tests")]
    WritingTests = 21,
    /// Категория, которой мы ещё не знаем.
    ///
    /// Плагин обновляется раньше сервера, и незнакомое значение не имеет права
    /// уронить разбор всего heartbeat'а: потерянное время дороже точности
    /// измерения. Номер `0` занят этим вариантом навсегда — под ним же
    /// осядут категории, выведенные из употребления.
    ///
    /// Строка `"unknown"` — наша собственная, в протоколе WakaTime её нет: там
    /// отсутствию категории соответствует `null`, а `null` вариантом-юнитом не
    /// выражается. Перевод `null` ⇄ `Unknown` — забота слоя HTTP.
    #[serde(other, rename = "unknown")]
    Unknown = 0,
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

    /// Полный контракт категории: вариант, строка в протоколе и номер, под
    /// которым вариант ляжет в хранилище. Таблица одна на три теста ниже —
    /// разъехаться они не могут.
    const CATEGORY_CONTRACT: &[(Category, &str, u8)] = &[
        (Category::Unknown, "unknown", 0),
        (Category::Advising, "advising", 1),
        (Category::AiCoding, "ai coding", 2),
        (Category::Browsing, "browsing", 3),
        (Category::Building, "building", 4),
        (Category::CodeReviewing, "code reviewing", 5),
        (Category::Coding, "coding", 6),
        (Category::Communicating, "communicating", 7),
        (Category::Debugging, "debugging", 8),
        (Category::Designing, "designing", 9),
        (Category::Indexing, "indexing", 10),
        (Category::Learning, "learning", 11),
        (Category::ManualTesting, "manual testing", 12),
        (Category::Meeting, "meeting", 13),
        (Category::Notes, "notes", 14),
        (Category::Planning, "planning", 15),
        (Category::Researching, "researching", 16),
        (Category::RunningTests, "running tests", 17),
        (Category::Supporting, "supporting", 18),
        (Category::Translating, "translating", 19),
        (Category::WritingDocs, "writing docs", 20),
        (Category::WritingTests, "writing tests", 21),
    ];

    #[test]
    fn every_category_round_trips_through_its_wakatime_string() {
        // Строки взяты из `pkg/heartbeat/category.go` wakatime-cli. Многие из
        // них состоят из двух слов, поэтому `rename_all = "lowercase"` их
        // выразить не может — каждый вариант назван явно.
        for (category, wire, _) in CATEGORY_CONTRACT {
            let json = serde_json::to_string(category).expect("категория сериализуема");
            assert_eq!(json, format!("\"{wire}\""), "неверная строка для {category:?}");

            let back: Category = serde_json::from_str(&json).expect("категория разбирается");
            assert_eq!(back, *category);
        }
    }

    #[test]
    fn unrecognised_category_becomes_unknown_instead_of_failing() {
        // Плагин новее нас присылает категорию, которой мы ещё не знаем. Терять
        // из-за этого весь heartbeat нельзя: время дороже точности измерения.
        let parsed: Category = serde_json::from_str("\"vibe coding\"").expect("не должно падать");
        assert_eq!(parsed, Category::Unknown);
    }

    #[test]
    fn category_discriminants_are_pinned_to_their_stored_numbers() {
        // Номера уезжают в БД как `category INTEGER`. Перестановка вариантов
        // обязана ломать этот тест, а не молча переименовывать чужие данные.
        for (category, _, number) in CATEGORY_CONTRACT {
            assert_eq!(*category as u8, *number, "сдвинулся номер {category:?}");
        }
        assert_eq!(
            CATEGORY_CONTRACT.len(),
            22,
            "новый вариант обязан быть добавлен в таблицу контракта"
        );
    }

    #[test]
    fn entity_kind_wire_strings_match_the_wakatime_protocol() {
        // `rename_all = "lowercase"` — такой же проволочный контракт, как и
        // явные имена категорий, только записанный одной строкой.
        for (kind, wire) in [
            (EntityKind::File, "file"),
            (EntityKind::App, "app"),
            (EntityKind::Url, "url"),
            (EntityKind::Domain, "domain"),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{wire}\""));
            assert_eq!(
                serde_json::from_str::<EntityKind>(&format!("\"{wire}\"")).unwrap(),
                kind
            );
        }
    }

    #[test]
    fn attrs_serialize_with_stable_field_names() {
        // Имена полей уедут в JSON сводок плана 3; переименование поля не
        // должно проходить незамеченным.
        let json = serde_json::to_value(attrs(7)).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "entity": 1,
                "kind": "file",
                "category": "coding",
                "project": 7,
                "branch": null,
                "language": null,
                "editor": null,
                "os": null,
                "machine": null,
            })
        );
    }

    #[test]
    fn heartbeat_time_serializes_as_bare_microseconds() {
        // Внимание слою HTTP: протокол WakaTime передаёт время float-секундами,
        // а `Micros` сериализуется целым числом микросекунд. Производная
        // реализация на границе протокола неприменима без конверсии.
        let hb = Heartbeat { time: Micros::from_secs(90), attrs: attrs(1) };
        let json = serde_json::to_value(hb).unwrap();

        assert_eq!(json["time"], serde_json::json!(90_000_000i64));
    }
}
