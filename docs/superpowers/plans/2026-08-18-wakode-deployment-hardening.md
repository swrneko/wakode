# Развёртывание: штатное завершение и токен первичной настройки

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** закрыть два блокера, оставленных планом 3a: сервер завершается по SIGTERM, дав писателю дописать принятое, и первичная настройка выполнима из-за обратного прокси без открытия её всему интернету.

**Architecture.** Первый блокер — `axum::serve(...).with_graceful_shutdown(signal)` плюс собственный предел ожидания в бинаре: если начатые запросы не закрылись за отведённое время, мы бросаем их сами, потому что чужой предел (`TimeoutStopSec` systemd) присылает SIGKILL, а он не даёт остановить писателя. Второй — одноразовый токен настройки: 32 случайных байта, живущие в памяти процесса, печатаемые в журнал при старте, пока в базе нет ни одного пользователя, и предъявляемые заголовком `X-Wakode-Setup-Token`. Токен снимает вопрос о топологии сети целиком: он не зависит ни от адреса пира, ни от заголовков посредника.

**Tech Stack:** Rust 2024, axum 0.8, tokio 1.53 (фичи `signal`, `time`), subtle 2.6 (сравнение за постоянное время), base64 0.23, rand 0.8.

## Контекст: откуда взялись обе задачи

Обе записаны в журнале плана 3a (`.superpowers/sdd/2026-08-17-wakode-server-foundation/progress.md`) как парковки с пометкой «блокер».

**Блокер 1, штатное завершение.** `Task 14: parked — штатное завершение serve по сигналу отсутствует; graceful shutdown не подключён, фича signal у tokio не используется. БЛОКЕР ДЛЯ ЗАДАЧИ, ВВОДЯЩЕЙ INGEST.» Сегодня SIGTERM убивает процесс на месте: `store.shutdown()` не отрабатывает, пишущий поток умирает вместе с процессом. Пока через очередь писателя не идёт ничего, кроме тестов, цена нулевая. С появлением эндпоинта приёма отметок цена — принятые и подтверждённые клиенту отметки, которых нет в базе.

Сюда же примыкает `Финал-3: parked — инвариант «shutdown зовётся всегда» не держится ни одним тестом: наблюдаемого признака у нарушения нет`. После этого плана признак появляется: по SIGTERM процесс обязан выйти сам, с нулевым кодом, напечатав строку об остановленном писателе. Инвариант становится проверяемым, и задача 1 обязана его проверить.

**Блокер 2, настройка за прокси.** `Финал: CRITICAL` и `Финал-2`: адресная проверка первичной настройки вырождается в «разрешено всем» при штатной установке за обратным прокси на том же хосте, потому что `ConnectInfo` — это TCP-пир, а им оказывается сам прокси. Закрыто подмножество: отказ при наличии любого из шести заголовков посредника. Не закрыто: голый `proxy_pass http://127.0.0.1:9000;` без единого `proxy_set_header` — минимальный рабочий конфиг nginx, не добавляющий ни одного из них. Спека 3a §6 называет остаток прямо и записывает решение в 3b: «белый список доверенных прокси или одноразовый токен настройки».

## Решения и их обоснование

**Почему токен, а не белый список доверенных посредников.** Оба закрывают долг, спека допускает любой. Токен выбран потому, что не требует от владельца рассуждать о топологии: белый список ошибается молча и в опасную сторону — вписанный не тот адрес, лишняя запись `0.0.0.0/0`, забытая после отладки, снова открывают экран настройки. Токен ошибиться в эту сторону не даёт: не предъявил — не прошёл. Кроме того, белый список нужен не ради настройки, а ради знания настоящего адреса клиента (журнал, ограничение частоты запросов); это отдельная работа с отдельным потребителем, и делать её сейчас — гадать об интерфейсе без вызывающего.

**Почему токен в заголовке, а не в теле запроса.** Тело разбирается **после** проверки доступа — так решено в задаче 12 плана 3a и закреплено тестом `the_address_is_checked_before_the_database`: чужому незачем слышать про формат JSON раньше, чем ему отказали. Токен в теле заставил бы разбирать тело до решения о доступе и сломал бы этот порядок.

**Почему предъявленный неверный токен — отказ, а не переход к адресной проверке.** Предъявление токена — явное утверждение «я знаю секрет». Ложное утверждение получает свой отказ со своим текстом. Иначе владелец, вставивший токен с опечаткой на петлевом адресе, прошёл бы по адресу и не узнал, что токен неверен, а на следующей машине получил бы отказ про адрес, держа в руках токен.

**Почему токен живёт только в памяти и только до первого пользователя.** Окно первичной настройки закрывается навсегда после первого пользователя — эндпоинт после этого отвечает `403` независимо от токена. Секрет, переживающий своё окно, — секрет без назначения. Перезапуск выдаёт новый токен; это цена, и она названа в тексте отказа.

**Почему токен печатается в журнал.** Это единственное место в проекте, где секрет пишется в лог намеренно, и решение осознанное: журнал читает тот, у кого доступ к машине уже есть, а владельцу за прокси взять токен больше неоткуда. Альтернатива — `setup_from_any_address = true`, то есть открыть настройку всем. Политика `wakode-auth` («`Display` печатает секрет дословно, `Debug` — никогда») остаётся в силе и распространяется на новый тип.

**Почему предел ожидания завершения свой, а не systemd'шный.** `axum` с graceful shutdown ждёт закрытия начатых соединений сколько угодно долго. Владелец под systemd получит SIGKILL через `TimeoutStopSec` (по умолчанию 90 секунд), и SIGKILL не даст остановить писателя — то есть ровно то, ради чего вводится завершение по сигналу, будет потеряно в единственном случае, когда оно нужно. Свой предел меньше чужого: он срабатывает первым, бросает начатые запросы и даёт `store.shutdown()` отработать.

## Global Constraints

- Язык кода: комментарии, докстринги, сообщения об ошибках и имена тестов — по действующему стилю репозитория. Имена тестов — английские фразы, описывающие поведение (`a_wrong_token_is_refused_even_from_loopback`), комментарии и тексты ошибок — русские.
- Никаких упоминаний Claude/AI в коммит-сообщениях.
- Тесты проверяют поведение через публичный интерфейс. Мок допустим только на границе системы.
- Каждая задача заканчивается прогоном `cargo test --workspace` без единого предупреждения.
- **Мутационная проверка обязательна.** Для каждого нового теста автор обязан внести мутацию, ради которой тест написан, убедиться, что тест краснеет, и вернуть код **из резервной копии файла**, а не `git checkout` — так уже терялась несохранённая работа.
- Зависимости добавляются в `[workspace.dependencies]` корневого `Cargo.toml` и подключаются в крейт через `.workspace = true`.
- `wakode-api` не содержит криптографии: она целиком в `wakode-auth`, и список зависимостей крейта — способ это проверить.

---

## Файлы

**Создаются:**
- `crates/wakode/src/signal.rs` — ожидание сигнала завершения и предел ожидания дочитывания. Чистая логика ожидания, без сокетов, тестируется юнит-тестами на подставных футурах.
- `crates/wakode-auth/src/setup_token.rs` — тип `SetupToken`: генерация, печать, сравнение за постоянное время.

**Изменяются:**
- `crates/wakode-api/src/lib.rs` — `serve` получает третий параметр (футура завершения) и меняет тип возврата.
- `crates/wakode-api/src/setup.rs` — решение о доступе выносится в одну функцию, добавляется ветка токена.
- `crates/wakode-api/src/state.rs` — поле `setup_token` и строитель к нему.
- `crates/wakode-auth/src/lib.rs` — подключение модуля и реэкспорт.
- `crates/wakode/src/main.rs` — проводка сигнала, предела, токена; строка об остановленном писателе.
- `crates/wakode/tests/cli.rs` — общий помощник поднятия сервера, тест SIGTERM, сквозной тест токена.
- `crates/wakode-api/tests/api.rs` — тесты завершения, статуса и токена.
- `Cargo.toml`, `crates/wakode/Cargo.toml`, `crates/wakode-auth/Cargo.toml` — зависимости.
- `docs/superpowers/specs/2026-08-17-wakode-server-foundation-design.md` — §6, последний абзац.

---

### Task 1: Штатное завершение по сигналу

