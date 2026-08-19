# WakaTime-совместимые эндпоинты, волна 0 — план реализации

> **Для агентов-исполнителей:** ОБЯЗАТЕЛЬНЫЙ СУБ-СКИЛЛ: `superpowers:subagent-driven-development` (рекомендуется) либо `superpowers:executing-plans`. Шаги помечены чекбоксами `- [ ]` для отслеживания.

**Goal:** Плагины редакторов пишут отметки в wakode и читают из него сводки тем же протоколом, что и из wakatime.com.

**Architecture:** Шесть эндпоинтов в `wakode-api::compat`, поверх уже готовых `wakode-core` (склейка интервалов, разбиение по локальным дням, агрегация) и `wakode-store` (запись отметок, чтение за период). Новой логики вычислений не появляется: калибровка подтвердила, что `build_intervals` уже воспроизводит модель WakaTime точно. Появляется слой сериализации — и он проверяется не на глазок, а сверкой формы с эталонами, снятыми с живого wakatime.com.

**Tech Stack:** axum 0.8, serde, `wakode-core`, `wakode-store`. Новых зависимостей план не вводит.

## Что в объёме и чего в нём нет

**В объёме — шесть эндпоинтов волны 0** (`docs/superpowers/specs/2026-08-15-wakode-design.md`, раздел «Волна 0»):

| Метод | Путь |
|---|---|
| POST | `/api/v1/users/current/heartbeats` |
| POST | `/api/v1/users/current/heartbeats.bulk` |
| GET | `/api/v1/users/current` |
| GET | `/api/v1/users/current/statusbar/today` |
| GET | `/api/v1/users/current/summaries` |
| GET | `/api/v1/users/current/all_time_since_today` |

**Вне объёма, намеренно:**

- **Подмодуль `internal`.** Он для собственного фронта, а фронта нет до плана 4. Эндпоинт без потребителя нельзя проверить на пригодность — только на то, что он отвечает. Заводится вместе с экраном, который его читает.
- **Поля `ai_*`.** Снимок 2026 года несёт их десятками (`ai_sessions`, `ai_model_costs`, `ai_prompt_length_avg`, …). Плагины редакторов их не читают; выдумывать значения хуже, чем не отдавать поле. Решение записано в спеке и продублировано в тесте формы явным списком исключений — чтобы оно оставалось видимым, а не растворилось.
- **Волна 2** (`/stats`, SVG-бейджи) и всё, что за ней.

## Откуда взялись формы ответов

Все формы сверены со снимками живого wakatime.com, снятыми `tools/capture-wakatime-fixtures.sh`. Это важно: прежняя редакция спеки выводила их из исходников `wakatime-cli` и документации, и ошиблась в пяти местах — у одиночной отметки лишние поля, у `statusbar/today` несуществующий `cached_at`, у `all_time_since_today` лишний `percent_calculated` и пропущенный `message`, у элемента `summaries` несуществующие `branches`/`entities`.

Два решения, зафиксированные измерением, а не рассуждением:

- `.claude/docs/decisions/no-tail-padding.md` — добавки к последней отметке сессии не существует, `tail_padding` обязан остаться нулём.
- `.claude/docs/decisions/duplicate-heartbeats-are-a-success.md` — дубликат это **успешный** элемент батча с кодом 202, нулевым (но версии 4!) идентификатором и полем `skip`.

## Global Constraints

- Язык: комментарии, докстринги и тексты ошибок — по-русски; имена тестов — английские фразы о поведении.
- Никаких упоминаний Claude/AI в коммит-сообщениях.
- **Мутационная проверка обязательна.** Для каждого нового теста автор вносит мутацию, ради которой тест написан, убеждается, что тест краснеет, и возвращает код **копированием из резервной копии файла**. Никогда `git checkout`/`stash`/`restore` — так уже терялась работа.
- Каждая задача заканчивается прогоном `cargo test --workspace` без единого предупреждения.
- `#[expect(..., reason = "...")]`, никогда `#[allow(...)]`.
- Зависимости — в `[workspace.dependencies]`, в крейт через `.workspace = true`.
- `wakode-api` не содержит криптографии.
- Новые маршруты добавляются в `router()` **выше** `method_not_allowed_fallback`, иначе останутся с пустым `405` axum'а.
- Журнальные тесты — только в `crates/wakode-api/tests/log.rs`, отдельным бинарём.

---

## Файлы

| Файл | Ответственность |
|---|---|
| `crates/wakode-api/tests/fixtures/wakatime/*.json` | Обезличенные эталоны форм. Создаются задачей 1. |
| `tools/scrub-wakatime-fixtures.py` | Обезличивание снимка. Создаётся задачей 1. |
| `crates/wakode-api/tests/shape.rs` | Сверка формы нашего ответа с эталоном. Отдельный бинарь: помощник нужен всем задачам, а `api.rs` уже за две тысячи строк. |
| `crates/wakode-api/src/compat/mod.rs` | Маршруты подмодуля и общее для них. |
| `crates/wakode-api/src/compat/shapes.rs` | Типы ответов: только сериализация, ни одного вычисления. |
| `crates/wakode-api/src/compat/user.rs` | `GET /api/v1/users/current`. |
| `crates/wakode-api/src/compat/heartbeats.rs` | Обе записи отметок. |
| `crates/wakode-api/src/compat/summaries.rs` | `summaries`, `statusbar/today`, `all_time_since_today` — общий вычислительный путь. |
| `crates/wakode-api/src/lib.rs` | Регистрация маршрутов. |

