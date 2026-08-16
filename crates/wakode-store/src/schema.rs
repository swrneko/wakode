/// Миграции по порядку: `MIGRATIONS[0]` поднимает схему до версии 1.
///
/// Массив только дополняется. Уже применённую миграцию **править нельзя** —
/// у существующих баз она не переприменится, и схема разъедется с кодом.
/// Изменение схемы = новая строка в конце.
pub const MIGRATIONS: &[&str] = &[WAVE_ZERO];

/// Схема волны 0.
///
/// Таблицы слоёв поверх сырья (`rules`, `overrides`, `manual_entries`,
/// `imports`) сюда не входят: к ним нет кода до волны 1, а таблица без кода
/// неизбежно разъезжается с тем кодом, который для неё однажды напишут.
/// `teams` — исключение, спека требует их с первого дня.
const WAVE_ZERO: &str = r#"
CREATE TABLE strings (
  id    INTEGER PRIMARY KEY,
  value TEXT NOT NULL UNIQUE
);

CREATE TABLE users (
  id            BLOB PRIMARY KEY,
  login         TEXT NOT NULL UNIQUE,
  email         TEXT,
  password_hash TEXT NOT NULL,
  display_name  TEXT,
  timezone      TEXT NOT NULL,
  timeout_secs  INTEGER NOT NULL,
  is_admin      INTEGER NOT NULL DEFAULT 0,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE api_keys (
  id            BLOB PRIMARY KEY,
  user_id       BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name          TEXT NOT NULL,
  key_encrypted BLOB NOT NULL,
  key_lookup    BLOB NOT NULL UNIQUE,
  created_at    INTEGER NOT NULL,
  last_used_at  INTEGER,
  revoked_at    INTEGER
) WITHOUT ROWID;

CREATE TABLE sessions (
  id         BLOB PRIMARY KEY,
  user_id    BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash BLOB NOT NULL UNIQUE,
  user_agent TEXT,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  revoked_at INTEGER
) WITHOUT ROWID;

CREATE TABLE heartbeats (
  id                 BLOB PRIMARY KEY,
  user_id            BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  time               INTEGER NOT NULL,
  received_at        INTEGER NOT NULL,
  entity_id          INTEGER NOT NULL REFERENCES strings(id),
  kind               INTEGER NOT NULL,
  category           INTEGER NOT NULL,
  project_id         INTEGER REFERENCES strings(id),
  branch_id          INTEGER REFERENCES strings(id),
  language_id        INTEGER REFERENCES strings(id),
  editor_id          INTEGER REFERENCES strings(id),
  os_id              INTEGER REFERENCES strings(id),
  machine_id         INTEGER REFERENCES strings(id),
  plugin_id          INTEGER REFERENCES strings(id),
  is_write           INTEGER NOT NULL,
  lines              INTEGER,
  lineno             INTEGER,
  cursorpos          INTEGER,
  line_additions     INTEGER,
  line_deletions     INTEGER,
  project_root_count INTEGER,
  dependencies       TEXT,
  ai_line_changes    INTEGER,
  human_line_changes INTEGER,
  ai_meta            TEXT,
  dedup_hash         INTEGER NOT NULL
) WITHOUT ROWID;

CREATE UNIQUE INDEX hb_dedup ON heartbeats(user_id, dedup_hash);
CREATE INDEX hb_time ON heartbeats(user_id, time);

CREATE TABLE dirty_days (
  user_id    BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  local_date TEXT NOT NULL,
  marked_at  INTEGER NOT NULL,
  PRIMARY KEY (user_id, local_date)
) WITHOUT ROWID;

CREATE TABLE teams (
  id         BLOB PRIMARY KEY,
  name       TEXT NOT NULL,
  owner_id   BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE team_members (
  team_id   BLOB NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  user_id   BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role      TEXT NOT NULL,
  joined_at INTEGER NOT NULL,
  PRIMARY KEY (team_id, user_id)
) WITHOUT ROWID;
"#;