**Files:**
- Modify: `crates/wakode-api/src/lib.rs:102-113` (`serve`)
- Modify: `crates/wakode-api/tests/api.rs:348` и `:1660` (два вызова `wakode_api::serve`)
- Create: `crates/wakode/src/signal.rs`
- Modify: `crates/wakode/src/main.rs` (объявление модуля, `serve`, строка после `shutdown`, устаревший комментарий)
- Modify: `crates/wakode/Cargo.toml` (фича `time` у tokio, dev-зависимость `libc`)
- Modify: `Cargo.toml` (`libc` в `[workspace.dependencies]`)
- Test: `crates/wakode-api/tests/api.rs`, `crates/wakode/src/signal.rs` (модуль `tests`), `crates/wakode/tests/cli.rs`

**Interfaces:**
- Produces:
  - `wakode_api::serve(listener: tokio::net::TcpListener, state: AppState, shutdown: impl Future<Output = ()> + Send + 'static)` → `()`
  - `crate::signal::wait_for_signal() -> &'static str` (имя пришедшего сигнала)
  - `crate::signal::wait_for_drain(served: impl Future<Output = ()>, signalled: impl Future<Output = ()>, grace: Duration) -> bool`
  - `crate::signal::GRACE: Duration`
  - `crates/wakode/tests/cli.rs::a_serving_child(tail: &[&str]) -> Serving` — общий помощник, используется задачей 3
- Consumes: `wakode_store::SqliteStore::shutdown` (уже есть, повторный вызов не ошибка)

**Важное про тип возврата.** *(Поправлено по факту при реализации: первая редакция плана утверждала, что у `WithGracefulShutdown` `Output = ()`. В axum 0.8.9 это не так — проверено по исходнику, `type Output = io::Result<()>` в обоих случаях. Сниппет документации, на который опирался план, взят из ветки `main`, а не из выпущенной версии.)*

У `Serve` (без graceful shutdown) футура резолвится в `io::Result<()>`, но **никогда не завершается и никогда не возвращает ошибку** — ошибки сокета обрабатываются сном и повтором приёма. У `WithGracefulShutdown` тип тот же, и `Err` в нём тоже не появляется: `Ok(())` приходит после завершения `shutdown`. Поэтому `wakode_api::serve` меняет тип возврата с `std::io::Result<()>` на `()`, отбрасывая результат у себя: `Result`, который не может быть ошибкой, обязывает вызывающего писать `?` там, где ветки отказа не существует.

- [ ] **Step 1: Написать падающий тест на возврат из `serve`**

В `crates/wakode-api/tests/api.rs`, рядом с `serve_actually_answers_on_a_real_socket`:

```rust
#[tokio::test]
async fn serve_returns_when_asked_to_stop_and_releases_the_port() {
    // Без этого теста завершение по сигналу держится обещанием: `serve`,
    // потерявшая `with_graceful_shutdown`, снаружи выглядит точно так же —
    // сервер работает, — а SIGTERM в бинаре просто убивал бы процесс мимо
    // остановки писателя.
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(wakode_api::serve(listener, a_state(&dir), async move {
        let _ = stopped.await;
    }));

    // Сначала — что сервер вообще поднялся: иначе «вернулась сразу»
    // прошло бы этот тест зелёным.
    assert!(
        raw_get(addr, "/healthz").await.starts_with("HTTP/1.1 200 OK"),
        "сервер не ответил до сигнала"
    );

    stop.send(()).unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("serve не вернулась через пять секунд после сигнала")
        .expect("задача с serve упала");

    // Порт отпущен — доказательство, что слушатель уничтожен, а не просто
    // функция вернулась, оставив приём соединений жить.
    tokio::net::TcpListener::bind(addr)
        .await
        .expect("порт всё ещё занят: слушатель пережил останов");
}
```

`raw_get` — вспомогательная функция сырого запроса; если её в файле нет, вынести её из тела `serve_actually_answers_on_a_real_socket` (там сырой `GET` уже написан) и использовать в обоих тестах.

- [ ] **Step 2: Прогнать — тест обязан не собираться**

Run: `cargo test -p wakode-api --test api serve_returns_when_asked_to_stop -- --nocapture`
Expected: ошибка компиляции — `serve` принимает два аргумента, а передано три.

- [ ] **Step 3: Изменить `serve`**

`crates/wakode-api/src/lib.rs`, замена всей функции:

```rust
/// Поднять сервер на готовом слушателе и работать, пока не попросят стать.
///
/// `shutdown` — футура, завершение которой означает «перестать принимать
/// новые соединения и дочитать начатые». Параметр обязателен и не имеет
/// умолчания намеренно: вызывающий обязан решить, чем его сервер
/// останавливается. Тому, кому останов не нужен (тесты одного запроса),
/// подходит `std::future::pending()`, и это видно в вызове.
///
/// Тип возврата — `()`, а не `io::Result<()>`. И без graceful shutdown, и
/// с ним футура axum объявлена как `io::Result<()>`, но `Err` в ней не
/// появляется: ошибки сокета обрабатываются сном и повтором приёма, а
/// `Ok(())` приходит только после завершения `shutdown`. `Result`,
/// который не может быть ошибкой, обязывает вызывающего писать `?` там,
/// где ветки отказа не существует, поэтому он отбрасывается здесь.
///
/// `into_make_service_with_connect_info` обязателен: экран первичной
/// настройки смотрит на адрес клиента, а без этого `ConnectInfo` в
/// обработчике не извлечётся. Держится тестом
/// `setup_over_a_real_socket_sees_the_client_address`.
///
/// Чего эта функция не делает: не ограничивает время дочитывания. Запрос,
/// который не закрывается никогда, задержит здесь навсегда. Предел ставит
/// вызывающий — см. `signal::wait_for_drain` в бинаре, — потому что
/// решение «бросить начатое» принимается на уровне процесса, а не
/// HTTP-слоя.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: AppState,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) {
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await;
}
```

Поправить два существующих вызова в `crates/wakode-api/tests/api.rs` (строки ~357 и ~1660): третьим аргументом `std::future::pending::<()>()`. Оба теста после этого продолжают снимать сервер через `server.abort()` — это по-прежнему верно, они проверяют не останов.

- [ ] **Step 4: Прогнать**

Run: `cargo test -p wakode-api`
Expected: PASS, включая новый тест.

- [ ] **Step 5: Мутация — убрать `with_graceful_shutdown`**

Сохранить копию: `cp crates/wakode-api/src/lib.rs /tmp/lib.rs.bak`. Убрать `.with_graceful_shutdown(shutdown)` (добавив `let _ = shutdown;` ради предупреждений).
Ожидание: `serve_returns_when_asked_to_stop_and_releases_the_port` падает по пятисекундному пределу.
Вернуть файл **из копии**: `cp /tmp/lib.rs.bak crates/wakode-api/src/lib.rs`.

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-api/src/lib.rs crates/wakode-api/tests/api.rs
git commit -m "feat(api): останов сервера по внешней футуре"
```

- [ ] **Step 7: Написать падающие юнит-тесты предела ожидания**

Создать `crates/wakode/src/signal.rs`, сначала только с тестами (реализация — следующим шагом):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_server_that_stops_on_its_own_is_reported_as_drained() {
        let drained = wait_for_drain(
            std::future::ready(()),
            std::future::pending(),
            Duration::from_millis(50),
        )
        .await;
        assert!(drained);
    }

    #[tokio::test]
    async fn a_server_that_hangs_past_the_grace_is_abandoned() {
        // `false` здесь — это «начатые запросы брошены». Хуже, чем
        // дождаться, но лучше, чем SIGKILL от systemd: тот не оставляет
        // шанса остановить писателя.
        let drained = wait_for_drain(
            std::future::pending(),
            std::future::ready(()),
            Duration::from_millis(50),
        )
        .await;
        assert!(!drained);
    }

    #[tokio::test]
    async fn the_grace_starts_at_the_signal_not_at_the_start() {
        // Предел, отсчитываемый от запуска, убивал бы работающий сервер
        // через десять секунд после старта. Сигнала здесь нет вовсе, а
        // сервер работает вчетверо дольше предела — и это штатная работа,
        // а не превышение.
        let served = async {
            tokio::time::sleep(Duration::from_millis(120)).await;
        };
        let drained = wait_for_drain(served, std::future::pending(), Duration::from_millis(20)).await;
        assert!(drained, "предел потёк до сигнала");
    }
}
```

- [ ] **Step 8: Прогнать — не собирается**

Run: `cargo test -p wakode signal`
Expected: ошибка компиляции — `wait_for_drain` не существует.

- [ ] **Step 9: Реализовать `signal.rs`**

Полное содержимое файла над модулем тестов:

