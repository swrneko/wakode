use wakode_store::{migrate, open_in_memory, schema_version, Interner};

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

/// `dictionary_survives_reopening_the_database` доказывает, что строка
/// пережила закрытие базы, но не доказывает, что коммит сделал именно
/// `intern_batch` — с автокоммитом SQLite это выглядело бы точно так же.
/// Здесь после интернирования на том же соединении открывается и
/// откатывается посторонняя транзакция: если бы `intern_batch` полагался на
/// коммит вызывающего (или вовсе не коммитил), откат унёс бы интернированную
/// строку вместе со своей.
#[test]
fn intern_batch_commits_on_its_own_even_if_a_later_transaction_on_the_same_connection_rolls_back()
{
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let interner = Interner::load(&conn).unwrap();

    let sid = interner.intern_batch(&conn, &["src/main.rs"]).unwrap()[0];

    {
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute("INSERT INTO strings(value) VALUES ('постороннее значение')", [])
            .unwrap();
        // Транзакция откатывается здесь, при выходе из области видимости,
        // без вызова `commit`.
    }

    let reloaded = Interner::load(&conn).unwrap();
    assert_eq!(&*reloaded.resolve(sid).unwrap(), "src/main.rs");
    assert_eq!(reloaded.lookup("src/main.rs"), Some(sid));
    assert_eq!(reloaded.lookup("постороннее значение"), None);
}
