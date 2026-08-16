//! Property-тесты конвейера: склейка, нарезка по дням, агрегация.
//!
//! Юнит-тесты в `src/` проверяют придуманные примеры; здесь проверяются
//! инварианты на сгенерированных сценариях. Генераторы намеренно злые: отметки
//! вплотную и с нулевым шагом, шаги ровно на границе таймаута и на микросекунду
//! мимо неё, произвольный порядок доставки, длинные серии, времена у краёв
//! `i64` — там, где включается saturating-арифметика.
//!
//! Отдельная забота — свойства, проходящие через весь конвейер целиком
//! (`build_intervals` → `split_by_local_day` → `aggregate_by`). Ровно так его
//! будут звать планы 2–4, и ровно этот крейт станет эталоном, по которому план
//! 2 сверяет свой кэш агрегатов. Инвариант, проверенный на пути, которым никто
//! не ходит, эталоном быть не может.

use std::collections::BTreeMap;

use proptest::prelude::*;
use wakode_core::{
    aggregate_by, build_intervals, grand_total, Attrs, Category, DurationConfig, EntityKind,
    Heartbeat, Interval, Micros, Sid,
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

/// Зоны для календарных свойств и моменты переводов часов, вокруг которых
/// сгущается генератор: равномерной случайностью в перевод почти не попасть.
///
/// Набор подобран по типам ловушек, а не по красоте названий: одной зоны мало
/// — именно нехватка второй зоны в этом наборе однажды пропустила ошибку с
/// отматыванием часов за полночь.
const CALENDAR_CASES: &[(&str, &str)] = &[
    // Летнее время кончается в местные 00:01 и отматывает часы за полночь:
    // локальная дата снова становится вчерашней уже внутри новых суток.
    ("America/Goose_Bay", "2009-11-01T03:01:00Z"),
    ("America/St_Johns", "2010-11-07T02:31:00Z"),
    // Обе полуночные ловушки сразу: 8 марта полуночи нет вовсе, 1 ноября она
    // случается дважды.
    ("America/Havana", "2026-03-08T05:00:00Z"),
    ("America/Havana", "2026-11-01T05:00:00Z"),
    // Полночь пропущена переводом вперёд.
    ("America/Santiago", "2026-09-06T04:00:00Z"),
    // Дыра кончается на нецелой минуте: отказ от смещения −0:44:30.
    ("Africa/Monrovia", "1972-01-07T00:44:30Z"),
    // Постоянное смещение без переводов вовсе — контрольная зона.
    ("Asia/Kolkata", "2026-08-15T00:00:00Z"),
];

/// Зона, на которой проверяются времена у краёв `i64`: с переводами часов,
/// чтобы насыщение календаря сталкивалось ещё и с ними.
const WILD_ZONE: &str = "America/Goose_Bay";

fn zone(name: &str) -> chrono_tz::Tz {
    name.parse().expect("зона есть в базе IANA")
}

fn at(iso: &str) -> i64 {
    iso.parse::<chrono::DateTime<chrono::Utc>>()
        .expect("валидная дата")
        .timestamp_micros()
}

/// Зона и время начала серии: рядом с её переводом часов либо просто в
/// двадцатых годах.
fn arb_calendar_case() -> impl Strategy<Value = (chrono_tz::Tz, i64)> {
    prop::sample::select(CALENDAR_CASES.to_vec()).prop_flat_map(|(name, anchor)| {
        let anchor = at(anchor);
        let window = 6 * 3600 * SEC;
        (
            Just(zone(name)),
            prop_oneof![
                3 => (anchor - window)..=(anchor + window),
                1 => (1_577_836_800i64 * SEC)..=(1_893_456_000i64 * SEC), // 2020..2030
            ],
        )
    })
}

/// Тот же сценарий, что и `arb_scenario`, но привязанный к конкретной зоне и
/// к окрестностям её перевода часов.
fn arb_calendar_scenario() -> impl Strategy<Value = (chrono_tz::Tz, DurationConfig, Vec<Heartbeat>)>
{
    (arb_calendar_case(), arb_config()).prop_flat_map(|((tz, start), cfg)| {
        let timeout = cfg.timeout().get();
        let padding = cfg.tail_padding().get();
        let stream = prop::collection::vec((arb_delta(timeout, padding), arb_attrs()), 0..64)
            .prop_map(move |steps| {
                let mut out = Vec::with_capacity(steps.len());
                let mut time = Micros::new(start);
                for (delta, attrs) in steps {
                    out.push(Heartbeat { time, attrs });
                    time = time.saturating_add(Micros::new(delta));
                }
                out
            })
            .prop_shuffle();
        (Just(tz), Just(cfg), stream)
    })
}

fn total(intervals: &[Interval]) -> i64 {
    intervals.iter().map(|iv| iv.duration().get()).sum()
}

/// Сходится ли сумма бакетов с общим итогом при группировке этим ключом.
///
/// Насыщение здесь не мешает: `aggregate_by` насыщает побакетно, а
/// `grand_total` — глобально, и на суммах под `i64::MAX` они обязаны совпадать.
/// Календарный генератор до края `i64` не дотягивается — времена в нём
/// настоящие, а интервал не длиннее таймаута, — поэтому равенство точное.
fn reassembles<K>(intervals: &[Interval], key_of: impl Fn(&Attrs) -> K) -> bool
where
    K: Copy + Eq + std::hash::Hash + Ord,
{
    let by_key: i64 = aggregate_by(intervals, key_of).iter().map(|b| b.total.get()).sum();
    by_key == grand_total(intervals).get()
}

/// Итоги по проектам в виде карты — так их удобно складывать по дням.
fn totals_by_project(intervals: &[Interval]) -> BTreeMap<Option<Sid>, i64> {
    aggregate_by(intervals, |a| a.project)
        .into_iter()
        .map(|bucket| (bucket.key, bucket.total.get()))
        .collect()
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
    fn daily_totals_sum_to_period_total((tz, cfg, hbs) in arb_calendar_scenario()) {
        let intervals = build_intervals(&hbs, cfg);
        let whole = total(&intervals);
        let by_days: i64 = wakode_core::split_by_local_day(&intervals, tz)
            .values()
            .flatten()
            .map(|piece| piece.duration().get())
            .sum();

        prop_assert_eq!(whole, by_days);
    }

    /// Каждый кусок целиком лежит внутри тех суток, под которыми записан, и ни
    /// один не вывернут наизнанку.
    ///
    /// Без этого свойства сходимость суммы не доказывает ничего: и движок,
    /// сваливающий всё в одну дату, и движок, выдающий кусок с концом раньше
    /// начала (а соседу отдающий лишнее), сумму сохраняют — врут только цифры
    /// дня. Проверка порядка тут не педантизм: ровно так выглядела ошибка на
    /// зонах, отматывающих часы за полночь.
    #[test]
    fn every_piece_stays_within_its_local_day((tz, cfg, hbs) in arb_calendar_scenario()) {
        let intervals = build_intervals(&hbs, cfg);
        for (date, pieces) in wakode_core::split_by_local_day(&intervals, tz) {
            let (day_start, day_end) = wakode_core::local_day_bounds(date, tz);
            for piece in pieces {
                prop_assert!(
                    piece.start < piece.end,
                    "кусок {:?} вывернут наизнанку в сутках {}",
                    piece,
                    date
                );
                prop_assert!(
                    piece.start >= day_start && piece.end <= day_end,
                    "кусок {:?} выходит за границы суток {} ({:?}..{:?})",
                    piece,
                    date,
                    day_start,
                    day_end
                );
                // Дата куска выводится из вложенности, а `local_date_of` даёт
                // для неё лишь нижнюю оценку: заново прожитый конец вчерашней
                // даты лежит уже внутри новых суток.
                prop_assert!(
                    wakode_core::local_date_of(piece.start, tz) <= date,
                    "дата момента {:?} опережает сутки {}",
                    piece.start,
                    date
                );
            }
        }
    }

    /// Нарезка переживает времена у краёв `i64`.
    ///
    /// Здесь намеренно берётся `arb_scenario` — тот самый злой генератор, что
    /// доходит до `i64::MIN` и `i64::MAX`. Календарь уже туда не дотягивается,
    /// но крайние значения крейт насыщает, а не отвергает, и роняться на них
    /// он не имеет права: обходить свой же вход стороной — значит не проверять
    /// его вовсе.
    #[test]
    fn splitting_survives_timestamps_outside_the_calendar((cfg, hbs) in arb_scenario()) {
        let intervals = build_intervals(&hbs, cfg);
        let days = wakode_core::split_by_local_day(&intervals, zone(WILD_ZONE));
        let mut counted = 0i64;
        for piece in days.values().flatten() {
            prop_assert!(piece.start < piece.end, "кусок {:?} вывернут наизнанку", piece);
            counted += piece.duration().get();
        }

        prop_assert_eq!(total(&intervals), counted);
    }

    /// Сумма по бакетам равна общему итогу — при группировке по любому ключу.
    ///
    /// Это первый названный инвариант спеки, и до сих пор его держал один
    /// юнит-тест на трёх интервалах, написанных руками. Ключи взяты разные не
    /// для симметрии: измерения различаются тем, теряется ли время у
    /// интервалов без значения. `editor` в генераторе всегда `None`, `project`
    /// и `language` бывают пустыми через раз, а `kind` не пуст никогда —
    /// движок, роняющий безымянные интервалы, обязан провалиться хотя бы на
    /// одном из них.
    #[test]
    fn bucket_totals_always_reassemble_into_the_grand_total(
        (_tz, cfg, hbs) in arb_calendar_scenario(),
    ) {
        let intervals = build_intervals(&hbs, cfg);

        prop_assert!(reassembles(&intervals, |a| a.project), "разъехалось по проектам");
        prop_assert!(reassembles(&intervals, |a| a.language), "разъехалось по языкам");
        prop_assert!(reassembles(&intervals, |a| a.editor), "разъехалось по редакторам");
        prop_assert!(reassembles(&intervals, |a| a.category), "разъехалось по категориям");
        prop_assert!(reassembles(&intervals, |a| a.kind), "разъехалось по типу сущности");
    }

    /// Посуточная агрегация складывается в агрегацию за период.
    ///
    /// Это ровно то число, ради которого план 2 заводит кэш: «сколько времени
    /// в проекте X за день D». Сумма таких чисел по дням обязана дать итог
    /// проекта X за весь период — иначе недельная сводка не сойдётся с семью
    /// дневными, и разойдутся они молча.
    ///
    /// Свойство строго сильнее сходимости общих итогов (её проверяет
    /// `daily_totals_sum_to_period_total`): совпадение сумм переживает и
    /// перепутанные при нарезке атрибуты, а совпадение по каждому ключу — нет.
    #[test]
    fn per_day_aggregation_reassembles_into_the_period_aggregation(
        (tz, cfg, hbs) in arb_calendar_scenario(),
    ) {
        let intervals = build_intervals(&hbs, cfg);
        let over_the_period = totals_by_project(&intervals);

        let mut summed_over_days: BTreeMap<Option<Sid>, i64> = BTreeMap::new();
        for pieces in wakode_core::split_by_local_day(&intervals, tz).values() {
            for (key, total) in totals_by_project(pieces) {
                *summed_over_days.entry(key).or_insert(0) += total;
            }
        }
        // Проекты с нулевым итогом за период в карте не появляются вовсе, а по
        // дням могут дать нулевую запись — сравниваются ненулевые части.
        summed_over_days.retain(|_, total| *total != 0);

        prop_assert_eq!(summed_over_days, over_the_period);
    }

    /// Куски одного дня идут по возрастанию и не наезжают друг на друга.
    ///
    /// Непересечение интервалов проверяется до нарезки, но нарезка способна его
    /// потерять сама: достаточно отдать в день кусок, начинающийся раньше
    /// предыдущего. Сумма при этом сойдётся, а лента дня и любой расчёт
    /// «сколько времени подряд» соврут.
    #[test]
    fn pieces_of_a_day_are_ordered_and_never_overlap(
        (tz, cfg, hbs) in arb_calendar_scenario(),
    ) {
        let intervals = build_intervals(&hbs, cfg);
        for (date, pieces) in wakode_core::split_by_local_day(&intervals, tz) {
            for pair in pieces.windows(2) {
                prop_assert!(
                    pair[0].end <= pair[1].start,
                    "в сутках {} куски {:?} и {:?} идут не по порядку или пересекаются",
                    date,
                    pair[0],
                    pair[1]
                );
            }
        }
    }
}