```rust
//! Штатное завершение процесса: чего ждём и сколько.

use std::future::Future;
use std::time::Duration;

/// Сколько ждать, пока сервер сам дочитает начатые запросы.
///
/// Меньше, чем `TimeoutStopSec` systemd по умолчанию (90 секунд), и это
/// главное свойство числа: срабатывать обязан наш предел, а не чужой.
/// Чужой присылает SIGKILL, а SIGKILL не оставляет шанса остановить
/// писателя — то есть теряет ровно то, ради чего завершение по сигналу и
/// заведено.
pub const GRACE: Duration = Duration::from_secs(10);

/// Дождаться сигнала завершения. Возвращает имя пришедшего — для журнала.
///
/// SIGTERM шлют systemd и `docker stop`; SIGINT — Ctrl-C в терминале.
/// Ждём оба: инстанс запускают и так, и так.
///
/// Отказ установки обработчика не прерывает работу: сервер нужен
/// владельцу больше, чем красивое завершение, и оставшийся сигнал
/// продолжает работать. Оба сразу отказать не могут по-тихому — тогда
/// функция просто никогда не вернётся, а процесс останется таким же
/// убиваемым, как до этого плана.
pub async fn wait_for_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut terminate = signal(SignalKind::terminate()).ok();
        let mut interrupt = signal(SignalKind::interrupt()).ok();

        let on_terminate = async {
            match terminate.as_mut() {
                Some(stream) => {
                    stream.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        let on_interrupt = async {
            match interrupt.as_mut() {
                Some(stream) => {
                    stream.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            () = on_terminate => "SIGTERM",
            () = on_interrupt => "SIGINT",
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "Ctrl-C"
    }
}

/// Дождаться конца работы сервера, но не дольше `grace` после сигнала.
///
/// Возвращает `true`, если сервер закрыл начатые соединения сам.
///
/// Отсчёт начинается **от сигнала**, а не от вызова: до сигнала сервер
/// работает штатно сколько угодно долго, и предел, потёкший раньше,
/// убивал бы живой инстанс. Это не теоретическое замечание — ровно так
/// написалась бы функция с одним `select!` на три ветки.
pub async fn wait_for_drain(
    served: impl Future<Output = ()>,
    signalled: impl Future<Output = ()>,
    grace: Duration,
) -> bool {
    tokio::pin!(served);

    tokio::select! {
        () = &mut served => return true,
        () = signalled => {}
    }

    tokio::select! {
        () = &mut served => true,
        () = tokio::time::sleep(grace) => false,
    }
}
```

В `crates/wakode/Cargo.toml` добавить фичу `time` в список фич tokio (нужна `tokio::time::sleep`):

```toml
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "net", "signal", "time"] }
```

В `crates/wakode/src/main.rs` объявить модуль рядом с остальными: `mod signal;`.

- [ ] **Step 10: Прогнать**

Run: `cargo test -p wakode signal`
Expected: PASS, три новых теста.

- [ ] **Step 11: Мутация — предел с начала работы**

Копия: `cp crates/wakode/src/signal.rs /tmp/signal.rs.bak`. Свести `wait_for_drain` к одному `select!`:

```rust
tokio::select! {
    () = &mut served => true,
    () = tokio::time::sleep(grace) => false,
}
```
(со `let _ = signalled;`)
Ожидание: падает `the_grace_starts_at_the_signal_not_at_the_start`.
Вернуть **из копии**.

- [ ] **Step 12: Коммит**

```bash
git add crates/wakode/src/signal.rs crates/wakode/src/main.rs crates/wakode/Cargo.toml
git commit -m "feat(cli): ожидание сигнала завершения и предел дочитывания"
```

- [ ] **Step 13: Общий помощник поднятия сервера в тестах CLI**

*(Поправка по факту реализации: шаги 13–16 буквально непроходимы — после шага 3 `wakode_api::serve` уже трёхаргументная, и `crates/wakode/src/main.rs` не собирается до шага 17. Порядок правильный по смыслу — сначала красный тест, потом проводка, — но чтобы прогнать точечные тесты на шагах 14 и 16, в `main.rs` нужна временная заглушка `std::future::pending::<()>()` третьим аргументом, снимаемая на шаге 17.)*

Существующий `serve_answers` поднимает процесс, ждёт `/healthz` и на отказе читает stderr из трубы. Задаче 3 нужен тот же процесс плюс чтение журнала **при живом процессе**, а `read_to_string` на трубе живого процесса висит до EOF. Поэтому stderr переводится в файл во временной папке — так журнал читается сколько угодно раз и в любой момент.

В `crates/wakode/tests/cli.rs` заменить `serve_answers` на пару «помощник + обёртка»:

```rust
/// Поднятый дочерний `wakode serve`: процесс, адрес и журнал.
///
/// `dir` держится живым намеренно: в нём лежат конфиг, база и файл
/// журнала, и уничтожение папки раньше времени вырвало бы их из-под
/// работающего сервера.
struct Serving {
    dir: tempfile::TempDir,
    child: Killed,
    addr: std::net::SocketAddr,
    log: std::path::PathBuf,
}

impl Serving {
    /// Всё, что сервер написал в stderr к этому моменту.
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

/// Поднять бинарь с заданным хвостом аргументов и дождаться `/healthz`.
///
/// Журнал уходит в файл, а не в трубу: труба живого процесса читается
/// только до EOF, то есть до его смерти, а тестам задачи 3 журнал нужен,
/// пока сервер работает.
fn a_serving_child(tail: &[&str]) -> Serving {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("wakode.toml");

    // Порт занимается и отпускается: узнать свободный номер заранее иначе
    // нечем, а передать готовый слушатель дочернему процессу нельзя.
    let addr = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap()
    };

    std::fs::write(
        &config,
        format!(
            "[server]\nlisten = \"{addr}\"\n\n[database]\npath = {:?}\n",
            dir.path().join("wakode.db").to_str().unwrap()
        ),
    )
    .unwrap();

    let log = dir.path().join("server.log");
    let sink = std::fs::File::create(&log).unwrap();

    let mut args = vec!["--config".to_owned(), config.to_str().unwrap().to_owned()];
    args.extend(tail.iter().map(|arg| (*arg).to_owned()));

    let child = Killed(
        wakode()
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::from(sink))
            .spawn()
            .unwrap(),
    );

    let mut serving = Serving { dir, child, addr, log };

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last: String;
    loop {
        if let Some(status) = serving.child.0.try_wait().unwrap() {
            panic!(
                "процесс `serve` завершился, не начав слушать: {status}\nstderr:\n{}",
                serving.log()
            );
        }
        match healthz(addr) {
            Ok(response) if response.starts_with("HTTP/1.1 200 OK") => {
                assert!(response.ends_with("ok"), "нет тела ответа: {response}");
                return serving;
            }
            Ok(response) => last = format!("сервер ответил не тем: {response}"),
            Err(err) => last = format!("соединение не установилось: {err}"),
        }
        if Instant::now() >= deadline {
            panic!("{last}\nstderr сервера:\n{}", serving.log());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
```

`serve_comes_up_and_answers` и `no_subcommand_means_serve` теперь вызывают `a_serving_child(&["serve"])` и `a_serving_child(&[])` соответственно — сам факт возврата и есть проверка (внутри стоит `assert!` на тело).

- [ ] **Step 14: Прогнать — оба существующих теста зелёные**

Run: `cargo test -p wakode --test cli serve`
Expected: PASS.

- [ ] **Step 15: Написать падающий тест на SIGTERM**

Добавить в `crates/wakode/Cargo.toml` dev-зависимость и в корневой `Cargo.toml` — в `[workspace.dependencies]`:

```toml
# Только в тестах и только ради `kill`: послать сигнал собственному
# ребёнку из std нечем, а тащить ради одного вызова `nix` — несоразмерно.
libc = "0.2"
```

```rust
#[cfg(unix)]
#[test]
fn sigterm_stops_the_server_cleanly_and_stops_the_writer() {
    // Три утверждения об одном: процесс уходит сам (а не висит с
    // проглоченным сигналом), уходит успехом (systemd иначе считает
    // штатную остановку отказом и пишет `Failed with result exit-code`),
    // и по дороге останавливает писателя.
    //
    // Последнее — единственный наблюдаемый признак инварианта «shutdown
    // зовётся всегда», который до этого плана не держался ничем: до
    // появления обработчика сигнала SIGTERM убивал процесс на месте.
    let mut serving = a_serving_child(&["serve"]);
    let pid = serving.child.0.id();

    // Безопасность: `pid` взят у живого ребёнка, которого мы сами
    // породили, и до `wait` его номер не переиспользуется.
    assert_eq!(
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) },
        0,
        "kill не отправился"
    );

    let status = wait_for_exit(&mut serving.child.0, Duration::from_secs(20))
        .expect("процесс не завершился по SIGTERM за двадцать секунд");

    assert!(
        status.success(),
        "SIGTERM — штатная остановка, а не отказ: {status}\n{}",
        serving.log()
    );

    let log = serving.log();
    assert!(
        log.contains("сигнал завершения"),
        "сигнал не отмечен в журнале:\n{log}"
    );
    assert!(
        log.contains("писатель остановлен"),
        "останов писателя не отработал по пути сигнала:\n{log}"
    );
}

/// Дождаться завершения процесса, но не дольше срока.
#[cfg(unix)]
fn wait_for_exit(
    child: &mut std::process::Child,
    within: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
```

