use chrono::NaiveDate;
use chrono_tz::Tz;
use wakode_core::{Category, EntityKind, Micros};
use wakode_store::{
    dirty_days_for, find_user_by_id, find_user_by_login, insert_heartbeats, insert_user,
    load_heartbeats, migrate, open_in_memory, schema_version, IncomingHeartbeat, Interner,
    NewUser, Outcome,
};

fn a_user(login: &str) -> NewUser {
    NewUser {
        login: login.to_owned(),
        email: None,
        password_hash: "непрозрачные байты из плана 3".to_owned(),
        display_name: None,
        timezone: "Europe/Moscow".parse().unwrap(),
        timeout_secs: 900,
        is_admin: false,
    }
}

/// Единственный тест в этом файле, которому позволено знать про SQL:
/// он проверяет саму схему, а не поведение поверх неё.
#[test]
fn wave_zero_schema_creates_every_table() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    assert_eq!(schema_version(&conn).unwrap(), 1);

    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    let tables: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .filter(|name: &String| !name.starts_with("sqlite_"))
        .collect();

    assert_eq!(
        tables,
        vec![
            "api_keys",
            "dirty_days",
            "heartbeats",
            "sessions",
            "strings",
            "team_members",
            "teams",
            "users",
        ]
    );
}

#[test]
fn heartbeat_dedup_index_is_unique() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    let unique: i64 = conn
        .query_row(
            "SELECT [unique] FROM pragma_index_list('heartbeats') WHERE name = 'hb_dedup'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unique, 1, "без уникальности индекса дедупликация не работает");
}

#[test]
fn interning_the_same_value_twice_gives_the_same_number() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let first = interner.intern_batch(&conn, &["src/main.rs"]).unwrap();
    let second = interner.intern_batch(&conn, &["src/main.rs"]).unwrap();

    assert_eq!(first, second);
}

#[test]
fn interned_value_resolves_back_to_the_original_string() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let ids = interner.intern_batch(&conn, &["wakode", "rust"]).unwrap();

    assert_eq!(&*interner.resolve(ids[0]).unwrap(), "wakode");
    assert_eq!(&*interner.resolve(ids[1]).unwrap(), "rust");
    assert_eq!(interner.lookup("rust"), Some(ids[1]));
    assert_eq!(interner.lookup("не интернировали"), None);
}

#[test]
fn a_batch_with_repeats_inside_it_stays_consistent() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let ids = interner
        .intern_batch(&conn, &["a", "b", "a", "b", "a"])
        .unwrap();

    assert_eq!(ids.len(), 5);
    assert_eq!(ids[0], ids[2]);
    assert_eq!(ids[0], ids[4]);
    assert_eq!(ids[1], ids[3]);
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn dictionary_survives_reopening_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wakode.db");

    let sid = {
        let mut conn = wakode_store::open(&path).unwrap();
        migrate(&mut conn).unwrap();
        let interner = Interner::load(&conn).unwrap();
        interner.intern_batch(&conn, &["постоянная строка"]).unwrap()[0]
    };

    let conn = wakode_store::open(&path).unwrap();
    let interner = Interner::load(&conn).unwrap();

    assert_eq!(&*interner.resolve(sid).unwrap(), "постоянная строка");
    assert_eq!(interner.lookup("постоянная строка"), Some(sid));
}

#[test]
fn intern_batch_commits_before_it_returns() {
    // Доказательство того, что коммит произошёл **внутри** `intern_batch`, а
    // не когда-нибудь потом: второе соединение — независимый наблюдатель, и
    // незакоммиченную запись первого оно увидеть не может в принципе.
    // Переоткрытие базы этого не показывает: там коммит мог случиться и на
    // закрытии соединения.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wakode.db");

    let mut writer = wakode_store::open(&path).unwrap();
    migrate(&mut writer).unwrap();
    let interner = Interner::load(&writer).unwrap();

    let sid = interner.intern_batch(&writer, &["видно снаружи"]).unwrap()[0];

    let observer = wakode_store::open(&path).unwrap();
    let seen = Interner::load(&observer).unwrap();

    assert_eq!(seen.lookup("видно снаружи"), Some(sid));
    assert_eq!(&*seen.resolve(sid).unwrap(), "видно снаружи");
}