---

### Task 1: Обезличенные эталоны и сверка формы

**Files:**
- Create: `tools/scrub-wakatime-fixtures.py`
- Create: `crates/wakode-api/tests/fixtures/wakatime/*.json` (результат прогона)
- Create: `crates/wakode-api/tests/shape.rs`

**Interfaces:**
- Produces: `fn assert_shape_matches(ours: &serde_json::Value, theirs: &serde_json::Value)` и `fn fixture(name: &str) -> serde_json::Value` в `tests/shape.rs`; далее каждая задача добавляет туда свой тест.
- Consumes: снимки в `fixtures/wakatime/` (в git не входят, лежат у владельца после прогона `tools/capture-wakatime-fixtures.sh`).

**Зачем сверять форму, а не значения.** Эталон снят с чужого аккаунта: у него другие проекты, другое время, другие итоги. Сравнивать значения бессмысленно — они не совпадут никогда. Совпасть обязана **форма**: те же ключи на всех уровнях вложенности и те же типы значений. Тест, сравнивающий значения, пришлось бы ослаблять до бесполезности; тест, сравнивающий форму, ловит ровно то, ради чего фикстура и снята, — расхождение с чужим протоколом.

**Почему список исключений явный.** Полей `ai_*` мы не отдаём осознанно. Если помощник будет молча прощать любое недостающее поле, он перестанет ловить случайно забытое. Поэтому исключения перечисляются поимённо, и добавление нового требует правки списка — то есть решения, а не умолчания.

- [ ] **Step 1: Написать обезличиватель**

`tools/scrub-wakatime-fixtures.py`:

```python
#!/usr/bin/env python3
"""Заменить личные данные в снимке WakaTime на устойчивые заглушки.

Форма — вот что делает фикстуру фикстурой. Значения заменяются, ключи,
типы и структура сохраняются в неприкосновенности.

Замена детерминированная: одно и то же исходное значение всегда даёт одну
и ту же заглушку. Иначе повторный прогон давал бы шумный диф, а сверять
обезличенное с предыдущей редакцией стало бы нечем.
"""
import json
import pathlib
import sys

SRC = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "fixtures/wakatime")
DST = pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else
                   "crates/wakode-api/tests/fixtures/wakatime")

# Ключи, значения которых личные. Значение заменяется на заглушку того же
# типа: строка на строку, чтобы форма не поехала.
PERSONAL = {
    "email", "username", "full_name", "display_name", "website",
    "human_readable_website", "photo", "public_email", "city",
    "name", "project", "branch", "entity", "machine",
    "machine_name_id", "user_id", "id", "color",
}

_seen: dict[tuple[str, str], str] = {}


def placeholder(key: str, value: str) -> str:
    """Устойчивая заглушка для значения. Разные значения одного ключа
    получают разные номера — иначе два проекта слились бы в один и
    фикстура перестала бы показывать, что их было два."""
    slot = _seen.setdefault((key, value), f"{key}-{len(_seen)}")
    return slot


def scrub(node, key=None):
    if isinstance(node, dict):
        return {k: scrub(v, k) for k, v in node.items()}
    if isinstance(node, list):
        return [scrub(v, key) for v in node]
    if isinstance(node, str) and key in PERSONAL and node:
        return placeholder(key, node)
    return node


DST.mkdir(parents=True, exist_ok=True)
for src in sorted(SRC.glob("*.json")):
    data = json.loads(src.read_text())
    (DST / src.name).write_text(
        json.dumps(scrub(data), ensure_ascii=False, indent=2) + "\n"
    )
    print(f"  {src.name}")
print(f"обезличено в {DST}")
```

- [ ] **Step 2: Прогнать и прочитать результат**

```bash
python3 tools/scrub-wakatime-fixtures.py
grep -rn '@' crates/wakode-api/tests/fixtures/wakatime/current.json | head
```

Expected: почты нет. **Читать глазами обязательно**: обезличиватель работает по списку ключей, а не по содержимому, и путь к файлу, попавший в незнакомый ключ, он пропустит. Список `PERSONAL` дополняется по факту прочитанного, а не заранее.

- [ ] **Step 3: Написать помощник сверки формы**

`crates/wakode-api/tests/shape.rs`:

