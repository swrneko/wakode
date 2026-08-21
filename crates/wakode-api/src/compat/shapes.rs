//! Как WakaTime печатает длительности и из чего состоят элементы сводок.
//!
//! Одно и то же число едет клиенту сразу в пяти видах — `text`, `digital`,
//! `decimal`, `hours`/`minutes`/`seconds` и сырым `total_seconds`, — и все
//! пять сняты с эталонов в `tests/fixtures/wakatime`, а не выдуманы. Здесь
//! они собраны в одном месте, потому что ими пользуются и сводки (задача 5),
//! и `statusbar/today` (задача 6), и `all_time_since_today` (задача 7):
//! разъехавшись, они разъехались бы незаметно.

use serde::Serialize;
use wakode_core::{Bucket, Micros};

/// Длительность, разобранная так, как её печатает WakaTime.
///
/// Целые части — **усечение**, а не округление: у эталона 21839.291 секунды
/// это `hours: 6, minutes: 3` (6 ч 3,988 мин), а не `6:04`. Проверено на
/// всех четырёх проектах `summaries-one-day.json` до последней секунды.
#[derive(Clone, Copy, Debug)]
pub struct Duration {
    total: Micros,
}

impl Duration {
    /// Отрицательная длительность обесценивается в ноль — тем же правилом,
    /// что и вывернутый интервал в `wakode-core`. Сюда она попасть не должна
    /// (суммы там насыщающие и неотрицательные), но печатать `-1:-30`
    /// клиенту нельзя ни при каких входных данных.
    pub fn new(total: Micros) -> Self {
        Self {
            total: total.max(Micros::ZERO),
        }
    }

    /// Секунды дробным числом — поле `total_seconds` эталона.
    pub fn total_seconds(self) -> f64 {
        self.total.as_secs_f64()
    }

    fn whole_seconds(self) -> i64 {
        self.total.get() / 1_000_000
    }

    pub fn hours(self) -> i64 {
        self.whole_seconds() / 3_600
    }

    pub fn minutes(self) -> i64 {
        self.whole_seconds() % 3_600 / 60
    }

    pub fn seconds(self) -> i64 {
        self.whole_seconds() % 60
    }

    /// `"6:03"` — часы и минуты. Форма итога дня и накопленного итога.
    ///
    /// Часы **не** переполняются в сутки: у `summaries-week.json`
    /// `cumulative_total.digital` это `"24:44"`, а не `"1:00:44"`.
    pub fn digital_hm(self) -> String {
        format!("{}:{:02}", self.hours(), self.minutes())
    }

    /// `"5:38:23"` — часы, минуты и секунды. Форма элемента массива.
    ///
    /// У элементов массивов и у итога дня форма разная, и это не оплошность
    /// снимка: в `summaries-one-day.json` `grand_total.digital` — `"6:03"`,
    /// а `projects[0].digital` — `"5:38:23"`.
    pub fn digital_hms(self) -> String {
        format!("{}:{:02}:{:02}", self.hours(), self.minutes(), self.seconds())
    }

    /// `"6.05"` — часы дробью, **считанные из усечённых часов и минут**.
    ///
    /// Не `total_seconds / 3600`: 21839.291 / 3600 = 6.0665, что дало бы
    /// `"6.07"`, а эталон печатает `"6.05"` = 6 + 3/60. То же на всех
    /// шестнадцати элементах `summaries-one-day.json`. Секунды в `decimal` не
    /// участвуют вовсе.
    pub fn decimal(self) -> String {
        format!("{:.2}", self.hours() as f64 + self.minutes() as f64 / 60.0)
    }

    /// `"6 hrs 3 mins"`, `"25 mins"`, `"55 secs"`, `"0 secs"`.
    ///
    /// Разряды не дополняют друг друга, а вытесняют: есть часы — секунд не
    /// видно («5 hrs 38 mins» при 20303 секундах), есть минуты — не видно ни
    /// часов (их нет), ни секунд. Единственное и множественное число у
    /// каждого разряда своё: эталон знает и `"1 hr"`, и `"4 hrs 1 min"`.
    /// Ноль печатается как `"0 secs"`, то есть по общему правилу
    /// множественного числа, а не отдельным случаем.
    pub fn text(self) -> String {
        let (hours, minutes, seconds) = (self.hours(), self.minutes(), self.seconds());
        if hours > 0 && minutes > 0 {
            format!("{} {} {}", plural(hours, "hr"), minutes, plural_word(minutes, "min"))
        } else if hours > 0 {
            plural(hours, "hr")
        } else if minutes > 0 {
            plural(minutes, "min")
        } else {
            plural(seconds, "sec")
        }
    }
}

