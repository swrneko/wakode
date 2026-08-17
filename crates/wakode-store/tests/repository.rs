use std::sync::Arc;

use chrono::NaiveDate;
use chrono_tz::Tz;
use wakode_core::{Category, EntityKind, Micros};
use wakode_store::{
    dirty_days_for, find_key_by_lookup, find_session_by_token_hash, find_user_by_id,
    find_user_by_login, first_api_key, insert_api_key, insert_heartbeats, insert_session,
    insert_user, load_heartbeats, migrate, open_in_memory, revoke_key, revoke_session,
    schema_version, spawn_writer, touch_key_used, user_count, HeartbeatRepo, IncomingHeartbeat,
    Interner, KeyRepo, NewApiKey, NewSession, NewUser, Outcome, SqliteStore, StoreError, UserRepo,
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

/// Сырой SQL в этом файле позволен только проверкам самой схемы, а не
/// поведения поверх неё — таких проверок здесь три: эта,
/// `heartbeat_dedup_index_is_unique` и
/// `deleting_a_user_takes_their_keys_and_sessions_with_them`. Четвёртой
/// не заводим.
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
fn an_empty_database_has_no_users_and_no_keys() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    assert_eq!(user_count(&conn).unwrap(), 0);
    assert!(first_api_key(&conn).unwrap().is_none());
}

#[test]
fn user_count_follows_the_users_actually_inserted() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    insert_user(&conn, &a_user("первый")).unwrap();
    assert_eq!(user_count(&conn).unwrap(), 1);

    insert_user(&conn, &a_user("второй")).unwrap();
    assert_eq!(user_count(&conn).unwrap(), 2);
}

#[test]
fn first_api_key_returns_the_oldest_one() {
    // Порядок обязан быть воспроизводимым: шаг 5 старта расшифровывает
    // именно этот ключ, и «какой-нибудь» здесь означал бы, что проверка
    // мастер-ключа то проходит, то нет.
    //
    // Сам `ORDER BY` этот тест НЕ доказывает: ключи вставляются подряд, и
    // порядок совпадает с сортировкой случайно. Настоящая проверка —
    // `first_api_key_orders_by_created_at_not_by_insertion` в
    // `src/keys.rs`, где `created_at` идёт против порядка вставки.
    // Не удаляй её как дубль.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let older = insert_api_key(
        &conn,
        &NewApiKey {
            user_id: user.id,
            name: "старый".to_owned(),
            key_encrypted: vec![1],
            key_lookup: vec![1],
        },
    )
    .unwrap();
    insert_api_key(
        &conn,
        &NewApiKey {
            user_id: user.id,
            name: "новый".to_owned(),
            key_encrypted: vec![2],
            key_lookup: vec![2],
        },
    )
    .unwrap();

    let found = first_api_key(&conn).unwrap().unwrap();
    assert_eq!(found.id, older.id);
    assert_eq!(found.key_encrypted, vec![1]);
}

#[test]
fn first_api_key_sees_keys_of_every_user() {
    // Шаг 5 старта проверяет мастер-ключ инстанса целиком, а не одного
    // пользователя: ключ любого владельца зашифрован тем же мастер-ключом.
    //
    // Первый пользователь заводится намеренно и остаётся без ключей. Без
    // него тест не проверял бы ничего: при единственном пользователе его
    // прошла бы и реализация, ищущая ключи только у самого раннего или
    // только у администратора.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    insert_user(&conn, &a_user("без ключей")).unwrap();
    let other = insert_user(&conn, &a_user("другой")).unwrap();

    insert_api_key(
        &conn,
        &NewApiKey {
            user_id: other.id,
            name: "чужой".to_owned(),
            key_encrypted: vec![9],
            key_lookup: vec![9],
        },
    )
    .unwrap();

    assert!(first_api_key(&conn).unwrap().is_some());
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

#[test]
fn api_key_is_found_by_its_lookup_fingerprint() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let created = insert_api_key(
        &conn,
        &NewApiKey {
            user_id: user.id,
            name: "рабочий ноутбук".to_owned(),
            key_encrypted: vec![1, 2, 3],
            key_lookup: vec![9, 9, 9],
        },
    )
    .unwrap();

    let found = find_key_by_lookup(&conn, &[9, 9, 9]).unwrap().unwrap();
    assert_eq!(found.id, created.id);
    assert_eq!(found.user_id, user.id);
    assert_eq!(found.key_encrypted, vec![1, 2, 3]);
    assert!(found.revoked_at.is_none());
}

