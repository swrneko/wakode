//! `GET /api/v1/users/current/summaries` — сводки по локальным дням.
//!
//! Первый эндпоинт волны, который **считает**, а не перекладывает поля:
//! отметки поднимаются из хранилища, склеиваются в интервалы, режутся по
//! границам локальных суток пользователя и суммируются по шести измерениям.
//! Тот же вычислительный путь переиспользуют `statusbar/today` (задача 6) и
//! `all_time_since_today` (задача 7), поэтому день считает отдельная
//! функция [`day_summary`], а не тело обработчика.

use std::sync::Arc;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::Json;
use chrono::{Datelike, NaiveDate};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use wakode_core::{
    aggregate_by, build_intervals, grand_total, heartbeat_window, local_day_bounds, local_day_of,
    split_by_local_day, Category, DurationConfig, Interval, Micros, Sid,
};
use wakode_store::HeartbeatRepo;

use crate::auth::KeyAuth;
use crate::compat::rfc3339;
use crate::compat::shapes::{
    items, CumulativeTotal, Duration, GrandTotal, Item, MachineItem, ProjectItem,
};
use crate::error::ApiError;
use crate::state::AppState;

/// Предел ширины запрошенного диапазона в днях.
///
/// **Это наш предел, а не WakaTime**, как и `MAX_BATCH` у батча отметок;
/// оба записаны в спеке (`docs/superpowers/specs/2026-08-15-wakode-design.md`).
///
/// Стоит он не ради экономии, а потому что запрос стоит дорого по трём
/// осям сразу, и ни одна из них не ограничена ничем другим:
///
/// 1. **Ответ** растёт линейно по дням независимо от того, есть ли в них
///    работа: пустой день — это девять ключей и семь пустых массивов.
/// 2. **Выборка** отметок идёт одним куском и целиком в память —
///    `heartbeats_in_range` не знает ни пагинации, ни курсора.
/// 3. **Интервалы** строятся поверх всей выборки, ещё одним вектором.
///
/// Тысячелетний диапазон (365 250 дней) — это сотни мегабайт JSON плюс все
/// отметки за тысячу лет рядом с ними, на один аутентифицированный запрос.
/// Год с запасом на високосный покрывает любую разумную панель; тому, кому
/// нужно больше, отвечает `all_time_since_today`.
///
/// Граница закреплена с **обеих** сторон: 366 дней проходят, 367 — нет
/// (`a_range_of_a_thousand_years_is_refused_and_a_year_is_not` и
/// `the_limit_on_the_width_of_a_range_is_exactly_where_it_says`). Без
/// верхней половины предел можно было бы поднять во сколько угодно раз, не
/// уронив ни одного теста.
const MAX_DAYS: i64 = 366;

/// Параметры запроса.
///
/// Оба поля `Option`, хотя оба обязательны: отсутствующий параметр обязан
/// получить **наш** текст отказа, а не английское сообщение `axum` мимо
/// формата `{"error": ...}`, который `ApiError` обещает на всех маршрутах.
///
/// Незнакомый параметр сам по себе не отвергается — по той же причине, по
/// которой их не отвергает разбор отметки: лишнее поле в запросе не повод
/// для отказа. Но и **заменить** собой `start`/`end` он не может:
/// `?range=last_7_days` без границ получит `400` «start: параметр
/// обязателен», а не сводку за неделю. `range` мы не поддерживаем, и
/// молчаливо подставлять под него окно относительно «сейчас» было бы
/// выдумкой — формы ответа на него в снимках нет.
#[derive(Deserialize)]
pub struct SummariesParams {
    pub start: Option<String>,
    pub end: Option<String>,
}

#[derive(Serialize)]
pub struct Summaries {
    pub data: Vec<DaySummary>,
    /// Момент UTC, соответствующий локальной полуночи **первого** дня
    /// диапазона, а не полуночи UTC: у эталона с `Europe/Moscow` это
    /// `2026-07-19T21:00:00Z` для дня `2026-07-20`.
    pub start: Option<String>,
    pub end: Option<String>,
    pub cumulative_total: CumulativeTotal,
    pub daily_average: DailyAverage,
}

