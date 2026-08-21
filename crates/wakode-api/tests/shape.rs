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