- [ ] **Step 16: Прогнать — тест обязан упасть по сроку**

Run: `cargo test -p wakode --test cli sigterm -- --nocapture`
Expected: FAIL. Сегодня `wakode_api::serve` не получает футуру сигнала из бинаря, и обработчика SIGTERM нет — процесс умирает по умолчательному действию сигнала, то есть `status.success()` ложно (`signal: 15`), а строк в журнале нет.

- [ ] **Step 17: Провести сигнал в `serve` бинаря**

`crates/wakode/src/main.rs`, замена функции `serve`:

```rust
/// Шаг 6 старта: поднять HTTP-слой и работать до сигнала.
async fn serve(started: &startup::Startup) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(&started.config.server.listen)
        .await
        .with_context(|| format!("не удалось занять адрес {}", started.config.server.listen))?;

    // Пишется после `bind`, а не до: до него это было бы обещанием, а не
    // фактом, — ровно та тихая ложь о состоянии, из-за которой владелец
    // идёт искать причину в брандмауэре.
    tracing::info!(listen = %started.config.server.listen, "сервер поднят");

    let state = AppState::new(
        started.store.clone(),
        started.master_key.clone(),
        app_settings(&started.config),
    );

    // Сигнал нужен обеим сторонам: сервер по нему перестаёт принимать
    // соединения, а предел ожидания по нему же начинает течь. Ждать одну
    // футуру дважды нельзя, поэтому факт сигнала раздаётся через канал.
    let (signalled, wait_signalled) = tokio::sync::oneshot::channel();
    let shutdown = async move {
        let name = signal::wait_for_signal().await;
        tracing::info!(signal = name, "сигнал завершения: закрываем приём новых соединений");
        let _ = signalled.send(());
    };

    let served = wakode_api::serve(listener, state, shutdown);
    let drained = signal::wait_for_drain(
        served,
        async move {
            let _ = wait_signalled.await;
        },
        signal::GRACE,
    )
    .await;

    if drained {
        tracing::info!("начатые запросы дочитаны");
    } else {
        tracing::warn!(
            grace_secs = signal::GRACE.as_secs(),
            "не все соединения закрылись в срок; бросаем начатое и завершаемся"
        );
    }

    Ok(())
}
```

- [ ] **Step 18: Сделать останов писателя наблюдаемым**

`crates/wakode/src/main.rs`, замена блока с `shutdown` в `run`:

```rust
    match started.store.shutdown().await {
        // Строка не косметическая: до неё у инварианта «останов зовётся
        // всегда» не было наблюдаемого признака, и тест на SIGTERM не мог
        // отличить «остановили писателя» от «процесс умер вовремя».
        Ok(()) => tracing::info!("писатель остановлен, база отпущена"),
        // Отказ останова не подменяет собой отказ подкоманды: подменив,
        // мы сообщили бы про писателя вместо того, что владелец просил
        // сделать. Отдельной строкой в журнал — и всё.
        Err(err) => tracing::warn!(error = %err, "останов писателя завершился с ошибкой"),
    }
```

Там же переписать устаревший комментарий перед `let outcome = ...`: утверждение «`serve` возвращается только на io-ошибке — штатного завершения по сигналу ещё нет, и SIGTERM убивает процесс, не дав `shutdown` отработать» стало ложным. Новый текст обязан говорить по факту: `serve` возвращается по сигналу, останов писателя после него — не задел на будущее, а работающий путь, закреплённый тестом `sigterm_stops_the_server_cleanly_and_stops_the_writer`.

- [ ] **Step 19: Прогнать**

Run: `cargo test -p wakode`
Expected: PASS, включая новый тест.

- [ ] **Step 20: Мутации**

Каждая — с резервной копией файла и возвратом **из копии**.

1. Убрать `tracing::info!("писатель остановлен, база отпущена")` → падает `sigterm_stops_the_server_cleanly_and_stops_the_writer`.
2. Убрать вызов `started.store.shutdown()` целиком → падает тот же тест (строки нет).
3. `signal::wait_for_signal()` заменить на `std::future::ready("SIGTERM")` → падают `serve_comes_up_and_answers` и `no_subcommand_means_serve`: сервер уходит, не начав слушать.
4. `GRACE` = `Duration::from_millis(0)` → ни один тест не падает. Это ожидаемо и должно быть записано в отчёт как парковка: значение предела наблюдаемого следствия в тестах не имеет, потому что ни один запрос в наборе не висит.

- [ ] **Step 21: Прогон всего набора и коммит**

Run: `cargo test --workspace 2>&1 | grep -E "^(test result|warning|error)"`
Expected: все наборы `ok`, ни одного предупреждения.

```bash
git add -A
git commit -m "feat(cli): штатное завершение по SIGTERM с остановом писателя"
```

---

### Task 2: Одна функция решения о доступе к настройке и `token_required` в статусе

**Files:**
- Modify: `crates/wakode-api/src/setup.rs`
- Test: `crates/wakode-api/tests/api.rs`

**Interfaces:**
- Produces: `fn address_allows_setup(setup_from_any_address: bool, peer: &SocketAddr, headers: &HeaderMap) -> Result<(), ApiError>` (приватная); поле `SetupStatus::token_required: bool`
- Consumes: константы `SETUP_IS_LOCAL_ONLY`, `SETUP_THROUGH_A_PROXY`, `PROXY_HEADERS` из того же файла

**Что меняется в поведении:** `POST /api/setup` — ничего. `GET /api/setup/status` начинает принимать `ConnectInfo` и заголовки и отдавать второе поле. Задача 3 добавит поверх этого ветку токена.

**Зачем.** Экран настройки (план 4) обязан знать, спрашивать ли токен, **до** того как отправит форму. Если он будет решать это сам — по своей копии правил, — копии разъедутся: поле токена появится там, где оно не нужно, или пропадёт там, где без него откажут. Одна функция на два читателя убирает вторую копию из вопроса.

- [ ] **Step 1: Написать падающие тесты статуса**

В `crates/wakode-api/tests/api.rs`. Форма подстановки адреса — та же, что в существующих тестах настройки (`ConnectInfo` кладётся расширением запроса); повторить её один в один.

```rust
#[tokio::test]
async fn the_status_asks_for_a_token_when_the_address_alone_would_be_refused() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/setup/status")
                .extension(ConnectInfo("203.0.113.5:41234".parse::<SocketAddr>().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status: serde_json::Value = json_body(response).await;
    assert_eq!(status["needed"], true);
    assert_eq!(
        status["token_required"], true,
        "чужому адресу настройка без токена не откроется, и статус обязан это сказать"
    );
}

#[tokio::test]
async fn a_loopback_client_without_proxy_headers_needs_no_token() {
    // Зеркало предыдущего. Без него «всегда true» прошло бы: экран
    // настройки на машине владельца спрашивал бы токен, которого он не
    // должен предъявлять.
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/setup/status")
                .extension(ConnectInfo("127.0.0.1:41234".parse::<SocketAddr>().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status: serde_json::Value = json_body(response).await;
    assert_eq!(status["token_required"], false);
}

#[tokio::test]
async fn a_proxy_header_makes_the_status_ask_for_a_token() {
    // Тот самый случай, ради которого всё это: пир петлевой, потому что
    // прокси стоит на том же хосте, а клиент — кто угодно.
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/setup/status")
                .header("x-forwarded-for", "203.0.113.5")
                .extension(ConnectInfo("127.0.0.1:41234".parse::<SocketAddr>().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status: serde_json::Value = json_body(response).await;
    assert_eq!(status["token_required"], true);
}

#[tokio::test]
async fn an_instance_open_to_any_address_never_asks_for_a_token() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(
        a_store(&dir),
        None,
        AppSettings { setup_from_any_address: true, ..a_settings() },
    );
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/setup/status")
                .header("x-forwarded-for", "203.0.113.5")
                .extension(ConnectInfo("203.0.113.5:41234".parse::<SocketAddr>().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status: serde_json::Value = json_body(response).await;
    assert_eq!(status["token_required"], false);
}
```