#[test]
fn interning_inside_an_open_transaction_is_refused() {
    // Контракт метода — «зовётся вне открытой транзакции». Нарушение обязано
    // быть ошибкой, а не тихой вложенностью: словарь, записавший в память
    // номера из чужой транзакции, переживёт её откат и начнёт врать.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let tx = conn.transaction().unwrap();
    assert!(interner.intern_batch(&tx, &["внутри транзакции"]).is_err());
    drop(tx);

    assert_eq!(interner.lookup("внутри транзакции"), None);
}

#[test]
fn inserted_user_is_found_by_login() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    let created = insert_user(&conn, &a_user("swrneko")).unwrap();
    let found = find_user_by_login(&conn, "swrneko").unwrap().unwrap();

    assert_eq!(found.id, created.id);
    assert_eq!(found.login, "swrneko");
    assert_eq!(found.timezone, Tz::Europe__Moscow);
    assert_eq!(found.timeout_secs, 900);
    assert!(!found.is_admin);
}

#[test]
fn every_field_survives_the_round_trip() {
    // Проверяются **все** поля, а не показательные. Отображение колонок —
    // ровно то место, где индекс `row.get(N)` съезжает на единицу между
    // двумя колонками одного типа: ни компилятор, ни тест по логину такого
    // не заметят. Необязательные поля заполнены намеренно: `None` в них
    // прошёл бы и при полностью потерянной колонке.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    let new = NewUser {
        login: "полный".to_owned(),
        email: Some("почта@пример.рф".to_owned()),
        password_hash: "непрозрачные байты из плана 3".to_owned(),
        display_name: Some("Отображаемое имя".to_owned()),
        timezone: "America/St_Johns".parse().unwrap(),
        timeout_secs: 1800,
        is_admin: true,
    };

    let created = insert_user(&conn, &new).unwrap();
    let found = find_user_by_id(&conn, created.id).unwrap().unwrap();

    assert_eq!(found.id, created.id);
    assert_eq!(found.login, "полный");
    assert_eq!(found.email.as_deref(), Some("почта@пример.рф"));
    assert_eq!(found.password_hash, "непрозрачные байты из плана 3");
    assert_eq!(found.display_name.as_deref(), Some("Отображаемое имя"));
    assert_eq!(found.timezone, Tz::America__St_Johns);
    assert_eq!(found.timeout_secs, 1800);
    assert!(found.is_admin);
    assert_eq!(found.created_at, created.created_at);
    assert_eq!(found.updated_at, created.updated_at);
}

#[test]
fn missing_user_is_none_not_an_error() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    assert!(find_user_by_login(&conn, "нет такого").unwrap().is_none());
    assert!(find_user_by_id(&conn, uuid::Uuid::now_v7()).unwrap().is_none());
}

#[test]
fn duplicate_login_is_refused() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    insert_user(&conn, &a_user("swrneko")).unwrap();
    assert!(insert_user(&conn, &a_user("swrneko")).is_err());
}

#[test]
fn timezone_survives_the_round_trip() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    let mut user = a_user("havana");
    user.timezone = "America/Havana".parse().unwrap();
    let created = insert_user(&conn, &user).unwrap();

    let found = find_user_by_id(&conn, created.id).unwrap().unwrap();
    assert_eq!(found.timezone, Tz::America__Havana);
}

fn incoming(time_secs: i64, entity: &str, project: Option<&str>) -> IncomingHeartbeat {
    IncomingHeartbeat {
        time: Micros::from_secs(time_secs),
        entity: entity.to_owned(),
        kind: EntityKind::File,
        category: Category::Coding,
        project: project.map(str::to_owned),
        branch: None,
        language: None,
        editor: None,
        os: None,
        machine: None,
        plugin: None,
        is_write: false,
        lines: None,
        lineno: None,
        cursorpos: None,
        line_additions: None,
        line_deletions: None,
        project_root_count: None,
        dependencies: None,
        ai_line_changes: None,
        human_line_changes: None,
        ai_meta: None,
    }
}

#[test]
fn heartbeats_are_stored_and_counted() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let batch = [
        incoming(1_755_000_000, "src/main.rs", Some("wakode")),
        incoming(1_755_000_060, "src/lib.rs", Some("wakode")),
    ];
    let report = insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();

    assert_eq!(report.inserted(), 2);
    assert_eq!(report.duplicates(), 0);
    assert_eq!(report.outcomes, vec![Outcome::Inserted, Outcome::Inserted]);
}