```rust
//! Сверка формы наших ответов с эталонами, снятыми с живого wakatime.com.
//!
//! Отдельный бинарь, а не часть `api.rs`: помощник нужен всем задачам
//! плана, а `api.rs` уже за две тысячи строк.

use serde_json::Value;

/// Поля, которых мы не отдаём осознанно.
///
/// Список именно явный. Прощай помощник любое недостающее поле — он
/// перестал бы ловить случайно забытое, а это и есть его работа.
/// Добавление строки сюда обязано быть решением, а не умолчанием.
const NOT_OURS: &[&str] = &[
    // Аналитика ИИ-ассистированного кода: плагины редакторов её не
    // читают, а выдумывать значения хуже, чем не отдавать поле.
    // Решение записано в спеке, раздел «Проверенные формы ответов».
    "ai_",
];

fn skipped(key: &str) -> bool {
    NOT_OURS.iter().any(|prefix| key.starts_with(prefix))
}

/// Прочитать эталон по имени.
pub fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/wakatime/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("эталон {path} не читается: {err}"));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("эталон {path} не JSON: {err}"))
}

/// Совпадают ли формы: те же ключи на всех уровнях, те же типы значений.
///
/// Значения не сравниваются и сравниваться не могут: эталон снят с чужого
/// аккаунта, у него другие проекты и другое время. Совпасть обязана форма.
pub fn assert_shape_matches(ours: &Value, theirs: &Value) {
    let mut problems = Vec::new();
    compare(ours, theirs, "", &mut problems);
    assert!(
        problems.is_empty(),
        "форма разошлась с эталоном:\n{}",
        problems.join("\n")
    );
}

fn compare(ours: &Value, theirs: &Value, path: &str, out: &mut Vec<String>) {
    match (ours, theirs) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, their_value) in b {
                if skipped(key) {
                    continue;
                }
                match a.get(key) {
                    Some(our_value) => compare(our_value, their_value, &format!("{path}.{key}"), out),
                    None => out.push(format!("  нет поля {path}.{key}")),
                }
            }
            for key in a.keys() {
                if !b.contains_key(key) {
                    out.push(format!("  лишнее поле {path}.{key}"));
                }
            }
        }
        // У массива сверяется форма первого элемента: остальные однородны
        // по построению. Пустой наш массив против непустого чужого — не
        // расхождение формы: у нас может не быть данных.
        (Value::Array(a), Value::Array(b)) => {
            if let (Some(x), Some(y)) = (a.first(), b.first()) {
                compare(x, y, &format!("{path}[]"), out);
            }
        }
        // `null` с обеих сторон — совпадение; `null` у одной из сторон о
        // типе не говорит ничего, и придираться тут не к чему.
        (Value::Null, _) | (_, Value::Null) => {}
        (x, y) if kind(x) == kind(y) => {}
        (x, y) => out.push(format!("  {path}: у нас {}, у них {}", kind(x), kind(y))),
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        // Целое и дробное не различаются: `total_seconds` приходит то
        // `0` то `21839.3` в зависимости от данных, и это одна форма.
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[test]
fn the_helper_notices_a_missing_field() {
    let theirs = serde_json::json!({"data": {"id": "x", "text": "y"}});
    let ours = serde_json::json!({"data": {"id": "x"}});
    let mut problems = Vec::new();
    compare(&ours, &theirs, "", &mut problems);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains(".data.text"), "{problems:?}");
}

#[test]
fn the_helper_notices_a_wrong_type() {
    let theirs = serde_json::json!({"total_seconds": 1.5});
    let ours = serde_json::json!({"total_seconds": "1.5"});
    let mut problems = Vec::new();
    compare(&ours, &theirs, "", &mut problems);
    assert_eq!(problems.len(), 1, "{problems:?}");
}

#[test]
fn the_helper_forgives_only_the_fields_we_declared() {
    // Зеркало: `ai_*` прощается, соседнее незнакомое поле — нет. Без
    // этой половины список исключений мог бы прощать всё подряд.
    let theirs = serde_json::json!({"ai_sessions": 3, "sessions": 3});
    let ours = serde_json::json!({});
    let mut problems = Vec::new();
    compare(&ours, &theirs, "", &mut problems);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains(".sessions"), "{problems:?}");
}
```

- [ ] **Step 4: Прогнать**

Run: `cargo test -p wakode-api --test shape`
Expected: PASS, три теста.

- [ ] **Step 5: Мутации**

С копией файла и возвратом **из копии**:

1. В `compare` убрать ветку «нет поля» → падает `the_helper_notices_a_missing_field`.
2. `kind` для `Number` заставить возвращать то же, что для `String` → падает `the_helper_notices_a_wrong_type`.
3. `skipped` заставить возвращать `true` всегда → падает `the_helper_forgives_only_the_fields_we_declared`.

- [ ] **Step 6: Коммит**

```bash
git add tools/scrub-wakatime-fixtures.py crates/wakode-api/tests/fixtures crates/wakode-api/tests/shape.rs
git commit -m "test(api): обезличенные эталоны WakaTime и сверка формы ответов"
```

---

### Task 2: `GET /api/v1/users/current`

**Files:**
- Create: `crates/wakode-api/src/compat/user.rs`
- Modify: `crates/wakode-api/src/compat/mod.rs`
- Modify: `crates/wakode-api/src/lib.rs`
- Test: `crates/wakode-api/tests/shape.rs`, `crates/wakode-api/tests/api.rs`

**Interfaces:**
- Produces: `pub async fn current(KeyAuth, State<AppState>) -> Result<Json<CurrentUser>, ApiError>`; `pub struct CurrentUser`
- Consumes: `KeyAuth { user: User, key_id: Uuid }` из `crate::auth` (`auth/api_key.rs:14`); `User { id, login, email, display_name, timezone: Tz, timeout_secs, is_admin, created_at, updated_at, .. }` (`wakode-store/src/users.rs:36`)

**Зачем он первым.** Плагины дёргают его, чтобы проверить ключ, — это первое, что делает свежеустановленный плагин, и первое, что сломается, если ключ не тот. Эндпоинт не считает ничего, поэтому проверяет ровно проводку: маршрут, аутентификацию, форму.

- [ ] **Step 1: Написать падающий тест формы**

В `crates/wakode-api/tests/shape.rs`. Понадобится доступ к состоянию с ключом — скопировать из `api.rs` помощники `a_settings`, `a_store`, `a_user_with_a_key`, `a_state_with_a_key` (`api.rs:21,35,65,98`) либо, если они уже вынесены, воспользоваться готовыми.