`json_body` — вспомогательная функция чтения тела как `serde_json::Value`; если её в файле нет под этим именем, использовать ту, что там уже применяется в тестах настройки.

- [ ] **Step 2: Прогнать — падают**

Run: `cargo test -p wakode-api --test api token_required`
Expected: FAIL — поля `token_required` в ответе нет (`Value::Null` вместо булева).

Отдельно проверить: существующие тесты статуса **тоже** упадут, как только `status` начнёт требовать `ConnectInfo` — их придётся дополнить расширением. Это ожидаемо, и это часть задачи.

- [ ] **Step 3: Вынести решение в одну функцию**

`crates/wakode-api/src/setup.rs`. Заменить `parts_have` (обёртка в одну строку, у которой не осталось второго вызывающего) и вырезанную из `setup` адресную часть на:

```rust
/// Разрешает ли **адрес клиента** выполнить первичную настройку.
///
/// Одна функция на двух читателей: `setup` по ней отказывает, `status` по
/// ней же сообщает экрану настройки, нужен ли токен. Разъехавшись, они
/// соврали бы экрану — он спрятал бы поле токена там, где без токена
/// откажут, или показал бы там, где он не нужен.
///
/// Журнала здесь нет намеренно: `status` дёргают на каждое открытие
/// страницы, и предупреждение на каждый опрос превратило бы журнал в
/// шум. Пишет тот, кто отказывает, — то есть `setup`.
fn address_allows_setup(
    setup_from_any_address: bool,
    peer: &SocketAddr,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    if setup_from_any_address {
        return Ok(());
    }

    // Смотрим на адрес клиента, а не на адрес прослушивания: проверка по
    // `listen` открыла бы настройку всему интернету на любом инстансе,
    // слушающем `0.0.0.0`.
    if !peer.ip().is_loopback() {
        return Err(ApiError::Forbidden(SETUP_IS_LOCAL_ONLY));
    }

    // Но одного петлевого пира мало, и это не педантизм. При штатной
    // установке — nginx на том же хосте, `proxy_pass
    // http://127.0.0.1:9000` — TCP-пиром всегда оказывается сам прокси,
    // то есть `127.0.0.1`, и проверка выше истинна для кого угодно из
    // интернета.
    //
    // Заголовок используется **по факту наличия, а не по содержимому**.
    // Доверять написанному нельзя: его подделает кто угодно. А вот
    // присутствие — надёжное свидетельство, что запрос прошёл через
    // посредника и адрес пира о клиенте не говорит ничего. Отказ в эту
    // сторону безопасен: подделав заголовок, чужой добьётся только
    // собственного отказа.
    //
    // Чего это по-прежнему не закрывает: голый `proxy_pass
    // http://127.0.0.1:9000;` без единого `proxy_set_header` не добавляет
    // ни одного из перечисленных заголовков. Для такой установки есть
    // токен настройки (см. `setup`), и он не зависит от заголовков вовсе.
    if let Some(header) = PROXY_HEADERS.iter().find(|name| headers.contains_key(**name)) {
        // Имя заголовка нужно вызывающему для журнала, но `ApiError`
        // несёт только текст для клиента, а он одинаков для всех шести:
        // называть чужому, какой именно заголовок его выдал, незачем.
        let _ = header;
        return Err(ApiError::Forbidden(SETUP_THROUGH_A_PROXY));
    }

    Ok(())
}
```

Журнальная строка про посредника переезжает в `setup` — там, где решение приводит к отказу:

```rust
    if let Err(err) = address_allows_setup(state.setup_from_any_address, &peer, &headers) {
        if let ApiError::Forbidden(reason) = &err {
            tracing::warn!(reason, "первичная настройка отклонена");
        }
        return Err(err);
    }
```

- [ ] **Step 4: Добавить поле в статус**

```rust
#[derive(Serialize)]
pub struct SetupStatus {
    /// Нужна ли первичная настройка. Становится `false` навсегда после
    /// появления первого пользователя.
    pub needed: bool,
    /// Потребуется ли **этому** клиенту токен настройки.
    ///
    /// Считается той же функцией, по которой `POST /api/setup` откажет,
    /// — иначе экран настройки решал бы по своей копии правил, а копии
    /// разъезжаются.
    pub token_required: bool,
}