#[test]
fn two_heartbeats_with_the_same_entity_but_different_projects_are_both_inserted() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    // Время и сущность совпадают нарочно — единственное различие в
    // проекте. Курсор укладки строк в `texts` и курсор их разбора должны
    // повторять один и тот же порядок один в один: если бы отметка
    // получила чужой номер (скажем, номер строки соседней отметки того же
    // батча), обе отметки могли бы схлопнуться в один dedup-хеш и вторая
    // тихо стала бы Duplicate вместо Inserted.
    let batch = [
        incoming(1_755_000_000, "src/main.rs", Some("alpha")),
        incoming(1_755_000_000, "src/main.rs", Some("beta")),
    ];
    let report = insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();

    assert_eq!(report.outcomes, vec![Outcome::Inserted, Outcome::Inserted]);
}

#[test]
fn a_repeated_heartbeat_within_the_same_batch_is_a_duplicate() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    // Один и тот же батч несёт одну отметку дважды подряд — не два
    // раздельных вызова, как в остальных тестах на повтор. Дедупликация
    // держится на уникальном индексе в базе, а не на сверке внутри
    // батча, так что тут легко случайно получить два Inserted вместо
    // Inserted и Duplicate.
    let batch = [
        incoming(1_755_000_000, "src/main.rs", Some("wakode")),
        incoming(1_755_000_000, "src/main.rs", Some("wakode")),
    ];
    let report = insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();

    assert_eq!(report.outcomes, vec![Outcome::Inserted, Outcome::Duplicate]);
}

#[test]
fn report_says_which_position_was_the_duplicate() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let first = [incoming(1_755_000_000, "src/main.rs", None)];
    insert_heartbeats(&mut conn, &interner, user.id, &first, user.timezone).unwrap();

    // Вторая отметка новая, первая — повтор. План 3 обязан уметь отличить их
    // по позиции, чтобы собрать пер-элементный ответ bulk-эндпоинта.
    let second = [
        incoming(1_755_000_000, "src/main.rs", None),
        incoming(1_755_000_060, "src/lib.rs", None),
    ];
    let report = insert_heartbeats(&mut conn, &interner, user.id, &second, user.timezone).unwrap();

    assert_eq!(report.outcomes, vec![Outcome::Duplicate, Outcome::Inserted]);
}

#[test]
fn resending_the_same_batch_inserts_nothing_new() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let batch = [incoming(1_755_000_000, "src/main.rs", Some("wakode"))];

    let first = insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();
    let second = insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();

    assert_eq!(first.inserted(), 1);
    assert_eq!(second.inserted(), 0);
    assert_eq!(second.duplicates(), 1, "повторная доставка очереди cli — норма, не ошибка");
}

#[test]
fn the_same_heartbeat_from_two_users_is_not_a_duplicate() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let one = insert_user(&conn, &a_user("one")).unwrap();
    let two = insert_user(&conn, &a_user("two")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let batch = [incoming(1_755_000_000, "src/main.rs", Some("wakode"))];

    let one_report = insert_heartbeats(&mut conn, &interner, one.id, &batch, one.timezone).unwrap();
    let two_report = insert_heartbeats(&mut conn, &interner, two.id, &batch, two.timezone).unwrap();

    assert_eq!(one_report.inserted(), 1);
    assert_eq!(two_report.inserted(), 1);
}

#[test]
fn heartbeat_for_a_missing_user_is_refused() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let ghost = uuid::Uuid::now_v7();
    let batch = [incoming(1_755_000_000, "src/main.rs", None)];
    assert!(
        insert_heartbeats(&mut conn, &interner, ghost, &batch, chrono_tz::UTC).is_err(),
        "внешний ключ должен сработать: без него отметки повиснут в никуда"
    );

    // Внешний ключ срабатывает на первой же вставке — то есть до
    // `mark_dirty`, так что пустой список дней получился бы и вовсе без
    // транзакции. Атомарность отката этот тест не доказывает: отказ
    // происходит раньше того места, которое должно было бы откатываться.
    // Прямая проверка атомарности появится в задаче 9, когда будет чем
    // подтвердить наличие или отсутствие самих отметок.
    assert!(dirty_days_for(&conn, ghost).unwrap().is_empty());
}

