use std::collections::HashMap;
use std::hash::Hash;

use crate::{Attrs, Interval, Micros};

/// Сумма длительности интервалов, попавших в одну группу.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bucket<K> {
    pub key: K,
    pub total: Micros,
}

/// Суммирует интервалы по произвольному признаку.
///
/// Измерение задаётся замыканием, а не перечислением: проекты, языки, редакторы
/// и категории различаются только тем, какое поле из атрибутов взять, и заводить
/// на это отдельный тип означало бы дублировать одну и ту же функцию несколько раз.
pub fn aggregate_by<K, F>(intervals: &[Interval], key_of: F) -> Vec<Bucket<K>>
where
    K: Copy + Eq + Hash + Ord,
    F: Fn(&Attrs) -> K,
{
    let mut totals: HashMap<K, Micros> = HashMap::new();
    for iv in intervals {
        let entry = totals.entry(key_of(&iv.attrs)).or_insert(Micros::ZERO);
        *entry = entry.saturating_add(iv.duration());
    }

    let mut out: Vec<Bucket<K>> = totals
        .into_iter()
        .map(|(key, total)| Bucket { key, total })
        .collect();
    // Сначала по убыванию времени, при равенстве — по ключу: результат обязан
    // быть детерминированным, иначе снапшот-тесты совместимого слоя поплывут.
    out.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.key.cmp(&b.key)));
    out
}

/// Суммарная длительность всех интервалов без группировки.
pub fn grand_total(intervals: &[Interval]) -> Micros {
    intervals
        .iter()
        .map(|iv| iv.duration())
        .fold(Micros::ZERO, Micros::saturating_add)
}

/// Доля `part` от `whole` в процентах.
///
/// Пустой вход — реальный случай (например, за выбранный день не было
/// активности): `whole` тогда нулевой, и делить не на что. `0.0` — разумное
/// значение по умолчанию, а не признак ошибки.
pub fn percent(part: Micros, whole: Micros) -> f64 {
    if whole.get() == 0 {
        return 0.0;
    }
    part.get() as f64 * 100.0 / whole.get() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attrs, Category, EntityKind, Sid};

    fn interval(start: i64, end: i64, project: u32) -> Interval {
        Interval {
            start: Micros::from_secs(start),
            end: Micros::from_secs(end),
            attrs: Attrs {
                entity: Sid(project),
                kind: EntityKind::File,
                category: Category::Coding,
                project: Some(Sid(project)),
                branch: None,
                language: None,
                editor: None,
                os: None,
                machine: None,
            },
        }
    }

    /// Интервал длиной почти в `i64::MAX` микросекунд — используется для проверки
    /// того, что суммирование не переполняется, а насыщается.
    fn huge_interval(project: u32) -> Interval {
        Interval {
            start: Micros::ZERO,
            end: Micros::new(i64::MAX),
            attrs: Attrs {
                entity: Sid(project),
                kind: EntityKind::File,
                category: Category::Coding,
                project: Some(Sid(project)),
                branch: None,
                language: None,
                editor: None,
                os: None,
                machine: None,
            },
        }
    }

    fn interval_without_project(start: i64, end: i64) -> Interval {
        Interval {
            start: Micros::from_secs(start),
            end: Micros::from_secs(end),
            attrs: Attrs {
                entity: Sid(0),
                kind: EntityKind::File,
                category: Category::Coding,
                project: None,
                branch: None,
                language: None,
                editor: None,
                os: None,
                machine: None,
            },
        }
    }

    #[test]
    fn sums_intervals_per_key() {
        let intervals = [interval(0, 60, 1), interval(60, 120, 2), interval(120, 200, 1)];
        let buckets = aggregate_by(&intervals, |a| a.project);

        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].key, Some(Sid(1)));
        assert_eq!(buckets[0].total, Micros::from_secs(140));
        assert_eq!(buckets[1].key, Some(Sid(2)));
        assert_eq!(buckets[1].total, Micros::from_secs(60));
    }

    #[test]
    fn buckets_are_sorted_by_total_descending_then_by_key() {
        // Детерминированный порядок нужен снапшот-тестам совместимого слоя.
        let intervals = [interval(0, 60, 3), interval(60, 120, 1), interval(120, 180, 2)];
        let buckets = aggregate_by(&intervals, |a| a.project);

        let keys: Vec<_> = buckets.iter().map(|b| b.key).collect();
        assert_eq!(keys, vec![Some(Sid(1)), Some(Sid(2)), Some(Sid(3))]);
    }

    #[test]
    fn larger_total_sorts_before_smaller_total_regardless_of_key() {
        // buckets_are_sorted_by_total_descending_then_by_key выше составлен из
        // интервалов одинаковой длины и на самом деле проверяет только разрыв
        // ничьей по ключу. Здесь ключ с меньшим числом (1) должен уступить
        // место ключу с большим числом (9), потому что у него больше суммарное
        // время — сортировка по total первична, по key вторична.
        let intervals = [interval(0, 60, 1), interval(0, 600, 9)];
        let buckets = aggregate_by(&intervals, |a| a.project);

        let keys: Vec<_> = buckets.iter().map(|b| b.key).collect();
        assert_eq!(keys, vec![Some(Sid(9)), Some(Sid(1))]);
    }

    #[test]
    fn bucket_totals_sum_to_grand_total() {
        let intervals = [interval(0, 60, 1), interval(60, 120, 2), interval(120, 200, 1)];
        let sum: i64 = aggregate_by(&intervals, |a| a.project)
            .iter()
            .map(|b| b.total.get())
            .sum();

        assert_eq!(sum, grand_total(&intervals).get());
    }

    #[test]
    fn empty_input_produces_no_buckets() {
        let buckets = aggregate_by(&[], |a: &Attrs| a.project);
        assert!(buckets.is_empty());
        assert_eq!(grand_total(&[]), Micros::ZERO);
    }

    #[test]
    fn percent_of_zero_whole_is_zero() {
        assert_eq!(percent(Micros::from_secs(10), Micros::ZERO), 0.0);
    }

    #[test]
    fn percent_is_computed_against_the_whole() {
        assert_eq!(percent(Micros::from_secs(25), Micros::from_secs(100)), 25.0);
    }

    #[test]
    fn interval_without_the_attribute_is_grouped_under_none() {
        // Не всякая отметка несёт проект (например, событие без открытого
        // файла). Такой интервал не выпадает из сводки молча — он оседает в
        // своей группе под ключом None, и сумма по группам по-прежнему
        // сходится с общим итогом.
        let intervals = [interval(0, 60, 1), interval_without_project(60, 100)];
        let buckets = aggregate_by(&intervals, |a| a.project);

        assert_eq!(buckets.len(), 2);
        let none_bucket = buckets.iter().find(|b| b.key.is_none()).unwrap();
        assert_eq!(none_bucket.total, Micros::from_secs(40));

        let sum: i64 = buckets.iter().map(|b| b.total.get()).sum();
        assert_eq!(sum, grand_total(&intervals).get());
    }

    #[test]
    fn aggregate_by_saturates_instead_of_overflowing() {
        // Две длительности, каждая почти i64::MAX, попадают в один бакет.
        // Сырое сложение i64 переполнилось бы; Micros обязан насытиться.
        let intervals = [huge_interval(1), huge_interval(1)];
        let buckets = aggregate_by(&intervals, |a| a.project);

        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].total, Micros::new(i64::MAX));
    }

    #[test]
    fn grand_total_saturates_instead_of_overflowing() {
        let intervals = [huge_interval(1), huge_interval(2)];
        assert_eq!(grand_total(&intervals), Micros::new(i64::MAX));
    }
}
