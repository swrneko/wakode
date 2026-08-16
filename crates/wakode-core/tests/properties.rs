//! Property-тесты движка длительностей.
//!
//! Юнит-тесты в `src/intervals.rs` проверяют придуманные примеры; здесь
//! проверяются инварианты на сгенерированных сценариях. Генераторы намеренно
//! злые: отметки вплотную и с нулевым шагом, шаги ровно на границе таймаута и
//! на микросекунду мимо неё, произвольный порядок доставки, длинные серии,
//! времена у краёв `i64` — там, где включается saturating-арифметика.

use proptest::prelude::*;
use wakode_core::{
    build_intervals, Attrs, Category, DurationConfig, EntityKind, Heartbeat, Interval, Micros, Sid,
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
/// таймаут, а именно там живут ошибки на единицу. При нулевой добавке шаг
/// `padding - 1` даёт `-1` — время идёт назад; это допустимый вход, движок
/// обязан сортировать сам.
fn arb_delta(timeout: i64, padding: i64) -> impl Strategy<Value = i64> {
    prop_oneof![
        3 => Just(0i64),                        // дубликат по времени
        2 => Just(1i64),                        // вплотную
        4 => Just(timeout),                     // ровно таймаут — ещё не разрыв
        4 => Just(timeout.saturating_add(1)),   // на микросекунду больше — разрыв
        2 => Just(timeout.saturating_sub(1)),
        2 => Just(padding),
        2 => Just(padding.saturating_add(1)),
        2 => Just(padding.saturating_sub(1)),
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

/// Куба — зона, где обе ловушки перевода часов приходятся ровно на полночь:
/// 8 марта 2026 местных 00:00 не существует вовсе (часы прыгают на 01:00), а
/// 1 ноября они случаются дважды. Свойства ниже проверяются на ней.
const HAVANA: &str = "America/Havana";

fn tz() -> chrono_tz::Tz {
    HAVANA.parse().expect("зона есть в базе IANA")
}

fn at(iso: &str) -> i64 {
    iso.parse::<chrono::DateTime<chrono::Utc>>()
        .expect("валидная дата")
        .timestamp_micros()
}

/// Времена для календарных свойств. В отличие от `arb_start`, края `i64` сюда
/// не попадают: у таких времён локальной даты не существует вовсе. Зато
/// диапазон намеренно сгущён вокруг переводов часов — иначе генератор почти
/// никогда бы в них не попал.
fn arb_calendar_start() -> impl Strategy<Value = i64> {
    let spring_forward = at("2026-03-08T05:00:00Z");
    let fall_back = at("2026-11-01T05:00:00Z");
    let window = 6 * 3600 * SEC;
    prop_oneof![
        2 => (1_577_836_800i64 * SEC)..=(1_893_456_000i64 * SEC), // 2020..2030
        3 => (spring_forward - window)..=(spring_forward + window),
        3 => (fall_back - window)..=(fall_back + window),
    ]
}

/// Тот же сценарий, что и `arb_scenario`, но во временах, у которых есть
/// календарная дата.
fn arb_calendar_scenario() -> impl Strategy<Value = (DurationConfig, Vec<Heartbeat>)> {
    arb_config().prop_flat_map(|cfg| {
        let timeout = cfg.timeout().get();
        let padding = cfg.tail_padding().get();
        let stream = (
            arb_calendar_start(),
            prop::collection::vec((arb_delta(timeout, padding), arb_attrs()), 0..64),
        )
            .prop_map(|(start, steps)| {
                let mut out = Vec::with_capacity(steps.len());
                let mut time = Micros::new(start);
                for (delta, attrs) in steps {
                    out.push(Heartbeat { time, attrs });
                    time = time.saturating_add(Micros::new(delta));
                }
                out
            })
            .prop_shuffle();
        (Just(cfg), stream)
    })
}

fn total(intervals: &[Interval]) -> i64 {
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

    /// Хвост сессии равен ровно настроенной добавке — ни больше, ни меньше.
    ///
    /// Без этого свойства `tail_padding` не проверялся вообще: движок, который
    /// игнорирует настройку и добивает хвост полным таймаутом, и движок,
    /// который молча выбрасывает добавку, проходят все остальные свойства.
    /// Оценка снизу тут не помогает: `padding <= timeout` всегда, поэтому
    /// раздутый до таймаута хвост любую нижнюю границу удовлетворяет. Нужна
    /// именно двусторонняя привязка, то есть точный интервал.
    ///
    /// Границы сессий тест не вычисляет: условие «следующей отметки нет или до
    /// неё дальше таймаута» — это определение разрыва из контракта
    /// `build_intervals`, а не деталь его реализации.
    #[test]
    fn session_tail_is_exactly_the_configured_padding((cfg, hbs) in arb_scenario()) {
        let padding = cfg.tail_padding();
        if padding == Micros::ZERO {
            // Нулевая добавка хвостовых интервалов не порождает вовсе —
            // это проверяет юнит-тест `zero_padding_produces_no_tail_interval`.
            return Ok(());
        }

        let mut sorted = hbs.clone();
        sorted.sort();
        let intervals = build_intervals(&hbs, cfg);

        for (i, hb) in sorted.iter().enumerate() {
            let closes_session = match sorted.get(i + 1) {
                Some(next) => next.time.saturating_sub(hb.time) > cfg.timeout(),
                None => true,
            };
            // У самого края `i64` добавка не помещается и хвост укорачивается
            // насыщением; точное значение там не определено, а непревышение
            // таймаута проверяет соседнее свойство.
            let Some(end) = hb.time.get().checked_add(padding.get()) else {
                continue;
            };
            if !closes_session {
                continue;
            }

            let expected = Interval { start: hb.time, end: Micros::new(end), attrs: hb.attrs };
            prop_assert!(
                intervals.contains(&expected),
                "сессия, закрытая отметкой {:?}, не получила хвост ровно в {:?}; получено {:?}",
                hb,
                padding,
                intervals.iter().filter(|iv| iv.start == hb.time).collect::<Vec<_>>()
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

    /// Сумма по дням равна сумме за весь период. Это тот самый инвариант,
    /// который ловит потерю времени на границе полуночи.
    #[test]
    fn daily_totals_sum_to_period_total((cfg, hbs) in arb_calendar_scenario()) {
        let intervals = build_intervals(&hbs, cfg);
        let whole = total(&intervals);
        let by_days: i64 = wakode_core::split_by_local_day(&intervals, tz())
            .values()
            .flatten()
            .map(|piece| piece.duration().get())
            .sum();

        prop_assert_eq!(whole, by_days);
    }

    /// Каждый кусок целиком лежит внутри тех суток, под которыми записан.
    ///
    /// Без этого свойства сходимость суммы ничего не доказывает: движок,
    /// сваливающий всё в одну дату, сумму сохраняет, а цифры дня врут.
    #[test]
    fn every_piece_stays_within_its_local_day((cfg, hbs) in arb_calendar_scenario()) {
        let intervals = build_intervals(&hbs, cfg);
        for (date, pieces) in wakode_core::split_by_local_day(&intervals, tz()) {
            let (day_start, day_end) = wakode_core::local_day_bounds(date, tz());
            for piece in pieces {
                prop_assert!(
                    piece.start >= day_start && piece.end <= day_end,
                    "кусок {:?} выходит за границы суток {} ({:?}..{:?})",
                    piece,
                    date,
                    day_start,
                    day_end
                );
                prop_assert_eq!(wakode_core::local_date_of(piece.start, tz()), date);
            }
        }
    }
}