#[test]
fn revoked_key_is_still_found_but_marked() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let created = insert_api_key(
        &conn,
        &NewApiKey {
            user_id: user.id,
            name: "старый".to_owned(),
            key_encrypted: vec![1],
            key_lookup: vec![2],
        },
    )
    .unwrap();

    revoke_key(&conn, created.id).unwrap();

    // Отозванный ключ обязан находиться: иначе слой аутентификации не сможет
    // отличить «ключа никогда не было» от «ключ отозван», а это разные ответы.
    let found = find_key_by_lookup(&conn, &[2]).unwrap().unwrap();
    assert!(found.revoked_at.is_some());
}

#[test]
fn a_keys_lookup_finds_its_own_row_and_leaves_others_alone() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let alice = insert_user(&conn, &a_user("alice")).unwrap();
    let bob = insert_user(&conn, &a_user("bob")).unwrap();

    let alice_key = insert_api_key(
        &conn,
        &NewApiKey {
            user_id: alice.id,
            name: "алисин".to_owned(),
            key_encrypted: vec![1],
            key_lookup: vec![11],
        },
    )
    .unwrap();
    let bob_key = insert_api_key(
        &conn,
        &NewApiKey {
            user_id: bob.id,
            name: "бобов".to_owned(),
            key_encrypted: vec![2],
            key_lookup: vec![22],
        },
    )
    .unwrap();

    // Строки в таблице есть, но искомого отпечатка среди них нет: `None`
    // обязан получиться из-за отсутствия совпадения, а не из-за пустой
    // таблицы — реализация вроде `SELECT ... LIMIT 1` без фильтра по
    // `key_lookup` эту разницу не ловит.
    assert!(find_key_by_lookup(&conn, &[99]).unwrap().is_none());

    let found = find_key_by_lookup(&conn, &[11]).unwrap().unwrap();
    assert_eq!(found.id, alice_key.id);
    assert_eq!(found.user_id, alice.id);

    // Отзыв и touch адресуются по `id`. Реализация, где в запросе нет
    // `id = ?1`, отозвала бы или пометила бы использованными ключи всех
    // пользователей разом — массовый разлогин вместо отзыва одного ключа.
    revoke_key(&conn, alice_key.id).unwrap();
    touch_key_used(&conn, alice_key.id).unwrap();

    let bob_after = find_key_by_lookup(&conn, &[22]).unwrap().unwrap();
    assert_eq!(bob_after.id, bob_key.id);
    assert!(bob_after.revoked_at.is_none(), "отзыв чужого ключа задел соседний");
    assert!(bob_after.last_used_at.is_none(), "touch чужого ключа задел соседний");
}

#[test]
fn duplicate_lookup_is_refused() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let key = |name: &str| NewApiKey {
        user_id: user.id,
        name: name.to_owned(),
        key_encrypted: vec![1],
        key_lookup: vec![7],
    };

    insert_api_key(&conn, &key("первый")).unwrap();
    assert!(insert_api_key(&conn, &key("второй")).is_err());
}

#[test]
fn touching_a_key_records_when_it_was_last_used() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let created = insert_api_key(
        &conn,
        &NewApiKey {
            user_id: user.id,
            name: "ключ".to_owned(),
            key_encrypted: vec![1],
            key_lookup: vec![5],
        },
    )
    .unwrap();
    assert!(created.last_used_at.is_none());

    touch_key_used(&conn, created.id).unwrap();

    let found = find_key_by_lookup(&conn, &[5]).unwrap().unwrap();
    assert!(found.last_used_at.is_some());
}