#[test]
fn the_marked_day_is_local_not_utc() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let moscow = insert_user(&conn, &a_user("swrneko")).unwrap();
    let mut utc = a_user("гринвич");
    utc.timezone = chrono_tz::UTC;
    let utc = insert_user(&conn, &utc).unwrap();
    let interner = Interner::load(&conn).unwrap();

    // 1 755 036 000 — 2025-08-12T22:00:00Z. В Москве (UTC+3) это уже
    // 2025-08-13, в UTC — ещё 2025-08-12. Реализация, считающая день по
    // смещению UTC вместо `local_day_of`, прошла бы старый тест
    // незамеченной — здесь для одного и того же момента у двух зон разные
    // авторитетные дни, и это видно прямо в утверждениях.
    let batch = [incoming(1_755_036_000, "src/main.rs", None)];
    insert_heartbeats(&mut conn, &interner, moscow.id, &batch, moscow.timezone).unwrap();
    insert_heartbeats(&mut conn, &interner, utc.id, &batch, utc.timezone).unwrap();

    assert_eq!(
        dirty_days_for(&conn, moscow.id).unwrap(),
        vec![NaiveDate::from_ymd_opt(2025, 8, 13).unwrap()]
    );
    assert_eq!(
        dirty_days_for(&conn, utc.id).unwrap(),
        vec![NaiveDate::from_ymd_opt(2025, 8, 12).unwrap()]
    );
}

#[test]
fn resending_a_batch_does_not_multiply_marked_days() {
    // Внимание: этот тест НЕ проверяет фильтр по `Outcome::Inserted` в
    // `insert_heartbeats` — и никакой тест волны 0 его не проверит. Повтор
    // по определению несёт то же время, что и уже вставленная отметка,
    // значит и тот же локальный день, а тот помечен ещё при первой вставке.
    // Снимать пометки будет волна 1; до неё разницы между «мерить по всему
    // батчу» и «мерить по вставленному» снаружи не видно.
    //
    // Что тест правда проверяет: повторная доставка очереди cli — штатный
    // сценарий, она не падает и не плодит вторую строку на тот же день.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let batch = [incoming(1_755_000_000, "src/main.rs", Some("wakode"))];
    insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();

    let second = insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();
    assert_eq!(second.outcomes, vec![Outcome::Duplicate]);

    assert_eq!(
        dirty_days_for(&conn, user.id).unwrap(),
        vec![NaiveDate::from_ymd_opt(2025, 8, 12).unwrap()]
    );
}

#[test]
fn an_empty_batch_touches_nothing() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    // Пустой батч — единственный путь выхода без открытия транзакции.
    let report = insert_heartbeats(&mut conn, &interner, user.id, &[], user.timezone).unwrap();

    assert_eq!(report.outcomes, Vec::new());
    assert!(dirty_days_for(&conn, user.id).unwrap().is_empty());
}

#[test]
fn loaded_heartbeats_come_back_as_core_types() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let batch = [incoming(1_000, "src/main.rs", Some("wakode"))];
    insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();

    let loaded = load_heartbeats(
        &conn,
        user.id,
        Micros::from_secs(0),
        Micros::from_secs(2_000),
    )
    .unwrap();

    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].time, Micros::from_secs(1_000));
    assert_eq!(loaded[0].attrs.category, Category::Coding);
    assert_eq!(loaded[0].attrs.kind, EntityKind::File);
    assert!(loaded[0].attrs.project.is_some());
    assert_eq!(
        interner.resolve(loaded[0].attrs.entity).unwrap().as_ref(),
        "src/main.rs"
    );
}

#[test]
fn range_is_half_open_and_sorted() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    // Вставляем не по порядку — чтение обязано отдать по возрастанию времени.
    let batch = [
        incoming(300, "c.rs", None),
        incoming(100, "a.rs", None),
        incoming(200, "b.rs", None),
    ];
    insert_heartbeats(&mut conn, &interner, user.id, &batch, user.timezone).unwrap();

    let loaded = load_heartbeats(
        &conn,
        user.id,
        Micros::from_secs(100),
        Micros::from_secs(300),
    )
    .unwrap();

    let times: Vec<i64> = loaded.iter().map(|hb| hb.time.get()).collect();
    assert_eq!(
        times,
        vec![Micros::from_secs(100).get(), Micros::from_secs(200).get()],
        "нижняя граница включена, верхняя — нет"
    );
}

