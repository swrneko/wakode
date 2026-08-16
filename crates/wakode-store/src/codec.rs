//! Отображение доменных типов в представление в базе и обратно.
//!
//! Разбор числа обратно в вариант перечисления идёт явным `match`, никогда
//! через serde: выведенный визитор идентификаторов принимает числовые
//! *индексы вариантов по порядку объявления*, а не объявленные дискриминанты
//! — при первом же расхождении он молча вернёт не тот вариант.

use uuid::Uuid;
use wakode_core::{Category, EntityKind, Sid};

use crate::error::{StoreError, StoreResult};

pub fn uuid_to_blob(id: Uuid) -> [u8; 16] {
    *id.as_bytes()
}

pub fn blob_to_uuid(blob: &[u8]) -> StoreResult<Uuid> {
    let bytes: [u8; 16] = blob
        .try_into()
        .map_err(|_| StoreError::Corrupt(format!("UUID из {} байт вместо 16", blob.len())))?;
    Ok(Uuid::from_bytes(bytes))
}

pub fn sid_to_i64(sid: Sid) -> i64 {
    i64::from(sid.0)
}

pub fn i64_to_sid(value: i64) -> StoreResult<Sid> {
    u32::try_from(value)
        .map(Sid)
        .map_err(|_| StoreError::OutOfRange("номер строки не помещается в u32"))
}

pub fn kind_to_i64(kind: EntityKind) -> i64 {
    kind as u8 as i64
}

/// Явный `match` вместо serde: выведенный визитор идентификаторов принимает
/// числовые индексы вариантов, а они равны позициям в объявлении, а не
/// дискриминантам. Совпадение сегодня и расхождение завтра.
pub fn i64_to_kind(value: i64) -> StoreResult<EntityKind> {
    match value {
        0 => Ok(EntityKind::File),
        1 => Ok(EntityKind::App),
        2 => Ok(EntityKind::Url),
        3 => Ok(EntityKind::Domain),
        other => Err(StoreError::Corrupt(format!("неизвестный вид сущности: {other}"))),
    }
}

pub fn category_to_i64(category: Category) -> i64 {
    category as u8 as i64
}

/// В отличие от вида сущности, неизвестная категория — не порча данных, а
/// более новый плагин. Отметку из-за неё терять нельзя.
pub fn i64_to_category(value: i64) -> StoreResult<Category> {
    Ok(match value {
        0 => Category::Unknown,
        1 => Category::Advising,
        2 => Category::AiCoding,
        3 => Category::Browsing,
        4 => Category::Building,
        5 => Category::CodeReviewing,
        6 => Category::Coding,
        7 => Category::Communicating,
        8 => Category::Debugging,
        9 => Category::Designing,
        10 => Category::Indexing,
        11 => Category::Learning,
        12 => Category::ManualTesting,
        13 => Category::Meeting,
        14 => Category::Notes,
        15 => Category::Planning,
        16 => Category::Researching,
        17 => Category::RunningTests,
        18 => Category::Supporting,
        19 => Category::Translating,
        20 => Category::WritingDocs,
        21 => Category::WritingTests,
        _ => Category::Unknown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Контракт хранения. Числа тут — то, что уже лежит в чужих базах.
    /// Менять их нельзя; можно только дописывать новые варианты в конец.
    const KIND_CONTRACT: &[(EntityKind, i64)] = &[
        (EntityKind::File, 0),
        (EntityKind::App, 1),
        (EntityKind::Url, 2),
        (EntityKind::Domain, 3),
    ];

    #[test]
    fn entity_kind_round_trips_through_its_pinned_number() {
        for (kind, number) in KIND_CONTRACT {
            assert_eq!(kind_to_i64(*kind), *number, "{kind:?}");
            assert_eq!(i64_to_kind(*number).unwrap(), *kind, "{number}");
        }
    }

    #[test]
    fn unknown_kind_number_is_reported_not_guessed() {
        assert!(i64_to_kind(99).is_err());
        assert!(i64_to_kind(-1).is_err());
    }

    #[test]
    fn category_round_trips_and_unknown_survives() {
        for category in [
            Category::Unknown,
            Category::Coding,
            Category::CodeReviewing,
            Category::WritingTests,
        ] {
            let number = category_to_i64(category);
            assert_eq!(i64_to_category(number).unwrap(), category);
        }
        assert_eq!(category_to_i64(Category::Unknown), 0);
        assert_eq!(category_to_i64(Category::Coding), 6);
    }

    #[test]
    fn unknown_category_number_maps_to_unknown_not_an_error() {
        // Тут поведение сознательно отличается от `EntityKind`: категорию мог
        // прислать более новый плагин, и терять из-за неё всю отметку нельзя.
        // Вид сущности приходит из закрытого списка нашего же кода.
        assert_eq!(i64_to_category(99).unwrap(), Category::Unknown);
    }

    #[test]
    fn uuid_round_trips_through_sixteen_bytes() {
        let id = uuid::Uuid::now_v7();
        let blob = uuid_to_blob(id);
        assert_eq!(blob.len(), 16);
        assert_eq!(blob_to_uuid(&blob).unwrap(), id);
    }

    #[test]
    fn wrong_length_blob_is_rejected() {
        assert!(blob_to_uuid(&[0u8; 15]).is_err());
        assert!(blob_to_uuid(&[]).is_err());
    }

    #[test]
    fn sid_round_trips_and_negative_is_rejected() {
        assert_eq!(i64_to_sid(sid_to_i64(Sid(4_000_000_000))).unwrap(), Sid(4_000_000_000));
        assert!(i64_to_sid(-1).is_err(), "номер строки не бывает отрицательным");
        assert!(
            i64_to_sid(i64::from(u32::MAX) + 1).is_err(),
            "Sid — u32, значение шире него потерялось бы молча"
        );
    }
}