```rust
#[tokio::test]
async fn the_current_user_has_the_shape_wakatime_has() {
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = wakode_api::router(state)
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/v1/users/current")
                .header("Authorization", format!("Basic {}", base64_of(&key)))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_shape_matches(&json_body(response).await, &fixture("current"));
}
```

- [ ] **Step 2: Прогнать — падает**

Run: `cargo test -p wakode-api --test shape the_current_user`
Expected: FAIL — маршрута нет, приходит `404`.

- [ ] **Step 3: Посмотреть, что в эталоне**

```bash
jq '.data | keys' crates/wakode-api/tests/fixtures/wakatime/current.json
```

Форму брать оттуда, а не из головы. Ниже перечислено то, что было в снимке на момент написания плана; если эталон покажет другое — прав эталон, а не план, и расхождение надо описать в отчёте.

- [ ] **Step 4: Написать тип и обработчик**

`crates/wakode-api/src/compat/user.rs`:

```rust
//! `GET /api/v1/users/current` — профиль и проверка ключа.
//!
//! Первое, что дёргает свежеустановленный плагин. Ничего не считает:
//! проверяет проводку — маршрут, аутентификацию, форму.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::auth::KeyAuth;
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize)]
pub struct CurrentUser {
    pub data: CurrentUserData,
}

/// Поля — как в эталоне `current.json`, и в том же порядке.
///
/// Чего здесь нет намеренно: платёжных признаков, командных полей и всего
/// `ai_*`. Отдавать `false` там, где у нас нет самого понятия, значило бы
/// утверждать факт о несуществующей подсистеме.
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
    pub timeout: i64,
    pub created_at: String,
    pub modified_at: String,
}

pub async fn current(
    KeyAuth { user, .. }: KeyAuth,
    State(_state): State<AppState>,
) -> Result<Json<CurrentUser>, ApiError> {
    Ok(Json(CurrentUser {
        data: CurrentUserData {
            id: user.id.to_string(),
            username: user.login.clone(),
            display_name: user.display_name.clone().unwrap_or_else(|| user.login.clone()),
            full_name: user.display_name.clone(),
            email: user.email.clone(),
            timezone: user.timezone.name().to_owned(),
            timeout: user.timeout_secs / 60,
            created_at: rfc3339(user.created_at),
            modified_at: rfc3339(user.updated_at),
        },
    }))
}
```

Помощник `rfc3339` кладётся в `compat/mod.rs` — он понадобится всем задачам:

```rust
/// Момент времени в том виде, в каком его отдаёт WakaTime.
///
/// Формат сверен с эталоном, а не взят из привычки: разные эндпоинты
/// WakaTime печатают время по-разному, и наугад тут делать нечего.
pub(crate) fn rfc3339(t: wakode_core::Micros) -> String {
    chrono::DateTime::from_timestamp_micros(t.get())
        .expect("время вне диапазона")
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
```

`chrono` уже в зависимостях `wakode-api`? Проверить `crates/wakode-api/Cargo.toml`; если нет — добавить `chrono.workspace = true`.

- [ ] **Step 5: Завести подмодуль и маршрут**

`crates/wakode-api/src/compat/mod.rs`:

```rust
//! Эндпоинты, совместимые с WakaTime API.
//!
//! Форма ответов заморожена чужим протоколом: она не наша, менять её по
//! вкусу нельзя, и сверяется она с эталонами в `tests/fixtures/wakatime`.

pub mod user;

pub use user::current;
```

В `crates/wakode-api/src/lib.rs`, в `router()`, **выше** `method_not_allowed_fallback`:

```rust
            .route("/api/v1/users/current", get(compat::current))
```

- [ ] **Step 6: Прогнать**

Run: `cargo test -p wakode-api`
Expected: PASS.

- [ ] **Step 7: Тест отказа без ключа**

В `crates/wakode-api/tests/api.rs`:

