use wakode_store::{migrate, open_in_memory, schema_version};

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