#[test]
fn one_user_never_sees_another_users_heartbeats() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let one = insert_user(&conn, &a_user("one")).unwrap();
    let two = insert_user(&conn, &a_user("two")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    insert_heartbeats(&mut conn, &interner, one.id, &[incoming(100, "a.rs", None)], one.timezone).unwrap();
    insert_heartbeats(&mut conn, &interner, two.id, &[incoming(100, "b.rs", None)], two.timezone).unwrap();

    let loaded = load_heartbeats(&conn, one.id, Micros::from_secs(0), Micros::from_secs(1_000)).unwrap();

    assert_eq!(loaded.len(), 1);
    assert_eq!(interner.resolve(loaded[0].attrs.entity).unwrap().as_ref(), "a.rs");
}

#[test]
fn empty_range_gives_an_empty_vector_not_an_error() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let loaded = load_heartbeats(&conn, user.id, Micros::from_secs(0), Micros::from_secs(1)).unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn every_attribute_survives_the_round_trip() {
    // Долг задачи 8. Все необязательные поля заполнены **различимыми**
    // значениями: только так ловится перестановка соседних полей при
    // разборе. Пустое поле не двигает курсор, поэтому на `None` подмена
    // проекта веткой выглядит точно так же, как её отсутствие.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let full = IncomingHeartbeat {
        time: Micros::from_secs(1_755_000_000),
        entity: "сущность".to_owned(),
        kind: EntityKind::App,
        category: Category::Debugging,
        project: Some("проект".to_owned()),
        branch: Some("ветка".to_owned()),
        language: Some("язык".to_owned()),
        editor: Some("редактор".to_owned()),
        os: Some("ос".to_owned()),
        machine: Some("машина".to_owned()),
        plugin: Some("плагин".to_owned()),
        is_write: true,
        lines: Some(1),
        lineno: Some(2),
        cursorpos: Some(3),
        line_additions: Some(4),
        line_deletions: Some(5),
        project_root_count: Some(6),
        dependencies: Some("зависимости".to_owned()),
        ai_line_changes: Some(7),
        human_line_changes: Some(8),
        ai_meta: Some("мета".to_owned()),
    };

    insert_heartbeats(&mut conn, &interner, user.id, &[full], user.timezone).unwrap();
    let loaded = load_heartbeats(
        &conn,
        user.id,
        Micros::from_secs(1_755_000_000),
        Micros::from_secs(1_755_000_001),
    )
    .unwrap();

    let attrs = loaded[0].attrs;
    let text = |sid| interner.resolve(sid).unwrap().to_string();

    assert_eq!(loaded[0].time, Micros::from_secs(1_755_000_000));
    assert_eq!(attrs.kind, EntityKind::App);
    assert_eq!(attrs.category, Category::Debugging);
    assert_eq!(text(attrs.entity), "сущность");
    assert_eq!(text(attrs.project.unwrap()), "проект");
    assert_eq!(text(attrs.branch.unwrap()), "ветка");
    assert_eq!(text(attrs.language.unwrap()), "язык");
    assert_eq!(text(attrs.editor.unwrap()), "редактор");
    assert_eq!(text(attrs.os.unwrap()), "ос");
    assert_eq!(text(attrs.machine.unwrap()), "машина");
}

#[test]
fn a_refused_batch_stores_no_heartbeats_at_all() {
    // Долг задачи 8, закрытый ровно настолько, насколько он закрываем.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let ghost = uuid::Uuid::now_v7();
    let doomed = [incoming(200, "b.rs", None), incoming(300, "c.rs", None)];
    assert!(insert_heartbeats(&mut conn, &interner, ghost, &doomed, chrono_tz::UTC).is_err());

    let loaded =
        load_heartbeats(&conn, ghost, Micros::from_secs(0), Micros::from_secs(1_000)).unwrap();
    assert!(loaded.is_empty());
}
