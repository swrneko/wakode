//! Property-тесты движка длительностей.
//!
//! Юнит-тесты в `src/intervals.rs` проверяют придуманные примеры; здесь
//! проверяются инварианты на сгенерированных сценариях. Генераторы намеренно
//! злые: отметки вплотную и с нулевым шагом, шаги ровно на границе таймаута и
//! на микросекунду мимо неё, произвольный порядок доставки, длинные серии,
//! времена у краёв `i64` — там, где включается saturating-арифметика.

use proptest::prelude::*;
use wakode_core::{
    build_intervals, Attrs, Category, DurationConfig, EntityKind, Heartbeat, Micros, Sid,
};

const SEC: i64 = 1_000_000;

/// Атрибуты из маленьких диапазонов: так генератор чаще создаёт полные
/// совпадения отметок и коллизии по времени — именно на них ломается склейка.
fn arb_attrs() -> impl Strategy<Value = Attrs> {
    (
        0u32..4,
        prop::option::of(0u32..3),
        prop::option::of(0u32..3),
        prop_oneof![Just(EntityKind::File), Just(EntityKind::App)],
        prop_oneof![
            Just(Category::Coding),
            Just(Category::Debugging),
            Just(Category::Browsing),
        ],
    )
        .prop_map(|(entity, project, language, kind, category)| Attrs {
            entity: Sid(entity),
            kind,
            category,
            project: project.map(Sid),
            branch: None,
            language: language.map(Sid),
            editor: None,
            os: None,
            machine: None,
        })
}

/// Конфигурации от вырожденных (таймаут в одну микросекунду) до реалистичных,
/// с частым попаданием в границу инварианта `tail_padding == timeout`.
fn arb_config() -> impl Strategy<Value = DurationConfig> {
    prop_oneof![
        1 => Just(1i64),
        1 => Just(SEC),
        3 => Just(900 * SEC),
        3 => 1i64..=(3600 * SEC),
    ]
    .prop_flat_map(|timeout| {
        let padding = prop_oneof![
            2 => Just(0i64),
            2 => Just(timeout),
            3 => 0i64..=timeout,
        ];
        (Just(timeout), padding)
    })
    .prop_map(|(timeout, padding)| {
        DurationConfig::new(Micros::new(timeout), Micros::new(padding))
            .expect("генератор обязан соблюдать инвариант tail_padding <= timeout")
    })
}

/// Шаг между соседними отметками. Смещён к границам, а не к «приличной»
/// равномерной случайности: равномерный шаг почти никогда не попадёт ровно в
/// таймаут, а именно там живут ошибки на единицу.
fn arb_delta(timeout: i64, padding: i64) -> impl Strategy<Value = i64> {
    prop_oneof![
        3 => Just(0i64),                        // дубликат по времени
        2 => Just(1i64),                        // вплотную
        4 => Just(timeout),                     // ровно таймаут — ещё не разрыв
        4 => Just(timeout.saturating_add(1)),   // на микросекунду больше — разрыв
        2 => Just(timeout.saturating_sub(1)),
        2 => Just(padding),
        4 => 0i64..=timeout.saturating_mul(3),
        2 => 0i64..=(86_400 * SEC),             // длинные простои
    ]
}

/// Начало серии: обычные времена и края диапазона `i64`.
fn arb_start() -> impl Strategy<Value = i64> {
    prop_oneof![
        4 => 0i64..=(2_000_000 * SEC),
        1 => Just(i64::MAX - 5 * SEC),
        1 => Just(i64::MIN + 5 * SEC),
        1 => any::<i64>(),
    ]
}

/// Отдельные отметки с произвольным абсолютным временем — они подмешиваются в
/// серию, чтобы в одном входе встречались значения у обоих краёв `i64` и
/// разность между ними насыщалась.
fn arb_wild_time() -> impl Strategy<Value = i64> {
    prop_oneof![
        1 => Just(i64::MAX),
        1 => Just(i64::MIN),
        1 => Just(0i64),
        3 => any::<i64>(),
    ]
}

/// Сценарий: конфигурация и поток отметок, сгенерированный под неё.
///
/// Шаги зависят от таймаута конкретной конфигурации — иначе граничные случаи
/// («разрыв ровно в таймаут») не воспроизводились бы вовсе. Порядок отметок на
/// выходе перемешан: движок обязан сортировать вход сам.
fn arb_scenario() -> impl Strategy<Value = (DurationConfig, Vec<Heartbeat>)> {
    arb_config().prop_flat_map(|cfg| {
        let timeout = cfg.timeout().get();
        let padding = cfg.tail_padding().get();
        let stream = (
            arb_start(),
            prop::collection::vec((arb_delta(timeout, padding), arb_attrs()), 0..128),
            prop::collection::vec((arb_wild_time(), arb_attrs()), 0..4),
        )
            .prop_map(|(start, steps, wild)| {
                let mut out = Vec::with_capacity(steps.len() + wild.len());
                let mut time = Micros::new(start);
                for (delta, attrs) in steps {
                    out.push(Heartbeat { time, attrs });
                    time = time.saturating_add(Micros::new(delta));
                }
                out.extend(
                    wild.into_iter()
                        .map(|(time, attrs)| Heartbeat { time: Micros::new(time), attrs }),
                );
                out
            })
            .prop_shuffle();
        (Just(cfg), stream)
    })
}