#[test]
fn every_api_key_field_survives_the_round_trip() {
    // Проверяются **все** поля, не только отпечаток. `name` и `created_at`
    // раньше не проверял ни один assert в этом файле, а в `api_keys` подряд
    // лежат три `INTEGER` (`created_at`, `last_used_at`, `revoked_at`) и два
    // `BLOB` (`id`, `user_id`) — ровно расстановка, где `row.get(N)` съезжает
    // на соседнюю колонку того же типа. `created_at` сравнивается с тем, что
    // вернул `insert_api_key`, а не с конкретным числом: часы здесь настоящие.
    //
    // Пару `last_used_at`/`revoked_at` этот тест не различает: у свежего
    // ключа обе пусты, заполнить их тут нечем, и взаимная перестановка
    // проходит через оба `is_none()` беспрепятственно. Её ловят три других
    // теста — `revoked_key_is_still_found_but_marked`,
    // `touching_a_key_records_when_it_was_last_used` и
    // `revoking_an_already_revoked_key_or_session_keeps_the_original_timestamp`.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let created = insert_api_key(
        &conn,
        &NewApiKey {
            user_id: user.id,
            name: "полный ключ".to_owned(),
            key_encrypted: vec![5, 6, 7],
            key_lookup: vec![55],
        },
    )
    .unwrap();

    let found = find_key_by_lookup(&conn, &[55]).unwrap().unwrap();

    assert_eq!(found.id, created.id);
    assert_eq!(found.user_id, created.user_id);
    assert_eq!(found.name, "полный ключ");
    assert_eq!(found.key_encrypted, vec![5, 6, 7]);
    assert_eq!(found.created_at, created.created_at);
    assert!(found.last_used_at.is_none());
    assert!(found.revoked_at.is_none());
}

#[test]
fn session_round_trips_by_token_hash() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let created = insert_session(
        &conn,
        &NewSession {
            user_id: user.id,
            token_hash: vec![4, 2],
            user_agent: Some("Firefox".to_owned()),
            expires_at: Micros::from_secs(2_000_000_000),
        },
    )
    .unwrap();

    let found = find_session_by_token_hash(&conn, &[4, 2]).unwrap().unwrap();
    assert_eq!(found.id, created.id);
    assert_eq!(found.user_id, user.id);
    assert_eq!(found.expires_at, Micros::from_secs(2_000_000_000));
}

#[test]
fn revoked_session_is_still_found_but_marked() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let created = insert_session(
        &conn,
        &NewSession {
            user_id: user.id,
            token_hash: vec![8, 8],
            user_agent: None,
            expires_at: Micros::from_secs(2_000_000_000),
        },
    )
    .unwrap();

    revoke_session(&conn, created.id).unwrap();

    // Отозванная сессия обязана находиться: слой аутентификации должен
    // отличать «сессии не было» от «сессия отозвана» — это разные ответы.
    let found = find_session_by_token_hash(&conn, &[8, 8]).unwrap().unwrap();
    assert!(found.revoked_at.is_some());
}

#[test]
fn every_session_field_survives_the_round_trip() {
    // Тот же долг, что закрывает `every_api_key_field_survives_the_round_trip`
    // для ключей: `session_round_trips_by_token_hash` проверяет три поля из
    // шести, `user_agent`, `created_at` и `revoked_at` не читает ни один
    // assert.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let created = insert_session(
        &conn,
        &NewSession {
            user_id: user.id,
            token_hash: vec![66],
            user_agent: Some("Chrome на Linux".to_owned()),
            expires_at: Micros::from_secs(2_000_000_000),
        },
    )
    .unwrap();

    let found = find_session_by_token_hash(&conn, &[66]).unwrap().unwrap();

    assert_eq!(found.id, created.id);
    assert_eq!(found.user_id, created.user_id);
    assert_eq!(found.user_agent.as_deref(), Some("Chrome на Linux"));
    assert_eq!(found.created_at, created.created_at);
    assert_eq!(found.expires_at, Micros::from_secs(2_000_000_000));
    assert!(found.revoked_at.is_none());
}

