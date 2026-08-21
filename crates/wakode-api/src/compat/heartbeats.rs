//! `POST /api/v1/users/current/heartbeats` — приём одиночной отметки.
//!
//! Главный поток данных трекера: всё остальное только читает то, что
//! записал этот путь.

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wakode_core::{Category, EntityKind, Micros};
use wakode_store::{HeartbeatRepo, IncomingHeartbeat, Outcome};

use crate::auth::KeyAuth;
use crate::error::ApiError;
use crate::state::AppState;

/// Тело отметки в том виде, в каком его шлёт плагин.
///
/// `deny_unknown_fields` здесь **не** ставится, и это не небрежность:
/// плагины разных версий шлют поля, которых мы не знаем, и отказывать им
/// значило бы ломать запись из-за того, что мы чего-то не читаем.
/// Обратное решение — у конфига, где неизвестный ключ это опечатка
/// владельца и молчать о ней нельзя.
#[derive(Deserialize)]
pub struct IncomingBody {
    pub entity: String,
    /// Незнакомый вид сущности — **отказ**, в отличие от незнакомой
    /// категории строкой ниже. Ассиметрия намеренная, и вот чем эти два
    /// поля различаются.
    ///
    /// У `Category` есть вариант `Unknown` с `#[serde(other)]`, заведённый
    /// ровно на этот случай: плагин обновляется раньше сервера, и
    /// незнакомое значение не имеет права уронить разбор всей отметки —
    /// потерянное время дороже точности измерения. У `EntityKind`
    /// (`wakode-core/src/domain.rs`) такого варианта нет.
    ///
    /// Заводить его я не стал, и довод не «дорого править чужой крейт», а
    /// вот какой: **у `Unknown` не было бы чем стать на обратном пути.**
    /// `Category::Unknown` обязан уезжать клиенту как `null` — законное
    /// значение чужого протокола; это записано в докстринге `Category`
    /// (`wakode-core/src/domain.rs`) как обязанность слоя совместимости.
    /// В будущем времени: наружу категории в волне 0 пока не отдаёт никто,
    /// отобразить `Unknown` придётся задачам 5–7. У `type`
    /// такого значения в протоколе нет вовсе — эталон `heartbeats-day.json`
    /// отдаёт `type` строкой всегда, а измеренная форма отказа WakaTime
    /// говорит про обязательное поле. Приняв незнакомый вид, мы записали бы
    /// в базу отметку, которую не сможем ни честно назвать, ни отдать
    /// обратно, — и выбирать пришлось бы между выдуманным значением на
    /// проводе и `file` вместо правды.
    ///
    /// Цена решения признаётся: добавь WakaTime пятый вид сущности — и
    /// отметки обновлённого плагина начнут отвергаться, пока не обновится
    /// сервер. Смягчает это ровно одно: отвергаются они кодом `400`,
    /// который плагин выбрасывает, а не копит (см.
    /// `a_body_we_cannot_parse_is_refused_with_a_code_the_plugin_drops`),
    /// так что владелец теряет незнакомые отметки, а не всю очередь.
    /// Появись у `type` на проводе законное «не знаю» — решение надо
    /// пересматривать.
    #[serde(rename = "type")]
    pub kind: EntityKind,
    /// Unix-секунды дробным числом — так их шлёт CLI.
    pub time: f64,
    // Путь именно `domain::`: в корне крейта `category_or_default` не
    // реэкспортирован, а `pub mod domain` есть.
    #[serde(default, deserialize_with = "wakode_core::domain::category_or_default")]
    pub category: Category,
    pub project: Option<String>,
    pub branch: Option<String>,
    pub language: Option<String>,
    pub editor: Option<String>,
    #[serde(rename = "operating_system")]
    pub os: Option<String>,
    pub machine: Option<String>,
    pub dependencies: Option<String>,
    pub lines: Option<i64>,
    pub lineno: Option<i64>,
    pub cursorpos: Option<i64>,
    pub line_additions: Option<i64>,
    pub line_deletions: Option<i64>,
    pub project_root_count: Option<i64>,
    #[serde(default)]
    pub is_write: bool,
}