/// Среднее по дням диапазона.
///
/// «Праздник» (`holidays`) в терминах WakaTime — день без работы: у
/// `summaries-month.json` их 12, и ровно столько же дней с нулевым
/// `grand_total.total_seconds`. Делится накопленный итог на дни **за
/// вычетом** праздников: 236484.833 / 18 = 13138.05, и эталон печатает
/// 13138.
///
/// Пара `seconds` и `seconds_including_other_language` — не дубликат:
/// первое считается без времени языка `"Other"`. Это **мера**, а не
/// толкование имени поля, и вот она целиком:
///
/// ```text
/// python3 -c 'import hashlib; print("name-"+hashlib.blake2b(
///     b"name\0Other", digest_size=8).hexdigest()[:6])'      # name-e27224
/// jq "[.data[].languages[] | select(.name==\"name-e27224\")]
///     | (map(.total_seconds)|add)" summaries-week.json      # 5678.415
/// python3 -c "print((89097.599001 - 5678.415)/7, 89097.599001/7)"
/// # 11917.026…  12728.228…
/// jq "{s: .daily_average.seconds,
///      o: .daily_average.seconds_including_other_language}" summaries-week.json
/// # {"s": 11917, "o": 12728}
/// ```
///
/// Сходится до последней единицы в обоих полях, с усечением, а не
/// округлением. У нас время без определённого языка не носит языка вовсе
/// (`Attrs::language == None`) и наружу называется `"Other"` — то же
/// множество, см. [`UNDETERMINED_LANGUAGE`]. Когда такого времени в
/// диапазоне нет, поля совпадают: так и у эталона за один день.
#[derive(Serialize)]
pub struct DailyAverage {
    pub holidays: i64,
    pub days_minus_holidays: i64,
    pub days_including_holidays: i64,
    pub seconds: i64,
    pub seconds_including_other_language: i64,
    pub text: String,
    pub text_including_other_language: String,
}

/// Элемент `data[]` — сводка за один локальный день.
///
/// `dependencies` всегда пуст: зависимости приезжают в отметке строкой, но
/// в атрибуты интервала (`wakode_core::Attrs`) не входят и по дням не
/// суммируются. Пустой массив — правда о том, что мы их не считаем, а не
/// заглушка; убрать ключ нельзя, он есть у эталона всегда.
#[derive(Serialize)]
pub struct DaySummary {
    pub grand_total: GrandTotal,
    pub range: DayRange,
    pub projects: Vec<ProjectItem>,
    pub languages: Vec<Item>,
    pub dependencies: Vec<Item>,
    pub editors: Vec<Item>,
    pub operating_systems: Vec<Item>,
    pub categories: Vec<Item>,
    pub machines: Vec<MachineItem>,
}

/// Границы дня в том виде, в каком их печатает WakaTime.
///
/// `end` — не начало следующих суток, а **последняя целая секунда** этих:
/// у эталона `2026-07-20T20:59:59Z` при начале следующего дня
/// `2026-07-20T21:00:00Z`. Полуинтервал `wakode-core` кончается вторым, и
/// разница в одну секунду здесь не описка, а чужой формат.
#[derive(Serialize)]
pub struct DayRange {
    pub start: Option<String>,
    pub end: Option<String>,
    pub date: String,
    pub text: String,
    pub timezone: String,
}