#[test]
fn a_sessions_lookup_finds_its_own_row_and_leaves_others_alone() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let alice = insert_user(&conn, &a_user("alice")).unwrap();
    let bob = insert_user(&conn, &a_user("bob")).unwrap();

    let alice_session = insert_session(
        &conn,
        &NewSession {
            user_id: alice.id,
            token_hash: vec![11],
            user_agent: None,
            expires_at: Micros::from_secs(2_000_000_000),
        },
    )
    .unwrap();
    let bob_session = insert_session(
        &conn,
        &NewSession {
            user_id: bob.id,
            token_hash: vec![22],
            user_agent: None,
            expires_at: Micros::from_secs(2_000_000_000),
        },
    )
    .unwrap();

    assert!(find_session_by_token_hash(&conn, &[99]).unwrap().is_none());

    let found = find_session_by_token_hash(&conn, &[11]).unwrap().unwrap();
    assert_eq!(found.id, alice_session.id);
    assert_eq!(found.user_id, alice.id);

    // Отзыв адресуется по `id`. Реализация без `id = ?1` в запросе отозвала
    // бы сессии всех пользователей разом — массовый разлогин вместо отзыва
    // одной сессии.
    revoke_session(&conn, alice_session.id).unwrap();

    let bob_after = find_session_by_token_hash(&conn, &[22]).unwrap().unwrap();
    assert_eq!(bob_after.id, bob_session.id);
    assert!(bob_after.revoked_at.is_none(), "отзыв чужой сессии задел соседнюю");
}

#[test]
fn revoking_an_already_revoked_key_or_session_keeps_the_original_timestamp() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let key = insert_api_key(
        &conn,
        &NewApiKey {
            user_id: user.id,
            name: "ключ".to_owned(),
            key_encrypted: vec![1],
            key_lookup: vec![41],
        },
    )
    .unwrap();
    let session = insert_session(
        &conn,
        &NewSession {
            user_id: user.id,
            token_hash: vec![41],
            user_agent: None,
            expires_at: Micros::from_secs(2_000_000_000),
        },
    )
    .unwrap();

    revoke_key(&conn, key.id).unwrap();
    revoke_session(&conn, session.id).unwrap();

    let key_revoked_at = find_key_by_lookup(&conn, &[41]).unwrap().unwrap().revoked_at.unwrap();
    let session_revoked_at = find_session_by_token_hash(&conn, &[41])
        .unwrap()
        .unwrap()
        .revoked_at
        .unwrap();

    // Повтор — обычное дело: ретрай HTTP, двойной клик в настройках. Без
    // `AND revoked_at IS NULL` в запросе второй вызов переписал бы отметку
    // текущим временем, и «когда отозвали» превратилось бы в «когда в
    // последний раз попытались отозвать».
    revoke_key(&conn, key.id).unwrap();
    revoke_session(&conn, session.id).unwrap();

    assert_eq!(
        find_key_by_lookup(&conn, &[41]).unwrap().unwrap().revoked_at,
        Some(key_revoked_at)
    );
    assert_eq!(
        find_session_by_token_hash(&conn, &[41]).unwrap().unwrap().revoked_at,
        Some(session_revoked_at)
    );
}

