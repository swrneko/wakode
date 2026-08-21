//! `GET /api/v1/users/current` — профиль и проверка ключа.
//!
//! Первое, что дёргает свежеустановленный плагин. Ничего не считает:
//! проверяет проводку — маршрут, аутентификацию, форму.

use axum::Json;
use serde::Serialize;

use crate::auth::KeyAuth;
use crate::compat::rfc3339;

#[derive(Serialize)]
pub struct CurrentUser {
    pub data: CurrentUserData,
}

/// Поля — все пятьдесят девять, что есть в эталоне `current.json`.
///
/// Исключений нет: `ai_*` в эталоне не оказалось ни одного, и запись в
/// `NOT_OURS` (`tests/shape.rs`) здесь не сработала ни разу.
///
/// **Почему все.** Протокол чужой и заморожен: плагин, читающий
/// `has_premium_features` или `timeout`, не обязан переживать отсутствие
/// поля. Отдавать девять полей из пятидесяти девяти значило бы придумать
/// свой протокол и назвать его совместимым.
///
/// Поля делятся на три вида, и вид каждого виден по значению:
///
/// 1. **Есть у нас по-настоящему** — берутся из `User`: `id`, `username`,
///    `display_name`, `full_name`, `email`, `timezone`, `timeout`,
///    `created_at`, `modified_at`.
/// 2. **Понятия нет, и пустое значение — правда.** Публичного профиля у
///    selfhosted-инстанса нет, поэтому все `*_public` — `false`
///    («не публично»); биографии, города и ссылок на соцсети нет, поэтому
///    `null`; тарифов и счетов нет, поэтому `needs_payment_method`
///    и `has_*_features` — `false`, а `invoice_counter` — `0`.
/// 3. **Понятия нет, а конкретное значение было бы выдумкой.** Такие поля
///    отдаются как `null` — «значения нет», а не как чужое умолчание:
///    `plan`, `weekday_start`, `durations_slice_by`, `date_format`,
///    `color_scheme` и прочие настройки отображения. `null` здесь честнее
///    строки: скажи мы `plan: "free"`, мы назвали бы тарифом отсутствие
///    тарифов, а `weekday_start: 0` объявило бы, что неделя начинается с
///    воскресенья, — конвенции, которой у нас нет.
///
/// Из `NOT_OURS` в сверке формы (`tests/shape.rs`) не понадобилось ни
/// одной новой строки: `null` — не пропуск поля, и **присутствие** всех
/// пятидесяти девяти ключей помощник проверяет по-прежнему.
///
/// **Чего сверка при этом не проверяет.** Тридцать один ключ мы отдаём
/// как `null` всегда, плюс `full_name` и `email` у того, кто их не
/// заполнил, плюс `username`: там `null` в эталоне, а строка у нас, и
/// прощающая ветка срабатывает с другой стороны. Итого тридцать четыре
/// поля без сверки типа. `null` с любой стороны помощник пропускает по своей же
/// документированной ветке. Для этих полей сверка вырождается в проверку
/// присутствия: тип у них не сверяется ничем. Восемнадцать из них — те,
/// где эталон значение шлёт всегда; это расхождение протокола, тест формы
/// его не поймает по построению, и записано оно отдельно —
/// `.claude/docs/decisions/null-for-settings-we-do-not-have.md`.
///
/// `last_*` остаются `null` и после задачи 3: отметки она писать научила,
/// а читать «последний проект» — нет. В волне 0 плана этой работы не
/// заведено ни за одной задачей, так что `null` тут не «пока нечего
/// считать», а незакрытый долг.
#[derive(Serialize)]
pub struct CurrentUserData {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub full_name: Option<String>,
    pub email: Option<String>,
    /// Часовой пояс именем IANA — так его ждёт плагин.
    pub timezone: String,
    /// Тайм-аут сессии в **минутах**: WakaTime отдаёт минуты, а у нас в
    /// базе секунды. Единица разная, и молча передать число нельзя.
    /// Перевод — `timeout_minutes` ниже, там же про округление.
    pub timeout: i64,
    /// Момент времени печатается `compat::rfc3339`; `null` — только у момента,
    /// которого нет в календаре, то есть у испорченной записи в базе.
    pub created_at: Option<String>,
    pub modified_at: Option<String>,