/// Тот же сценарий плюс независимая перестановка того же потока отметок.
fn arb_scenario_with_permutation(
) -> impl Strategy<Value = (DurationConfig, Vec<Heartbeat>, Vec<Heartbeat>)> {
    arb_scenario().prop_flat_map(|(cfg, hbs)| {
        let shuffled = Just(hbs.clone()).prop_shuffle();
        (Just(cfg), Just(hbs), shuffled)
    })
}

fn total(intervals: &[wakode_core::Interval]) -> i64 {
    intervals.iter().map(|iv| iv.duration().get()).sum()
}

proptest! {
    /// Ни один интервал не может быть длиннее таймаута: всё, что длиннее,
    /// означает, что пауза была засчитана как работа.
    #[test]
    fn no_interval_exceeds_timeout((cfg, hbs) in arb_scenario()) {
        for iv in build_intervals(&hbs, cfg) {
            prop_assert!(
                iv.duration() <= cfg.timeout(),
                "интервал {:?} длиннее таймаута {:?}",
                iv,
                cfg.timeout()
            );
        }
    }

    /// Интервалы не пересекаются — иначе одно и то же время засчитывается
    /// дважды и сумма по проектам разъезжается с итогом. Ради этого инварианта
    /// и существует ограничение `tail_padding <= timeout`.
    #[test]
    fn intervals_never_overlap((cfg, hbs) in arb_scenario()) {
        let intervals = build_intervals(&hbs, cfg);
        for pair in intervals.windows(2) {
            prop_assert!(
                pair[0].end <= pair[1].start,
                "интервалы пересекаются: {:?} и {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    /// Порядок доставки не влияет на результат: оффлайн-очередь присылает
    /// отметки как попало.
    #[test]
    fn result_is_invariant_under_permutation(
        (cfg, hbs, shuffled) in arb_scenario_with_permutation(),
    ) {
        prop_assert_eq!(build_intervals(&hbs, cfg), build_intervals(&shuffled, cfg));
    }

    /// Повторная доставка того же батча не меняет ничего.
    #[test]
    fn result_is_invariant_under_duplication((cfg, hbs) in arb_scenario()) {
        let mut doubled = hbs.clone();
        doubled.extend(hbs.iter().copied());
        prop_assert_eq!(build_intervals(&hbs, cfg), build_intervals(&doubled, cfg));
    }

    /// Добавление отметки не может уменьшить суммарное время: новая отметка
    /// либо делит существующий интервал, либо продлевает сессию, но никогда не
    /// стирает уже засчитанную работу.
    #[test]
    fn adding_a_heartbeat_never_reduces_total(
        (cfg, hbs) in arb_scenario(),
        extra_time in arb_wild_time(),
        extra_attrs in arb_attrs(),
    ) {
        let before = total(&build_intervals(&hbs, cfg));
        let mut more = hbs.clone();
        more.push(Heartbeat { time: Micros::new(extra_time), attrs: extra_attrs });
        let after = total(&build_intervals(&more, cfg));
        prop_assert!(after >= before, "было {}, стало {}", before, after);
    }

    /// Движок ничего не выдумывает: каждый интервал начинается в момент
    /// реально пришедшей отметки и несёт именно её атрибуты. Атрибуты берутся
    /// у более ранней из пары — промежуток принадлежит тому, что было открыто
    /// раньше, а не тому, куда пользователь только что перешёл.
    #[test]
    fn every_interval_is_anchored_to_a_real_heartbeat((cfg, hbs) in arb_scenario()) {
        for iv in build_intervals(&hbs, cfg) {
            prop_assert!(
                hbs.iter().any(|hb| hb.time == iv.start && hb.attrs == iv.attrs),
                "интервал {:?} не соответствует ни одной входной отметке",
                iv
            );
        }
    }

    /// Промежуток между соседними отметками в пределах таймаута обязан попасть
    /// в результат целиком.
    ///
    /// Без этого свойства все остальные выполнялись бы и для движка, который
    /// всегда возвращает пустой список: непересечение, ограничение по длине,
    /// независимость от порядка и монотонность на пустом ответе тривиальны.
    #[test]
    fn no_time_within_timeout_is_lost((cfg, hbs) in arb_scenario()) {
        let mut sorted = hbs.clone();
        sorted.sort();
        let expected: i64 = sorted
            .windows(2)
            .map(|w| w[1].time.saturating_sub(w[0].time))
            .filter(|gap| *gap <= cfg.timeout())
            .map(|gap| gap.get())
            .sum();

        let counted = total(&build_intervals(&hbs, cfg));
        prop_assert!(
            counted >= expected,
            "засчитано {}, а только промежутки в пределах таймаута дают {}",
            counted,
            expected
        );
    }
}