#[test]
fn deleting_a_user_takes_their_keys_and_sessions_with_them() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    insert_api_key(
        &conn,
        &NewApiKey {
            user_id: user.id,
            name: "ключ".to_owned(),
            key_encrypted: vec![1],
            key_lookup: vec![1],
        },
    )
    .unwrap();
    insert_session(
        &conn,
        &NewSession {
            user_id: user.id,
            token_hash: vec![1],
            user_agent: None,
            expires_at: Micros::from_secs(1),
        },
    )
    .unwrap();

    // Третье и последнее из трёх мест, где в этом файле позволен сырой SQL:
    // удаления пользователя в волне 0 нет, каскад через публичный интерфейс
    // не проверить, а он — свойство самой схемы, как список таблиц в
    // `wave_zero_schema_creates_every_table` и уникальность индекса в
    // `heartbeat_dedup_index_is_unique`. Четвёртого места не будет.
    conn.execute(
        "DELETE FROM users WHERE id = ?1",
        [wakode_store::codec::uuid_to_blob(user.id)],
    )
    .unwrap();

    assert!(find_key_by_lookup(&conn, &[1]).unwrap().is_none());
    assert!(find_session_by_token_hash(&conn, &[1]).unwrap().is_none());
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
fn range_is_half_open() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Interner::load(&conn).unwrap();

    // Вставляем не по порядку. Порядок в результате здесь берётся из
    // индекса `hb_time`, а не из `ORDER BY` — обход по индексу для этого
    // запроса и так отдаёт строки по возрастанию времени. Значит тест
    // ловит неверную явную сортировку (`ORDER BY time DESC` его роняет),
    // но не её отсутствие: убери `ORDER BY time` из запроса вовсе — план
    // выполнения не изменится, и тест не заметит подмены.
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
    //
    // Тест закрывает время и девять полей `Attrs` — всё, что видно через
    // публичное чтение. `plugin_id`, `is_write` и десять числовых и
    // текстовых колонок ниже заполняются ради проверки позиций параметров
    // вокруг них в `INSERT`; их собственные позиции проверяет модульный тест
    // `every_unread_column_lands_in_the_place_the_insert_promised`
    // (`src/heartbeats.rs`) — там для этого есть сырой `SELECT`, здесь его
    // быть не должно.
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

#[tokio::test]
async fn writer_commits_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wakode.db");
    let mut conn = wakode_store::open(&path).unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Arc::new(Interner::load(&conn).unwrap());

    let handle = spawn_writer(conn, interner, 8);

    let batch = vec![incoming(1_000, "src/main.rs", None)];
    let report = handle
        .insert_heartbeats(user.id, batch.clone(), user.timezone)
        .await
        .unwrap();
    assert_eq!(report.inserted(), 1);

    // Коммит до ответа доказывается тем же каналом, а не другим соединением:
    // цикл писателя строго последователен — вторая заявка снимается с канала
    // только после того, как первый `insert_heartbeats` вернулся. Если бы
    // ответ уходил до вызова `insert_heartbeats`, второй, повторный батч
    // ничего не знал бы о первом (он ещё не вставлен) и отчитался бы
    // `Inserted` вместо `Duplicate` — а это ловится на любой машине, без
    // гонки с фоновым потоком и без шва для остановки писателя.
    let second = handle
        .insert_heartbeats(user.id, batch, user.timezone)
        .await
        .unwrap();
    assert_eq!(second.duplicates(), 1);

    // Отдельное соединение здесь проверяет другое свойство — что закоммиченные
    // данные видны снаружи, а не только внутри писателя. «Отдельное» тут не
    // авторский выбор: `conn` уже переехал в `spawn_writer`, и попытка читать
    // им же не скомпилируется (E0382, use of moved value) — гарантию держит
    // перемещение владения, а не эта строка.
    let read = wakode_store::open(&path).unwrap();
    let loaded = load_heartbeats(&read, user.id, Micros::from_secs(0), Micros::from_secs(9_999)).unwrap();
    assert_eq!(loaded.len(), 1);
}