```rust
#[tokio::test]
async fn the_current_user_needs_a_key() {
    // Без этого теста маршрут, забывший `KeyAuth`, отдавал бы чужой
    // профиль кому угодно, а тест формы остался бы зелёным: форма-то
    // правильная.
    let dir = tempfile::tempdir().unwrap();
    let (state, _key) = a_state_with_a_key(&dir).await;

    let response = router(state)
        .oneshot(
            Request::builder()
                .uri("/api/v1/users/current")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 8: Мутации**

1. Снять `KeyAuth` из сигнатуры `current` (взять пользователя иначе) → падает `the_current_user_needs_a_key`.
2. `timeout: user.timeout_secs` без деления на 60 → **не падает ничего**: форма та же, тип тот же. Это ожидаемо и требует своего теста — см. шаг 9.

- [ ] **Step 9: Закрыть единицу измерения тестом**

```rust
#[tokio::test]
async fn the_timeout_is_reported_in_minutes_not_seconds() {
    // Эталон отдаёт `timeout: 15` при 900 секундах. Единица — часть
    // контракта, а по форме её не видно: и минуты, и секунды это number.
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;
    // пользователь заводится с `default_timeout_secs` из `a_settings()`

    let body = json_body(current_user_response(state, &key).await).await;
    assert_eq!(body["data"]["timeout"], 777 / 60);
}
```

Прогнать мутацию из шага 8 пункт 2 ещё раз: теперь падает.

- [ ] **Step 10: Коммит**

```bash
git add crates/wakode-api/src/compat crates/wakode-api/src/lib.rs crates/wakode-api/tests
git commit -m "feat(compat): GET /api/v1/users/current"
```

---

### Task 3: `POST /api/v1/users/current/heartbeats` — одиночная отметка

**Files:**
- Create: `crates/wakode-api/src/compat/heartbeats.rs`
- Modify: `crates/wakode-api/src/compat/mod.rs`, `crates/wakode-api/src/lib.rs`
- Test: `crates/wakode-api/tests/api.rs`, `crates/wakode-api/tests/shape.rs`

**Interfaces:**
- Produces: `pub async fn post_heartbeat(...) -> Result<(StatusCode, Json<SingleAccepted>), ApiError>`; `pub struct IncomingBody` (разбор тела WakaTime); `pub fn to_store(body: IncomingBody) -> IncomingHeartbeat`
- Consumes: `HeartbeatRepo::record_heartbeats(user: Uuid, batch: Vec<IncomingHeartbeat>, tz: Tz) -> StoreResult<InsertReport>` (`wakode-store/src/repo.rs:20`); `IncomingHeartbeat` (`heartbeats.rs:17`); `InsertReport { outcomes: Vec<Outcome> }`, `Outcome::{Inserted, Duplicate}` (`heartbeats.rs:44`)

**Трейт обязан быть в области видимости.** `record_heartbeats` и `heartbeats_in_range` — методы трейта `HeartbeatRepo`, а не собственные методы `SqliteStore`; без `use wakode_store::HeartbeatRepo;` вызов не скомпилируется, а сообщение компилятора укажет на отсутствующий метод, а не на отсутствующий импорт. Трейт экспортируется из корня (`wakode-store/src/lib.rs:34`).

**Форма ответа, снятая с живого:** `201` с телом `{"data": {"id": "<uuid>"}}`. **Только `id`.** Полей `entity`, `type`, `time`, которые обещала прежняя редакция спеки, в ответе нет.

- [ ] **Step 1: Написать падающий тест приёма**

```rust
#[tokio::test]
async fn a_heartbeat_is_accepted_and_gets_an_id() {
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    let response = router(state)
        .oneshot(keyed(
            Request::builder()
                .method("POST")
                .uri("/api/v1/users/current/heartbeats")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"entity":"/дом/проект/файл.rs","type":"file","time":1755500000.0}"#,
                ))
                .unwrap(),
            &key,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    assert!(
        uuid::Uuid::parse_str(body["data"]["id"].as_str().unwrap()).is_ok(),
        "идентификатор не UUID: {body}"
    );
}
```

- [ ] **Step 2: Прогнать — падает** (`404`, маршрута нет)

- [ ] **Step 3: Разбор тела**

```rust
/// Тело отметки в том виде, в каком его шлёт плагин.
///
/// `deny_unknown_fields` здесь **не** ставится, и это не небрежность:
/// плагины разных версий шлют поля, которых мы не знаем, и отказывать
/// им значило бы ломать запись из-за того, что мы чего-то не читаем.
/// Обратное решение — у конфига, где неизвестный ключ это опечатка
/// владельца и молчать о ней нельзя.
#[derive(Deserialize)]
pub struct IncomingBody {
    pub entity: String,
    #[serde(rename = "type")]
    pub kind: EntityKind,
    /// Unix-секунды дробным числом — так их шлёт CLI.
    pub time: f64,
    // Путь именно `domain::`: в корне крейта `category_or_default` не
    // реэкспортирован, а `pub mod domain` есть (`wakode-core/src/lib.rs:70`).
    // Проверено, а не предположено.
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
```

- [ ] **Step 4: Обработчик**

Отметка ставится в очередь писателя тем же путём, что и батч: `record_heartbeats` с батчем из одного элемента. Отдельного пути для одиночной отправки не заводить — он разошёлся бы с батчем ровно там, где это труднее всего заметить.

```rust
pub async fn post_heartbeat(
    KeyAuth { user, .. }: KeyAuth,
    State(state): State<AppState>,
    body: Result<Json<IncomingBody>, JsonRejection>,
) -> Result<(StatusCode, Json<SingleAccepted>), ApiError> {
    let Json(body) = body.map_err(|_| ApiError::BadRequest("тело не разобралось".to_owned()))?;
    let report = state
        .store
        .record_heartbeats(user.id, vec![to_store(body)], user.timezone)
        .await?;
    // `id` берётся не из отчёта: `InsertReport` его не несёт. См. шаг 6 —
    // это и есть открытый вопрос задачи.
    ...
}
```

- [ ] **Step 5: Решить вопрос идентификатора**

`InsertReport` сегодня несёт только `Vec<Outcome>` — идентификаторов вставленных строк в нём нет, а ответ обязан их отдавать. Два пути, выбрать один и обосновать в отчёте:

1. **Расширить `InsertReport`** до `Vec<(Outcome, Uuid)>` в `wakode-store`. Честно: идентификатор рождается там, где происходит вставка. Стоит правки чужого крейта и его тестов.
2. **Считать идентификатор в `wakode-api`** по тому же правилу, по которому его считает хранилище. Дешевле, но заводит вторую копию правила — а копии разъезжаются, и это ровно тот дефект, который в этом проекте ловят каждое ревью.

**Рекомендация плана — путь 1.** Вторая копия правила порождения идентификатора обойдётся дороже, чем правка `InsertReport`, и обойдётся молча.

- [ ] **Step 6: Прогнать**

Run: `cargo test -p wakode-api`
Expected: PASS.

- [ ] **Step 7: Тест формы против эталона**

```rust
#[tokio::test]
async fn an_accepted_heartbeat_has_the_shape_wakatime_has() {
    // Эталон снят с живого: `{"data": {"id"}}` и ничего больше.
    ...
    assert_shape_matches(&body, &fixture("heartbeat-single"));
}
```

- [ ] **Step 8: Мутации**

1. Добавить в `SingleAccepted` поле `entity` → падает тест формы («лишнее поле»).
2. Отдавать `200` вместо `201` → падает `a_heartbeat_is_accepted_and_gets_an_id`.
3. Не звать `record_heartbeats` вовсе, отдавая свежий UUID → **проверить, падает ли хоть что-нибудь.** Если нет, тест написан вакуумно: он проверяет ответ, а не запись. Закрыть чтением через `heartbeats_in_range` в том же тесте.

- [ ] **Step 9: Коммит**

```bash
git commit -m "feat(compat): POST /api/v1/users/current/heartbeats"
```

---

### Task 4: `POST .../heartbeats.bulk` — батч

**Files:**
- Modify: `crates/wakode-api/src/compat/heartbeats.rs`, `crates/wakode-api/src/lib.rs`
- Test: `crates/wakode-api/tests/api.rs`, `crates/wakode-api/tests/shape.rs`

**Interfaces:**
- Produces: `pub async fn post_heartbeats_bulk(...) -> Result<(StatusCode, Json<BulkAccepted>), ApiError>`
- Consumes: `to_store`, `IncomingBody` из задачи 3

**Форма, снятая с живого** (`.claude/docs/decisions/duplicate-heartbeats-are-a-success.md`): верхний уровень `202`, тело `{"responses": [[body, status], …]}`. Тела элементов идут **без обёртки `data`** — в отличие от одиночной отправки.

| Исход | Код элемента | Тело элемента |
|---|---|---|
| принято | 2xx | `{"id": "<uuid>"}` |
| дубликат | **202** | `{"id": "00000000-0000-4000-a000-000000000000", "skip": "Too many duplicate heartbeats."}` |
| отвергнуто | 400 | `{"errors": {"entity": ["This field is required."]}}` |

**Ловушка, названная явно:** нулевой идентификатор — **не** `Uuid::nil()`. В нём стоят нибблы версии 4 (`…-4000-a000-…`). Тест, написанный против `Uuid::nil()`, прошёл бы на неверной реализации и упал на верной.

- [ ] **Step 1: Падающий тест дубликата**

```rust
#[tokio::test]
async fn a_duplicate_element_is_a_success_with_a_skip_note() {
    let dir = tempfile::tempdir().unwrap();
    let (state, key) = a_state_with_a_key(&dir).await;

    // Две одинаковые отметки в одном батче: вторая обязана оказаться
    // дубликатом первой.
    let body = r#"[{"entity":"/ф.rs","type":"file","time":1755500000.0},
                   {"entity":"/ф.rs","type":"file","time":1755500000.0}]"#;
    let response = /* POST .../heartbeats.bulk */;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = json_body(response).await;