/// Идентификатор, которым WakaTime отвечает на отметку-повтор.
///
/// Снят с живого (`tests/fixtures/wakatime/heartbeat-bulk.json`) и записан
/// в `.claude/docs/decisions/duplicate-heartbeats-are-a-success.md`. Это
/// **не** `Uuid::nil()`: нибблы версии 4 в нём стоят, и код, отдающий
/// нулевой UUID, разошёлся бы с эталоном на четырёх битах, которых не
/// видно глазами.
///
/// Отдавать вместо него свежий идентификатор нельзя: строки в базе нет, и
/// клиент, сохранивший такой идентификатор, не найдёт по нему ничего
/// никогда. Отдавать ошибку — тоже: клиент дошлёт отметку заново и
/// получит ещё один повтор.
pub const DUPLICATE_ID: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_a000_0000_0000_0000);

/// Чем тело не годится: поле протокола и объяснение.
///
/// Именем поля здесь пока пользуется только текст ошибки одиночного
/// эндпоинта. Хранится оно отдельно от объяснения потому, что батч
/// (задача 4) отдаёт отказ пер-элементным `{"errors": {"<поле>": [...]}}`,
/// где имя поля — ключ, а не часть фразы.
#[derive(Debug, PartialEq, Eq)]
pub struct Rejected {
    pub field: &'static str,
    pub why: &'static str,
}

#[derive(Serialize)]
pub struct SingleAccepted {
    pub data: AcceptedHeartbeat,
}

/// Форма снята с живого: в ответе **только** `id`. Полей `entity`, `type`
/// и `time`, которые обещала прежняя редакция спеки, там нет.
#[derive(Serialize)]
pub struct AcceptedHeartbeat {
    pub id: String,
}

/// Время отметки из дробных Unix-секунд.
///
/// `Micros::from_secs_f64` — это `(secs * 1e6).round() as i64`, а
/// приведение `as` в Rust насыщается. Измерено, а не выведено: `NaN` даёт
/// **ноль**, `+inf` и любая величина за пределами `i64` — `i64::MAX`,
/// `-inf` — `i64::MIN`.
///
/// **Что отсюда реально прилетает по проводу.** Насыщение до краёв `i64` —
/// прилетает: `{"time": 1e30}` это совершенно обычный `f64`, разбираемый
/// без возражений, а `1e30` секунд лежит далеко за календарём `chrono`
/// (он кончается на 262143 году). Такая отметка без проверки легла бы в
/// базу, и по ней считало бы всё, что её потом читает.
///
/// Ноль от `NaN` по проводу **не** прилетает: в JSON нет литерала `NaN`, а
/// `1e400` `serde_json` отбивает сам, ещё до обработчика. `is_finite`
/// стоит здесь не против плагина, а потому что это единственный вход
/// `f64 -> Micros` в крейте, и из трёх исходов насыщения ровно этот
/// проходит **внутрь** календаря: ноль микросекунд — 1 января 1970 года,
/// момент вполне законный. Отсекать его позже станет нечем.
fn time_from_secs(secs: f64) -> Result<Micros, Rejected> {
    let outside = Rejected {
        field: "time",
        why: "время отметки не лежит в календаре",
    };
    if !secs.is_finite() {
        return Err(outside);
    }
    let time = Micros::from_secs_f64(secs);
    if chrono::DateTime::from_timestamp_micros(time.get()).is_none() {
        return Err(outside);
    }
    Ok(time)
}

/// Проволочное тело — в то, что понимает хранилище.
///
/// `plugin` и поля `ai_*` остаются пустыми: первый приезжает
/// `User-Agent`'ом, который этот путь пока не читает, вторых мы не считаем
/// вовсе.
pub fn to_store(body: IncomingBody) -> Result<IncomingHeartbeat, Rejected> {
    Ok(IncomingHeartbeat {
        time: time_from_secs(body.time)?,
        entity: body.entity,
        kind: body.kind,
        category: body.category,
        project: body.project,
        branch: body.branch,
        language: body.language,
        editor: body.editor,
        os: body.os,
        machine: body.machine,
        plugin: None,
        is_write: body.is_write,
        lines: body.lines,
        lineno: body.lineno,
        cursorpos: body.cursorpos,
        line_additions: body.line_additions,
        line_deletions: body.line_deletions,
        project_root_count: body.project_root_count,
        dependencies: body.dependencies,
        ai_line_changes: None,
        human_line_changes: None,
        ai_meta: None,
    })
}