#[tokio::test]
async fn a_full_queue_refuses_instead_of_buffering() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wakode.db");
    let mut conn = wakode_store::open(&path).unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Arc::new(Interner::load(&conn).unwrap());

    // Канал на одну заявку: заполнить его тривиально.
    let handle = spawn_writer(conn, interner, 1);

    let mut refused = 0;
    let mut tasks = Vec::new();
    for i in 0..64 {
        let handle = handle.clone();
        let tz = user.timezone;
        let id = user.id;
        tasks.push(tokio::spawn(async move {
            handle
                .insert_heartbeats(id, vec![incoming(1_000 + i, "f.rs", None)], tz)
                .await
        }));
    }
    for task in tasks {
        if let Err(StoreError::WriteQueueFull) = task.await.unwrap() {
            refused += 1;
        }
    }

    // Честно про определённость: пишущая задача живёт в настоящем потоке ОС
    // и разгребает очередь параллельно с тем, как эта задача её наполняет —
    // железной гарантии отказа тут нет ни у какой формулировки, кроме той,
    // что вводит шов для остановки писателя, а он несоразмерен масштабу
    // задачи. Устойчивость держится на разнице масштабов. `#[tokio::test]`
    // даёт однопоточный рантайм, поэтому все 64 задачи опрашиваются подряд,
    // без точки уступки перед `try_send`: пачка отправок укладывается в
    // микросекунды. Писатель при этом никуда не блокируется — он отдельный
    // поток ОС и вполне может крутиться на другом ядре, — но каждая его
    // заявка стоит файловой транзакции с fsync, то есть миллисекунды. За
    // время непрерывной пачки он успевает разобрать заявку-другую, а чтобы
    // отказов не случилось ни одного, ему пришлось бы закоммитить все 63.
    // Отсюда следствие для CI: чем медленнее машина, тем дороже коммит
    // относительно `try_send`, и тем больше отказов, а не меньше.
    assert!(
        refused > 0,
        "при канале на одну заявку и 64 одновременных записях отказы обязаны появиться: \
         молчаливая буферизация здесь означала бы потерю при падении процесса"
    );
}

#[tokio::test]
async fn writer_survives_a_failing_batch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wakode.db");
    let mut conn = wakode_store::open(&path).unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();
    let interner = Arc::new(Interner::load(&conn).unwrap());

    let handle = spawn_writer(conn, interner, 8);

    // Несуществующий пользователь — внешний ключ не пустит.
    let failed = handle
        .insert_heartbeats(uuid::Uuid::now_v7(), vec![incoming(1, "f.rs", None)], user.timezone)
        .await;
    assert!(failed.is_err());

    // Задача обязана остаться живой: одна битая заявка не должна уносить
    // с собой запись для всех остальных.
    let ok = handle
        .insert_heartbeats(user.id, vec![incoming(2, "f.rs", None)], user.timezone)
        .await
        .unwrap();
    assert_eq!(ok.inserted(), 1);
}

#[tokio::test]
async fn store_goes_through_the_trait_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap();

    let user = store.create_user(a_user("swrneko")).await.unwrap();

    let report = store
        .record_heartbeats(user.id, vec![incoming(1_000, "src/main.rs", Some("wakode"))], user.timezone)
        .await
        .unwrap();
    assert_eq!(report.inserted(), 1);

    let loaded = store
        .heartbeats_in_range(user.id, Micros::from_secs(0), Micros::from_secs(9_999))
        .await
        .unwrap();
    assert_eq!(loaded.len(), 1);

    let found = store.user_by_login("swrneko").await.unwrap().unwrap();
    assert_eq!(found.id, user.id);
}

#[tokio::test]
async fn the_store_answers_both_questions_through_the_traits() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap();

    assert_eq!(store.user_count().await.unwrap(), 0);
    assert!(store.first_key().await.unwrap().is_none());

    let user = store.create_user(a_user("swrneko")).await.unwrap();
    store
        .create_key(NewApiKey {
            user_id: user.id,
            name: "ключ".to_owned(),
            key_encrypted: vec![3],
            key_lookup: vec![3],
        })
        .await
        .unwrap();

    assert_eq!(store.user_count().await.unwrap(), 1);
    assert_eq!(store.first_key().await.unwrap().unwrap().key_encrypted, vec![3]);
}

