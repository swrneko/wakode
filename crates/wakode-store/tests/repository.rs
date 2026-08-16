use chrono_tz::Tz;
use wakode_store::{
    find_user_by_id, find_user_by_login, insert_user, migrate, open_in_memory, schema_version,
    Interner, NewUser,
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
