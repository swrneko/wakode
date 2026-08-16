//! Детерминированный хеш дедупликации отметок.

use uuid::Uuid;
use wakode_core::{Attrs, Micros, Sid};
use xxhash_rust::xxh3::Xxh3;

use crate::codec::{category_to_i64, kind_to_i64, sid_to_i64};

/// Хеш содержимого отметки для уникального индекса `hb_dedup`.
///
/// Алгоритм и порядок полей — часть формата хранения. Их изменение ломает
/// дедупликацию у всех существующих баз так же, как правка применённой
/// миграции ломает схему. `DefaultHasher` из стандартной библиотеки тут
/// неприменим: он прямо документирован как нестабильный между релизами Rust.
///
/// Возвращается `i64`, потому что колонка в SQLite целочисленная и знаковая.
/// Верхний бит переносится как есть — это перетолкование битов, не усечение.
pub fn dedup_hash(user: Uuid, time: Micros, attrs: &Attrs, is_write: bool) -> i64 {
    let mut h = Xxh3::new();

    h.update(user.as_bytes());
    h.update(&time.get().to_le_bytes());
    h.update(&sid_to_i64(attrs.entity).to_le_bytes());
    h.update(&kind_to_i64(attrs.kind).to_le_bytes());
    h.update(&category_to_i64(attrs.category).to_le_bytes());
    feed_optional(&mut h, attrs.project);
    feed_optional(&mut h, attrs.branch);
    h.update(&[u8::from(is_write)]);

    h.digest() as i64
}

/// Отсутствие значения кодируется отдельным маркером, а не нулём: иначе
/// «проекта нет» и «проект под номером ноль» дали бы один хеш.
fn feed_optional(h: &mut Xxh3, value: Option<Sid>) {
    match value {
        Some(sid) => {
            h.update(&[1]);
            h.update(&sid_to_i64(sid).to_le_bytes());
        }
        None => h.update(&[0]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wakode_core::{Category, EntityKind, Sid};

    /// Значение, посчитанное текущим алгоритмом. Проверено вручную один раз;
    /// дальше тест сторожит, чтобы оно не поменялось.
    const PINNED: i64 = 6644763553317909985;

    fn attrs() -> Attrs {
        Attrs {
            entity: Sid(1),
            kind: EntityKind::File,
            category: Category::Coding,
            project: Some(Sid(2)),
            branch: Some(Sid(3)),
            language: Some(Sid(4)),
            editor: Some(Sid(5)),
            os: Some(Sid(6)),
            machine: Some(Sid(7)),
        }
    }

    #[test]
    fn same_input_gives_the_same_hash() {
        let user = Uuid::now_v7();
        let time = Micros::from_secs(1_755_000_000);
        assert_eq!(
            dedup_hash(user, time, &attrs(), true),
            dedup_hash(user, time, &attrs(), true)
        );
    }

    #[test]
    fn hash_is_pinned_to_an_exact_number() {
        // Значение зафиксировано намеренно: если оно изменится, у всех
        // существующих баз перестанет работать дедупликация. Тест обязан
        // упасть при подмене алгоритма или порядка полей.
        let user = Uuid::from_bytes([7; 16]);
        let time = Micros::from_secs(1_700_000_000);
        assert_eq!(dedup_hash(user, time, &attrs(), true), PINNED);
    }

    #[test]
    fn every_field_changes_the_hash() {
        let user = Uuid::now_v7();
        let other_user = Uuid::now_v7();
        let time = Micros::from_secs(1_755_000_000);
        let base = dedup_hash(user, time, &attrs(), true);

        assert_ne!(base, dedup_hash(other_user, time, &attrs(), true), "пользователь");
        assert_ne!(base, dedup_hash(user, Micros::from_secs(1), &attrs(), true), "время");
        assert_ne!(base, dedup_hash(user, time, &attrs(), false), "признак записи");

        let mut a = attrs();
        a.entity = Sid(99);
        assert_ne!(base, dedup_hash(user, time, &a, true), "сущность");

        let mut a = attrs();
        a.kind = EntityKind::App;
        assert_ne!(base, dedup_hash(user, time, &a, true), "вид");

        let mut a = attrs();
        a.category = Category::Debugging;
        assert_ne!(base, dedup_hash(user, time, &a, true), "категория");

        let mut a = attrs();
        a.project = Some(Sid(99));
        assert_ne!(base, dedup_hash(user, time, &a, true), "проект");

        let mut a = attrs();
        a.branch = Some(Sid(99));
        assert_ne!(base, dedup_hash(user, time, &a, true), "ветка");
    }

    #[test]
    fn absent_and_zero_are_different() {
        // `None` и `Some(Sid(0))` обязаны разойтись: иначе отметка без проекта
        // склеится с отметкой, у которой проект под номером ноль. У `branch`
        // тот же маркер присутствия в `feed_optional`, что и у `project`, —
        // одна правка в `feed_optional` ломает оба поля сразу, поэтому обе
        // половины проверяются в одном тесте.
        let user = Uuid::now_v7();
        let time = Micros::from_secs(1);

        let mut none = attrs();
        none.project = None;
        let mut zero = attrs();
        zero.project = Some(Sid(0));

        assert_ne!(
            dedup_hash(user, time, &none, true),
            dedup_hash(user, time, &zero, true),
            "проект"
        );

        let mut none = attrs();
        none.branch = None;
        let mut zero = attrs();
        zero.branch = Some(Sid(0));

        assert_ne!(
            dedup_hash(user, time, &none, true),
            dedup_hash(user, time, &zero, true),
            "ветка"
        );
    }

    #[test]
    fn client_environment_stays_out_of_the_hash() {
        // `language`, `editor`, `os`, `machine` выводятся из user-agent
        // клиента, а не описывают саму отметку. Если случайная правка
        // затянет любое из них в хеш, у всех уже сохранённых отметок
        // сменится dedup-хеш: очередь wakatime-cli досылает их заново,
        // уникальный индекс перестаёт их узнавать, и база тихо наполняется
        // дублями задним числом — без единой ошибки в логах.
        let user = Uuid::now_v7();
        let time = Micros::from_secs(1_755_000_000);
        let base = dedup_hash(user, time, &attrs(), true);

        let mut a = attrs();
        a.language = Some(Sid(777));
        assert_eq!(base, dedup_hash(user, time, &a, true), "язык");

        let mut a = attrs();
        a.editor = Some(Sid(777));
        assert_eq!(base, dedup_hash(user, time, &a, true), "редактор");

        let mut a = attrs();
        a.os = Some(Sid(777));
        assert_eq!(base, dedup_hash(user, time, &a, true), "ОС");

        let mut a = attrs();
        a.machine = Some(Sid(777));
        assert_eq!(base, dedup_hash(user, time, &a, true), "машина");
    }
}