/// Принять отметку.
///
/// Отметка ставится в очередь писателя тем же путём, что и батч:
/// `record_heartbeats` с батчем из одного элемента. Отдельного пути для
/// одиночной отправки нет — он разошёлся бы с батчем ровно там, где это
/// труднее всего заметить.
///
/// **Отказ отдаётся нашей формой `{"error": "..."}`, а не чужой.** С
/// живого она не снята: единственная измеренная форма отказа WakaTime —
/// пер-элементная `{"errors": {"<поле>": [...]}}` в теле батча, и
/// `duplicate-heartbeats-are-a-success.md` отдельно отмечает, что
/// единственного числа `error` у них не встречается. Здесь сознательно
/// выбрана внутренняя конвенция проекта: `ApiError` держит обещание «тело
/// всегда JSON с полем `error`» на всех маршрутах разом (`error.rs`), и
/// один эндпоинт, отвечающий иначе, это обещание сломал бы. Задача 4
/// решает иначе и по делу: там форма элемента измерена.
pub async fn post_heartbeat(
    KeyAuth { user, .. }: KeyAuth,
    State(state): State<AppState>,
    body: Result<Json<IncomingBody>, JsonRejection>,
) -> Result<(StatusCode, Json<SingleAccepted>), ApiError> {
    // Подробность разбора уходит в журнал, а не клиенту: тексты `axum`
    // английские, а тексты ошибок этого проекта русские, — но владельцу
    // инстанса, у которого плагин шлёт неразбираемое, нужна как раз она.
    let Json(body) = body.map_err(|err| {
        tracing::warn!(error = %err, "тело отметки не разобралось");
        ApiError::BadRequest("тело отметки не разобралось".to_owned())
    })?;
    let incoming = to_store(body)
        .map_err(|rejected| ApiError::BadRequest(format!("{}: {}", rejected.field, rejected.why)))?;

    let report = state
        .store
        .record_heartbeats(user.id, vec![incoming], user.timezone)
        .await?;

    let id = match report.outcomes.first() {
        Some(Outcome::Inserted(id)) => *id,
        // Повтор — успех, а не отказ: очередь плагина штатно доставляется
        // по второму разу, и `4xx` заставил бы её копиться. Отличает его от
        // вставки идентификатор, а не код ответа.
        //
        // Форма ответа на повтор **у этого эндпоинта не измерена** — снят
        // только батч. Мы расходимся с единственной известной формой сразу
        // по двум признакам: кодом (`201` против `202`) и составом тела (у
        // них рядом с нулевым идентификатором едет `skip` с объяснением, у
        // нас его нет). Чем это грозит, чем удерживается и одной какой
        // пробой закрывается —
        // `.claude/docs/decisions/duplicate-heartbeats-are-a-success.md`.
        Some(Outcome::Duplicate) => DUPLICATE_ID,
        // Недостижимо: батч из одного элемента даёт отчёт из одного
        // исхода. Молчаливый `201` с выдуманным идентификатором здесь был
        // бы хуже пятисотки — клиент сохранил бы его и не нашёл по нему
        // ничего.
        None => {
            tracing::error!("отчёт хранилища пуст на батче из одной отметки");
            return Err(ApiError::Internal);
        }
    };

    Ok((
        StatusCode::CREATED,
        Json(SingleAccepted {
            data: AcceptedHeartbeat { id: id.to_string() },
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_outside_the_calendar_is_refused_and_not_a_panic() {
        // `from_secs_f64` насыщается: бесконечности уезжают на края `i64`,
        // а те лежат вне календаря `chrono`.
        assert!(time_from_secs(f64::INFINITY).is_err());
        assert!(time_from_secs(f64::NEG_INFINITY).is_err());
        assert!(time_from_secs(1e30).is_err());
    }

    #[test]
    fn a_time_that_is_not_a_number_is_refused_rather_than_read_as_the_epoch() {
        // Единственный исход насыщения, который календарь пропустил бы:
        // `NaN as i64` — ноль, а ноль это 1 января 1970 года. Вторая строка
        // не украшение — она и есть причина существования первой, и без неё
        // проверка `is_finite` выглядела бы лишней.
        //
        // По проводу этот случай не приезжает (в JSON нет литерала `NaN`),
        // так что тест сторожит границу `f64 -> Micros`, а не эндпоинт.
        assert_eq!(Micros::from_secs_f64(f64::NAN), Micros::ZERO);
        assert!(time_from_secs(f64::NAN).is_err());
    }

    #[test]
    fn an_ordinary_time_survives_to_the_microsecond() {
        assert_eq!(
            time_from_secs(1_755_500_000.5),
            Ok(Micros::new(1_755_500_000_500_000))
        );
    }

    /// Восемь полей, которых не читает ни один путь кода.
    ///
    /// Довод «читателя нет, значит и проверять нечего» в этом проекте уже
    /// отвергнут — в `wakode-store/src/heartbeats.rs`, у
    /// `every_unread_column_lands_in_the_place_the_insert_promised`:
    /// отсутствие читателя не делает ошибку неважной, оно делает её
    /// бесшумной и необратимой. Переставленные местами `lines` и `lineno`
    /// не уронят ничего и всплывут в волне 1, когда в базе уже месяцы
    /// отметок, которые задним числом не расшить. Там за неимением
    /// читателя понадобился сырой `SELECT`; здесь дешевле — `to_store`
    /// чистая, и база не нужна вовсе.
    ///
    /// Числа **попарно различны**, включая `is_write`: с одинаковыми
    /// значениями перестановка двух полей невидима и здесь тоже, а
    /// `is_write: true` дало бы единицу — ровно то же, что в соседнем
    /// `lines`.
    ///
    /// Тело разбирается из JSON, а не собирается полями: проверяются
    /// заодно проволочные имена, и `operating_system` среди них — то
    /// единственное, что зовётся у нас иначе (`os`).
    #[test]
    fn the_numbers_of_the_body_land_where_the_protocol_names_them() {
        let body: IncomingBody = serde_json::from_str(
            r#"{
                "entity": "/дом/проект/файл.rs",
                "type": "file",
                "time": 1755500000.0,
                "operating_system": "ос",
                "dependencies": "зависимости",
                "lines": 11,
                "lineno": 12,
                "cursorpos": 13,
                "line_additions": 14,
                "line_deletions": 15,
                "project_root_count": 16,
                "is_write": false
            }"#,
        )
        .unwrap();

        let hb = to_store(body).unwrap();

        assert_eq!(hb.lines, Some(11), "lines");
        assert_eq!(hb.lineno, Some(12), "lineno");
        assert_eq!(hb.cursorpos, Some(13), "cursorpos");
        assert_eq!(hb.line_additions, Some(14), "line_additions");
        assert_eq!(hb.line_deletions, Some(15), "line_deletions");
        assert_eq!(hb.project_root_count, Some(16), "project_root_count");
        assert_eq!(hb.dependencies.as_deref(), Some("зависимости"), "dependencies");
        assert!(!hb.is_write, "is_write");
        assert_eq!(hb.os.as_deref(), Some("ос"), "operating_system");

        // Три поля, которые этот путь не заполняет и заполнять не обещал:
        // `plugin` приезжает `User-Agent`'ом, которого мы не читаем, а
        // `ai_*` мы не считаем вовсе. `Some` здесь означал бы выдумку.
        assert_eq!(hb.plugin, None, "plugin");
        assert_eq!(hb.ai_line_changes, None, "ai_line_changes");
        assert_eq!(hb.human_line_changes, None, "human_line_changes");
        assert_eq!(hb.ai_meta, None, "ai_meta");
    }

    /// Вторая половина `is_write`, без которой первой не хватает.
    ///
    /// В тесте выше это поле подано как `false` — ради попарной
    /// различности чисел, — и `assert!(!hb.is_write)` там выполняется при
    /// любой прошитой константе. То есть ровно то поле, которое соседний
    /// тест перечисляет среди проверенных, оставалось не привязанным к
    /// телу ничем: подмена `body.is_write` на `false` проходила весь
    /// набор зелёной.
    ///
    /// Цена такой ошибки — та самая, ради которой тест и писался: все
    /// отметки навсегда легли бы в базу как чтения. Поле идёт в
    /// `dedup_hash` и понадобится фильтру `writes_only` в волне 1, а
    /// переписать его задним числом будет не из чего.
    #[test]
    fn a_write_is_recorded_as_a_write() {
        let body: IncomingBody = serde_json::from_str(
            r#"{
                "entity": "/дом/проект/файл.rs",
                "type": "file",
                "time": 1755500000.0,
                "is_write": true
            }"#,
        )
        .unwrap();

        assert!(to_store(body).unwrap().is_write, "is_write");
    }
}