    // Отметки пишутся (задача 3), а вот читателя «последнего» у них нет.
    pub last_heartbeat_at: Option<String>,
    pub last_project: Option<String>,
    pub last_language: Option<String>,
    pub last_branch: Option<String>,
    pub last_plugin: Option<String>,
    pub last_plugin_name: Option<String>,

    // Публичного профиля нет: показывать нечего и негде.
    pub logged_time_public: bool,
    pub languages_used_public: bool,
    pub editors_used_public: bool,
    pub categories_used_public: bool,
    pub os_used_public: bool,
    pub is_email_public: bool,
    pub is_photo_public: bool,
    pub show_machine_name_ip: bool,
    pub public_email: Option<String>,
    pub public_profile_time_range: Option<String>,
    pub profile_url: Option<String>,
    pub profile_url_escaped: Option<String>,
    pub photo: Option<String>,
    pub share_all_time_badge: Option<String>,
    pub share_last_year_days: Option<i64>,

    // Анкеты нет: ни биографии, ни города, ни ссылок на соцсети.
    pub bio: Option<String>,
    pub city: Option<String>,
    pub location: Option<String>,
    pub website: Option<String>,
    pub human_readable_website: Option<String>,
    pub github_username: Option<String>,
    pub twitter_username: Option<String>,
    pub linkedin_username: Option<String>,
    pub wonderfuldev_username: Option<String>,
    pub is_hireable: bool,

    // Тарифов, оплаты и счетов нет и не предвидится: инстанс свой.
    pub plan: Option<String>,
    pub needs_payment_method: bool,
    pub has_basic_features: bool,
    pub has_premium_features: bool,
    pub can_show_pro_status: bool,
    pub invoice_counter: i64,
    pub invoice_id_format: Option<String>,

    // Подтверждения почты у нас нет — и почта, стало быть, не
    // подтверждена. Это не заглушка, а точное описание положения дел.
    pub is_email_confirmed: bool,
    // Ключ выдан, значит, заводить больше нечего: незавершённых шагов нет.
    pub is_onboarding_finished: bool,

    // Настройки отображения: у нас их нет, а чужое умолчание было бы
    // выдумкой, поэтому `null`/`false`.
    pub color_scheme: Option<String>,
    pub date_format: Option<String>,
    pub time_format_display: Option<String>,
    pub time_format_24hr: Option<bool>,
    pub default_dashboard_range: Option<String>,
    pub durations_slice_by: Option<String>,
    pub weekday_start: Option<i64>,
    // Три отдельных «мы этого не делаем», и все три — правда, а не
    // умолчание: фильтра «считать только записи» у нас нет и мы считаем
    // все отметки; висящие ветки мы не подсказываем; прятать
    // ИИ-статистику нечего — её нет вовсе, `ai_*` мы не отдаём и не
    // считаем.
    pub writes_only: bool,
    pub suggest_dangling_branches: bool,
    pub hide_ai_coding: bool,
}

/// Тайм-аут в минутах — единица, в которой его отдаёт WakaTime.
///
/// **Округление, а не усечение.** В секундах тайм-аут задаётся конфигом и
/// кратным шестидесяти быть не обязан: `crates/wakode/src/config.rs` берёт
/// число как есть, не проверяя ни знака, ни величины. Усечение отдало бы
/// за 30 секунд `timeout: 0`, а ноль в этом поле — не неточность, а другое
/// утверждение: «сессия не разрывается никогда». Поэтому округление к
/// ближайшей минуте и пол в одну минуту для любого положительного
/// тайм-аута. Нулём отвечаем на ноль и на любое отрицательное значение:
/// обоим осмысленной длительности не соответствует, и `0` тут — способ
/// сказать «настройка негодна», не выдумывая минуту.
///
/// `saturating_add` не украшение: `timeout_secs` приезжает из конфига
/// нетронутым, и `i64::MAX` уронил бы обработчик переполнением — в
/// отладочной сборке паникой, в релизной молча вернув `1`.
fn timeout_minutes(secs: i64) -> i64 {
    if secs <= 0 {
        return 0;
    }
    (secs.saturating_add(30) / 60).max(1)
}