/// Нужна ли первичная настройка.
///
/// Отвечает всем без разбора адреса: экран настройки — первое, что
/// открывает браузер, и до создания администратора предъявлять нечего.
/// Единственный факт, который отсюда узнаёт чужой, — «инстанс уже занят»
/// и «мне понадобится токен»; заводить на них секретность бессмысленно,
/// потому что оба видны по любому отказу `POST /api/setup`.
pub async fn status(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<SetupStatus>, ApiError> {
    Ok(Json(SetupStatus {
        needed: state.store.user_count().await? == 0,
        token_required: address_allows_setup(state.setup_from_any_address, &peer, &headers).is_err(),
    }))
}
```

Дополнить существующие тесты статуса расширением `ConnectInfo` — без него экстрактор отвечает `500`.

- [ ] **Step 5: Прогнать**

Run: `cargo test -p wakode-api`
Expected: PASS — и новые тесты, и все прежние тесты настройки (поведение `POST /api/setup` не менялось).

- [ ] **Step 6: Мутации**

С копией файла и возвратом **из копии**:

1. `token_required: false` константой → падают два из четырёх новых тестов.
2. `token_required: true` константой → падают другие два.
3. В `address_allows_setup` убрать проверку `PROXY_HEADERS` → падает `a_proxy_header_makes_the_status_ask_for_a_token` **и** существующий `a_loopback_peer_is_not_enough_when_the_request_came_through_a_proxy`.
4. Убрать ранний возврат по `setup_from_any_address` → падает `an_instance_open_to_any_address_never_asks_for_a_token`.

- [ ] **Step 7: Коммит**

```bash
git add crates/wakode-api/src/setup.rs crates/wakode-api/tests/api.rs
git commit -m "feat(api): статус настройки сообщает, нужен ли токен"
```

---

### Task 3: Одноразовый токен первичной настройки

**Files:**
- Create: `crates/wakode-auth/src/setup_token.rs`
- Modify: `crates/wakode-auth/src/lib.rs` (объявление модуля, реэкспорт, абзац политики секретов)
- Modify: `crates/wakode-auth/Cargo.toml` (+`subtle`)
- Modify: `Cargo.toml` (`subtle = "2.6"` в `[workspace.dependencies]`)
- Modify: `crates/wakode-api/src/state.rs`
- Modify: `crates/wakode-api/src/setup.rs`
- Modify: `crates/wakode/src/main.rs`
- Test: `crates/wakode-auth/src/setup_token.rs` (модуль `tests`), `crates/wakode-api/tests/api.rs`, `crates/wakode/tests/cli.rs`
- Docs: `docs/superpowers/specs/2026-08-17-wakode-server-foundation-design.md` §6

**Interfaces:**
- Produces:
  - `wakode_auth::SetupToken` — `generate()`, `Display`, `matches(&self, presented: &str) -> bool`, `Clone`, ручной `Debug`
  - `wakode_auth::SETUP_TOKEN_BYTES: usize` = 32
  - `wakode_api::AppState::with_setup_token(self, token: Option<SetupToken>) -> Self`, поле `AppState::setup_token: Option<SetupToken>`
  - `wakode_api::setup::SETUP_TOKEN_HEADER: &str` = `"x-wakode-setup-token"`
- Consumes: `address_allows_setup` из задачи 2

**Проверка `subtle` перед началом:** крейт уже присутствует в `Cargo.lock` (версия 2.6.1, пришёл транзитивно). Добавляется как прямая зависимость `wakode-auth`, новой сборки не тянет.

- [ ] **Step 1: Написать падающие юнит-тесты токена**

Создать `crates/wakode-auth/src/setup_token.rs`, сначала только с модулем тестов:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_matches_its_own_printed_form() {
        let token = SetupToken::generate();
        assert!(token.matches(&token.to_string()));
    }

    #[test]
    fn a_different_token_does_not_match() {
        // Без этого «matches всегда true» прошло бы предыдущий тест.
        let token = SetupToken::generate();
        let other = SetupToken::generate();
        assert!(!token.matches(&other.to_string()));
    }

    #[test]
    fn two_generated_tokens_differ() {
        // Ловит генератор, возвращающий одно и то же (нули, константу):
        // такой токен знал бы кто угодно, читавший исходники.
        let printed: std::collections::HashSet<String> =
            (0..16).map(|_| SetupToken::generate().to_string()).collect();
        assert_eq!(printed.len(), 16);
    }

    #[test]
    fn the_printed_form_is_url_safe_base64_of_the_whole_token() {
        // Алфавит пришпилен, потому что токен вставляют руками и возят
        // через заголовок: `+` и `/` из стандартного алфавита делают его
        // ломким там, где его когда-нибудь положат в query. Урок задачи 4
        // плана 3a: на нулевых байтах разница алфавитов не проявляется, и
        // мутация проходит зелёной, — поэтому смотрим на случайные.
        for _ in 0..64 {
            let printed = SetupToken::generate().to_string();
            assert_eq!(printed.len(), 43, "не 32 байта в base64 без набивки: {printed}");
            assert!(
                !printed.contains(['+', '/', '=']),
                "алфавит не URL-safe: {printed}"
            );
        }
    }

    #[test]
    fn debug_never_prints_the_token() {
        // Сверка с полной формой, а не поиск подстроки: производный
        // `Debug` на `[u8; 32]` печатает байты десятичными числами, и
        // поиск base64-формы в таком выводе зелен на утёкшем секрете.
        let token = SetupToken::generate();
        assert_eq!(format!("{token:?}"), "SetupToken(\"<скрыт>\")");
    }

    #[test]
    fn junk_does_not_match() {
        let token = SetupToken::generate();
        let printed = token.to_string();

        assert!(!token.matches(""), "пустая строка не токен");
        assert!(!token.matches("не base64!"), "мусор не токен");
        assert!(
            !token.matches(&printed[..printed.len() - 1]),
            "обрезанный токен не токен"
        );
        assert!(
            !token.matches(&format!("{printed}A")),
            "дописанный токен не токен"
        );
    }

    #[test]
    fn whitespace_around_a_pasted_token_is_forgiven() {
        // Токен берут из журнала мышью, и хвостовой перевод строки
        // приезжает вместе с ним. Отказ по невидимому символу владелец
        // отличить от неверного токена не сможет никак.
        let token = SetupToken::generate();
        assert!(token.matches(&format!("  {}\n", token)));
    }
}
```

- [ ] **Step 2: Прогнать — не собирается**

Run: `cargo test -p wakode-auth setup_token`
Expected: ошибка компиляции — `SetupToken` не существует.

- [ ] **Step 3: Реализовать `SetupToken`**

Над модулем тестов:

```rust
//! Одноразовый токен первичной настройки.

use base64::Engine as _;
use rand::RngCore as _;
use subtle::ConstantTimeEq as _;

/// Длина токена в байтах.
pub const SETUP_TOKEN_BYTES: usize = 32;

/// Кодировка печатной формы.
///
/// URL-safe без набивки: токен вставляют руками, возят заголовком и рано
/// или поздно положат в адресную строку. `+` и `/` из стандартного
/// алфавита там ломаются, `=` мешает всему сразу.
const ENCODING: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Одноразовый токен первичной настройки.
///
/// Живёт только в памяти процесса и только пока в базе нет ни одного
/// пользователя. Перезапуск выдаёт новый, прежний перестаёт работать — и
/// это не недосмотр: окно первичной настройки закрывается навсегда после
/// первого администратора, а секрет, переживающий своё окно, — секрет без
/// назначения.
///
/// **Печатается в журнал при старте, и это единственное место в проекте,
/// где секрет пишется в лог намеренно.** Смысл: владельцу, поставившему
/// сервер за обратным прокси, взять токен больше неоткуда, а журнал
/// доступен тому, у кого доступ к машине уже есть. Альтернатива —
/// `setup_from_any_address = true`, то есть открыть настройку всему
/// интернету на всё время до создания администратора.
///
/// Отсюда политика типа, та же, что у остальных секретов крейта:
/// `Display` печатает значение дословно, `Debug` — никогда.
#[derive(Clone)]
pub struct SetupToken([u8; SETUP_TOKEN_BYTES]);

impl SetupToken {
    /// Новый случайный токен.
    pub fn generate() -> Self {
        let mut bytes = [0u8; SETUP_TOKEN_BYTES];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    /// Тот ли это токен, что предъявили.
    ///
    /// Сравнение — за постоянное время и по байтам, а не по строке.
    /// Причина не в педантизме: это единственный секрет проекта, который
    /// лежит открытым в журнале и вводится руками, то есть единственный,
    /// который осмысленно подбирать. Побайтовое сравнение строк выдало бы
    /// длину совпавшего префикса временем ответа.
    ///
    /// Пробелы по краям срезаются: токен копируют из журнала, и хвостовой
    /// перевод строки приезжает вместе с ним. Отказ по невидимому символу
    /// владелец не отличит от неверного токена ничем.
    pub fn matches(&self, presented: &str) -> bool {
        let Ok(bytes) = ENCODING.decode(presented.trim()) else {
            return false;
        };
        if bytes.len() != SETUP_TOKEN_BYTES {
            return false;
        }
        self.0.ct_eq(&bytes[..]).into()
    }
}

impl std::fmt::Display for SetupToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&ENCODING.encode(self.0))
    }
}

impl std::fmt::Debug for SetupToken {
    /// Ручной, а не производный: производный на `[u8; 32]` печатает байты
    /// десятичными числами — секрет наружу, только не в той записи, в
    /// которой его будут искать глазами.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SetupToken").field(&"<скрыт>").finish()
    }
}
```

В `crates/wakode-auth/src/lib.rs`: `mod setup_token;` и `pub use setup_token::{SetupToken, SETUP_TOKEN_BYTES};` рядом с остальными реэкспортами. В абзац о политике секретов дописать `SetupToken` с оговоркой, что его `Display` печатают в журнал намеренно.

В `crates/wakode-auth/Cargo.toml`: `subtle.workspace = true`. В корневой `Cargo.toml`, в `[workspace.dependencies]`:

```toml
# Сравнение токена настройки за постоянное время. Единственный секрет
# проекта, который лежит открытым в журнале и вводится руками, — то есть
# единственный, который осмысленно подбирать.
subtle = "2.6"
```

- [ ] **Step 4: Прогнать**

Run: `cargo test -p wakode-auth`
Expected: PASS, семь новых тестов.

- [ ] **Step 5: Мутации токена**

С копией и возвратом **из копии**:

1. `ENCODING` → `base64::engine::general_purpose::STANDARD` → падает `the_printed_form_is_url_safe_base64_of_the_whole_token`.
2. `matches` без проверки длины → падает `junk_does_not_match` (обрезанный токен).
3. `matches` → `true` константой → падает `a_different_token_does_not_match`.
4. `generate` → `Self([0u8; SETUP_TOKEN_BYTES])` → падает `two_generated_tokens_differ`.
5. Ручной `Debug` заменить на производный (`#[derive(Debug)]`) → падает `debug_never_prints_the_token`.
6. Убрать `.trim()` → падает `whitespace_around_a_pasted_token_is_forgiven`.

- [ ] **Step 6: Коммит**

```bash
git add Cargo.toml Cargo.lock crates/wakode-auth
git commit -m "feat(auth): одноразовый токен первичной настройки"
```

- [ ] **Step 7: Написать падающие тесты HTTP-ветки**

В `crates/wakode-api/tests/api.rs`:

```rust
#[tokio::test]
async fn a_correct_token_opens_setup_from_any_address() {
    // Ради этого всё и делается: владелец за обратным прокси заводит
    // администратора, не открывая настройку всему интернету.
    let dir = tempfile::tempdir().unwrap();
    let token = wakode_auth::SetupToken::generate();
    let state = a_state(&dir).with_setup_token(Some(token.clone()));

    let response = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .header("x-wakode-setup-token", token.to_string())
                .header("x-forwarded-for", "203.0.113.5")
                .extension(ConnectInfo("127.0.0.1:41234".parse::<SocketAddr>().unwrap()))
                .body(Body::from(
                    r#"{"login":"админ","password":"достаточнодлинный","timezone":"Europe/Moscow"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn a_wrong_token_is_refused_even_from_a_loopback_address() {
    // Предъявление токена — утверждение «я знаю секрет», и ложное
    // утверждение получает свой отказ. Провалиться в адресную ветку и
    // пройти по петлевому адресу оно не должно: владелец, вставивший
    // токен с опечаткой, иначе не узнал бы об опечатке вовсе, а на
    // следующей машине услышал бы про адрес, держа токен в руках.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir).with_setup_token(Some(wakode_auth::SetupToken::generate()));

    let response = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .header("x-wakode-setup-token", "SGVsbG8sIHRoaXMgaXMgbm90IHRoZSB0b2tlbg")
                .extension(ConnectInfo("127.0.0.1:41234".parse::<SocketAddr>().unwrap()))
                .body(Body::from(
                    r#"{"login":"админ","password":"достаточнодлинный","timezone":"Europe/Moscow"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = json_body(response).await;
    let text = body["error"].as_str().unwrap();
    assert!(text.contains("токен"), "отказ не назвал причину: {text}");
}

#[tokio::test]
async fn a_token_presented_to_an_instance_that_issued_none_is_refused() {
    // Инстанс с уже заведённым администратором токена не выдаёт, и
    // «токена нет» обязано означать отказ, а не «сравнивать не с чем,
    // значит проходи».
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir); // без with_setup_token

    let response = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .header(
                    "x-wakode-setup-token",
                    wakode_auth::SetupToken::generate().to_string(),
                )
                .extension(ConnectInfo("203.0.113.5:41234".parse::<SocketAddr>().unwrap()))
                .body(Body::from(
                    r#"{"login":"админ","password":"достаточнодлинный","timezone":"Europe/Moscow"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn two_setup_token_headers_are_refused() {
    // Урок парковки задачи 11: `CookieJar::get` при дубликатах отдаёт
    // последнюю пару, и «какое из двух значений считается предъявленным»
    // — источник тихих расхождений. Здесь ответ дан явно: два токена —
    // это не предъявление, а попытка угадать, какой из них мы возьмём.
    let dir = tempfile::tempdir().unwrap();
    let token = wakode_auth::SetupToken::generate();
    let state = a_state(&dir).with_setup_token(Some(token.clone()));

    let response = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .header("x-wakode-setup-token", "мусор")
                .header("x-wakode-setup-token", token.to_string())
                .extension(ConnectInfo("203.0.113.5:41234".parse::<SocketAddr>().unwrap()))
                .body(Body::from(
                    r#"{"login":"админ","password":"достаточнодлинный","timezone":"Europe/Moscow"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_refusal_never_echoes_the_presented_token() {
    // Тело отказа уезжает клиенту и попадает в чужие скриншоты. Эхо
    // предъявленного значения — тот же класс дефекта, что подстановка
    // пароля в сообщение о таймзоне, найденная в задаче 12.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir).with_setup_token(Some(wakode_auth::SetupToken::generate()));
    let presented = "SGVsbG8sIHRoaXMgaXMgbm90IHRoZSB0b2tlbg";

    let response = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .header("x-wakode-setup-token", presented)
                .extension(ConnectInfo("127.0.0.1:41234".parse::<SocketAddr>().unwrap()))
                .body(Body::from(
                    r#"{"login":"админ","password":"достаточнодлинный","timezone":"Europe/Moscow"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let dump = format!("{:?}", json_body(response).await);
    assert!(!dump.contains(presented), "предъявленный токен вернулся клиенту: {dump}");
}

#[tokio::test]
async fn without_a_token_the_address_still_decides() {
    // Зеркало всей ветки: токен не должен был отменить прежнюю защиту.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir).with_setup_token(Some(wakode_auth::SetupToken::generate()));

    let response = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .extension(ConnectInfo("203.0.113.5:41234".parse::<SocketAddr>().unwrap()))
                .body(Body::from(
                    r#"{"login":"админ","password":"достаточнодлинный","timezone":"Europe/Moscow"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = json_body(response).await;
    assert!(body["error"].as_str().unwrap().contains("локального адреса"));
}
```

Дополнить существующий тест `Debug` состояния: `AppState` с выданным токеном не печатает его значение, а сам состав полей по-прежнему сверяется целиком.

- [ ] **Step 8: Прогнать — падают**

Run: `cargo test -p wakode-api --test api token`
Expected: ошибка компиляции (`with_setup_token` нет).

- [ ] **Step 9: Поле состояния**

`crates/wakode-api/src/state.rs`:

```rust
pub struct AppState {
    pub store: SqliteStore,
    pub master_key: Option<MasterKey>,
    pub registration: bool,
    pub session_ttl_days: i64,
    pub setup_from_any_address: bool,
    pub default_timeout_secs: i64,
    /// Токен первичной настройки — только пока администратора нет.
    ///
    /// `None` — закрытая сторона: инстанс без токена настройку по токену
    /// не пускает вовсе. Поэтому поле и не входит в `AppSettings`:
    /// умолчание должно получаться само, а не задаваться каждым
    /// вызывающим.
    pub setup_token: Option<SetupToken>,
}
```

Строитель рядом с `new`:

```rust
impl AppState {
    /// Выдать состоянию токен первичной настройки.
    ///
    /// Отдельным методом, а не четвёртым аргументом `new`: токен есть
    /// ровно у одного вызывающего из дюжины — у `serve` в бинаре, — и
    /// умолчание `None` закрытое. Четвёртый аргумент заставил бы одиннадцать
    /// вызовов писать `None` и ничего бы этим не доказал.
    #[must_use]
    pub fn with_setup_token(mut self, token: Option<SetupToken>) -> Self {
        self.setup_token = token;
        self
    }
}
```

В `new` — `setup_token: None`. В ручном `Debug` — `.field("setup_token", &self.setup_token.is_some())`, рядом с `master_key`.

- [ ] **Step 10: Ветка токена в `setup`**

`crates/wakode-api/src/setup.rs`:

```rust
/// Заголовок, которым предъявляют токен первичной настройки.
///
/// Заголовок, а не поле тела: тело разбирается **после** проверки
/// доступа (задача 12 плана 3a, тест `the_address_is_checked_before_the_database`),
/// и токен в теле заставил бы разбирать тело до решения о доступе — то
/// есть рассказывать чужому про формат JSON раньше, чем ему отказали.
pub const SETUP_TOKEN_HEADER: &str = "x-wakode-setup-token";

const SETUP_TOKEN_WRONG: &str = "токен первичной настройки не подходит; \
     возьмите его из журнала сервера — он печатается при старте, пока \
     администратора нет, и меняется при каждом перезапуске";

/// Предъявленный токен, если он вообще предъявлен.
///
/// Нечитаемое значение — это всё равно предъявление: вернуть здесь `None`
/// значило бы, что мусорный заголовок стирает сам факт попытки и запрос
/// уходит в адресную ветку. Отдаём заведомо не подходящую строку, чтобы
/// решение принимал `matches`, а не разбор.
///
/// Два заголовка — тоже отказ. Урок парковки задачи 11: при дубликатах
/// «какое значение считается предъявленным» — вопрос, на который у
/// клиента и сервера легко оказываются разные ответы.
fn presented_token(headers: &HeaderMap) -> Option<String> {
    let mut values = headers.get_all(SETUP_TOKEN_HEADER).iter();
    let first = values.next()?;
    if values.next().is_some() {
        return Some(String::new());
    }
    Some(first.to_str().unwrap_or_default().to_owned())
}
```

В теле `setup`, вместо адресного блока задачи 2:

```rust
    match presented_token(&headers) {
        Some(presented) => {
            let opens = state
                .setup_token
                .as_ref()
                .is_some_and(|expected| expected.matches(&presented));
            if !opens {
                // Значение не пишется — ни в журнал, ни в ответ. В
                // журнале оно было бы подсказкой подбирающему о том, что
                // до сервера дошло; в ответе — эхом клиенту самому себе.
                tracing::warn!("предъявлен неверный токен первичной настройки");
                return Err(ApiError::Forbidden(SETUP_TOKEN_WRONG));
            }
            // Токен подошёл — адрес больше ничего не решает. В этом и
            // весь смысл: владельцу за прокси иначе пришлось бы открыть
            // настройку всем через `setup_from_any_address`.
        }
        None => {
            if let Err(err) = address_allows_setup(state.setup_from_any_address, &peer, &headers) {
                if let ApiError::Forbidden(reason) = &err {
                    tracing::warn!(reason, "первичная настройка отклонена");
                }
                return Err(err);
            }
        }
    }
```

- [ ] **Step 11: Прогнать**

Run: `cargo test -p wakode-api`
Expected: PASS.

- [ ] **Step 12: Мутации HTTP-ветки**

С копией и возвратом **из копии**:

1. Неверный токен проваливается в адресную ветку вместо отказа → падает `a_wrong_token_is_refused_even_from_a_loopback_address`.
2. `state.setup_token.as_ref().is_none()` считается успехом (`is_none_or` вместо `is_some_and`) → падает `a_token_presented_to_an_instance_that_issued_none_is_refused`.
3. `presented_token` использует `headers.get` вместо `get_all` → падает `two_setup_token_headers_are_refused`.
4. Ветка токена ставится **после** адресной → падает `a_correct_token_opens_setup_from_any_address`.
5. `SETUP_TOKEN_WRONG` дополняется предъявленным значением → падает `the_refusal_never_echoes_the_presented_token`.
6. Убрать ветку токена целиком → падает `a_correct_token_opens_setup_from_any_address`, но **не** падает `without_a_token_the_address_still_decides` — так и должно быть, зеркало сторожит обратную сторону.

- [ ] **Step 13: Коммит**

```bash
git add crates/wakode-api
git commit -m "feat(api): первичная настройка по одноразовому токену"
```

- [ ] **Step 14: Написать падающий сквозной тест проводки**

`crates/wakode/tests/cli.rs`. Это единственная проверка того, что напечатанный токен — тот самый, который сервер принимает. Ни один тест в `wakode-api` этого не доказывает: там токен кладут в состояние руками.

```rust
#[test]
fn the_setup_token_from_the_log_opens_setup_through_a_proxy() {
    // Сквозная проводка: токен из журнала — тот самый, который принимает
    // сервер, и он снимает ровно тот отказ, который иначе получает
    // запрос с прокси-заголовком. Мутация «в состояние уходит None»
    // роняет этот тест и не роняет ни одного теста в wakode-api.
    let serving = a_serving_child(&["serve"]);

    let token = wait_for_setup_token(&serving);

    // Сначала — что отказ вообще есть. Без этой половины 201 ниже
    // ничего не доказывал бы: он получился бы и без токена.
    let refused = raw_setup(serving.addr, Some("203.0.113.5"), None);
    assert!(
        refused.starts_with("HTTP/1.1 403"),
        "запрос через посредника без токена обязан быть отвергнут: {refused}"
    );

    let created = raw_setup(serving.addr, Some("203.0.113.5"), Some(&token));
    assert!(
        created.starts_with("HTTP/1.1 201"),
        "токен из журнала не открыл настройку: {created}\nжурнал:\n{}",
        serving.log()
    );
}

/// Дождаться строки с токеном в журнале и вернуть его значение.
fn wait_for_setup_token(serving: &Serving) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let log = serving.log();
        if let Some(token) = log
            .split_whitespace()
            .find_map(|field| field.strip_prefix("token="))
        {
            return token.to_owned();
        }
        if Instant::now() >= deadline {
            panic!("токен настройки не появился в журнале:\n{log}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Сырой `POST /api/setup` через настоящий сокет.
fn raw_setup(
    addr: std::net::SocketAddr,
    forwarded_for: Option<&str>,
    token: Option<&str>,
) -> String {
    let body = br#"{"login":"admin","password":"достаточнодлинный","timezone":"Europe/Moscow"}"#;

    let mut request = format!(
        "POST /api/setup HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(value) = forwarded_for {
        request.push_str(&format!("X-Forwarded-For: {value}\r\n"));
    }
    if let Some(value) = token {
        request.push_str(&format!("X-Wakode-Setup-Token: {value}\r\n"));
    }
    request.push_str("\r\n");

    let mut stream =
        std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(body).unwrap();

    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}
```

- [ ] **Step 15: Прогнать — падает**

Run: `cargo test -p wakode --test cli setup_token -- --nocapture`
Expected: FAIL — токен в журнал не пишется, `wait_for_setup_token` падает по сроку.

- [ ] **Step 16: Провести токен в бинаре**

`crates/wakode/src/main.rs`, в `serve`, после строки «сервер поднят» и до сборки состояния:

```rust
    // Токен заводится, только пока настройка не выполнена: после первого
    // пользователя эндпоинт закрыт навсегда, и печатать секрет в журнал
    // на каждый перезапуск было бы раздачей секрета без назначения.
    //
    // `?` здесь безопасен: `serve` вызывается из `dispatch`, чей результат
    // уходит в `outcome`, а `outcome` возвращается уже после останова
    // писателя. Мимо `shutdown` управление не уходит.
    let setup_token = if started.store.user_count().await? == 0 {
        let token = wakode_auth::SetupToken::generate();
        // Единственное место в проекте, где секрет пишется в журнал
        // намеренно. Обоснование — в докстринге `SetupToken`.
        tracing::info!(
            token = %token,
            header = wakode_api::setup::SETUP_TOKEN_HEADER,
            "администратора ещё нет: первичная настройка открыта по этому токену"
        );
        Some(token)
    } else {
        None
    };

    let state = AppState::new(
        started.store.clone(),
        started.master_key.clone(),
        app_settings(&started.config),
    )
    .with_setup_token(setup_token);
```

Понадобится `use wakode_store::UserRepo;` в области видимости `main.rs`.

- [ ] **Step 17: Прогнать**

Run: `cargo test -p wakode`
Expected: PASS.

- [ ] **Step 18: Мутации проводки**

С копией и возвратом **из копии**:

1. `.with_setup_token(setup_token)` убрать → падает `the_setup_token_from_the_log_opens_setup_through_a_proxy`, и падает только он.
2. `.with_setup_token(Some(SetupToken::generate()))` (свежий, не тот, что напечатан) → падает тот же тест: доказано, что принимается **напечатанный** токен, а не какой-нибудь.
3. Условие `user_count() == 0` снять (токен всегда) → ни один тест не падает. Записать парковкой: сегодня наблюдаемого следствия нет, потому что тест поднимает сервер только на пустой базе. Закрывается вместе с формой входа в 3b, где появится инстанс с пользователем.

- [ ] **Step 19: Поправить спеку**

`docs/superpowers/specs/2026-08-17-wakode-server-foundation-design.md`, §6, последний абзац главы про то, чего проверка заголовков не закрывает. Заменить предложение «Настоящее решение — белый список доверенных прокси или одноразовый токен настройки, печатаемый в журнал при первом старте; и то, и другое — план 3b, и это блокер для развёртывания за прокси» на текст, говорящий по факту:

- одноразовый токен реализован планом `2026-08-18-wakode-deployment-hardening`, заголовок `X-Wakode-Setup-Token`, значение печатается в журнал при старте, пока в базе нет пользователей;
- блокер развёртывания за прокси этим закрыт: голый `proxy_pass` без `proxy_set_header` больше не требует ни `setup_from_any_address = true`, ни CLI;
- белый список доверенных посредников по-прежнему отсутствует, но он и не про настройку: он нужен, чтобы **знать** адрес клиента (журнал, ограничение частоты запросов), и остаётся открытым вопросом с собственным потребителем.

Тем же абзацем упомянуть, что `GET /api/setup/status` отдаёт `token_required` — экран настройки плана 4 читает его, а не решает сам.

- [ ] **Step 20: Прогон всего набора и коммит**

Run: `cargo test --workspace 2>&1 | grep -E "^(test result|warning|error)"`
Expected: все наборы `ok`, ни одного предупреждения.

```bash
git add -A
git commit -m "feat(cli): токен первичной настройки печатается при старте"
```

---

## Самопроверка плана

**Покрытие долгов.** Блокер «graceful shutdown на SIGTERM» — задача 1. Блокер «белый список или токен настройки» — задачи 2 и 3. Парковка «инвариант `shutdown` зовётся всегда не держится ничем» — задача 1, шаги 15 и 18. Парковка «переход `serve → app_settings` не покрыт» — закрывается **частично**: задача 3 доказывает переход `serve → AppState` для токена, но не для флагов из `AppSettings`; это надо записать в отчёт, а не выдать за закрытое.

**Что этот план сознательно не делает:**
- не заводит белый список доверенных посредников — у него другой потребитель (журнал и ограничение частоты), и без потребителя его интерфейс пришлось бы угадывать;
- не различает 413/415 и не вводит верхние границы длины логина и пароля — это отдельные парковки задачи 12, не блокеры;
- не трогает `touch_key_used`, гонку `user_count → create_user` и `CookieJar::get` при дубликатах — все три ждут 3b, где появляются их потребители;
- не ставит `TimeoutLayer` на обычные запросы: предел касается только завершения, и расширять его на штатную работу означало бы менять поведение приёма отметок ради задачи об остановке.

**Согласованность имён.** `SetupToken::matches` (не `verify`), `AppState::with_setup_token` (не `set_setup_token`), `SETUP_TOKEN_HEADER` в нижнем регистре, потому что `HeaderMap` сравнивает имена регистронезависимо, а `contains_key` со строкой в верхнем регистре читается как заявка на неверное поведение. `address_allows_setup` возвращает `Result<(), ApiError>`, а не `bool`: у отказа два разных текста, и `bool` их потерял бы.