/// Как WakaTime называет язык, который не смог определить.
///
/// # Чем отвечать на имя, которого у нас нет: решение целиком
///
/// Случаев три, и они разные. Все три решены здесь, в одном месте, а не
/// порознь по докстрингам — потому что различает их только измерение.
///
/// **1. Язык не определён — строка `"Other"`.** Это измерено, а не выбрано.
/// Обезличивание детерминировано, поэтому заглушку можно посчитать
/// обратно:
///
/// ```text
/// python3 -c 'import hashlib; print("name-"+hashlib.blake2b(
///     b"name\0Other", digest_size=8).hexdigest()[:6])'      # name-e27224
/// jq "[.data[].languages[] | select(.name==\"name-e27224\")]
///     | {n: length, sum: (map(.total_seconds)|add)}" summaries-week.json
/// # {"n": 5, "sum": 5678.415}
/// ```
///
/// И тот же бакет объясняет `daily_average` (см. [`DailyAverage`]):
/// `(89097.599001 − 5678.415) / 7 = 11917.02 → 11917`, что и напечатано в
/// поле `seconds`. То есть `"Other"` у WakaTime — обычный именованный
/// элемент массива, а не пропуск, и наш `Attrs::language == None` — то же
/// самое множество, названное иначе. Отдавать по нему `null` значило бы
/// печатать в панели пустоту там, где чужой сервер печатает имя.
///
/// **2. Категория незнакома — `null`.** Здесь `null` не по аналогии, а
/// потому что имени в протоколе действительно нет: строка `"unknown"` наша
/// собственная, и докстринг `Category::Unknown`
/// (`wakode-core/src/domain.rs`) прямо обязывает слой совместимости
/// отобразить её во что-то допустимое. Элемент при этом остаётся: сумма
/// `categories[]` у эталона сходится с `grand_total` до последнего знака, и
/// выброшенный элемент сломал бы равенство — время исчезло бы из разбивки,
/// оставшись в итоге.
///
/// **3. Проект, редактор, ОС или машина без имени — `null`, и это
/// неизмеренный случай.** Аналога `"Other"` у них в эталонах нет, и это
/// проверено, а не предположено: `operating_systems` знает ровно одно имя
/// (`Linux`), `editors` — два (`Claude Code`, `Neovim`), `machines` — одно,
/// `projects` — двадцать одно, и ни одно из них не расшифровывается ни в
/// `Other`, ни в `Unknown Project`. `null` во всех шести массивах не
/// встречается тоже (ноль вхождений на три фикстуры), так что измерения
/// нет ни за, ни против: у снятого аккаунта эти поля были заполнены
/// всегда. Выбран `null` — «значения нет», конвенция этого проекта
/// (`compat/user.rs`), — а не выдуманная чужая строка. Появится проба —
/// решение пересматривается; пока это признанный пробел, а не факт о
/// протоколе.
const UNDETERMINED_LANGUAGE: &str = "Other";