/// Отказ приходит из `KeyAuth`: без годного ключа обработчик не
/// вызывается вовсе, а сам он не может не ответить — считать тут нечего и
/// ходить некуда. Поэтому `Json`, а не `Result`: тип, у которого ветка
/// ошибки недостижима, обещает больше, чем делает код.
pub async fn current(KeyAuth { user, .. }: KeyAuth) -> Json<CurrentUser> {
    Json(CurrentUser {
        data: CurrentUserData {
            id: user.id.to_string(),
            username: user.login.clone(),
            // Отображаемого имени может не быть — тогда логин: пустая
            // строка в интерфейсе плагина хуже, чем логин.
            display_name: user.display_name.clone().unwrap_or_else(|| user.login.clone()),
            full_name: user.display_name.clone(),
            email: user.email.clone(),
            timezone: user.timezone.name().to_owned(),
            timeout: timeout_minutes(user.timeout_secs),
            created_at: rfc3339(user.created_at),
            modified_at: rfc3339(user.updated_at),

            last_heartbeat_at: None,
            last_project: None,
            last_language: None,
            last_branch: None,
            last_plugin: None,
            last_plugin_name: None,

            logged_time_public: false,
            languages_used_public: false,
            editors_used_public: false,
            categories_used_public: false,
            os_used_public: false,
            is_email_public: false,
            is_photo_public: false,
            show_machine_name_ip: false,
            public_email: None,
            public_profile_time_range: None,
            profile_url: None,
            profile_url_escaped: None,
            photo: None,
            share_all_time_badge: None,
            share_last_year_days: None,

            bio: None,
            city: None,
            location: None,
            website: None,
            human_readable_website: None,
            github_username: None,
            twitter_username: None,
            linkedin_username: None,
            wonderfuldev_username: None,
            is_hireable: false,

            plan: None,
            needs_payment_method: false,
            has_basic_features: false,
            has_premium_features: false,
            can_show_pro_status: false,
            invoice_counter: 0,
            invoice_id_format: None,

            is_email_confirmed: false,
            is_onboarding_finished: true,

            color_scheme: None,
            date_format: None,
            time_format_display: None,
            time_format_24hr: None,
            default_dashboard_range: None,
            durations_slice_by: None,
            weekday_start: None,
            writes_only: false,
            suggest_dangling_branches: false,
            hide_ai_coding: false,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_timeout_is_rounded_to_the_nearest_minute() {
        // 900 секунд — те самые 15 минут из эталона.
        assert_eq!(timeout_minutes(900), 15);
        // Ровно половина минуты округляется вверх, а не отбрасывается.
        assert_eq!(timeout_minutes(90), 2);
        assert_eq!(timeout_minutes(89), 1);
    }

    #[test]
    fn a_timeout_shorter_than_a_minute_is_a_minute_and_not_zero() {
        // Конфиг принимает и такое (`config.rs` заводит 3 секунды в
        // тестах). Ноль в этом поле прочитался бы как «сессия не
        // разрывается никогда» — утверждение, обратное трёхсекундному
        // тайм-ауту.
        assert_eq!(timeout_minutes(3), 1);
        assert_eq!(timeout_minutes(1), 1);
    }

    #[test]
    fn a_timeout_that_is_not_a_duration_is_reported_as_zero() {
        // Ноль и отрицательное — одна категория: осмысленной длительности
        // им не соответствует никакой, и выдумывать минуту не из чего.
        assert_eq!(timeout_minutes(0), 0);
        assert_eq!(timeout_minutes(-1), 0);
        assert_eq!(timeout_minutes(i64::MIN), 0);
    }

    #[test]
    fn an_absurd_timeout_answers_instead_of_overflowing() {
        // `timeout_secs` приезжает из конфига непроверенным, так что
        // `i64::MAX` достижим. На `secs + 30` он переполняется: в
        // отладочной сборке паникой посреди обработчика, в релизной —
        // молчаливым `1`, то есть ответом, обратным заданному.
        assert_eq!(timeout_minutes(i64::MAX), i64::MAX / 60);
    }
}
