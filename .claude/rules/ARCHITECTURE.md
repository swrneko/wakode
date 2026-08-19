# Карта проекта wakode

Единственная карта. Дублировать её содержимое в `AGENTS.md` запрещено — там указатели, здесь факты.

## Стек

- Rust, edition 2024, resolver 3; workspace из пяти крейтов. Лицензия AGPL-3.0-or-later.
- HTTP — `axum` 0.8 поверх `tokio`, слои `tower-http` (`TraceLayer`, `CatchPanicLayer`).
- Хранилище — SQLite через `rusqlite` 0.40 в режиме WAL, за репозиторными трейтами (RPITIT, без `async-trait`).
- Криптография — `argon2` (пароли), `chacha20poly1305` (шифрование ключей мастер-ключом), `hmac`+`sha2`, `subtle` (сравнение за постоянное время).
- Журнал — `tracing` + `tracing-subscriber`, всё в stderr: stdout занят данными подкоманд.
- CLI — `clap` 4.6, конфиг — `toml`.
- PK/FK — UUIDv7 (RFC 9562). Таблицы отметок — `WITHOUT ROWID`, кластеризованы по PK.

Источник правды по версиям — `Cargo.toml` в корне, не этот файл.

## Команды

```bash
cargo test --workspace                  # весь набор (~365 тестов)
cargo test -p wakode-api --test api     # HTTP-слой
cargo test -p wakode-api --test log     # журнал; отдельный бинарь не по вкусу — см. ниже
cargo test -p wakode --test cli         # сквозные, через настоящий процесс
cargo build --workspace --all-targets   # сборка, обязана быть без предупреждений
cargo run -p wakode -- --config wakode.toml serve
cargo run -p wakode -- --config wakode.toml migrate
cargo run -p wakode -- master-key generate
cargo run -p wakode -- --config wakode.toml user create --login <логин> --admin
cargo run -p wakode -- --config wakode.toml key issue --login <логин>
cargo run -p wakode -- --config wakode.toml backup --to <путь>
```

`cargo fmt` в проекте не гоняется: установленный rustfmt переставляет импорты по стилю edition 2024, и репозиторий по нему не чист. Форматирование — по соседнему коду.

## Структура

```
.
├── Cargo.toml                  workspace, все версии зависимостей здесь
├── crates/
│   ├── wakode-core/            чистое ядро: отметки → интервалы → дни → сводки
│   ├── wakode-store/           SQLite за репозиторными трейтами
│   ├── wakode-auth/            криптография: чистые функции над байтами
│   ├── wakode-api/             HTTP-слой
│   └── wakode/                 бинарь: CLI, конфиг, старт, сигналы
├── deploy/                     юнит systemd и примеры конфигурации
├── docs/superpowers/
│   ├── plans/                  планы реализации по итерациям
│   └── specs/                  спецификации (§-нумерация, на них ссылается код)
└── .claude/
    ├── rules/ARCHITECTURE.md   этот файл
    └── docs/                   операционная память
```

## Границы крейтов

Границы держатся **списком зависимостей**, а не соглашением, — это проверяемо, а соглашение нет.

- `wakode-core` не ходит ни в базу, ни в сеть, ни в файловую систему, ни к часам. Строк не видит: проект, язык и редактор приезжают номерами `Sid`, их разрешает слой хранения. Время внутри — всегда `Micros` (микросекунды от эпохи UTC). `chrono` подключён без фичи `clock`: она тянет `iana-time-zone`, а тот читает `/etc/localtime`.
- `wakode-auth` не знает ни про базу, ни про axum. Появление там `rusqlite` или `axum` означает, что криптография перестала быть отдельной.
- `wakode-api` не содержит криптографии — она целиком в `wakode-auth`.

## Модули

Файлов в `.claude/docs/modules/` пока нет: слой заведён этой миграцией. Строки ниже — оглавление кода; по мере работы над модулем заводится `docs/modules/<slug>.md`, и строка получает ссылку.

**wakode-core** — `domain.rs` (типы), `intervals.rs` (склейка отметок в интервалы), `calendar.rs` (локальные дни), `aggregate.rs` (сводки), `time.rs` (`Micros`), `config.rs` (пороги). Свойства проверяются `proptest` в `tests/properties.rs`.

**wakode-store** — `schema.rs`/`migrate.rs` (схема и версии), `conn.rs` (WAL, `busy_timeout`), `repo.rs` (трейты), `users.rs`/`keys.rs`/`sessions.rs`/`heartbeats.rs` (репозитории), `writer.rs` (единственная пишущая задача для потока отметок; редкие одиночные записи идут мимо неё своими соединениями), `interner.rs` (строки → `Sid`), `dedup.rs`, `dirty.rs`, `codec.rs`, `clock.rs`.

**wakode-auth** — `password.rs` (argon2), `api_key.rs` (значение ключа и его шифрование), `master_key.rs`, `session.rs`, `setup_token.rs` (одноразовый токен первичной настройки). Политика секретов — в докстринге `lib.rs`: `Debug` не печатает секрет никогда и подпирается тестом со сверкой **точной строки**; `Display` печатает дословно только у тех типов, которым положено уехать наружу.

**wakode-api** — `lib.rs` (`router`, `with_layers`, `serve`), `setup.rs` (первичная настройка, токен, адресная проверка), `auth/` (сессии и API-ключи), `compat/` (WakaTime-совместимые эндпоинты), `internal/`, `health.rs`, `error.rs`, `state.rs`.

**wakode** — `main.rs` (`run`/`dispatch`), `cli/` (подкоманды), `config.rs`, `startup.rs`, `signal.rs` (SIGTERM/SIGINT и предел дочитывания).

## Инварианты, которые ломали

- **`main.rs`: ни одного `?` между `startup::start` и `store.shutdown()`**, выходящего мимо останова писателя. Результаты сходятся в `outcome` и возвращаются после `shutdown`. Ломали дважды.
- **Журнальные тесты — только в `crates/wakode-api/tests/log.rs`**, отдельным бинарём. `tracing` кеширует Interest глобально на процесс: соседи из `api.rs` отравляли кеш, набор падал примерно раз в четыре прогона без единой правки кода.
- **Секрет не ездит в пути URL.** В журнал пишется `path`, не `uri`: `uri` уносил бы `?api_key=…`. Маршрут вида `/api/keys/{ключ}` уронил бы ключ в журнал мимо всех проверок.
- **`method_not_allowed_fallback` ставится после всех `route`.** Новые маршруты добавляются выше, иначе останутся с пустым `405` axum'а.
- **`ConnectInfo` — это TCP-пир, а не клиент.** За прокси на том же хосте он всегда `127.0.0.1`, и `is_loopback()` вырождается в «разрешено всем».

## Конвенции

- `#[expect(..., reason = "...")]`, никогда `#[allow(...)]`: `expect` сам требует снятия, когда перестаёт быть верным.
- Комментарии, докстринги и тексты ошибок — по-русски; имена тестов — английские фразы о поведении.
- Зависимости объявляются в `[workspace.dependencies]`, в крейт подключаются через `.workspace = true`.
- `anyhow` — только в бинаре; библиотечные крейты на `thiserror`.