/// `"1 hr"` / `"6 hrs"`.
fn plural(count: i64, word: &str) -> String {
    format!("{count} {}", plural_word(count, word))
}

fn plural_word(count: i64, word: &str) -> String {
    if count == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

/// Доля до сотых — как её печатает эталон.
///
/// Сверено арифметикой, а не на глаз: 20303.722006 от 21839.291 это
/// 92.9686 %, и эталон печатает `92.97`; 480.676999 даёт 2.2010 % и
/// печатается как `2.2`. То есть округление к ближайшей сотой, а не
/// усечение и не фиксированные два знака в строке — поле числовое.
pub fn round_percent(percent: f64) -> f64 {
    (percent * 100.0).round() / 100.0
}

/// Итог дня. Поля `seconds` у него нет — в отличие от элемента массива.
#[derive(Serialize)]
pub struct GrandTotal {
    pub hours: i64,
    pub minutes: i64,
    pub total_seconds: f64,
    pub digital: String,
    pub decimal: String,
    pub text: String,
}

impl GrandTotal {
    pub fn of(total: Micros) -> Self {
        let d = Duration::new(total);
        Self {
            hours: d.hours(),
            minutes: d.minutes(),
            total_seconds: d.total_seconds(),
            digital: d.digital_hm(),
            decimal: d.decimal(),
            text: d.text(),
        }
    }
}

/// Элемент массива сводки: проект, язык, редактор, категория, машина.
///
/// `name` — `Option`, потому что имени у нас может не быть. Чем именно
/// отвечать на каждый такой случай — решает не этот тип, а `summaries.rs`:
/// решение там одно на все шесть массивов и записано в одном месте
/// (`UNDETERMINED_LANGUAGE` и соседний абзац).
#[derive(Serialize)]
pub struct Item {
    pub name: Option<String>,
    pub total_seconds: f64,
    pub digital: String,
    pub decimal: String,
    pub text: String,
    pub hours: i64,
    pub minutes: i64,
    pub seconds: i64,
    pub percent: f64,
}

impl Item {
    /// Приватна намеренно: базу процента выбирать не вызывающему.
    ///
    /// Единственный способ построить элемент снаружи — [`items`], а она
    /// базу считает сама. Пока эта функция была публичной, `day_summary`
    /// передавал сюда итог дня, и ошибка была видна только на массиве,
    /// который день не разбивает.
    fn of(name: Option<String>, total: Micros, whole: Micros) -> Self {
        let d = Duration::new(total);
        Self {
            name,
            total_seconds: d.total_seconds(),
            digital: d.digital_hms(),
            decimal: d.decimal(),
            text: d.text(),
            hours: d.hours(),
            minutes: d.minutes(),
            seconds: d.seconds(),
            percent: round_percent(wakode_core::percent(total, whole)),
        }
    }
}

/// Массив элементов из бакетов; процент — от суммы **этого массива**.
///
/// # Откуда база
///
/// Из измерения, а не из соображений. Пять массивов из шести день
/// разбивают нацело, и по ним две базы — итог дня и сумма массива —
/// неразличимы. Различает их `dependencies`: одна отметка несёт несколько
/// зависимостей сразу, и их сумма итогу дня не равна. Прогон по всем трём
/// эталонам:
///
/// ```text
/// python3 - <<'PY'
/// import json
/// for f in ["summaries-one-day.json","summaries-week.json","summaries-month.json"]:
///     for day in json.load(open(f))["data"]:
///         g = day["grand_total"]["total_seconds"]
///         for a in ["projects","languages","dependencies","editors",
///                   "operating_systems","categories","machines"]:
///             s = sum(i["total_seconds"] for i in day[a])
///             if abs(s - g) < 1e-6: continue          # база неразличима
///             if s == 0 or g == 0: continue           # обе базы вырождены
///             for i in day[a]:
///                 print(a,
///                       abs(round(i["total_seconds"]*100/g, 2) - i["percent"]) < 5e-3,
///                       abs(round(i["total_seconds"]*100/s, 2) - i["percent"]) < 5e-3)
/// PY
/// ```
///
/// 555 элементов, у которых базы различаются, и все 555 сходятся с суммой
/// массива; с итогом дня — ни один. Ещё 34 сходятся с обеими (округление
/// до сотой их не разводит) и 10 вырождены нулевой суммой.
///
/// Сегодня цена нулевая — `dependencies` мы не считаем и массив всегда
/// пуст, — но `shapes.rs` общий для задач 6 и 7, и первая же реализация
/// зависимостей унаследовала бы неверную базу.
///
/// Сумма процентов при этом не обязана давать ровно 100: у `languages`
/// эталона она 100.01. Это след поэлементного округления до сотой, а не
/// свидетельство о базе.
/// **Одноимённые корзины сливаются.** Разбивка приходит сюда по ключам —
/// номерам строк словаря, — и два разных ключа могут дать одно имя. Живой
/// случай ровно один, но настоящий: неопределённый язык мы называем
/// `"Other"`, потому что так его называет чужой сервер, а плагин вправе
/// прислать язык, который так и называется. Без слияния в массиве оказались
/// бы **два** элемента с именем `"Other"`, и клиент, раскладывающий массив
/// по имени, один из них потерял бы молча.
///
/// Слияние идёт до подсчёта процента, поэтому доля слитого элемента —
/// доля суммы, а не одной из половин. На базу это не влияет: сумма массива
/// от перегруппировки не меняется.
pub fn items<K, N>(buckets: Vec<Bucket<K>>, name: N) -> Vec<Item>
where
    N: Fn(K) -> Option<String>,
{
    let whole = buckets
        .iter()
        .map(|bucket| bucket.total)
        .fold(Micros::ZERO, Micros::saturating_add);

    // `Vec`, а не `HashMap`: порядок элементов — часть ответа, а корзин
    // здесь единицы, так что линейный поиск дешевле хеша.
    let mut merged: Vec<(Option<String>, Micros)> = Vec::with_capacity(buckets.len());
    for bucket in buckets {
        let name = name(bucket.key);
        match merged.iter_mut().find(|(seen, _)| *seen == name) {
            Some((_, total)) => *total = total.saturating_add(bucket.total),
            None => merged.push((name, bucket.total)),
        }
    }

    merged
        .into_iter()
        .map(|(name, total)| Item::of(name, total, whole))
        .collect()
}

/// Проект — тот же элемент плюс цвет, которого у нас нет.
///
/// Цвет в эталоне `null` и у чужого аккаунта: его назначает пользователь в
/// веб-интерфейсе, которого у нас пока нет вовсе.
#[derive(Serialize)]
pub struct ProjectItem {
    #[serde(flatten)]
    pub item: Item,
    pub color: Option<String>,
}

/// Машина — тот же элемент плюс идентификатор записи о машине.
///
/// Записи о машинах у нас нет: имя машины интернируется вместе с прочими
/// строками и своего идентификатора не получает. `null`, а не номер строки
/// словаря: номер — наша внутренняя деталь, и выдавать его за чужой
/// идентификатор значило бы обещать ссылку, по которой ничего не найти.
#[derive(Serialize)]
pub struct MachineItem {
    #[serde(flatten)]
    pub item: Item,
    pub machine_name_id: Option<String>,
}

/// Накопленный итог за весь диапазон. Ключ здесь `seconds`, а не
/// `total_seconds`, — у эталона именно так.
#[derive(Serialize)]
pub struct CumulativeTotal {
    pub seconds: f64,
    pub text: String,
    pub digital: String,
    pub decimal: String,
}

impl CumulativeTotal {
    pub fn of(total: Micros) -> Self {
        let d = Duration::new(total);
        Self {
            seconds: d.total_seconds(),
            text: d.text(),
            digital: d.digital_hm(),
            decimal: d.decimal(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(secs: f64) -> Duration {
        Duration::new(Micros::from_secs_f64(secs))
    }

    #[test]
    fn a_duration_is_printed_the_five_ways_wakatime_prints_it() {
        // Таблица целиком с эталонов: строка «сколько секунд» → пять
        // представлений. Первые три строки — `grand_total`, `projects[0]`,
        // `projects[3]` из `summaries-one-day.json`; четвёртая —
        // `cumulative_total` из `summaries-week.json`, и она здесь ради
        // суток: часы в `digital` не переполняются в дни.
        let table: &[(f64, i64, i64, i64, &str, &str, &str, &str)] = &[
            // секунды, ч, м, с, digital_hm, digital_hms, decimal, text
            (0.0, 0, 0, 0, "0:00", "0:00:00", "0.00", "0 secs"),
            (1.0, 0, 0, 1, "0:00", "0:00:01", "0.00", "1 sec"),
            (55.0, 0, 0, 55, "0:00", "0:00:55", "0.00", "55 secs"),
            (109.685, 0, 1, 49, "0:01", "0:01:49", "0.02", "1 min"),
            (1535.568994, 0, 25, 35, "0:25", "0:25:35", "0.42", "25 mins"),
            (3600.0, 1, 0, 0, "1:00", "1:00:00", "1.00", "1 hr"),
            (20303.722006, 5, 38, 23, "5:38", "5:38:23", "5.63", "5 hrs 38 mins"),
            (21839.291, 6, 3, 59, "6:03", "6:03:59", "6.05", "6 hrs 3 mins"),
            (89097.599001, 24, 44, 57, "24:44", "24:44:57", "24.73", "24 hrs 44 mins"),
        ];

        for &(total, hours, minutes, seconds, hm, hms, decimal, text) in table {
            let d = secs(total);
            assert_eq!((d.hours(), d.minutes(), d.seconds()), (hours, minutes, seconds), "{total}");
            assert_eq!(d.digital_hm(), hm, "{total}");
            assert_eq!(d.digital_hms(), hms, "{total}");
            assert_eq!(d.decimal(), decimal, "{total}");
            assert_eq!(d.text(), text, "{total}");
        }
    }

    #[test]
    fn a_lone_minute_and_a_lone_hour_are_singular_next_to_a_plural_neighbour() {
        // Эталон знает `"4 hrs 1 min"`: число ставится в единственное число
        // по своему разряду, а не по соседнему.
        assert_eq!(secs(4.0 * 3600.0 + 60.0).text(), "4 hrs 1 min");
        assert_eq!(secs(3600.0 + 120.0).text(), "1 hr 2 mins");
    }

    #[test]
    fn the_parts_are_truncated_and_not_rounded() {
        // 21839.291 с — это 6 ч 3,988 мин. Округление дало бы `6:04`, а
        // эталон печатает `6:03`.
        assert_eq!(secs(21839.291).digital_hm(), "6:03");
        // Полминуты не превращаются в минуту ни в одном представлении.
        assert_eq!(secs(59.9).text(), "59 secs");
        assert_eq!(secs(59.9).minutes(), 0);
    }

    #[test]
    fn the_decimal_comes_from_the_whole_minutes_and_not_from_the_seconds() {
        // Мутация, ради которой тест написан: `total_seconds / 3600.0`.
        // 21839.291 / 3600 = 6.0665 → «6.07», а эталон печатает «6.05».
        assert_eq!(secs(21839.291).decimal(), "6.05");
        assert_ne!(format!("{:.2}", 21839.291 / 3600.0), "6.05");
    }

    #[test]
    fn a_negative_duration_is_worth_nothing_rather_than_printing_a_minus() {
        let d = Duration::new(Micros::from_secs(-100));
        assert_eq!(d.text(), "0 secs");
        assert_eq!(d.digital_hms(), "0:00:00");
        assert_eq!(d.total_seconds(), 0.0);
    }

    #[test]
    fn a_percent_is_rounded_to_the_hundredth_the_way_the_fixture_rounds_it() {
        // Числа с эталона: 20303.722006 и 480.676999 от 21839.291.
        assert_eq!(round_percent(92.968_6), 92.97);
        assert_eq!(round_percent(2.201_0), 2.2);
        assert_eq!(round_percent(0.236_5), 0.24);
    }

    #[test]
    fn an_item_takes_its_percent_from_the_whole_it_was_given() {
        // Нижний слой: доля считается от переданного целого, а не от
        // самого элемента. Мутация: `percent(total, total)` — все
        // элементы стали бы по 100 %.
        let item = Item::of(
            Some("проект".to_owned()),
            Micros::from_secs(900),
            Micros::from_secs(3600),
        );
        assert_eq!(item.percent, 25.0);
        assert_eq!(item.name.as_deref(), Some("проект"));
        // И `digital` у элемента — часы:минуты:**секунды**, в отличие от
        // итога дня. Форма разная, а тип один, и сверка с эталоном обе
        // строки видит одинаково.
        assert_eq!(item.digital, "0:15:00");
        assert_eq!(GrandTotal::of(Micros::from_secs(900)).digital, "0:15");
    }

    #[test]
    fn a_percent_is_taken_from_the_sum_of_its_own_array() {
        // Тот самый массив, что различает две базы: сумма 90 секунд, и
        // никакого «итога дня» у `items` в параметрах нет вовсе — 200
        // секунд ниже она увидеть не может. Измерено по `dependencies`
        // эталонов, 555 элементов из 555; вывод — в докстринге `items`.
        let buckets = vec![
            Bucket { key: 1u32, total: Micros::from_secs(60) },
            Bucket { key: 2u32, total: Micros::from_secs(30) },
        ];
        let items = items(buckets, |key| Some(format!("зависимость {key}")));

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].percent, 66.67);
        assert_eq!(items[1].percent, 33.33);
        assert_eq!(items[0].name.as_deref(), Some("зависимость 1"));
        // Считай `items` от итога дня в 200 секунд — вышло бы 30 и 15.
        assert_ne!(items[0].percent, 30.0);
    }

    #[test]
    fn two_buckets_with_one_name_become_one_item() {
        // Живой случай: неопределённый язык мы называем `"Other"`, потому
        // что так его называет чужой сервер, — а плагин вправе прислать
        // язык, который так и называется. Ключи у них разные, имя одно, и
        // без слияния клиент, раскладывающий массив по имени, потерял бы
        // одну из половин молча.
        //
        // Сверка формы этого не поймала бы никогда: два элемента с
        // одинаковым именем — совершенно законный по форме массив.
        let buckets = vec![
            Bucket { key: 1u32, total: Micros::from_secs(60) },
            Bucket { key: 2u32, total: Micros::from_secs(30) },
            Bucket { key: 3u32, total: Micros::from_secs(10) },
        ];
        // Ключи 1 и 3 дают одно имя, 2 — своё.
        let items = items(buckets, |key| {
            Some(if key == 2 { "Rust".to_owned() } else { "Other".to_owned() })
        });

        let names: Vec<_> = items.iter().map(|i| i.name.clone()).collect();
        assert_eq!(items.len(), 2, "одноимённые корзины не слились: {names:?}");
        assert_eq!(items[0].name.as_deref(), Some("Other"));
        assert_eq!(items[0].total_seconds, 70.0, "слились, но время потеряно");
        // Доля считается от суммы **после** слияния: 70 из 100, а не 60.
        assert_eq!(items[0].percent, 70.0);
        // Порядок сохраняется по первому появлению имени, а не по величине.
        assert_eq!(items[1].name.as_deref(), Some("Rust"));
    }

    #[test]
    fn an_empty_array_of_buckets_divides_by_nothing_instead_of_panicking() {
        // База — сумма массива, а у пустого массива она ноль. Деление на
        // ноль обесценено в `wakode_core::percent`, и полагаться на это
        // здесь нужно осознанно: пустые массивы в сводке — норма.
        let items = items(Vec::<Bucket<u32>>::new(), |_| None);
        assert!(items.is_empty());
    }

    #[test]
    fn a_project_carries_a_color_and_a_machine_an_identifier() {
        // Оба поля есть у эталона и нет у соседних массивов: элемент
        // `languages` с `color` был бы лишним полем, а `machines` без
        // `machine_name_id` — недостающим.
        let project = ProjectItem {
            item: Item::of(None, Micros::ZERO, Micros::ZERO),
            color: None,
        };
        let json = serde_json::to_value(&project).unwrap();
        assert!(json.get("color").is_some(), "{json}");
        assert!(json.get("percent").is_some(), "плоское поле потерялось: {json}");
        assert!(json.get("machine_name_id").is_none(), "{json}");
    }
}