/// Имя категории для клиента, `None` — у [`Category::Unknown`].
///
/// Почему именно `None` — случай 2 в [`UNDETERMINED_LANGUAGE`].
///
/// # Регистр: `"AI Coding"`, а не `"ai coding"`
///
/// Проволочное имя категории в **отметке** и в **сводке** — разные строки,
/// и это измерено. Все 31 элемент `categories[]` трёх эталонов
/// расшифровываются в две заглушки, и обе — с заглавных:
///
/// ```text
/// python3 -c 'import hashlib
/// for v in ("AI Coding", "Coding", "ai coding", "coding"):
///     print(v, "name-"+hashlib.blake2b(("name\0"+v).encode(),
///           digest_size=8).hexdigest()[:6])'
/// # AI Coding name-61d903   <- 26 вхождений в эталонах
/// # Coding    name-407518   <- 5 вхождений
/// # ai coding name-07ca4c   <- не встречается
/// # coding    name-2482ef   <- не встречается
/// ```
///
/// Разницу сверка формы не поймает: обе строки для неё просто `string`.
/// Отдай мы проволочное имя приёма — панель показала бы `coding` там, где
/// чужой сервер показывает `Coding`.
///
/// Поэтому имя берётся у `serde` (единственный список проволочных имён —
/// `#[serde(rename)]` в `Category`, второй разошёлся бы с ним при
/// добавлении категории) и переводится в заглавные по словам. Измерены из
/// двадцати одной категории ровно две; правило «каждое слово с заглавной,
/// `ai` целиком заглавными» их обе воспроизводит и на остальных
/// девятнадцати остаётся предположением — но предположением, у которого
/// нет ни одного контрпримера, в отличие от нижнего регистра, у которого
/// контрпримеров тридцать один.
fn category_name(category: Category) -> Option<String> {
    if category == Category::Unknown {
        return None;
    }
    let serde_json::Value::String(wire) = serde_json::to_value(category).ok()? else {
        // Недостижимо: `Category` сериализуется в строку. Сломайся это —
        // «имени нет» всё равно честнее выдуманной строки.
        return None;
    };
    Some(
        wire.split(' ')
            .map(display_word)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Слово проволочного имени в том виде, в каком его печатает сводка.
///
/// `ai` целиком заглавными — измерено (`"AI Coding"`); остальные слова с
/// одной заглавной.
fn display_word(word: &str) -> String {
    if word == "ai" {
        return "AI".to_owned();
    }
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Дата словами: `"Mon Jul 20th 2026"`.
///
/// Порядковый суффикс `chrono` не умеет, а эталон печатает именно его —
/// `"Sat Aug 1st 2026"`, `"Mon Aug 3rd 2026"`. День без ведущего нуля.
fn day_text(date: NaiveDate) -> String {
    let day = date.day();
    let suffix = match (day % 10, day % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{} {day}{suffix} {}", date.format("%a %b"), date.year())
}

/// Сводка за один локальный день по уже нарезанным на него интервалам.
///
/// `intervals` — куски, принадлежащие **этому** дню: их даёт
/// `split_by_local_day`. Функция ничего не режет и ничего не фильтрует,
/// поэтому вызывающему нельзя передать сюда интервалы соседних дней.
///
/// `resolve` разворачивает номер строки в текст. Зависимость передаётся
/// параметром, а не берётся из состояния: словарь живёт в хранилище, а этот
/// расчёт обязан оставаться проверяемым без базы.
pub fn day_summary<R>(intervals: &[Interval], date: NaiveDate, tz: Tz, resolve: R) -> DaySummary
where
    R: Fn(Sid) -> Option<Arc<str>>,
{
    let total = grand_total(intervals);
    let name = |sid: Option<Sid>| sid.and_then(&resolve).map(|s| s.to_string());
    let (day_start, day_end) = local_day_bounds(date, tz);

    DaySummary {
        grand_total: GrandTotal::of(total),
        range: DayRange {
            start: rfc3339(day_start),
            // Последняя целая секунда суток — см. докстринг `DayRange`.
            end: rfc3339(day_end.saturating_sub(Micros::from_secs(1))),
            date: date.to_string(),
            text: day_text(date),
            timezone: tz.name().to_owned(),
        },
        // Базу процента `items` считает сама — сумму своего же массива.
        // Итог дня ей не передаётся и передан быть не может; почему —
        // в её докстринге.
        projects: items(aggregate_by(intervals, |a| a.project), &name)
            .into_iter()
            .map(|item| ProjectItem { item, color: None })
            .collect(),
        // Язык, которого мы не знаем, называется `"Other"` — так его
        // называет чужой сервер, и это измерено; см.
        // `UNDETERMINED_LANGUAGE`.
        languages: items(aggregate_by(intervals, |a| a.language), |sid| {
            Some(name(sid).unwrap_or_else(|| UNDETERMINED_LANGUAGE.to_owned()))
        }),
        dependencies: Vec::new(),
        editors: items(aggregate_by(intervals, |a| a.editor), &name),
        operating_systems: items(aggregate_by(intervals, |a| a.os), &name),
        categories: items(aggregate_by(intervals, |a| a.category), category_name),
        machines: items(aggregate_by(intervals, |a| a.machine), &name)
            .into_iter()
            .map(|item| MachineItem { item, machine_name_id: None })
            .collect(),
    }
}

/// Разбор границы диапазона.
///
/// Принимается либо календарная дата (`2026-08-18`), либо момент времени по
/// RFC 3339 — панели присылают и то, и другое. Момент переводится в день
/// пользователя через `local_day_of`, а не через `local_date_of`: это
/// разные ответы там, где перевод часов отматывает полночь, и нужен здесь
/// именно тот день, чьи границы момент содержат.
///
/// Отказ — `400`, и это не косметика: `500` совместимый клиент считает
/// временной поломкой и повторяет запрос вечно, а негодная дата от повтора
/// годной не станет.
fn parse_boundary(raw: &str, tz: Tz, which: &str) -> Result<NaiveDate, ApiError> {
    let raw = raw.trim();
    if let Ok(date) = raw.parse::<NaiveDate>() {
        return Ok(date);
    }
    if let Ok(moment) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(local_day_of(Micros::new(moment.timestamp_micros()), tz));
    }
    Err(ApiError::BadRequest(format!(
        "{which}: ожидается дата вида 2026-08-18 или момент по RFC 3339"
    )))
}

/// Границы диапазона из параметров запроса.
fn range_of(params: &SummariesParams, tz: Tz) -> Result<(NaiveDate, NaiveDate), ApiError> {
    let missing = |which: &str| ApiError::BadRequest(format!("{which}: параметр обязателен"));
    let start = params.start.as_deref().ok_or_else(|| missing("start"))?;
    let end = params.end.as_deref().ok_or_else(|| missing("end"))?;

    let start = parse_boundary(start, tz, "start")?;
    let end = parse_boundary(end, tz, "end")?;

    // Перевёрнутый диапазон — отказ, а не пустой ответ. Пустая сводка
    // выглядит как «работы не было» и уводит владельца искать потерянные
    // отметки вместо опечатки в запросе.
    if start > end {
        return Err(ApiError::BadRequest(
            "start: начало диапазона позже конца".to_owned(),
        ));
    }
    // `signed_duration_since` вместо вычитания дат: разность `NaiveDate` в
    // днях у `chrono` есть только через `TimeDelta`, и она не переполняется
    // на краях календаря.
    let days = end.signed_duration_since(start).num_days() + 1;
    if days > MAX_DAYS {
        return Err(ApiError::BadRequest(format!(
            "диапазон шире {MAX_DAYS} дней: запрошено {days}"
        )));
    }

    Ok((start, end))
}

/// Отдать сводки за диапазон дней.
pub async fn summaries(
    KeyAuth { user, .. }: KeyAuth,
    State(state): State<AppState>,
    params: Result<Query<SummariesParams>, QueryRejection>,
) -> Result<Json<Summaries>, ApiError> {
    let Query(params) = params.map_err(|err| {
        tracing::warn!(error = %err, "query-строка сводки не разобралась");
        ApiError::BadRequest("query-строка не разобралась".to_owned())
    })?;
    let tz = user.timezone;
    let (first, last) = range_of(&params, tz)?;

    // Таймаут — **пользовательский**, а не `AppSettings::default_timeout_secs`:
    // настройка приложения задаёт умолчание для заводимых пользователей, а
    // рвёт сессии то значение, что записано в самой учётной записи.
    // Хвостовая добавка — ноль, и это измеренное значение, а не
    // предосторожность: `.claude/docs/decisions/no-tail-padding.md`.
    let cfg = DurationConfig::new(Micros::from_secs(user.timeout_secs), Micros::ZERO)
        .map_err(|err| {
            // Конфиг принимает `timeout_secs` без проверки знака, так что
            // непригодное значение доезжает до базы. Считать по нему
            // нечего, а подставить умолчание значило бы отчитаться о
            // времени, которого владелец не настраивал.
            tracing::error!(timeout_secs = user.timeout_secs, error = %err, "негодный таймаут пользователя");
            ApiError::Internal
        })?;

    // Окно шире суток на таймаут в обе стороны: интервал рождается парой
    // отметок, и работа сразу после полуночи приезжает парой, чья ранняя
    // отметка лежит во вчерашнем дне. Выборка ровно по границам суток эту
    // пару не составила бы, и день недосчитался бы молча.
    let (from, _) = heartbeat_window(first, tz, cfg);
    let (_, to) = heartbeat_window(last, tz, cfg);

    let heartbeats = state.store.heartbeats_in_range(user.id, from, to).await?;
    let intervals = build_intervals(&heartbeats, cfg);
    let by_day = split_by_local_day(&intervals, tz);

    let mut data = Vec::new();
    let mut cumulative = Micros::ZERO;
    let mut without_language = Micros::ZERO;
    let mut worked_days = 0i64;

    let mut date = first;
    loop {
        // Пустых дней `split_by_local_day` не порождает — их добавляет
        // эндпоинт: диапазон из 30 дней у эталона даёт 30 элементов, из
        // которых 12 пустых.
        let pieces: &[Interval] = by_day.get(&date).map_or(&[], Vec::as_slice);
        let total = grand_total(pieces);
        if total > Micros::ZERO {
            worked_days += 1;
        }
        cumulative = cumulative.saturating_add(total);
        without_language = pieces
            .iter()
            .filter(|iv| iv.attrs.language.is_none())
            .map(|iv| iv.duration())
            .fold(without_language, Micros::saturating_add);

        data.push(day_summary(pieces, date, tz, |sid| state.store.resolve(sid)));

        if date >= last {
            break;
        }
        match date.succ_opt() {
            Some(next) => date = next,
            // Край календаря: следующего дня не существует, и диапазон
            // кончается здесь.
            None => break,
        }
    }

    let days = data.len() as i64;
    // Делить не на что, когда работы не было ни в одном дне: ноль дней в
    // знаменателе. Средним тогда служит ноль — не «нет данных», а «нисколько».
    let average = |total: Micros| {
        if worked_days == 0 {
            0
        } else {
            (total.as_secs_f64() / worked_days as f64) as i64
        }
    };
    let seconds = average(cumulative.saturating_sub(without_language));
    let seconds_including_other_language = average(cumulative);

    let first_range = data.first().and_then(|day| day.range.start.clone());
    let last_range = data.last().and_then(|day| day.range.end.clone());

    Ok(Json(Summaries {
        data,
        start: first_range,
        end: last_range,
        cumulative_total: CumulativeTotal::of(cumulative),
        daily_average: DailyAverage {
            holidays: days - worked_days,
            days_minus_holidays: worked_days,
            days_including_holidays: days,
            seconds,
            seconds_including_other_language,
            text: Duration::new(Micros::from_secs(seconds)).text(),
            text_including_other_language: Duration::new(Micros::from_secs(
                seconds_including_other_language,
            ))
            .text(),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("валидная дата")
    }

    fn tz() -> Tz {
        "Europe/Moscow".parse().expect("зона существует")
    }

    /// Атрибуты, у которых не заполнено ничего, кроме обязательного:
    /// отметка без проекта, языка, редактора, ОС и машины — реальный
    /// случай, а не выдуманный.
    fn attrs_without_a_language() -> wakode_core::Attrs {
        wakode_core::Attrs {
            entity: Sid(1),
            kind: wakode_core::EntityKind::File,
            category: Category::Coding,
            project: None,
            branch: None,
            language: None,
            editor: None,
            os: None,
            machine: None,
        }
    }

    #[test]
    fn a_date_is_spelled_the_way_the_fixture_spells_it() {
        // Все четыре суффикса и обе границы правила «11–13 всегда th».
        // Строки — дословно с `summaries-month.json`.
        assert_eq!(day_text(date(2026, 7, 20)), "Mon Jul 20th 2026");
        assert_eq!(day_text(date(2026, 8, 1)), "Sat Aug 1st 2026");
        assert_eq!(day_text(date(2026, 8, 3)), "Mon Aug 3rd 2026");
        assert_eq!(day_text(date(2026, 7, 21)), "Tue Jul 21st 2026");
        assert_eq!(day_text(date(2026, 7, 22)), "Wed Jul 22nd 2026");
        assert_eq!(day_text(date(2026, 7, 23)), "Thu Jul 23rd 2026");
        assert_eq!(day_text(date(2026, 7, 31)), "Fri Jul 31st 2026");
        // 11, 12 и 13 — «th» вопреки последней цифре.
        assert_eq!(day_text(date(2026, 8, 11)), "Tue Aug 11th 2026");
        assert_eq!(day_text(date(2026, 8, 12)), "Wed Aug 12th 2026");
        assert_eq!(day_text(date(2026, 8, 13)), "Thu Aug 13th 2026");
    }

    #[test]
    fn a_category_we_do_not_know_has_no_name_the_protocol_would_accept() {
        // Строки `"unknown"` в протоколе WakaTime не существует, и наружу
        // она уезжать не имеет права — обязанность записана в докстринге
        // `Category::Unknown`.
        assert_eq!(category_name(Category::Unknown), None);
        // Зеркало: категория, которую протокол знает, имя получает.
        assert!(category_name(Category::Coding).is_some());
    }

    #[test]
    fn a_category_is_named_the_way_the_summary_names_it_and_not_the_way_a_heartbeat_does() {
        // Обе строки измерены: 26 и 5 вхождений на три эталона, и ни
        // одного вхождения нижнего регистра. Расшифровка — в докстринге
        // `category_name`.
        assert_eq!(category_name(Category::AiCoding).as_deref(), Some("AI Coding"));
        assert_eq!(category_name(Category::Coding).as_deref(), Some("Coding"));
        // Правило распространяется на многословные имена; эти двадцать
        // девять букв не измерены и заявлены как правило, а не как факт.
        assert_eq!(
            category_name(Category::WritingTests).as_deref(),
            Some("Writing Tests")
        );
        assert_eq!(
            category_name(Category::ManualTesting).as_deref(),
            Some("Manual Testing")
        );
        // И ни одно имя не совпадает с проволочным именем приёма: разойдись
        // они только регистром — сверка формы этого не увидит.
        for category in [Category::AiCoding, Category::Coding, Category::WritingDocs] {
            let name = category_name(category).unwrap();
            let wire = serde_json::to_value(category).unwrap();
            assert_ne!(serde_json::Value::String(name), wire, "{category:?}");
        }
    }

    #[test]
    fn a_language_we_could_not_determine_is_called_other_and_not_nothing() {
        // Измерено: `"Other"` — обычный именованный элемент массива у
        // WakaTime (5 вхождений и 5678.415 секунды в `summaries-week`), и
        // тот же бакет объясняет `daily_average.seconds`. `null` тут
        // напечатал бы в панели пустоту вместо имени.
        let attrs = attrs_without_a_language();
        let intervals = [Interval {
            start: Micros::from_secs(0),
            end: Micros::from_secs(60),
            attrs,
        }];
        let summary = day_summary(&intervals, date(1970, 1, 1), Tz::UTC, |_| None);

        assert_eq!(summary.languages.len(), 1);
        assert_eq!(summary.languages[0].name.as_deref(), Some("Other"));
        // Зеркало: имени, которого у нас нет, в остальных массивах
        // соответствует `null` — случай неизмеренный, см.
        // `UNDETERMINED_LANGUAGE`.
        assert_eq!(summary.projects[0].item.name, None);
        assert_eq!(summary.editors[0].name, None);
        assert_eq!(summary.machines[0].item.name, None);
    }

    #[test]
    fn the_day_range_ends_a_second_before_the_next_day_starts() {
        // Чужой формат: полуинтервал ядра кончается началом следующих
        // суток, а эталон печатает последнюю целую секунду этих.
        let summary = day_summary(&[], date(2026, 7, 20), tz(), |_| None);

        assert_eq!(summary.range.start.as_deref(), Some("2026-07-19T21:00:00Z"));
        assert_eq!(summary.range.end.as_deref(), Some("2026-07-20T20:59:59Z"));
        assert_eq!(summary.range.date, "2026-07-20");
        assert_eq!(summary.range.timezone, "Europe/Moscow");
    }

    #[test]
    fn an_empty_day_keeps_every_key_and_says_zero() {
        let summary = day_summary(&[], date(2026, 7, 20), tz(), |_| None);

        assert_eq!(summary.grand_total.total_seconds, 0.0);
        assert_eq!(summary.grand_total.text, "0 secs");
        assert_eq!(summary.grand_total.digital, "0:00");
        assert!(summary.projects.is_empty());
        assert!(summary.languages.is_empty());
        assert!(summary.categories.is_empty());
    }

    #[test]
    fn a_range_needs_both_of_its_ends() {
        let both = SummariesParams {
            start: None,
            end: Some("2026-08-18".to_owned()),
        };
        let err = range_of(&both, tz()).unwrap_err();
        assert!(matches!(&err, ApiError::BadRequest(why) if why.starts_with("start")), "{err:?}");

        let no_end = SummariesParams {
            start: Some("2026-08-18".to_owned()),
            end: None,
        };
        let err = range_of(&no_end, tz()).unwrap_err();
        assert!(matches!(&err, ApiError::BadRequest(why) if why.starts_with("end")), "{err:?}");
    }

    #[test]
    fn a_boundary_may_be_a_date_or_a_moment() {
        // Панели присылают и то, и другое. Момент разрешается в день
        // пользователя, а не в день UTC: 20:30 UTC — это уже 23:30 в
        // Москве того же дня, а 21:30 UTC — уже следующий день.
        assert_eq!(
            parse_boundary("2026-08-18", tz(), "start").unwrap(),
            date(2026, 8, 18)
        );
        assert_eq!(
            parse_boundary("2026-08-18T20:30:00Z", tz(), "start").unwrap(),
            date(2026, 8, 18)
        );
        assert_eq!(
            parse_boundary("2026-08-18T21:30:00Z", tz(), "start").unwrap(),
            date(2026, 8, 19)
        );
    }

    #[test]
    fn a_boundary_we_cannot_read_is_the_clients_fault_and_not_ours() {
        // `400`, а не `500`: пятисотку совместимый клиент считает
        // временной поломкой и повторяет вечно.
        for bad in ["вчера", "2026-13-40", "", "1755500000"] {
            let err = parse_boundary(bad, tz(), "start").unwrap_err();
            assert!(matches!(err, ApiError::BadRequest(_)), "{bad}: {err:?}");
        }
    }

    #[test]
    fn a_backwards_range_is_refused_rather_than_answered_with_nothing() {
        let params = SummariesParams {
            start: Some("2026-08-18".to_owned()),
            end: Some("2026-08-11".to_owned()),
        };
        let err = range_of(&params, tz()).unwrap_err();
        assert!(matches!(&err, ApiError::BadRequest(why) if why.contains("позже конца")), "{err:?}");
    }

    #[test]
    fn a_range_of_a_thousand_years_is_refused_and_a_year_is_not() {
        let thousand_years = SummariesParams {
            start: Some("1026-08-18".to_owned()),
            end: Some("2026-08-18".to_owned()),
        };
        let err = range_of(&thousand_years, tz()).unwrap_err();
        assert!(matches!(&err, ApiError::BadRequest(why) if why.contains("шире")), "{err:?}");

        // Граница проходит ровно по `MAX_DAYS`, а не «где-то там»: 366
        // дней включительно — это 2025-08-18..2026-08-18 в невисокосном
        // счёте, и такой диапазон обязан пройти.
        let a_year = SummariesParams {
            start: Some("2025-08-18".to_owned()),
            end: Some("2026-08-18".to_owned()),
        };
        assert!(range_of(&a_year, tz()).is_ok());
    }

    #[test]
    fn the_limit_on_the_width_of_a_range_is_exactly_where_it_says() {
        // Верхняя половина границы. Без неё `MAX_DAYS` можно поднять хоть
        // до 365 000, и единственный тест про тысячу лет останется
        // зелёным — предел перестал бы что-либо ограничивать, не покраснев.
        //
        // Даты здесь **литеральные**, и `MAX_DAYS` в тесте не упомянут
        // намеренно: посчитай мы границу из самой константы — тест ездил
        // бы вместе с ней и не поймал бы ни подъёма, ни спуска. Первая
        // редакция этого теста так и была написана, и обе мутации ниже
        // оставили её зелёной.
        //
        // 2026 год не високосный, поэтому 2026-01-01..2027-01-01 — ровно
        // 366 дней включительно, а 2027-01-02 добавляет триста
        // шестьдесят седьмой.
        let range = |start: &str, end: &str| SummariesParams {
            start: Some(start.to_owned()),
            end: Some(end.to_owned()),
        };

        assert!(
            range_of(&range("2026-01-01", "2027-01-01"), tz()).is_ok(),
            "366 дней обязаны пройти"
        );
        let err = range_of(&range("2026-01-01", "2027-01-02"), tz()).unwrap_err();
        assert!(
            matches!(&err, ApiError::BadRequest(why) if why.contains("367")),
            "367 дней обязаны быть отвергнуты с числом в тексте: {err:?}"
        );
    }

    #[test]
    fn a_single_day_range_is_one_day_and_not_zero() {
        let params = SummariesParams {
            start: Some("2026-08-18".to_owned()),
            end: Some("2026-08-18".to_owned()),
        };
        assert_eq!(
            range_of(&params, tz()).unwrap(),
            (date(2026, 8, 18), date(2026, 8, 18))
        );
    }
}