    assert_eq!(body["responses"][1][1], 202, "дубликат объявлен неуспехом");
    assert_eq!(
        body["responses"][1][0]["id"], "00000000-0000-4000-a000-000000000000",
        "у дубликата обязан быть нулевой идентификатор — записи-то не было"
    );
    assert!(
        body["responses"][1][0]["skip"].is_string(),
        "дубликат не объяснён полем skip: {body}"
    );
}
```

- [ ] **Step 2: Падающий тест отвергнутого элемента**

```rust
#[tokio::test]
async fn a_bad_element_fails_alone_without_failing_the_batch() {
    // Это и есть весь смысл батча: один негодный элемент не должен
    // отменять соседние. Реализация, отвечающая 400 на весь запрос,
    // заставит плагин потерять годные отметки.
    let body = r#"[{"entity":"/ф.rs","type":"file","time":1755500000.0},
                   {"entity":"","type":"file","time":1755500001.0}]"#;
    ...
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(body["responses"][0][1], 201);
    assert_eq!(body["responses"][1][1], 400);
    assert!(body["responses"][1][0]["errors"]["entity"].is_array());
}
```

- [ ] **Step 3: Прогнать — падают**

- [ ] **Step 4: Реализовать**

Порядок элементов ответа обязан совпадать с порядком запроса: клиент сопоставляет их по индексу, других ключей у него нет.

- [ ] **Step 5: Предел размера батча**

Спека называет 25 отметок. Решить и записать в отчёте, что делать с батчем больше: отказ целиком или обработка первых 25. **Рекомендация плана:** отказ целиком с `400` и внятным текстом — молча отбросить хвост значит потерять отметки, не сказав об этом, а это худший из двух исходов. Завести тест.

- [ ] **Step 6: Прогнать, сверить форму с `heartbeat-bulk`**

- [ ] **Step 7: Мутации**

1. Отдавать дубликату код `409` → падает `a_duplicate_element_is_a_success_with_a_skip_note`.
2. Отдавать дубликату свежий UUID → падает тот же тест.
3. Использовать `Uuid::nil()` вместо нулевого v4 → падает тот же тест (ради этого он и сверяет строку целиком).
4. Отвечать `400` на весь батч при одном негодном элементе → падает `a_bad_element_fails_alone_without_failing_the_batch`.
5. Перевернуть порядок элементов ответа → падает он же.

- [ ] **Step 8: Коммит**

---

### Task 5: `GET .../summaries`

**Files:**
- Create: `crates/wakode-api/src/compat/summaries.rs`
- Create: `crates/wakode-api/src/compat/shapes.rs`
- Modify: `crates/wakode-api/src/compat/mod.rs`, `crates/wakode-api/src/lib.rs`
- Test: `crates/wakode-api/tests/api.rs`, `crates/wakode-api/tests/shape.rs`

**Interfaces:**
- Produces: `pub async fn summaries(...)`; `pub struct DaySummary` (элемент `data[]`, переиспользуется задачей 6); `pub fn day_summary(intervals: &[Interval], date: NaiveDate, tz: Tz, resolve: impl Fn(Sid) -> Option<Arc<str>>) -> DaySummary`
- Consumes: `heartbeats_in_range(user, from, to)` (`repo.rs:28`); `heartbeat_window(date, tz, cfg)` (`calendar.rs:219`); `build_intervals(&[Heartbeat], DurationConfig)` (`intervals.rs:41`); `split_by_local_day(&[Interval], tz)` (`calendar.rs:244`); `aggregate_by(&[Interval], |a| …)` (`aggregate.rs:26`); `grand_total`, `percent`; `Interner::resolve` через `HeartbeatRepo::resolve`

**Три вещи, которые здесь легко сделать неправильно:**

1. **Окно выборки шире дня.** Брать отметки ровно за сутки нельзя: интервал, начавшийся до полуночи, обязан дать свою часть этому дню. Для этого и существует `heartbeat_window` (`calendar.rs:219`) — она расширяет границы на `timeout` в обе стороны. Границы дня для **итогов** при этом другие: `local_day_bounds`.
2. **Пустые дни присутствуют.** Диапазон из 30 дней даёт 30 элементов `data[]`, даже если работы в 12 из них не было: у пустого те же ключи, массивы пустые, `grand_total.text` — `"0 secs"`. Проверено эталоном `summaries-month`. `split_by_local_day` сама пустых дней не порождает — их добавляет этот эндпоинт.
3. **`start`/`end` верхнего уровня — моменты UTC, соответствующие полуночи часового пояса пользователя.** В эталоне: `2026-07-19T21:00:00Z` для `Europe/Moscow`. Не полночь UTC.

**`DurationConfig` берётся из пользователя, а не из настроек приложения:** `User::timeout_secs` (`users.rs:43`), потому что таймаут у каждого свой. `tail_padding` — ноль; см. `.claude/docs/decisions/no-tail-padding.md`, это измеренное значение.

- [ ] **Step 1: Падающий тест пустых дней**

```rust
#[tokio::test]
async fn every_day_of_the_range_is_present_even_when_empty() {
    // Проверено чужим поведением: 30-дневный диапазон даёт 30 элементов,
    // из них 12 пустых. `split_by_local_day` пустых дней не порождает —
    // их обязан добавить эндпоинт, и без этого теста он о них забудет.
    ...
    assert_eq!(body["data"].as_array().unwrap().len(), 7);
    assert_eq!(body["data"][3]["grand_total"]["total_seconds"], 0);
    assert_eq!(body["data"][3]["grand_total"]["text"], "0 secs");
    assert!(body["data"][3]["projects"].as_array().unwrap().is_empty());
}
```

- [ ] **Step 2: Падающий тест переноса через полночь**

```rust
#[tokio::test]
async fn work_that_crosses_local_midnight_is_split_between_the_two_days() {
    // Отметки за 23:50 и 00:10 по часовому поясу пользователя.
    // Реализация, выбирающая отметки ровно за сутки, потеряет начало
    // интервала и недосчитает первому дню — а по одному дню это
    // незаметно.
    ...
}
```

- [ ] **Step 3: Прогнать — падают**

- [ ] **Step 4: Реализовать вычислительный путь**

```rust
let cfg = DurationConfig::new(
    Micros::from_secs(user.timeout_secs),
    Micros::ZERO,  // добавки не существует — решение no-tail-padding
)?;
let (from, to) = /* объединение heartbeat_window по краям диапазона */;
let heartbeats = state.store.heartbeats_in_range(user.id, from, to).await?;
let intervals = build_intervals(&heartbeats, cfg);
let by_day = split_by_local_day(&intervals, user.timezone);
// затем: для каждой даты диапазона — by_day.get(&date), либо пустой день
```

- [ ] **Step 5: Форматирование длительностей**

`text`, `digital`, `decimal`, `hours`, `minutes` — пять представлений одного числа. Вынести в `shapes.rs` одной функцией и покрыть таблицей значений, включая ноль (`"0 secs"`), меньше минуты, ровно час и больше суток. Форматы сверять с эталоном: `jq '.data[0].grand_total' …/summaries-one-day.json`.

- [ ] **Step 6: Прогнать, сверить форму с `summaries-one-day` и `summaries-week`**

`summaries-week` нужен отдельно: `cumulative_total` и `daily_average` на одном дне вырождаются и формы не показывают.

- [ ] **Step 7: Мутации**

1. Убрать добавление пустых дней → падает `every_day_of_the_range_is_present_even_when_empty`.
2. Брать отметки ровно за `local_day_bounds` вместо `heartbeat_window` → падает `work_that_crosses_local_midnight_is_split_between_the_two_days`.
3. Считать `DurationConfig` из `AppSettings::default_timeout_secs` вместо `User::timeout_secs` → **завести тест с пользователем, у которого таймаут не совпадает с настройкой** (в `a_settings()` он намеренно 777, а не 900 — этим и воспользоваться).
4. `tail_padding` ненулевой → итог разъезжается; тест на точную сумму по известным отметкам.

- [ ] **Step 8: Коммит**

---

### Task 6: `GET .../statusbar/today`

**Files:**
- Modify: `crates/wakode-api/src/compat/summaries.rs`, `crates/wakode-api/src/lib.rs`
- Test: `crates/wakode-api/tests/shape.rs`

**Interfaces:**
- Consumes: `day_summary(...)` из задачи 5

**Форма, снятая с живого:** `{"data": <элемент data[] из summaries>, "has_team_features": <bool>}`. Поля `cached_at`, которое предполагала прежняя редакция спеки, **нет**.

`has_team_features` отдавать `false`: командных возможностей у wakode нет вовсе, и это не заглушка, а факт.

- [ ] **Step 1: Падающий тест формы** против `fixture("statusbar-today")`
- [ ] **Step 2: Прогнать — падает**
- [ ] **Step 3: Реализовать** переиспользованием `day_summary` для сегодняшнего дня в поясе пользователя
- [ ] **Step 4: Тест «сегодня» считается в поясе пользователя, а не сервера**

```rust
#[tokio::test]
async fn today_is_the_users_today_not_the_servers() {
    // Пользователь в Australia/Sydney; UTC-«сегодня» и его «сегодня» —
    // разные даты часть суток. Реализация на `Utc::now().date_naive()`
    // покажет ему чужой день, и заметит он это в статусной строке.
    ...
}
```

- [ ] **Step 5: Мутации** — `cached_at` вернуть → падает форма; `Utc::now().date_naive()` → падает тест пояса
- [ ] **Step 6: Коммит**

---

### Task 7: `GET .../all_time_since_today`

**Files:**
- Modify: `crates/wakode-api/src/compat/summaries.rs`, `crates/wakode-api/src/lib.rs`
- Test: `crates/wakode-api/tests/shape.rs`, `crates/wakode-api/tests/api.rs`

**Форма, снятая с живого:** верхний уровень — `data` **и `message`**. `data`: `total_seconds`, `text`, `decimal`, `digital`, `daily_average`, `is_up_to_date`, `timeout`, `range`. Поля `percent_calculated`, которое обещала прежняя редакция спеки, **нет**.

`is_up_to_date` отдавать `true`: у wakode нет отложенного пересчёта, итог всегда свежий. `message` — пустая строка либо то, что окажется в эталоне.

- [ ] **Step 1: Падающий тест формы**
- [ ] **Step 2: Прогнать — падает**
- [ ] **Step 3: Реализовать**

Диапазон — от первой отметки пользователя до конца сегодняшнего дня в его поясе. Первую отметку взять запросом к хранилищу; если отметок нет вовсе — `total_seconds: 0` и диапазон в один сегодняшний день. **Завести тест на пустого пользователя:** эндпоинт, падающий на пользователе без отметок, ломает первый же запуск свежего инстанса.

- [ ] **Step 4: Тест пустого пользователя**
- [ ] **Step 5: Мутации** — вернуть `percent_calculated` → падает форма; убрать `message` → падает форма; на пустом пользователе паниковать → падает тест пустого
- [ ] **Step 6: Коммит**

---

## Самопроверка плана

**Покрытие спеки.** Шесть эндпоинтов волны 0 — задачи 2–7. Аутентификация ключом — переиспользуется готовый `KeyAuth`, отдельной задачи не требует, но проверяется тестом в задаче 2. Поля heartbeat из раздела спеки «Поля heartbeat» — задача 3, шаг 3. Формы ответов — задачи 3, 4, 5, 6, 7, каждая сверяется с эталоном. `internal` — вне объёма, обосновано выше. Волна 2 — вне объёма.

**Незакрытые места, названные честно:**

- **Задача 3, шаг 5** оставляет выбор между расширением `InsertReport` и второй копией правила. План рекомендует первое и требует обоснования выбора в отчёте. Это единственное место, где план не решает за исполнителя, — и решает не он, а тот, кто увидит код `wakode-store` перед глазами.
- **Задача 4, шаг 5** оставляет выбор поведения при батче больше 25 с рекомендацией. Спека называет число, но не называет поведение при превышении.
- **Формы взяты из снимка на 2026-08-19.** Если эталон покажет другое — прав эталон. Каждая задача, где это возможно, начинается с `jq` по фикстуре, а не с копирования из плана.

**Согласованность имён.** `day_summary` (задача 5) переиспользуется задачами 6 и 7 под тем же именем. `IncomingBody` и `to_store` (задача 3) переиспользуются задачей 4. `assert_shape_matches` и `fixture` (задача 1) — всеми. `rfc3339` (задача 2) — всеми.

**Чего в плане намеренно нет.** Ни одного шага «добавить обработку ошибок» или «покрыть краевые случаи»: краевые случаи названы поимённо — пустой пользователь, перенос через полночь, чужой часовой пояс, батч сверх предела, дубликат, негодный элемент.