#[tokio::test]
async fn backup_produces_a_readable_copy() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap();
    let user = store.create_user(a_user("swrneko")).await.unwrap();
    store
        .record_heartbeats(user.id, vec![incoming(1_000, "f.rs", None)], user.timezone)
        .await
        .unwrap();

    let dest = dir.path().join("backup.db");
    store.backup(&dest).await.unwrap();

    // Доказывает ровно то, что копия открывается и содержит закоммиченные
    // данные — не факт существования файла. Не доказывает: настоящую
    // консистентность снимка под параллельной записью этот тест не ловит —
    // здесь никто не пишет во время `backup`, а замена `VACUUM INTO` на
    // `wal_checkpoint` + `fs::copy` (то есть реализацию, некорректную именно
    // под конкурентной записью) этот тест не различает от настоящей.
    let copy = wakode_store::open(&dest).unwrap();
    let loaded = load_heartbeats(&copy, user.id, Micros::from_secs(0), Micros::from_secs(9_999)).unwrap();
    assert_eq!(loaded.len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn backup_to_a_non_utf8_path_fails_instead_of_writing_a_differently_named_file() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap();

    // 0xFF не встречается ни в одной валидной UTF-8 последовательности, а на
    // Linux имена файлов — произвольные байты, так что такой путь легален.
    // `to_string_lossy` подменила бы этот байт на U+FFFD, и VACUUM INTO
    // создал бы файл с другим именем, отрапортовав Ok(()) — бэкап потерялся
    // бы молча.
    let dest = dir.path().join(OsStr::from_bytes(b"back\xffup.db"));

    let err = store.backup(&dest).await.unwrap_err();
    assert!(matches!(err, StoreError::Sqlite(_)), "получили {err:?}");
    assert!(!dest.exists(), "файла по запрошенному пути быть не должно");
}

#[tokio::test]
async fn opening_the_store_applies_migrations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wakode.db");

    let _store = SqliteStore::open(&path, 16).unwrap();

    let conn = wakode_store::open(&path).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_beside_the_writer_waits_instead_of_failing_busy() {
    // Центральное обещание архитектуры, которое до сих пор держалось только
    // на докстринге `on_own_connection`: редкие одиночные записи идут своими
    // соединениями МИМО пишущей задачи, и параллельного писателя разводит не
    // очередь, а сам SQLite по `busy_timeout` из `conn.rs`. Если бы прагмы
    // там не было, `create_user` во время чужой транзакции получил бы
    // `SQLITE_BUSY` немедленно — то есть логин падал бы под нагрузкой ровно
    // тогда, когда отметки идут потоком.
    //
    // Многопоточный исполнитель тут обязателен: на однопоточном `create_user`
    // и батч не наложились бы по времени, и тест прошёл бы, ничего не
    // проверив.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap();
    let owner = store.create_user(a_user("owner")).await.unwrap();

    // Батч, который держит транзакцию писателя заметное время.
    let batch: Vec<IncomingHeartbeat> = (0..20_000)
        .map(|i| incoming(1_000 + i, "f.rs", Some("wakode")))
        .collect();

    let writing = {
        let store = store.clone();
        let tz = owner.timezone;
        tokio::spawn(async move { store.record_heartbeats(owner.id, batch, tz).await })
    };

    // Пока батч в работе, пишем мимо очереди — и обязаны дождаться, а не
    // упасть. Записей много и подряд: одна могла бы целиком уложиться в
    // промежуток до того, как писатель откроет свою транзакцию, и тогда
    // тест не проверил бы ничего.
    let mut beside = Vec::new();
    for i in 0..200 {
        beside.push(store.create_user(a_user(&format!("beside-{i}"))).await);
    }

    let report = writing.await.unwrap().unwrap();
    assert_eq!(report.inserted(), 20_000);

    for (i, one) in beside.into_iter().enumerate() {
        one.unwrap_or_else(|err| {
            panic!("запись {i} мимо очереди упала вместо ожидания busy_timeout: {err:?}")
        });
    }
    assert!(store.user_by_login("beside-199").await.unwrap().is_some());
}
