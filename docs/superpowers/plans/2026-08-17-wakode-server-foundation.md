# Фундамент сервера wakode (план 3a)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Поднять сервер wakode, который применяет миграции, принимает API-ключ и сессию, отвергает неверные с различимыми причинами, и CLI, который создаёт пользователя, выдаёт ключ и делает бэкап. Прикладных эндпоинтов нет ни одного — они в плане 3b.

**Architecture:** Три новых крейта. `wakode-auth` — вся криптография проекта чистыми функциями над байтами, без БД, HTTP и файлов. `wakode-api` — каркас axum: состояние, извлечение учётных данных, ошибки, перехват паники. `wakode` — бинарь: конфиг, шестишаговая последовательность старта, запуск, подкоманды CLI.

**Tech Stack:** Rust 2024, axum 0.8, tower 0.5, tower-http 0.7, axum-extra 0.12 (cookie), tokio 1, clap 4.6 (derive), toml 1.1, serde 1, tracing 0.1, argon2 0.5, chacha20poly1305 0.10, hmac 0.12, sha2 0.10, rand 0.8, base64 0.23, uuid 1 (v4 + v7).

## Global Constraints

Обязательны для каждой задачи; нарушение — возврат в правку.

- **Версии зависимостей взяты выше буквально.** Связка проверена на сборку целиком и когерентна: один `digest 0.10.7`, один `generic-array 0.14.7`, `password-hash 0.5.0`. Не бери `argon2 0.6.0-rc.8` — это релиз-кандидат, и он тянет вторую генерацию RustCrypto.
- **Вся криптография — только в `wakode-auth`.** Ни `wakode-api`, ни `wakode` не подключают argon2, chacha20poly1305, hmac и sha2 в свои `Cargo.toml`. Это проверяемое свойство: граница держится списком зависимостей, а не намерением.
- **В `wakode-store` криптографии по-прежнему нет.** `password_hash`, `key_encrypted`, `key_lookup`, `token_hash` там — непрозрачные байты. Правило выдержано двенадцатью задачами плана 2 и не отменяется.
- **Ни один публичный тип не печатает секрет производным `Debug`.** Урок финального ревью плана 2: `Debug` дампил весь словарь строк и `password_hash` дословно. Каждый тип, несущий ключ, пароль, токен или их хеш, получает ручной `Debug` с заглушкой, и это закрепляется тестом.
- **Мастер-ключ живёт только в `WAKODE_MASTER_KEY`.** В конфиг-файл он не пишется никогда, в лог не попадает никогда.
- Время — микросекунды UTC (`wakode_core::Micros`); PK — UUIDv7 как 16-байтовый `BLOB`. **Значение API-ключа — UUIDv4**, потому что плагины валидируют ключ как UUID с проверкой версии `[1-5]`.
- Никакого самодельного retry на `SQLITE_BUSY` — `busy_timeout` в `conn.rs` и есть этот retry.
- Схема волны 0 **не меняется**: миграция 1 закрыта планом 2. Новые функции хранилища — только чтение существующих таблиц.
- Каждая задача заканчивается коммитом. Сообщения на русском, **без каких-либо упоминаний ИИ-ассистентов** (`Co-Authored-By`, Claude, Anthropic, «Generated with»). Жёсткий запрет владельца репозитория.
- Ноль предупреждений компиляции, чистый вывод тестов.
- HTTP-тесты идут через `tower::ServiceExt::oneshot`, без поднятия сокета. Исключение — задача 7, где проверяется само поднятие.

## Файловая структура

```
crates/wakode-auth/src/
  lib.rs          — реэкспорты, общий REDACTED
  error.rs        — AuthError
  master_key.rs   — MasterKey: разбор из base64, генерация
  password.rs     — argon2id: хеш и проверка
  api_key.rs      — ApiKeyValue: генерация, шифрование, отпечаток
  session.rs      — SessionToken: генерация, хеш

crates/wakode-api/src/
  lib.rs          — сборка Router
  state.rs        — AppState
  error.rs        — ApiError → IntoResponse
  auth/mod.rs     — реэкспорты экстракторов
  auth/api_key.rs — экстрактор ключа
  auth/session.rs — экстрактор сессии
  health.rs       — /healthz
  setup.rs        — первичная настройка и защита петлевым адресом
  compat/mod.rs   — пусто, наполняет 3b
  internal/mod.rs — пусто, наполняет 3b

crates/wakode/src/
  main.rs         — разбор аргументов, диспетчер подкоманд
  config.rs       — Config: TOML + перекрытие из окружения
  startup.rs      — шесть шагов старта и их отказы
  cli/mod.rs      — объявление подкоманд
  cli/user.rs     — user create, user list
  cli/key.rs      — key issue, key revoke
  cli/backup.rs   — backup
```

---

### Task 1: Крейт `wakode-auth` и мастер-ключ

**Files:**
- Create: `crates/wakode-auth/Cargo.toml`
- Create: `crates/wakode-auth/src/lib.rs`
- Create: `crates/wakode-auth/src/error.rs`
- Create: `crates/wakode-auth/src/master_key.rs`
- Modify: `Cargo.toml` (корень: члены workspace и версии зависимостей)

**Interfaces:**
- Produces: `MasterKey`, `MasterKey::generate() -> MasterKey`, `MasterKey::from_base64(&str) -> Result<MasterKey, AuthError>`, `MasterKey::to_base64(&self) -> String`, `MasterKey::as_bytes(&self) -> &[u8; 32]`; `AuthError`; `AuthResult<T>`.

**Почему ключ — отдельный тип, а не `[u8; 32]`.** Голый массив теряется среди других массивов, печатается производным `Debug` и подставляется куда угодно. Именованный тип с ручным `Debug` делает утечку в лог невозможной по построению, а не по внимательности.

- [ ] **Step 1: Завести крейт и зависимости**

В корневой `Cargo.toml` добавь `"crates/wakode-auth"` в `members` и в `[workspace.dependencies]`:

```toml
argon2 = "0.5"
chacha20poly1305 = "0.10"
hmac = "0.12"
sha2 = "0.10"
rand = "0.8"
base64 = "0.23"
```

`crates/wakode-auth/Cargo.toml`:

```toml
[package]
name = "wakode-auth"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
argon2.workspace = true
base64.workspace = true
chacha20poly1305.workspace = true
hmac.workspace = true
rand.workspace = true
sha2.workspace = true
thiserror.workspace = true
uuid = { workspace = true, features = ["v4"] }
```

- [ ] **Step 2: Написать падающие тесты**

`crates/wakode-auth/src/master_key.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_key_survives_the_base64_round_trip() {
        let key = MasterKey::generate();
        let restored = MasterKey::from_base64(&key.to_base64()).unwrap();
        assert_eq!(key.as_bytes(), restored.as_bytes());
    }

    #[test]
    fn two_generated_keys_differ() {
        // Иначе генератор мог бы возвращать константу, и круговой тест выше
        // прошёл бы, ничего не доказав.
        assert_ne!(
            MasterKey::generate().as_bytes(),
            MasterKey::generate().as_bytes()
        );
    }

    #[test]
    fn a_key_of_the_wrong_length_is_refused() {
        // Длина пришпилена с обеих сторон. Только снизу — недостаточно:
        // реализация, молча откусывающая хвост у слишком длинного ключа,
        // прошла бы такую проверку. А это худший из отказов — оператор
        // поставил ключ из 64 байт, сервер поднялся и шифрует не тем.
        let short = base64::engine::general_purpose::STANDARD.encode([0u8; 31]);
        assert!(matches!(
            MasterKey::from_base64(&short),
            Err(AuthError::MasterKeyLength { got: 31 })
        ));

        let long = base64::engine::general_purpose::STANDARD.encode([0u8; 33]);
        assert!(matches!(
            MasterKey::from_base64(&long),
            Err(AuthError::MasterKeyLength { got: 33 })
        ));
    }

    #[test]
    fn garbage_is_refused_with_its_own_error() {
        assert!(matches!(
            MasterKey::from_base64("это не base64!"),
            Err(AuthError::MasterKeyEncoding)
        ));
    }

    #[test]
    fn debug_does_not_print_the_key() {
        // Сравнение с точной ожидаемой строкой, а не поиск подстроки.
        // Поиск здесь не работает: производный `Debug` для `[u8; 32]`
        // печатает байты десятичными (`[153, 188, …]`), поэтому ни
        // base64-представление, ни шестнадцатеричное в дампе не появятся —
        // и обе такие проверки прошли бы на утёкшем ключе.
        let key = MasterKey::generate();
        assert_eq!(format!("{key:?}"), format!("MasterKey({REDACTED:?})"));
    }
}
```

- [ ] **Step 3: Убедиться, что падает**

Run: `cargo test -p wakode-auth`
Expected: FAIL — `MasterKey` не существует.

- [ ] **Step 4: Реализовать**

`crates/wakode-auth/src/error.rs`:

```rust
use thiserror::Error;

/// Ошибки криптографического слоя.
///
/// Ни один вариант не несёт секрета: сообщение об ошибке уезжает в лог и
/// в ответ клиенту, и приложить к нему ключ значило бы отдать его даром.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("мастер-ключ не является корректным base64")]
    MasterKeyEncoding,

    #[error("мастер-ключ должен быть длиной 32 байта, получено {got}")]
    MasterKeyLength { got: usize },

    #[error("не удалось зашифровать значение")]
    Encrypt,

    #[error("не удалось расшифровать значение: неверный мастер-ключ или повреждённые данные")]
    Decrypt,

    #[error("хеш пароля повреждён")]
    PasswordHashMalformed,

    #[error("не удалось посчитать хеш пароля")]
    PasswordHashFailed,
}

pub type AuthResult<T> = Result<T, AuthError>;
```

`crates/wakode-auth/src/lib.rs`:

```rust
//! Криптография wakode: чистые функции над байтами.
//!
//! Крейт не обращается ни к базе, ни к сети, ни к файлам, ни к часам.
//! Граница держится списком зависимостей: если здесь появится `rusqlite`
//! или `axum`, значит криптография перестала быть отдельной и проверять
//! её изоляцию станет нечем.

// Модули добавляются по одному, задачами 2–4: объявить их все сразу
// значило бы не собрать крейт до конца задачи 4.
pub mod error;
pub mod master_key;

pub use error::{AuthError, AuthResult};
pub use master_key::MasterKey;

/// Заглушка вместо секрета в `Debug`.
pub(crate) const REDACTED: &str = "<скрыт>";
```

`crates/wakode-auth/src/master_key.rs`:

```rust
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use rand::RngCore;

use crate::error::{AuthError, AuthResult};
use crate::REDACTED;

/// Мастер-ключ инстанса: 32 байта, которыми шифруются значения API-ключей.
///
/// Живёт только в `WAKODE_MASTER_KEY`. В конфиг-файл не пишется никогда:
/// файл с ключом рядом с базой означает, что украденный бэкап содержит и
/// шифротекст, и ключ к нему, — то есть шифрование не купило ничего.
#[derive(Clone)]
pub struct MasterKey([u8; 32]);

impl MasterKey {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_base64(encoded: &str) -> AuthResult<Self> {
        let raw = STANDARD
            .decode(encoded.trim())
            .map_err(|_| AuthError::MasterKeyEncoding)?;
        let bytes: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| AuthError::MasterKeyLength { got: raw.len() })?;
        Ok(Self(bytes))
    }

    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("MasterKey").field(&REDACTED).finish()
    }
}
```

- [ ] **Step 5: Прогнать**

Run: `cargo test -p wakode-auth`
Expected: PASS, пять тестов.

- [ ] **Step 6: Мутационная проверка**

Сломай по одной, убедись, что краснеет ожидаемый тест, верни обратно:

| Мутация | Обязан упасть |
|---|---|
| `generate` возвращает `Self([0u8; 32])` | `two_generated_keys_differ` |
| убрать проверку длины (`raw` длиной 31 принимается) | `a_key_of_the_wrong_length_is_refused` |
| вернуть производный `Debug` | `debug_does_not_print_the_key` |

Тест, не краснеющий ни от одной мутации, ничего не проверяет — переделай его, а не оставляй.

- [ ] **Step 7: Коммит**

```bash
git add Cargo.toml crates/wakode-auth
git commit -m "feat(auth): крейт криптографии и мастер-ключ"
```

---

### Task 2: Пароли

**Files:**
- Create: `crates/wakode-auth/src/password.rs`

**Interfaces:**
- Consumes: `AuthError`, `AuthResult` из задачи 1.
- Produces: `hash_password(password: &str) -> AuthResult<String>`, `verify_password(password: &str, hash: &str) -> AuthResult<bool>`.

Хеш возвращается строкой в формате PHC — он самоописывающий, несёт соль и параметры внутри себя. Хранилище кладёт его в `password_hash` как непрозрачную строку и ничего о нём не знает.

`verify_password` возвращает `Ok(false)` при неверном пароле и `Err` только при повреждённом хеше. Разница существенная: неверный пароль — обычное дело, повреждённый хеш — поломка данных, и смешивать их в одном варианте значит терять сигнал.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_verifies_against_its_own_hash() {
        let hash = hash_password("правильный конский хвост").unwrap();
        assert!(verify_password("правильный конский хвост", &hash).unwrap());
    }

    #[test]
    fn a_wrong_password_does_not_verify() {
        let hash = hash_password("правильный").unwrap();
        assert!(!verify_password("неправильный", &hash).unwrap());
    }

    #[test]
    fn the_same_password_hashes_differently_every_time() {
        // Соль случайна. Без этого два одинаковых пароля в базе были бы
        // видны как одинаковые строки, и утечка одного выдала бы второй.
        let a = hash_password("одинаковый").unwrap();
        let b = hash_password("одинаковый").unwrap();
        assert_ne!(a, b);
        assert!(verify_password("одинаковый", &a).unwrap());
        assert!(verify_password("одинаковый", &b).unwrap());
    }

    #[test]
    fn a_malformed_hash_is_an_error_not_a_false() {
        // Повреждённый хеш — поломка данных, а не неверный пароль.
        // Слить их в одно значило бы молча пускать «неверный пароль» там,
        // где на самом деле испорчена строка в базе.
        assert!(matches!(
            verify_password("любой", "это не PHC-строка"),
            Err(AuthError::PasswordHashMalformed)
        ));
    }

    #[test]
    fn the_hash_is_argon2id() {
        // Параметр по умолчанию, но зафиксированный: argon2i и argon2d
        // слабее против разных классов атак, и молчаливая смена варианта
        // при обновлении крейта должна ронять тест.
        let hash = hash_password("любой").unwrap();
        assert!(hash.starts_with("$argon2id$"), "получили {hash}");

        // Стоимость проверяется на двух уровнях, и оба нужны. Первый —
        // что уехавшее в хеш совпадает с нашими константами: сторожит
        // проводку. Второй — что сами константы не ниже порога: числа
        // здесь литеральные намеренно, потому что проверка, читающая те же
        // константы, что и код, двигается вместе с ними и ослабление
        // пропускает. Порог, а не равенство: усиление ронять незачем.
        assert!(
            hash.contains(&format!(
                "$v=19$m={ARGON2_MEMORY_KIB},t={ARGON2_ITERATIONS},p={ARGON2_PARALLELISM}$"
            )),
            "параметры стоимости разошлись с константами: {hash}"
        );
        // Порог проверяется на этапе компиляции — `const _: () = assert!(…)`
        // рядом с константами. `assert!` на константах внутри теста clippy
        // справедливо считает бессмыслицей: условие известно до запуска.

        // Длина соли — та же категория тихого ослабления.
        let salt = argon2::password_hash::PasswordHash::new(&hash)
            .unwrap()
            .salt
            .unwrap();
        assert_eq!(salt.decode_b64(&mut [0u8; 64]).unwrap().len(), 16);
    }

    #[test]
    fn a_hash_without_a_digest_is_malformed_not_a_wrong_password() {
        // PHC-строка, обрезанная ровно перед дайджестом, разбирается без
        // ошибки. Сверять с ней нечего: такой хеш не откроется ничем и
        // никогда, и отвечать на него «пароль не подошёл» значит прятать
        // поломку данных под обычное событие.
        let full = hash_password("любой").unwrap();
        let without_digest = &full[..full.rfind('$').unwrap()];

        assert!(matches!(
            verify_password("любой", without_digest),
            Err(AuthError::PasswordHashMalformed)
        ));
    }
}
```

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p wakode-auth password`
Expected: FAIL — функций нет.

- [ ] **Step 3: Реализовать**

```rust
use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;

use crate::error::{AuthError, AuthResult};

/// Посчитать хеш пароля.
///
/// Возвращается PHC-строка: она самоописывающая — соль и параметры лежат
/// внутри неё, поэтому хранилищу достаточно одной колонки, а смена
/// параметров не требует миграции.
pub fn hash_password(password: &str) -> AuthResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    hasher()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| AuthError::PasswordHashFailed)
}

/// Проверить пароль против хеша.
///
/// `Ok(false)` — пароль не подошёл, обычное дело. `Err` — хеш повреждён,
/// то есть сломаны данные. Смешивать эти два случая нельзя: во втором
/// пользователь не сможет войти никогда, и это надо увидеть в логе.
pub fn verify_password(password: &str, hash: &str) -> AuthResult<bool> {
    let parsed = PasswordHash::new(hash).map_err(|_| AuthError::PasswordHashMalformed)?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}
```

- [ ] **Step 4: Прогнать**

Run: `cargo test -p wakode-auth`
Expected: PASS, десять тестов (пять из задачи 1 плюс пять здесь).

- [ ] **Step 5: Мутационная проверка**

| Мутация | Обязан упасть |
|---|---|
| `SaltString::generate` заменить на фиксированную соль | `the_same_password_hashes_differently_every_time` |
| `verify_password` возвращает `Ok(false)` вместо `Err` при разборе | `a_malformed_hash_is_an_error_not_a_false` |
| `Argon2::default()` заменить на `Argon2::new(Algorithm::Argon2i, ...)` | `the_hash_is_argon2id` |

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-auth
git commit -m "feat(auth): хеширование и проверка паролей"
```

---

### Task 3: Значение API-ключа, шифрование и отпечаток

**Files:**
- Create: `crates/wakode-auth/src/api_key.rs`

**Interfaces:**
- Consumes: `MasterKey`, `AuthError`, `AuthResult`.
- Produces: `ApiKeyValue`, `ApiKeyValue::generate() -> ApiKeyValue`, `ApiKeyValue::parse(&str) -> Option<ApiKeyValue>`, `ApiKeyValue::to_string(&self) -> String`, `ApiKeyValue::encrypt(&self, &MasterKey) -> AuthResult<EncryptedKey>`, `ApiKeyValue::decrypt(&EncryptedKey, &MasterKey) -> AuthResult<ApiKeyValue>`, `ApiKeyValue::lookup(&self, &MasterKey) -> Vec<u8>`; `EncryptedKey(Vec<u8>)` с `EncryptedKey::as_bytes`/`from_bytes`.

**Три вещи в одном месте, потому что они об одном значении.** Ключ надо породить (UUIDv4 — плагины валидируют версию `[1-5]`, и v7 не пройдёт), сохранить обратимо (шифротекст, чтобы показать в настройках) и найти за один запрос (детерминированный отпечаток, потому что по шифротексту со случайным nonce искать невозможно).

`parse` принимает значение с префиксом `waka_` и без него: cli присылает по-разному, а срезать префикс в HTTP-слое значило бы размазать знание о формате ключа по двум крейтам.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_key_is_uuid_v4() {
        // Плагины валидируют ключ регуляркой UUID с проверкой версии [1-5].
        // UUIDv7, который проект использует для первичных ключей, её не
        // пройдёт, и редакторы молча перестанут отправлять отметки.
        // Регулярка плагинов смотрит два поля: версию и вариант. Проверять
        // только версию значит не защищать от ручной сборки UUID из
        // случайных байт с забытым вариантом — а это ровно та ошибка,
        // из-за которой редакторы молча перестают слать отметки.
        let value = ApiKeyValue::generate();
        let parsed = uuid::Uuid::parse_str(&value.to_string()).unwrap();
        assert_eq!(parsed.get_version_num(), 4);
        assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122);
    }

    #[test]
    fn a_short_or_empty_ciphertext_is_refused_not_panicked() {
        // `EncryptedKey::from_bytes` зовут на байтах из базы. Усечённая
        // строка не должна превращать отказ авторизации в панику потока:
        // без проверки длины `split_at` паникует на первом же байте.
        let master = MasterKey::generate();
        let full = ApiKeyValue::generate().encrypt(&master).unwrap();

        for bytes in [
            Vec::new(),
            vec![0u8; 1],
            full.as_bytes()[..full.as_bytes().len() / 2].to_vec(),
        ] {
            assert!(matches!(
                ApiKeyValue::decrypt(&EncryptedKey::from_bytes(bytes), &master),
                Err(AuthError::Decrypt)
            ));
        }
    }

    #[test]
    fn the_ciphertext_does_not_contain_the_value_in_the_clear() {
        // Круговой обход доказывает обратимость, но не шифрование:
        // реализация, склеивающая nonce с открытым текстом, прошла бы его.
        let master = MasterKey::generate();
        let value = ApiKeyValue::generate();
        let encrypted = value.encrypt(&master).unwrap();

        let text = value.to_string();
        assert!(
            !encrypted
                .as_bytes()
                .windows(text.len())
                .any(|window| window == text.as_bytes()),
            "значение лежит в шифротексте открытым"
        );
    }

    #[test]
    fn a_key_encrypted_by_an_earlier_build_still_opens() {
        // Формат хранения — обязательство перед всеми существующими
        // базами. Смена шифра, порядка склейки или длины nonce сделала бы
        // нечитаемыми все выданные ключи, и заметить это можно только
        // фиксированным вектором: круговой обход внутри одного запуска
        // согласован сам с собой и такую поломку не видит.
        //
        // Вектор снят с рабочей реализации один раз. Если он покраснел —
        // это не повод его перегенерировать: сначала пойми, что изменилось
        // в формате, и что делать с ключами, уже лежащими в базах.
        let master = MasterKey::from_base64("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=").unwrap();
        let stored: [u8; 76] = [
            160, 247, 27, 57, 200, 201, 215, 88, 101, 223, 187, 244, 224, 117, 41, 226, 19, 174,
            66, 76, 150, 126, 246, 7, 3, 119, 133, 80, 108, 50, 30, 127, 6, 223, 202, 194, 66, 133,
            50, 182, 81, 30, 88, 206, 20, 60, 130, 211, 51, 31, 49, 83, 49, 246, 47, 76, 153, 131,
            217, 40, 40, 155, 127, 52, 51, 70, 51, 32, 208, 240, 237, 58, 230, 180, 234, 180,
        ];

        let restored = ApiKeyValue::decrypt(&EncryptedKey::from_bytes(stored.to_vec()), &master)
            .expect("ключ, зашифрованный прежней сборкой, перестал открываться");
        assert_eq!(restored.to_string(), "6f1e8d3a-2c4b-4a9e-8f7d-1b2c3d4e5f60");
    }

    #[test]
    fn the_lookup_algorithm_is_pinned_to_a_fixed_vector() {
        // Отпечаток ложится в уникальный индекс. Смена алгоритма или его
        // усечение — тихая поломка: старые ключи перестанут находиться, а
        // короткий отпечаток ослабит стойкость к коллизиям, и ни то, ни
        // другое не видно из тестов, сравнивающих отпечаток сам с собой.
        let master = MasterKey::from_base64("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=").unwrap();
        let value = ApiKeyValue::parse("6f1e8d3a-2c4b-4a9e-8f7d-1b2c3d4e5f60").unwrap();

        assert_eq!(
            value.lookup(&master),
            vec![
                76, 184, 159, 202, 153, 23, 75, 9, 206, 0, 192, 148, 139, 127, 11, 63, 219, 57,
                129, 192, 151, 142, 132, 128, 171, 224, 228, 185, 231, 65, 57, 117
            ]
        );
    }

    #[test]
    fn surrounding_whitespace_is_tolerated_on_parse() {
        // Значение приезжает из конфига редактора и часто с переводом
        // строки на конце.
        let value = ApiKeyValue::generate();
        let text = value.to_string();

        assert_eq!(ApiKeyValue::parse(&format!("  {text}\n")).unwrap(), value);
        assert_eq!(ApiKeyValue::parse(&format!("\twaka_{text}  ")).unwrap(), value);
    }

    #[test]
    fn the_waka_prefix_is_optional_on_parse() {
        let value = ApiKeyValue::generate();
        let plain = value.to_string();
        let prefixed = format!("waka_{plain}");

        assert_eq!(ApiKeyValue::parse(&plain).unwrap(), value);
        assert_eq!(ApiKeyValue::parse(&prefixed).unwrap(), value);
    }

    #[test]
    fn garbage_does_not_parse() {
        assert!(ApiKeyValue::parse("не ключ").is_none());
        assert!(ApiKeyValue::parse("waka_тоже не ключ").is_none());
    }

    #[test]
    fn a_key_survives_the_encryption_round_trip() {
        let master = MasterKey::generate();
        let value = ApiKeyValue::generate();

        let encrypted = value.encrypt(&master).unwrap();
        let restored = ApiKeyValue::decrypt(&encrypted, &master).unwrap();

        assert_eq!(restored, value);
    }

    #[test]
    fn the_ciphertext_differs_every_time() {
        // Nonce случаен. Без этого два одинаковых ключа дали бы одинаковый
        // шифротекст, и по базе было бы видно, что они совпадают.
        let master = MasterKey::generate();
        let value = ApiKeyValue::generate();

        let a = value.encrypt(&master).unwrap();
        let b = value.encrypt(&master).unwrap();

        assert_ne!(a.as_bytes(), b.as_bytes());
        assert_eq!(ApiKeyValue::decrypt(&a, &master).unwrap(), value);
        assert_eq!(ApiKeyValue::decrypt(&b, &master).unwrap(), value);
    }

    #[test]
    fn another_master_key_cannot_decrypt() {
        // Ради этого теста существует пятый шаг последовательности старта:
        // сервер обязан заметить подменённый мастер-ключ сразу, а не
        // отвечать 401 на все ключи и выглядеть как поломка хранилища.
        let master = MasterKey::generate();
        let other = MasterKey::generate();
        let encrypted = ApiKeyValue::generate().encrypt(&master).unwrap();

        assert!(matches!(
            ApiKeyValue::decrypt(&encrypted, &other),
            Err(AuthError::Decrypt)
        ));
    }

    #[test]
    fn a_corrupted_ciphertext_is_refused() {
        // AEAD обязан поймать порчу, а не выдать мусор за ключ.
        let master = MasterKey::generate();
        let encrypted = ApiKeyValue::generate().encrypt(&master).unwrap();

        let mut broken = encrypted.as_bytes().to_vec();
        let last = broken.len() - 1;
        broken[last] ^= 0xff;

        assert!(ApiKeyValue::decrypt(&EncryptedKey::from_bytes(broken), &master).is_err());
    }

    #[test]
    fn the_lookup_is_stable_for_one_key_and_differs_between_keys() {
        // Отпечаток детерминирован — по нему ищут одним запросом.
        // Будь он случайным, поиск сломался бы; будь он одинаковым у
        // разных ключей, уникальный индекс отверг бы вторую выдачу.
        let master = MasterKey::generate();
        let one = ApiKeyValue::generate();
        let two = ApiKeyValue::generate();

        assert_eq!(one.lookup(&master), one.lookup(&master));
        assert_ne!(one.lookup(&master), two.lookup(&master));
    }

    #[test]
    fn the_lookup_depends_on_the_master_key() {
        // Иначе отпечаток был бы простым хешем значения, и укравший базу
        // мог бы проверять догадки офлайн без мастер-ключа.
        let value = ApiKeyValue::generate();
        assert_ne!(
            value.lookup(&MasterKey::generate()),
            value.lookup(&MasterKey::generate())
        );
    }

    #[test]
    fn debug_prints_neither_the_value_nor_the_ciphertext() {
        let master = MasterKey::generate();
        let value = ApiKeyValue::generate();
        let encrypted = value.encrypt(&master).unwrap();

        let value_dump = format!("{value:?}");
        assert!(
            !value_dump.contains(&value.to_string()),
            "значение ключа утекло: {value_dump}"
        );

        let encrypted_dump = format!("{encrypted:?}");
        assert!(
            !encrypted_dump.contains(&format!("{:?}", encrypted.as_bytes())),
            "шифротекст утёк: {encrypted_dump}"
        );
    }
}
```

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p wakode-auth api_key`
Expected: FAIL — типов нет.

- [ ] **Step 3: Реализовать**

```rust
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use uuid::Uuid;

use crate::error::{AuthError, AuthResult};
use crate::master_key::MasterKey;
use crate::REDACTED;

/// Длина nonce у XChaCha20-Poly1305.
///
/// Широкий nonce выбран именно потому, что он случайный: у 24 байт
/// вероятность коллизии пренебрежима без счётчика, а счётчик потребовал бы
/// хранить состояние между запусками.
const NONCE_LEN: usize = 24;

/// Значение API-ключа — то, что видит пользователь и присылает редактор.
///
/// UUIDv4, а не v7, которым проект пользуется для первичных ключей:
/// плагины валидируют ключ регуляркой UUID с проверкой версии `[1-5]`.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKeyValue(Uuid);

/// Значение ключа под мастер-ключом: nonce и следом шифротекст.
#[derive(Clone, PartialEq, Eq)]
pub struct EncryptedKey(Vec<u8>);

impl EncryptedKey {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl ApiKeyValue {
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }

    /// Разобрать значение, пришедшее с провода.
    ///
    /// Префикс `waka_` необязателен: cli присылает и так, и так. Знание о
    /// формате ключа живёт здесь целиком — HTTP-слою о префиксе знать
    /// незачем.
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        let without_prefix = trimmed.strip_prefix("waka_").unwrap_or(trimmed);
        Uuid::parse_str(without_prefix).ok().map(Self)
    }

    pub fn encrypt(&self, master: &MasterKey) -> AuthResult<EncryptedKey> {
        let cipher = XChaCha20Poly1305::new(master.as_bytes().into());

        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, self.0.to_string().as_bytes())
            .map_err(|_| AuthError::Encrypt)?;

        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(EncryptedKey(out))
    }

    pub fn decrypt(encrypted: &EncryptedKey, master: &MasterKey) -> AuthResult<Self> {
        let bytes = encrypted.as_bytes();
        if bytes.len() <= NONCE_LEN {
            return Err(AuthError::Decrypt);
        }
        let (nonce_bytes, ciphertext) = bytes.split_at(NONCE_LEN);

        let plaintext = XChaCha20Poly1305::new(master.as_bytes().into())
            .decrypt(XNonce::from_slice(nonce_bytes), ciphertext)
            .map_err(|_| AuthError::Decrypt)?;

        let text = String::from_utf8(plaintext).map_err(|_| AuthError::Decrypt)?;
        Uuid::parse_str(&text).map(Self).map_err(|_| AuthError::Decrypt)
    }

    /// Детерминированный отпечаток для поиска ключа одним запросом.
    ///
    /// HMAC под мастер-ключом, а не простой хеш: иначе укравший базу
    /// проверял бы догадки офлайн, не имея мастер-ключа вовсе.
    pub fn lookup(&self, master: &MasterKey) -> Vec<u8> {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(master.as_bytes())
            .expect("HMAC-SHA256 принимает ключ любой длины");
        mac.update(self.0.to_string().as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

impl std::fmt::Display for ApiKeyValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::fmt::Debug for ApiKeyValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ApiKeyValue").field(&REDACTED).finish()
    }
}

impl std::fmt::Debug for EncryptedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedKey")
            .field("bytes", &self.0.len())
            .finish()
    }
}
```

- [ ] **Step 4: Прогнать**

Run: `cargo test -p wakode-auth`
Expected: PASS, двадцать шесть тестов в wakode-auth.

- [ ] **Step 5: Мутационная проверка**

| Мутация | Обязан упасть |
|---|---|
| `Uuid::new_v4()` → `Uuid::now_v7()` | `a_generated_key_is_uuid_v4` |
| `parse` не срезает `waka_` | `the_waka_prefix_is_optional_on_parse` |
| nonce фиксирован нулями | `the_ciphertext_differs_every_time` |
| `lookup` считает `Sha256` от значения без ключа | `the_lookup_depends_on_the_master_key` |
| производный `Debug` у обоих типов | `debug_prints_neither_the_value_nor_the_ciphertext` |

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-auth
git commit -m "feat(auth): значение API-ключа, шифрование и отпечаток"
```

---

### Task 4: Токен сессии

**Files:**
- Create: `crates/wakode-auth/src/session.rs`

**Interfaces:**
- Consumes: `REDACTED`.
- Produces: `SessionToken`, `SessionToken::generate() -> SessionToken`, `SessionToken::parse(&str) -> Option<SessionToken>`, `SessionToken::to_string(&self)`, `SessionToken::hash(&self) -> Vec<u8>`.

**Почему хеш, а не шифрование.** Токен сессии показывать обратно незачем — в отличие от API-ключа, который пользователь копирует в редактор. Поэтому в базе лежит односторонний хеш, и утечка базы не даёт войти под чужой сессией. Мастер-ключ здесь не нужен вовсе.

**Почему SHA-256, а не argon2.** Токен — 32 случайных байта, а не пароль: перебирать нечего, растягивание вычислений защищает от подбора по словарю, которого здесь не существует. Argon2 на каждом запросе стоил бы десятки миллисекунд без единой выгоды.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_survives_the_text_round_trip() {
        let token = SessionToken::generate();
        assert_eq!(SessionToken::parse(&token.to_string()).unwrap(), token);
    }

    #[test]
    fn two_tokens_differ() {
        assert_ne!(SessionToken::generate(), SessionToken::generate());
    }

    #[test]
    fn the_hash_is_stable_for_one_token_and_differs_between_tokens() {
        let one = SessionToken::generate();
        let two = SessionToken::generate();

        assert_eq!(one.hash(), one.hash());
        assert_ne!(one.hash(), two.hash());
        assert_eq!(one.hash().len(), 32);
    }

    #[test]
    fn the_hash_does_not_contain_the_token() {
        // Односторонность нужна затем, что утечка базы не должна давать
        // возможность войти под чужой сессией.
        //
        // Сравнение идёт по байтам, а не по тексту. Текстовая проверка
        // вида `String::from_utf8_lossy(&hash()).contains(&to_string())`
        // не ловит ничего: хеш — 32 сырых байта, токен печатается 43
        // символами base64url, и короткая строка не может содержать
        // длинную ни при какой реализации.
        let token = SessionToken::generate();
        let raw = URL_SAFE_NO_PAD.decode(token.to_string()).unwrap();
        assert_ne!(token.hash(), raw, "хеш совпал с самим токеном");
    }

    #[test]
    fn the_hash_algorithm_is_pinned_to_a_fixed_vector() {
        // Хеш ложится в уникальный индекс `sessions.token_hash`. Тихая
        // смена алгоритма разлогинила бы всех разом, и круговой обход
        // внутри одного запуска этого не увидит: он согласован сам с собой.
        let token = SessionToken::parse("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8").unwrap();
        assert_eq!(
            token.hash(),
            // SHA-256 от байтов 0x00..0x1f, посчитан независимо.
            vec![
                99, 13, 205, 41, 102, 196, 51, 102, 145, 18, 84, 72, 187, 178, 91, 79, 244, 18,
                164, 156, 115, 45, 178, 200, 171, 193, 184, 88, 27, 215, 16, 221
            ]
        );
    }

    #[test]
    fn garbage_does_not_parse() {
        assert!(SessionToken::parse("короткий").is_none());
        assert!(SessionToken::parse("").is_none());
    }

    #[test]
    fn debug_does_not_print_the_token() {
        // Сравнение с точной строкой, а не поиск подстроки. Та же ловушка,
        // что в задаче 1: производный `Debug` печатает `[u8; 32]`
        // десятичными, а `to_string()` даёт base64url — эти представления
        // не пересекаются, и поиск подстроки зелёный на утёкшем токене.
        let token = SessionToken::generate();
        assert_eq!(format!("{token:?}"), format!("SessionToken({REDACTED:?})"));
    }
}
```

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p wakode-auth session`
Expected: FAIL — типа нет.

- [ ] **Step 3: Реализовать**

```rust
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::REDACTED;

/// Длина токена сессии в байтах.
const TOKEN_LEN: usize = 32;

/// Токен сессии: 32 случайных байта, живущие в cookie.
///
/// В базе лежит только его хеш — показывать токен обратно, в отличие от
/// API-ключа, незачем, и односторонность здесь бесплатна.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionToken([u8; TOKEN_LEN]);

impl SessionToken {
    pub fn generate() -> Self {
        let mut bytes = [0u8; TOKEN_LEN];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(raw.trim()).ok()?;
        bytes.as_slice().try_into().ok().map(Self)
    }

    /// Хеш для хранения и поиска.
    ///
    /// SHA-256, а не argon2: токен — 32 случайных байта, перебирать нечего,
    /// а растягивание вычислений на каждом запросе стоило бы десятки
    /// миллисекунд без выгоды.
    pub fn hash(&self) -> Vec<u8> {
        Sha256::digest(self.0).to_vec()
    }
}

impl std::fmt::Display for SessionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl std::fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SessionToken").field(&REDACTED).finish()
    }
}
```

- [ ] **Step 4: Прогнать**

Run: `cargo test -p wakode-auth`
Expected: PASS, тридцать шесть тестов в wakode-auth.

- [ ] **Step 5: Мутационная проверка**

| Мутация | Обязан упасть |
|---|---|
| `generate` возвращает `Self([7u8; 32])` | `two_tokens_differ` |
| `hash` возвращает байты токена как есть | `the_hash_is_stable_for_one_token_and_differs_between_tokens` не упадёт — проверь, упадёт ли `the_hash_does_not_contain_the_token`; если нет, тест негоден и его надо переписать |
| производный `Debug` | `debug_does_not_print_the_token` |

Вторая строка — намеренная ловушка. Разберись, действительно ли тест на односторонность что-то доказывает, и если нет — напиши это прямо и почини.

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-auth
git commit -m "feat(auth): токен сессии"
```

---

### Task 5: Две новые функции хранилища

**Files:**
- Modify: `crates/wakode-store/src/users.rs`
- Modify: `crates/wakode-store/src/keys.rs`
- Modify: `crates/wakode-store/src/repo.rs`
- Modify: `crates/wakode-store/src/lib.rs`
- Modify: `crates/wakode-store/tests/repository.rs`

**Interfaces:**
- Produces: `user_count(conn: &Connection) -> StoreResult<i64>`, `first_api_key(conn: &Connection) -> StoreResult<Option<ApiKey>>`; методы трейтов `UserRepo::user_count()` и `KeyRepo::first_key()`.

**Зачем.** Последовательность старта на шагах 4 и 5 спрашивает «есть ли в базе хоть один API-ключ» и берёт один, чтобы проверить мастер-ключ. Экран первичной настройки спрашивает «есть ли хоть один пользователь». Ни того, ни другого в хранилище нет.

`first_api_key` отдаёт именно ключ, а не булево: шагу 5 нужен шифротекст, а два запроса вместо одного здесь ничего не покупают. Порядок — по `created_at`, чтобы результат был воспроизводим.

Схема **не меняется**: обе функции только читают.

- [ ] **Step 1: Написать падающие тесты**

В `crates/wakode-store/tests/repository.rs`:

```rust
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
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let user = insert_user(&conn, &a_user("swrneko")).unwrap();

    let older = insert_api_key(&conn, &NewApiKey {
        user_id: user.id,
        name: "старый".to_owned(),
        key_encrypted: vec![1],
        key_lookup: vec![1],
    })
    .unwrap();
    insert_api_key(&conn, &NewApiKey {
        user_id: user.id,
        name: "новый".to_owned(),
        key_encrypted: vec![2],
        key_lookup: vec![2],
    })
    .unwrap();

    let found = first_api_key(&conn).unwrap().unwrap();
    assert_eq!(found.id, older.id);
    assert_eq!(found.key_encrypted, vec![1]);
}

#[test]
fn first_api_key_sees_keys_of_every_user() {
    // Шаг 5 старта проверяет мастер-ключ инстанса целиком, а не одного
    // пользователя: ключ любого владельца зашифрован тем же мастер-ключом.
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    let other = insert_user(&conn, &a_user("другой")).unwrap();

    insert_api_key(&conn, &NewApiKey {
        user_id: other.id,
        name: "чужой".to_owned(),
        key_encrypted: vec![9],
        key_lookup: vec![9],
    })
    .unwrap();

    assert!(first_api_key(&conn).unwrap().is_some());
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
```

Добавь в общий блок импортов `first_api_key`, `user_count`, `KeyRepo`, `SessionRepo` — последние два понадобятся дальше.

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p wakode-store --test repository`
Expected: FAIL — функций нет.

- [ ] **Step 3: Реализовать**

В `crates/wakode-store/src/users.rs`:

```rust
/// Сколько всего пользователей в базе.
///
/// Нужно экрану первичной настройки: он открыт ровно до появления первого
/// пользователя и закрывается навсегда после.
pub fn user_count(conn: &Connection) -> StoreResult<i64> {
    let mut stmt = conn.prepare_cached("SELECT count(*) FROM users")?;
    Ok(stmt.query_row([], |row| row.get(0))?)
}
```

В `crates/wakode-store/src/keys.rs`:

```rust
/// Самый ранний API-ключ в базе, если он есть.
///
/// Последовательность старта берёт им две вещи разом: сам факт наличия
/// ключей (без мастер-ключа стартовать нельзя) и шифротекст для проверки,
/// что мастер-ключ тот самый. Порядок по `created_at` делает проверку
/// воспроизводимой — «какой-нибудь» ключ означал бы, что она то проходит,
/// то нет.
pub fn first_api_key(conn: &Connection) -> StoreResult<Option<ApiKey>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, user_id, name, key_encrypted, created_at, last_used_at, revoked_at
         FROM api_keys ORDER BY created_at, id LIMIT 1",
    )?;

    let row = stmt
        .query_row([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .optional()?;

    let Some((id, user_id, name, key_encrypted, created, used, revoked)) = row else {
        return Ok(None);
    };

    Ok(Some(ApiKey {
        id: blob_to_uuid(&id)?,
        user_id: blob_to_uuid(&user_id)?,
        name,
        key_encrypted,
        created_at: Micros::new(created),
        last_used_at: used.map(Micros::new),
        revoked_at: revoked.map(Micros::new),
    }))
}
```

В `crates/wakode-store/src/repo.rs` добавь по методу в трейты и их реализации:

```rust
// в trait UserRepo
    fn user_count(&self) -> impl std::future::Future<Output = StoreResult<i64>> + Send;

// в trait KeyRepo
    fn first_key(&self) -> impl std::future::Future<Output = StoreResult<Option<ApiKey>>> + Send;

// в impl UserRepo for SqliteStore
    async fn user_count(&self) -> StoreResult<i64> {
        on_own_connection(self, |conn| crate::user_count(&conn)).await
    }

// в impl KeyRepo for SqliteStore
    async fn first_key(&self) -> StoreResult<Option<ApiKey>> {
        on_own_connection(self, |conn| crate::first_api_key(&conn)).await
    }
```

Реэкспортируй `user_count` и `first_api_key` из `lib.rs`.

- [ ] **Step 4: Прогнать**

Run: `cargo test -p wakode-store`
Expected: PASS, пять новых тестов в `tests/repository.rs` и два в `src/keys.rs`.

- [ ] **Step 5: Мутационная проверка**

| Мутация | Обязан упасть |
|---|---|
| `ORDER BY created_at, id` убрать, оставив `LIMIT 1` | `first_api_key_orders_by_created_at_not_by_insertion` (модульный тест в `src/keys.rs`) |
| `ORDER BY created_at DESC` | он же |
| `user_count` возвращает `Ok(0)` всегда | `user_count_follows_the_users_actually_inserted` |
| добавить в `first_api_key` условие `WHERE revoked_at IS NULL` | `first_api_key_sees_revoked_keys_too` |

**Почему два теста порядка живут в `src/keys.rs`, а не в `tests/repository.rs`.** Интеграционный тест порядок не доказывает: два ключа, вставленные подряд, ложатся по возрастанию и по `created_at`, и по первичному ключу — `api_keys` объявлена `WITHOUT ROWID`, её обход идёт по кластерному индексу UUIDv7, а тот монотонен по времени. Поэтому обход совпадает с `ORDER BY` случайно, и сортировку можно снять незаметно. На том же в `load_heartbeats` уже обжигались, только там причиной был индекс `hb_time`. Различающий тест требует `created_at`, идущего против порядка вставки, а задать его можно только напрямую. Сырой SQL в модульном тесте внутри `src/` для того и позволен: три места в `tests/repository.rs` — про схему, а это про то, чего через публичный интерфейс не выразить.

**Про отозванные ключи.** `first_api_key` их **видит**, и это не недосмотр: шаг 5 старта расшифровывает найденным ключом пробное значение, а отозванный зашифрован тем же мастер-ключом и годится ровно так же. Инстанс, где единственный ключ отозвали, обязан продолжать отказываться стартовать с чужим мастер-ключом.

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-store
git commit -m "feat(store): счётчик пользователей и первый ключ для старта"
```

---

### Task 6: Конфиг

**Files:**
- Create: `crates/wakode/Cargo.toml`
- Create: `crates/wakode/src/main.rs`
- Create: `crates/wakode/src/config.rs`
- Modify: `Cargo.toml` (корень)

**Interfaces:**
- Produces: `Config` с полями `server`, `database`, `auth`, `durations`; `Config::load(path: Option<&Path>) -> Result<Config, ConfigError>`; `ConfigError`.

**Правила разрешения.** Один файл, путь из `--config`, по умолчанию `./wakode.toml`. Каскада поиска нет: один путь — один ответ на вопрос «какой файл он прочёл». Сверху перекрывают `WAKODE_*` по схеме `WAKODE_<СЕКЦИЯ>_<ПОЛЕ>` — например `WAKODE_SERVER_LISTEN`.

**Файла может не быть.** Если `--config` не задан и `./wakode.toml` отсутствует — это не ошибка: инстанс поднимается на умолчаниях плюс окружении, что и нужно в Docker. Но если `--config` задан явно, а файла нет — ошибка с абсолютным путём: пользователь сказал, где смотреть, и промолчать здесь значило бы запустить сервер не с той конфигурацией.

Мастер-ключ в `Config` не входит — он только в окружении и читается отдельно, на шаге 2 старта.

- [ ] **Step 1: Завести крейт**

В корневой `Cargo.toml` добавь `"crates/wakode"` в `members` и в `[workspace.dependencies]`:

```toml
axum = "0.8"
axum-extra = "0.12"
clap = "4.6"
toml = "1.1"
tower = "0.5"
tower-http = "0.7"
tracing = "0.1"
tracing-subscriber = "0.3"
wakode-api = { version = "0.1.0", path = "crates/wakode-api" }
wakode-auth = { version = "0.1.0", path = "crates/wakode-auth" }
wakode-store = { version = "0.1.0", path = "crates/wakode-store" }
```

`crates/wakode/Cargo.toml`:

```toml
[package]
name = "wakode"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
chrono-tz.workspace = true
clap = { workspace = true, features = ["derive"] }
serde = { workspace = true, features = ["derive"] }
thiserror.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "net", "signal"] }
toml.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
uuid = { workspace = true, features = ["v7"] }
wakode-auth.workspace = true
wakode-core.workspace = true
wakode-store.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

`wakode-api` добавляется в зависимости в задаче 8 — сейчас крейта ещё нет.

- [ ] **Step 2: Написать падающие тесты**

`crates/wakode/src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_config(dir: &tempfile::TempDir, body: &str) -> std::path::PathBuf {
        let path = dir.path().join("wakode.toml");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn defaults_apply_when_the_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("нет-такого.toml");

        // Путь не задан вовсе — умолчания, без ошибки.
        let config = Config::load_from(None, &missing, |_| None).unwrap();

        assert_eq!(config.server.listen, "127.0.0.1:9000");
        assert_eq!(config.database.write_queue, 256);
        assert!(!config.auth.registration);
        assert!(!config.auth.setup_from_any_address);
        assert_eq!(config.durations.timeout_secs, 900);
        assert_eq!(config.durations.tail_padding_secs, 0);
    }

    #[test]
    fn an_explicitly_named_file_that_is_missing_is_an_error() {
        // Пользователь сказал, где смотреть. Промолчать здесь значило бы
        // поднять сервер не с той конфигурацией.
        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("нет-такого.toml");

        let err = Config::load_from(Some(&named), &named, |_| None).unwrap_err();
        assert!(matches!(err, ConfigError::Missing { .. }));
        assert!(format!("{err}").contains("нет-такого.toml"));
    }

    #[test]
    fn the_file_overrides_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            r#"
[server]
listen = "0.0.0.0:8080"

[durations]
timeout_secs = 1800
"#,
        );

        let config = Config::load_from(Some(&path), &path, |_| None).unwrap();

        assert_eq!(config.server.listen, "0.0.0.0:8080");
        assert_eq!(config.durations.timeout_secs, 1800);
        // Не упомянутые в файле поля остаются умолчаниями.
        assert_eq!(config.database.write_queue, 256);
    }

    #[test]
    fn the_environment_overrides_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "[server]\nlisten = \"0.0.0.0:8080\"\n");

        let config = Config::load_from(Some(&path), &path, |name| match name {
            "WAKODE_SERVER_LISTEN" => Some("127.0.0.1:1234".to_owned()),
            _ => None,
        })
        .unwrap();

        assert_eq!(config.server.listen, "127.0.0.1:1234");
    }

    #[test]
    fn a_numeric_field_from_the_environment_is_parsed_not_pasted() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "");

        let config = Config::load_from(Some(&path), &path, |name| match name {
            "WAKODE_DURATIONS_TIMEOUT_SECS" => Some("1200".to_owned()),
            _ => None,
        })
        .unwrap();

        assert_eq!(config.durations.timeout_secs, 1200);
    }

    #[test]
    fn a_broken_numeric_value_names_the_variable() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "");

        let err = Config::load_from(Some(&path), &path, |name| match name {
            "WAKODE_DURATIONS_TIMEOUT_SECS" => Some("много".to_owned()),
            _ => None,
        })
        .unwrap_err();

        assert!(format!("{err}").contains("WAKODE_DURATIONS_TIMEOUT_SECS"));
    }

    #[test]
    fn broken_toml_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "[server\nlisten =");

        let err = Config::load_from(Some(&path), &path, |_| None).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
        assert!(format!("{err}").contains("wakode.toml"));
    }

    #[test]
    fn the_config_carries_no_secrets() {
        // Мастер-ключ в конфиге отсутствует по построению: он живёт только
        // в `WAKODE_MASTER_KEY`. Файл с ключом рядом с базой означал бы,
        // что украденный бэкап содержит и шифротекст, и ключ к нему.
        //
        // Сторожит это сверка полного состава полей, а не поиск подстроки
        // «master». Поиск подстроки не поймал бы поле с любым другим
        // именем — а `Config` печатается производным `Debug`, и первая же
        // строка старта отправила бы такое поле в лог. Сверка состава
        // краснеет на **любом** новом поле и заставляет автора решить,
        // секрет это или нет. Если поле не секрет — просто допиши его сюда.
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "");
        let config = Config::load_from(Some(&path), &path, |_| None).unwrap();

        assert_eq!(
            format!("{config:?}"),
            "Config { \
             server: ServerConfig { listen: \"127.0.0.1:9000\", \
             public_url: \"http://localhost:9000\" }, \
             database: DatabaseConfig { path: \"./wakode.db\", write_queue: 256 }, \
             auth: AuthConfig { registration: false, session_ttl_days: 30, \
             setup_from_any_address: false }, \
             durations: DurationsConfig { timeout_secs: 900, tail_padding_secs: 0 } }"
        );
    }
}
```

`load_from` принимает читалку окружения замыканием, чтобы тесты не трогали переменные процесса: параллельные тесты, дёргающие `std::env::set_var`, влияют друг на друга и дают плавающие падения.

- [ ] **Step 3: Убедиться, что падает**

Run: `cargo test -p wakode`
Expected: FAIL — `Config` не существует.

- [ ] **Step 4: Реализовать**

```rust
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("файл конфигурации не найден: {path}")]
    Missing { path: PathBuf },

    #[error("не удалось прочитать {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("не удалось разобрать {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("переменная {name} содержит {value:?}, а ожидалось число")]
    NotANumber { name: &'static str, value: String },

    #[error("переменная {name} содержит {value:?}, а ожидалось true или false")]
    NotABool { name: &'static str, value: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    pub durations: DurationsConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: String,
    pub public_url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    pub path: PathBuf,
    pub write_queue: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    pub registration: bool,
    pub session_ttl_days: i64,
    pub setup_from_any_address: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DurationsConfig {
    pub timeout_secs: i64,
    /// Добавка последней отметке сессии.
    ///
    /// Ноль до калибровки по живому аккаунту WakaTime: величина нигде не
    /// задокументирована. Пока здесь ноль, wakode недосчитывает хвост
    /// каждой сессии — работать это не мешает, совпасть цифрам мешает.
    pub tail_padding_secs: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            database: DatabaseConfig::default(),
            auth: AuthConfig::default(),
            durations: DurationsConfig::default(),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            // Петлевой адрес: инстанс, выставленный наружу, должен быть
            // выставлен осознанно, а не по умолчанию.
            listen: "127.0.0.1:9000".to_owned(),
            public_url: "http://localhost:9000".to_owned(),
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("./wakode.db"),
            write_queue: 256,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            registration: false,
            session_ttl_days: 30,
            setup_from_any_address: false,
        }
    }
}

impl Default for DurationsConfig {
    fn default() -> Self {
        Self {
            timeout_secs: wakode_core::DEFAULT_TIMEOUT_SECS,
            tail_padding_secs: 0,
        }
    }
}

pub const DEFAULT_CONFIG_PATH: &str = "./wakode.toml";

impl Config {
    /// Прочитать конфигурацию.
    ///
    /// `explicit` — путь, названный флагом `--config`. Если он задан, а
    /// файла нет, это ошибка: пользователь сказал, где смотреть. Если не
    /// задан и умолчания на диске нет — берутся значения по умолчанию плюс
    /// окружение, что и нужно в контейнере.
    pub fn load(explicit: Option<&Path>) -> Result<Self, ConfigError> {
        let default_path = PathBuf::from(DEFAULT_CONFIG_PATH);
        Self::load_from(explicit, &default_path, |name| std::env::var(name).ok())
    }

    /// То же, но с явной читалкой окружения — так тестируется без
    /// `std::env::set_var`, который в параллельных тестах течёт между ними.
    pub fn load_from(
        explicit: Option<&Path>,
        default_path: &Path,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let path = explicit.unwrap_or(default_path);

        let mut config = if path.exists() {
            let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
                path: path.to_path_buf(),
                source,
            })?;
            toml::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            })?
        } else if explicit.is_some() {
            // Путь приводится к абсолютному: под systemd рабочий каталог
            // не тот, что думает админ, и «файл не найден: wakode.toml» не
            // говорит, где его искали.
            return Err(ConfigError::Missing {
                path: std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()),
            });
        } else {
            Config::default()
        };

        config.apply_env(&env)?;
        Ok(config)
    }

    fn apply_env(&mut self, env: &impl Fn(&str) -> Option<String>) -> Result<(), ConfigError> {
        if let Some(value) = env("WAKODE_SERVER_LISTEN") {
            self.server.listen = value;
        }
        if let Some(value) = env("WAKODE_SERVER_PUBLIC_URL") {
            self.server.public_url = value;
        }
        if let Some(value) = env("WAKODE_DATABASE_PATH") {
            self.database.path = PathBuf::from(value);
        }
        if let Some(value) = env("WAKODE_DATABASE_WRITE_QUEUE") {
            // Разбор сразу в `usize`, а не `i64 as usize`: беззнаковый
            // каст превратил бы `-1` в `usize::MAX` молча.
            self.database.write_queue = parse_size("WAKODE_DATABASE_WRITE_QUEUE", &value)?;
        }
        if let Some(value) = env("WAKODE_AUTH_REGISTRATION") {
            self.auth.registration = parse_bool("WAKODE_AUTH_REGISTRATION", &value)?;
        }
        if let Some(value) = env("WAKODE_AUTH_SESSION_TTL_DAYS") {
            self.auth.session_ttl_days = parse_number("WAKODE_AUTH_SESSION_TTL_DAYS", &value)?;
        }
        if let Some(value) = env("WAKODE_AUTH_SETUP_FROM_ANY_ADDRESS") {
            self.auth.setup_from_any_address =
                parse_bool("WAKODE_AUTH_SETUP_FROM_ANY_ADDRESS", &value)?;
        }
        if let Some(value) = env("WAKODE_DURATIONS_TIMEOUT_SECS") {
            self.durations.timeout_secs = parse_number("WAKODE_DURATIONS_TIMEOUT_SECS", &value)?;
        }
        if let Some(value) = env("WAKODE_DURATIONS_TAIL_PADDING_SECS") {
            self.durations.tail_padding_secs =
                parse_number("WAKODE_DURATIONS_TAIL_PADDING_SECS", &value)?;
        }
        Ok(())
    }
}

fn parse_number(name: &'static str, value: &str) -> Result<i64, ConfigError> {
    value.trim().parse().map_err(|_| ConfigError::NotANumber {
        name,
        value: value.to_owned(),
    })
}

fn parse_size(name: &'static str, value: &str) -> Result<usize, ConfigError> {
    value.trim().parse().map_err(|_| ConfigError::NotANumber {
        name,
        value: value.to_owned(),
    })
}

fn parse_bool(name: &'static str, value: &str) -> Result<bool, ConfigError> {
    match value.trim() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        other => Err(ConfigError::NotABool {
            name,
            value: other.to_owned(),
        }),
    }
}
```

`crates/wakode/src/main.rs` пока минимальный — подкоманды приезжают в задаче 13:

```rust
mod config;

fn main() {
    println!("wakode");
}
```

- [ ] **Step 5: Прогнать**

Run: `cargo test -p wakode`
Expected: PASS, двенадцать тестов.

**Четыре теста сверх блока кода**, каждый закрывает то, что иначе проявится не в CI, а на живом инстансе:

- `every_field_can_be_overridden_from_the_environment` — все девять полей разом. Обещание «`WAKODE_*` перекрывают сверху» дано на девять, а доказывалось на два: опечатка в имени переменной или выпавшая ветка не роняли ничего.
- `an_unknown_key_in_the_file_is_an_error` — опечатка в имени поля и в имени секции. Без `deny_unknown_fields` обе дают запуск с умолчаниями и ни слова в лог; это хуже отсутствующего файла, там хотя бы есть отказ.
- `a_negative_queue_size_is_an_error_not_a_huge_number` — `-1` в ёмкости очереди.
- `a_relative_missing_path_is_reported_absolutely` — путь в отказе абсолютный.

- [ ] **Step 6: Мутационная проверка**

| Мутация | Обязан упасть |
|---|---|
| `explicit.is_some()` → `false` (пропавший явный файл молча даёт умолчания) | `an_explicitly_named_file_that_is_missing_is_an_error` |
| `apply_env` вызывается **до** чтения файла | `the_environment_overrides_the_file` |
| `parse_number` возвращает `Ok(0)` при ошибке | `a_broken_numeric_value_names_the_variable` |
| `listen` по умолчанию `0.0.0.0:9000` | `defaults_apply_when_the_file_is_absent` |

- [ ] **Step 7: Коммит**

```bash
git add Cargo.toml crates/wakode
git commit -m "feat(cli): конфигурация с перекрытием из окружения"
```

---

### Task 7: Последовательность старта

**Files:**
- Create: `crates/wakode/src/startup.rs`
- Modify: `crates/wakode/src/main.rs`

**Interfaces:**
- Consumes: `Config`, `MasterKey`, `ApiKeyValue`, `EncryptedKey`, `SqliteStore`, `UserRepo::user_count`, `KeyRepo::first_key`.
- Produces: `Startup { pub store: SqliteStore, pub master_key: Option<MasterKey>, pub config: Config }`, `start(config: Config, master_key_raw: Option<String>) -> Result<Startup, StartupError>`, `StartupError`.

**Шесть шагов — они же порядок отказов.** Каждый падает со своей причиной, и причина называет, что именно не так.

Шаги 4 и 5 — не перестраховка. Без шага 4 сервер поднимется без мастер-ключа и будет отвечать `401` на все существующие ключи; владелец пойдёт искать поломку в хранилище. Без шага 5 то же самое произойдёт при подменённом ключе, только ещё незаметнее — конфигурация выглядит правильной.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wakode_store::{KeyRepo, NewApiKey, UserRepo};

    fn a_config(dir: &tempfile::TempDir) -> Config {
        let mut config = Config::default();
        config.database.path = dir.path().join("wakode.db");
        config
    }

    async fn a_user(store: &SqliteStore) -> uuid::Uuid {
        store
            .create_user(wakode_store::NewUser {
                login: "swrneko".to_owned(),
                email: None,
                password_hash: "непрозрачно".to_owned(),
                display_name: None,
                timezone: "Europe/Moscow".parse().unwrap(),
                timeout_secs: 900,
                is_admin: true,
            })
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn an_empty_database_starts_without_a_master_key() {
        // Инстанс, где ключей ещё не выдавали, обязан подниматься: иначе
        // первый запуск требовал бы ключа, которым нечего шифровать.
        let dir = tempfile::tempdir().unwrap();
        let started = start(a_config(&dir), None).await.unwrap();

        assert!(started.master_key.is_none());
        assert_eq!(started.store.user_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn migrations_are_applied_on_start() {
        let dir = tempfile::tempdir().unwrap();
        let config = a_config(&dir);
        let path = config.database.path.clone();

        start(config, None).await.unwrap();

        let conn = wakode_store::open(&path).unwrap();
        assert_eq!(wakode_store::schema_version(&conn).unwrap(), 1);
    }

    #[tokio::test]
    async fn keys_without_a_master_key_refuse_to_start() {
        // Шаг 4. Без него сервер поднялся бы и отвечал 401 на все ключи, а
        // владелец искал бы поломку в хранилище.
        let dir = tempfile::tempdir().unwrap();
        let master = MasterKey::generate();

        {
            let started = start(a_config(&dir), Some(master.to_base64())).await.unwrap();
            let user = a_user(&started.store).await;
            let value = ApiKeyValue::generate();
            started
                .store
                .create_key(NewApiKey {
                    user_id: user,
                    name: "ключ".to_owned(),
                    key_encrypted: value.encrypt(&master).unwrap().as_bytes().to_vec(),
                    key_lookup: value.lookup(&master),
                })
                .await
                .unwrap();
        }

        let err = start(a_config(&dir), None).await.unwrap_err();
        assert!(matches!(err, StartupError::MasterKeyMissing));
    }

    #[tokio::test]
    async fn a_different_master_key_refuses_to_start() {
        // Шаг 5. Подменённый ключ иначе даёт ту же тихую поломку, только
        // конфигурация при этом выглядит правильной.
        let dir = tempfile::tempdir().unwrap();
        let master = MasterKey::generate();

        {
            let started = start(a_config(&dir), Some(master.to_base64())).await.unwrap();
            let user = a_user(&started.store).await;
            let value = ApiKeyValue::generate();
            started
                .store
                .create_key(NewApiKey {
                    user_id: user,
                    name: "ключ".to_owned(),
                    key_encrypted: value.encrypt(&master).unwrap().as_bytes().to_vec(),
                    key_lookup: value.lookup(&master),
                })
                .await
                .unwrap();
        }

        let other = MasterKey::generate();
        let err = start(a_config(&dir), Some(other.to_base64()))
            .await
            .unwrap_err();
        assert!(matches!(err, StartupError::MasterKeyMismatch));
    }

    #[tokio::test]
    async fn the_right_master_key_starts_fine() {
        // Зеркало предыдущего: без него «отказ всегда» прошёл бы обе
        // проверки на отказ и выглядел бы правильным.
        let dir = tempfile::tempdir().unwrap();
        let master = MasterKey::generate();

        {
            let started = start(a_config(&dir), Some(master.to_base64())).await.unwrap();
            let user = a_user(&started.store).await;
            let value = ApiKeyValue::generate();
            started
                .store
                .create_key(NewApiKey {
                    user_id: user,
                    name: "ключ".to_owned(),
                    key_encrypted: value.encrypt(&master).unwrap().as_bytes().to_vec(),
                    key_lookup: value.lookup(&master),
                })
                .await
                .unwrap();
        }

        let started = start(a_config(&dir), Some(master.to_base64())).await.unwrap();

        // `master_key.is_some()` было бы тавтологией: ключ только что
        // передали на вход. Проверяем прямо: ключ на месте и открывается
        // тем же мастер-ключом, с которым старт прошёл.
        let stored = started.store.first_key().await.unwrap().unwrap();
        let opened = ApiKeyValue::decrypt(
            &EncryptedKey::from_bytes(stored.key_encrypted),
            started.master_key.as_ref().unwrap(),
        )
        .expect("ключ не открылся тем мастер-ключом, с которым старт прошёл");
        assert_eq!(opened.to_string().len(), 36);
    }

    #[tokio::test]
    async fn a_malformed_master_key_names_the_variable() {
        let dir = tempfile::tempdir().unwrap();
        let err = start(a_config(&dir), Some("не base64!".to_owned()))
            .await
            .unwrap_err();

        assert!(matches!(err, StartupError::MasterKeyInvalid(_)));
        assert!(format!("{err}").contains("WAKODE_MASTER_KEY"));
    }

    #[tokio::test]
    async fn a_zero_write_queue_is_refused_before_it_panics() {
        // `tokio::sync::mpsc::channel(0)` роняет процесс паникой изнутри
        // tokio, и сообщение будет про его внутренности. Ноль в конфиге —
        // правдоподобная опечатка, и отвечать на неё надо своей ошибкой.
        let dir = tempfile::tempdir().unwrap();
        let mut config = a_config(&dir);
        config.database.write_queue = 0;

        let err = start(config, None).await.unwrap_err();
        assert!(matches!(err, StartupError::WriteQueueZero));
    }
}
```

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p wakode startup`
Expected: FAIL — `start` не существует.

- [ ] **Step 3: Реализовать**

```rust
use thiserror::Error;
use wakode_auth::{ApiKeyValue, AuthError, EncryptedKey, MasterKey};
use wakode_store::{KeyRepo, SqliteStore, StoreError};

use crate::config::Config;

#[derive(Debug, Error)]
pub enum StartupError {
    #[error("переменная WAKODE_MASTER_KEY задана неверно: {0}")]
    MasterKeyInvalid(#[source] AuthError),

    #[error(
        "в базе есть выданные API-ключи, но WAKODE_MASTER_KEY не задана. \
         Без мастер-ключа расшифровать их нельзя, и сервер отвечал бы отказом \
         на каждый ключ — это выглядело бы как поломка хранилища"
    )]
    MasterKeyMissing,

    #[error(
        "WAKODE_MASTER_KEY не подходит к ключам в базе: пробный ключ не \
         расшифровался. Скорее всего задан мастер-ключ от другого инстанса"
    )]
    MasterKeyMismatch,

    #[error("ёмкость очереди записи должна быть больше нуля")]
    WriteQueueZero,

    #[error("хранилище: {0}")]
    Store(#[from] StoreError),
}

/// Поднятое состояние процесса.
#[derive(Debug)]
pub struct Startup {
    /// `expect`, а не `allow`: как только задача 9 поднимет на хранилище
    /// HTTP-слой, компилятор сообщит, что ожидание не оправдалось, и
    /// атрибут придётся снять.
    #[expect(dead_code, reason = "HTTP-слой поднимается в задаче 9")]
    pub store: SqliteStore,
    pub master_key: Option<MasterKey>,
    pub config: Config,
}

/// Шесть шагов старта, они же порядок отказов.
///
/// Конфиг к этому моменту уже прочитан — это шаг 1, он живёт в `config.rs`,
/// потому что нужен и подкомандам CLI, которым сервер поднимать незачем.
pub async fn start(config: Config, master_key_raw: Option<String>) -> Result<Startup, StartupError> {
    // Шаг 2: мастер-ключ. Его отсутствие пока не ошибка — решает шаг 4.
    let master_key = match master_key_raw {
        Some(raw) => Some(MasterKey::from_base64(&raw).map_err(StartupError::MasterKeyInvalid)?),
        None => None,
    };

    // Своя ошибка вместо паники изнутри tokio: ноль в конфиге —
    // правдоподобная опечатка, и сообщение про mpsc её не объяснит.
    if config.database.write_queue == 0 {
        return Err(StartupError::WriteQueueZero);
    }

    // Шаг 3: база и миграции. `SqliteStore::open` делает и то, и другое.
    let store = SqliteStore::open(&config.database.path, config.database.write_queue)?;

    // Шаги 4 и 5: сверка мастер-ключа с тем, что уже лежит в базе.
    if let Some(key) = store.first_key().await? {
        match &master_key {
            None => return Err(StartupError::MasterKeyMissing),
            Some(master) => {
                let encrypted = EncryptedKey::from_bytes(key.key_encrypted);
                ApiKeyValue::decrypt(&encrypted, master)
                    .map_err(|_| StartupError::MasterKeyMismatch)?;
            }
        }
    }

    // Шаг 6 — поднятие сервера — делает вызывающий: подкомандам CLI сервер
    // не нужен, а последовательность проверок нужна им ровно та же.
    Ok(Startup {
        store,
        master_key,
        config,
    })
}
```

- [ ] **Step 4: Прогнать**

Run: `cargo test -p wakode`
Expected: PASS, пятнадцать тестов.

- [ ] **Step 5: Мутационная проверка**

| Мутация | Обязан упасть |
|---|---|
| убрать ветку `None => MasterKeyMissing` | `keys_without_a_master_key_refuse_to_start` |
| `ApiKeyValue::decrypt(...)` результат игнорируется | `a_different_master_key_refuses_to_start` |
| `start` всегда возвращает `MasterKeyMismatch` | `the_right_master_key_starts_fine` |
| убрать проверку `write_queue == 0` | `a_zero_write_queue_is_refused_before_it_panics` — упадёт паникой, а не ошибкой; это тоже красный, но убедись, что видно причину |

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode
git commit -m "feat(cli): последовательность старта и её отказы"
```

---

### Task 8: Управляемое завершение писателя

**Files:**
- Modify: `crates/wakode-store/src/writer.rs`
- Modify: `crates/wakode-store/src/repo.rs`
- Modify: `crates/wakode-store/tests/repository.rs`

**Interfaces:**
- Produces: `WriteHandle::shutdown(&self) -> StoreResult<()>`; `SqliteStore::shutdown(&self) -> StoreResult<()>`.

**Это долг плана 2, вынесенный сюда явно.** Там остались две парковки, и обе закрываются одним изменением:

1. `StoreError::WriterGone` не покрыт ни одним тестом — до сих пор не было способа остановить писателя, не убив процесс.
2. Паника внутри `insert_heartbeats` молча убивает поток писателя навсегда, и единственный сигнал наружу — тот самый непокрытый `WriterGone`. Ничего не логируется.

**Решение.** В канал добавляется вариант-сигнал `Stop`. `shutdown` посылает его и ждёт подтверждения; писатель отвечает и выходит из цикла. Приёмник при этом уничтожается, и все последующие `try_send` дают `Closed` — то есть `WriterGone` становится достижимым состоянием, а не гипотезой.

Заодно тело цикла оборачивается в `catch_unwind`: паника превращается в `Err(TaskPanicked)` в ответе, а писатель продолжает жить. Молчаливая смерть перестаёт существовать как режим. `StoreError::TaskPanicked` уже объявлен — новых вариантов не заводим.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_writer_that_was_shut_down_reports_it_is_gone() {
    // Долг плана 2: до появления shutdown у `WriterGone` не было ни одного
    // достижимого пути, и вариант ошибки существовал на веру.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 8).unwrap();
    let user = store.create_user(a_user("swrneko")).await.unwrap();

    store
        .record_heartbeats(user.id, vec![incoming(1_000, "f.rs", None)], user.timezone)
        .await
        .unwrap();

    store.shutdown().await.unwrap();

    let err = store
        .record_heartbeats(user.id, vec![incoming(2_000, "f.rs", None)], user.timezone)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::WriterGone), "получили {err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_lets_the_writer_finish_what_it_accepted() {
    // Остановка не должна терять уже принятое: cli, получивший успех,
    // стёр отметки у себя, и «приняли, но не записали» — потеря.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wakode.db");
    let store = SqliteStore::open(&path, 64).unwrap();
    let user = store.create_user(a_user("swrneko")).await.unwrap();

    let batch: Vec<IncomingHeartbeat> = (0..5_000)
        .map(|i| incoming(1_000 + i, "f.rs", Some("wakode")))
        .collect();
    let report = store
        .record_heartbeats(user.id, batch, user.timezone)
        .await
        .unwrap();
    assert_eq!(report.inserted(), 5_000);

    store.shutdown().await.unwrap();

    let conn = wakode_store::open(&path).unwrap();
    let loaded = load_heartbeats(
        &conn,
        user.id,
        Micros::from_secs(0),
        Micros::from_secs(999_999),
    )
    .unwrap();
    assert_eq!(loaded.len(), 5_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_twice_is_not_an_error() {
    // Останов зовут и при штатном завершении, и из обработчика сигнала;
    // второй вызов не должен превращаться в отказ.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 8).unwrap();

    store.shutdown().await.unwrap();
    store.shutdown().await.unwrap();
}
```

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p wakode-store --test repository shutdown`
Expected: FAIL — метода нет.

- [ ] **Step 3: Реализовать**

В `crates/wakode-store/src/writer.rs` заменить `WriteJob` на перечисление и обновить цикл:

```rust
/// Заявка писателю.
enum WriteJob {
    Insert {
        user: Uuid,
        batch: Vec<IncomingHeartbeat>,
        tz: Tz,
        reply: oneshot::Sender<StoreResult<InsertReport>>,
    },
    /// Сигнал остановиться. Писатель отвечает и выходит из цикла, уничтожая
    /// приёмник, — после этого все отправители получают `WriterGone`.
    Stop {
        ack: oneshot::Sender<()>,
    },
}
```

Цикл:

```rust
    std::thread::spawn(move || {
        let mut stop_ack: Option<oneshot::Sender<()>> = None;

        while let Some(job) = rx.blocking_recv() {
            match job {
                WriteJob::Stop { ack } => {
                    // Подтверждение уходит не здесь, а после выхода из
                    // цикла — когда соединение уже отпущено. Ответить
                    // раньше значило бы сказать «остановился», продолжая
                    // держать базу.
                    stop_ack = Some(ack);
                    break;
                }
                WriteJob::Insert { user, batch, tz, reply } => {
                    // Паника внутри вставки не должна убивать писателя
                    // навсегда: до этого она уносила поток молча, и
                    // единственным сигналом наружу был `WriterGone`,
                    // неотличимый от штатной остановки.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        insert_heartbeats(&mut conn, &interner, user, &batch, tz)
                    }))
                    .unwrap_or(Err(StoreError::TaskPanicked));

                    let _ = reply.send(result);
                }
            }
        }

        // Соединение закрывается до подтверждения: получивший `ack` вправе
        // считать, что база отпущена и файл можно переоткрывать.
        drop(conn);
        if let Some(ack) = stop_ack {
            let _ = ack.send(());
        }
    });
```

Метод остановки:

```rust
impl WriteHandle {
    /// Остановить писателя, дождавшись, пока он разберёт принятое.
    ///
    /// Повторный вызов не ошибка: останов зовут и при штатном завершении,
    /// и из обработчика сигнала, и эти пути пересекаются.
    pub async fn shutdown(&self) -> StoreResult<()> {
        let (ack, wait) = oneshot::channel();
        match self.tx.send(WriteJob::Stop { ack }).await {
            Ok(()) => {
                let _ = wait.await;
                Ok(())
            }
            // Приёмник уже уничтожен — писатель остановлен, цель достигнута.
            Err(_) => Ok(()),
        }
    }
}
```

Здесь `send().await`, а не `try_send`: останов обязан дойти, а очередь может быть полна. Правило «не ждать места в очереди» касается отметок, которые в этом случае лучше отклонить с `503`; сигнал остановки отклонять некуда.

В `crates/wakode-store/src/repo.rs`:

```rust
impl SqliteStore {
    /// Остановить пишущую задачу и дождаться, пока она разберёт принятое.
    pub async fn shutdown(&self) -> StoreResult<()> {
        self.writer.shutdown().await
    }
}
```

- [ ] **Step 4: Прогнать**

Run: `cargo test -p wakode-store`
Expected: PASS, пять новых тестов.

- [ ] **Step 5: Мутационная проверка**

| Мутация | Обязан упасть |
|---|---|
| `Stop` отвечает `ack`, но не делает `break` | `a_writer_that_was_shut_down_reports_it_is_gone` |
| `shutdown` возвращает `Err(WriterGone)` при закрытом канале | `shutdown_twice_is_not_an_error` |
| `shutdown` использует `try_send` вместо `send().await` | проверь на полной очереди; если ни один тест не краснеет — это пробел покрытия, доложи его |
| убрать `catch_unwind` | ни один существующий тест не упадёт — напиши это прямо и предложи, чем закрыть |

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-store
git commit -m "feat(store): управляемая остановка писателя и переживание паники"
```

---

### Task 9: Каркас `wakode-api` и запуск сервера

**Files:**
- Create: `crates/wakode-api/Cargo.toml`
- Create: `crates/wakode-api/src/lib.rs`
- Create: `crates/wakode-api/src/state.rs`
- Create: `crates/wakode-api/src/error.rs`
- Create: `crates/wakode-api/src/health.rs`
- Create: `crates/wakode-api/src/compat/mod.rs`
- Create: `crates/wakode-api/src/internal/mod.rs`
- Modify: `crates/wakode/src/main.rs` (только комментарий: `wakode-api` теперь существует, но поднимает его подкоманда `serve` из задачи 14)
- Modify: `crates/wakode/src/startup.rs` (ожидание `dead_code` переадресовано задаче 14 и сужено до `not(test)`)
- Modify: `Cargo.toml` (корень)

**Interfaces:**
- Consumes: `SqliteStore`, `MasterKey`, `Config`.
- Produces: `AppState { store: SqliteStore, master_key: Option<MasterKey>, registration: bool, session_ttl_days: i64, setup_from_any_address: bool }`, `AppState::new(...)`; `ApiError` с `IntoResponse`; `router(state: AppState) -> axum::Router`; `serve(listener: TcpListener, state: AppState) -> std::io::Result<()>`.

**Почему `AppState` не держит весь `Config`.** Слою HTTP нужны четыре поля из него, а не адрес прослушивания и путь к базе. Узкое состояние — это ещё и защита: `Config` со временем обрастёт полями, и часть из них будет чувствительной.

**Обещание «тело всегда JSON» держится двумя вызовами, а не одним.** `fallback` ловит только несовпадение пути; путь, у которого есть обработчик на другой метод, до него не доходит, и axum отдаёт пустой `405`. Нужен ещё `method_not_allowed_fallback`, и ставить его надо **после** всех `route`: он раздаёт запасной обработчик уже зарегистрированным маршрутам, поэтому маршрут, добавленный ниже него, останется с пустым ответом.

**`StoreError::WriteQueueFull` — это `503`, а не `500`.** Обещание записано в докстринге `spawn_writer` («отказ здесь превращается в 503 с `Retry-After`, и cli дошлёт отметки из собственной очереди»), но жило в чужом крейте и не держалось ничем. Разница поведенческая: на `500` клиент отметки выбросит, на `503` с `Retry-After` — дошлёт. Отсюда отдельный вариант `ApiError::Unavailable`.

**`AppState` выводит `Debug` вручную.** `SqliteStore` печатает число строк словаря, а не сам словарь (правка финального ревью плана 2), но `master_key` производный `Debug` напечатал бы, будь он выведен — `MasterKey` от этого защищён своей реализацией. Тест сторожит связку целиком, потому что именно на стыке таких решений утечка и появилась в прошлый раз.

- [ ] **Step 1: Завести крейт**

В корневой `Cargo.toml` добавь `"crates/wakode-api"` в `members`. `crates/wakode-api/Cargo.toml`:

```toml
[package]
name = "wakode-api"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
axum.workspace = true
axum-extra = { workspace = true, features = ["cookie"] }
chrono-tz.workspace = true
serde = { workspace = true, features = ["derive"] }
tokio = { workspace = true, features = ["rt", "net"] }
tower-http = { workspace = true, features = ["catch-panic", "trace"] }
tracing.workspace = true
uuid = { workspace = true, features = ["v7"] }
wakode-auth.workspace = true
wakode-core.workspace = true
wakode-store.workspace = true

[dev-dependencies]
base64.workspace = true
http-body-util = "0.1"
serde_json.workspace = true
tempfile.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
tower = { workspace = true, features = ["util"] }
```

`base64` здесь пока в dev-зависимостях — тестам он нужен, чтобы собрать заголовок `Basic`. В задаче 10 он переезжает в `[dependencies]`: разбор того же заголовка в рабочем коде без него не обойдётся.

Криптографических крейтов нет ни там, ни там. `base64` кодированием не является — это транспортное представление, и путать его с шифрованием не надо.

Криптографических крейтов здесь нет и не будет: вся криптография живёт в `wakode-auth`, и список зависимостей — способ это проверить.

- [ ] **Step 2: Написать падающие тесты**

`crates/wakode-api/tests/api.rs`:

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use wakode_api::{router, AppState};
use wakode_store::SqliteStore;

pub fn a_state(dir: &tempfile::TempDir) -> AppState {
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap();
    AppState::new(store, None, false, 30, false)
}

#[tokio::test]
async fn healthz_answers_ok() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ok");
}

#[tokio::test]
async fn an_unknown_path_is_a_json_error_not_an_empty_404() {
    // Совместимые клиенты разбирают тело ответа. Пустая 404 без тела
    // выглядит для них как сломанный сервер, а не как «нет такого пути».
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(Request::builder().uri("/нет-такого").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("error").is_some(), "нет поля error: {json}");
}

#[tokio::test]
async fn state_debug_prints_neither_the_master_key_nor_the_dictionary() {
    // Урок финального ревью плана 2: утечка появилась на стыке трёх
    // по отдельности разумных решений, и увидеть её можно было только
    // на собранном состоянии целиком.
    let dir = tempfile::tempdir().unwrap();
    let master = wakode_auth::MasterKey::generate();
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap();
    let state = AppState::new(store, Some(master.clone()), false, 30, false);

    let dump = format!("{state:?}");
    assert!(!dump.contains(&master.to_base64()), "мастер-ключ утёк: {dump}");
}
```

- [ ] **Step 3: Убедиться, что падает**

Run: `cargo test -p wakode-api`
Expected: FAIL — крейта и типов нет.

- [ ] **Step 4: Реализовать**

`crates/wakode-api/src/error.rs`:

```rust
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

/// Ошибка, отдаваемая клиенту.
///
/// Тело всегда JSON с полем `error`: совместимые клиенты разбирают ответ,
/// и пустое тело для них неотличимо от сломанного сервера.
#[derive(Debug)]
pub enum ApiError {
    /// Учётные данные не предъявлены или не подошли. Причина уезжает
    /// клиенту текстом: «ключ отозван» и «ключа не существует» — разные
    /// ответы, и склеивать их значит прятать от владельца, что произошло.
    Unauthorized(&'static str),
    Forbidden(&'static str),
    NotFound,
    BadRequest(String),
    Internal,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            ApiError::Unauthorized(why) => (StatusCode::UNAUTHORIZED, *why),
            ApiError::Forbidden(why) => (StatusCode::FORBIDDEN, *why),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "нет такого пути"),
            ApiError::BadRequest(why) => (StatusCode::BAD_REQUEST, why.as_str()),
            ApiError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
        };
        (status, Json(ErrorBody { error: message })).into_response()
    }
}

impl From<wakode_store::StoreError> for ApiError {
    /// Ошибки хранилища наружу не пробрасываются текстом: они содержат
    /// подробности схемы и путей, которые клиенту знать незачем.
    fn from(err: wakode_store::StoreError) -> Self {
        tracing::error!(error = %err, "ошибка хранилища");
        ApiError::Internal
    }
}
```

`crates/wakode-api/src/state.rs`:

```rust
use wakode_auth::MasterKey;
use wakode_store::SqliteStore;

/// Состояние приложения.
///
/// Держит четыре поля конфигурации, а не весь `Config`: слою HTTP не нужны
/// ни адрес прослушивания, ни путь к базе, а узкое состояние — ещё и
/// защита от того, что в конфиг со временем приедет что-то чувствительное.
#[derive(Clone)]
pub struct AppState {
    pub store: SqliteStore,
    pub master_key: Option<MasterKey>,
    pub registration: bool,
    pub session_ttl_days: i64,
    pub setup_from_any_address: bool,
}

impl AppState {
    pub fn new(
        store: SqliteStore,
        master_key: Option<MasterKey>,
        registration: bool,
        session_ttl_days: i64,
        setup_from_any_address: bool,
    ) -> Self {
        Self {
            store,
            master_key,
            registration,
            session_ttl_days,
            setup_from_any_address,
        }
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("store", &self.store)
            .field("master_key", &self.master_key.is_some())
            .field("registration", &self.registration)
            .field("session_ttl_days", &self.session_ttl_days)
            .field("setup_from_any_address", &self.setup_from_any_address)
            .finish()
    }
}
```

`crates/wakode-api/src/health.rs`:

```rust
/// Проба живости. Ничего не проверяет, кроме того, что процесс отвечает:
/// проверка базы здесь превратила бы healthz в источник нагрузки.
pub async fn healthz() -> &'static str {
    "ok"
}
```

`crates/wakode-api/src/lib.rs`:

```rust
//! HTTP-слой wakode.
//!
//! Криптографии здесь нет: она целиком в `wakode-auth`, и список
//! зависимостей этого крейта — способ это проверить.

pub mod compat;
pub mod error;
pub mod health;
pub mod internal;
pub mod state;

pub use error::ApiError;
pub use state::AppState;

use axum::routing::get;
use axum::Router;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health::healthz))
        .fallback(|| async { ApiError::NotFound })
        .with_state(state)
}

/// Поднять сервер на готовом слушателе.
///
/// `into_make_service_with_connect_info` обязателен: экран первичной
/// настройки смотрит на адрес клиента, а без этого `ConnectInfo` в
/// обработчике не извлечётся.
pub async fn serve(listener: tokio::net::TcpListener, state: AppState) -> std::io::Result<()> {
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
}
```

`compat/mod.rs` и `internal/mod.rs` — по одной строке комментария о том, что их наполняет план 3b.

- [ ] **Step 5: Прогнать**

Run: `cargo test -p wakode-api`
Expected: PASS, семь тестов.

- [ ] **Step 6: Мутационная проверка**

| Мутация | Обязан упасть |
|---|---|
| убрать `.fallback(...)` | `an_unknown_path_is_a_json_error_not_an_empty_404` |
| убрать `.method_not_allowed_fallback(...)` | `a_wrong_method_is_a_json_error_too` |
| в `Debug` для `AppState` печатать `&self.master_key` | `state_debug_prints_neither_the_master_key_nor_the_dictionary` |
| `Interner::fmt` печатает `by_id` целиком | `state_debug_prints_neither_the_master_key_nor_the_dictionary` |
| `healthz` возвращает пустую строку | `healthz_answers_ok`, `serve_actually_answers_on_a_real_socket` |
| тело `serve` выпотрошено до `Ok(())` | `serve_actually_answers_on_a_real_socket` |
| `From<StoreError>` отдаёт `BadRequest(err.to_string())` | `a_storage_error_does_not_leak_its_text_to_the_client` |
| `WriteQueueFull` отображается в `Internal` | `a_full_write_queue_is_a_retryable_503_not_a_500` |
| `Retry-After` не ставится | `a_full_write_queue_is_a_retryable_503_not_a_500` |
| тело ошибки отдаётся как `text/plain` | три теста, читающих тело как JSON |

Мутация, которую **не ловит ничто**: `into_make_service()` вместо
`into_make_service_with_connect_info::<SocketAddr>()`. Потребителя
`ConnectInfo` до задачи 12 нет, и заводить его ради теста — шов ради шва.
Осознанный долг: тест на экран первичной настройки покраснеет там.

- [ ] **Step 7: Коммит**

```bash
git add Cargo.toml crates/wakode-api crates/wakode
git commit -m "feat(api): каркас HTTP-слоя и проба живости"
```

---

### Task 10: Экстрактор API-ключа

**Files:**
- Create: `crates/wakode-api/src/auth/mod.rs`
- Create: `crates/wakode-api/src/auth/api_key.rs`
- Modify: `crates/wakode-api/src/lib.rs`
- Modify: `crates/wakode-api/tests/api.rs`

**Interfaces:**
- Consumes: `AppState`, `ApiError`, `ApiKeyValue`, `KeyRepo::key_by_lookup`, `UserRepo::user_by_id`.
- Produces: `KeyAuth { pub user: wakode_store::User, pub key_id: uuid::Uuid }`, реализующий `FromRequestParts<AppState>`.

**Три источника ключа, один разбор.** `Authorization: Basic base64(ключ)`, `Authorization: Bearer ключ` и `?api_key=`. Базовая схема — та, что описана в спеке; `Bearer` встречается у части плагинов и стоит одной строки. Префикс `waka_` срезает `ApiKeyValue::parse` — знание о формате ключа живёт в `wakode-auth` целиком.

**Отозванный ключ отвергается со своей причиной.** Хранилище умеет отличать «ключа не было» от «ключ отозван», и терять это различие на пути наружу нельзя: владелец, отозвавший ключ и забывший об этом, иначе будет искать поломку в редакторе.

**Без мастер-ключа ключ проверить нечем** — отпечаток считается под ним. Это состояние недостижимо после шага 4 старта (он не даёт подняться с ключами в базе и без мастер-ключа), но экстрактор обязан ответить осмысленно, а не паниковать: недостижимое сегодня становится достижимым при первом же рефакторинге старта.

**Источники соперничают, а не выбираются по порядку.** Найденное в заголовке не отменяет query-параметр. Сценарий не выдуманный: владелец ставит перед wakode прокси с собственным basic-auth, cli кладёт ключ в query — заголовок разбирается успешно и содержит `admin`. Пока побеждал первый источник, такая установка отвечала `401` на каждую отметку, причём с формулировкой «API-ключ не предъявлен», хотя он был предъявлен. Отсюда: `candidates(parts) -> Vec<String>` собирает всё похожее на ключ, а решает `ApiKeyValue::parse`. Различие «не предъявлен» / «неверный формат» при этом сохраняется — пустой список против непустого без единого разобравшегося.

**Ключ без владельца — это `500`, а не `401`.** `api_keys.user_id` объявлен `REFERENCES users(id) ON DELETE CASCADE` при включённом `PRAGMA foreign_keys`, так что состояние недостижимо; ветка оставлена страховкой на случай базы, открытой без внешних ключей. Отвечать на нарушенный инвариант хранилища «вы не авторизованы» значило бы отправить владельца чинить ключ вместо базы.

- [ ] **Step 1: Написать падающие тесты**

Добавь в `crates/wakode-api/tests/api.rs`:

```rust
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use wakode_auth::{ApiKeyValue, MasterKey};
use wakode_store::{KeyRepo, NewApiKey, NewUser, UserRepo};

/// Состояние с мастер-ключом, пользователем и выданным ему ключом.
async fn a_state_with_a_key(dir: &tempfile::TempDir) -> (AppState, ApiKeyValue) {
    let master = MasterKey::generate();
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap();

    let user = store
        .create_user(NewUser {
            login: "swrneko".to_owned(),
            email: None,
            password_hash: "непрозрачно".to_owned(),
            display_name: None,
            timezone: "Europe/Moscow".parse().unwrap(),
            timeout_secs: 900,
            is_admin: false,
        })
        .await
        .unwrap();

    let value = ApiKeyValue::generate();
    store
        .create_key(NewApiKey {
            user_id: user.id,
            name: "рабочий ноутбук".to_owned(),
            key_encrypted: value.encrypt(&master).unwrap().as_bytes().to_vec(),
            key_lookup: value.lookup(&master),
        })
        .await
        .unwrap();

    (AppState::new(store, Some(master), false, 30, false), value)
}

/// Пробный маршрут: единственный смысл — потребовать `KeyAuth`.
fn app_requiring_a_key(state: AppState) -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route("/кто-я", get(|auth: wakode_api::auth::KeyAuth| async move { auth.user.login }))
        .with_state(state)
}

#[tokio::test]
async fn a_valid_key_in_the_basic_header_identifies_the_user() {
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;
    let app = app_requiring_a_key(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/кто-я")
                .header("authorization", format!("Basic {}", STANDARD.encode(value.to_string())))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"swrneko");
}

#[tokio::test]
async fn the_waka_prefix_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;
    let app = app_requiring_a_key(state);

    let prefixed = STANDARD.encode(format!("waka_{value}"));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/кто-я")
                .header("authorization", format!("Basic {prefixed}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_key_in_the_query_string_works_too() {
    // wakatime-cli умеет и так; ось «плагины пишут к нам» на этом держится.
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;
    let app = app_requiring_a_key(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/кто-я?api_key={value}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn no_credentials_at_all_is_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let app = app_requiring_a_key(state);

    let response = app
        .oneshot(Request::builder().uri("/кто-я").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_unknown_key_is_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let app = app_requiring_a_key(state);

    let stranger = ApiKeyValue::generate();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/кто-я?api_key={stranger}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_revoked_key_says_so_instead_of_pretending_it_never_existed() {
    // Владелец, отозвавший ключ и забывший об этом, иначе будет искать
    // поломку в редакторе. Хранилище это различает — терять различие
    // на пути наружу нельзя.
    let dir = tempfile::tempdir().unwrap();
    let (state, value) = a_state_with_a_key(&dir).await;

    let key = state.store.first_key().await.unwrap().unwrap();
    state.store.revoke_key(key.id).await.unwrap();

    let app = app_requiring_a_key(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/кто-я?api_key={value}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let message = json["error"].as_str().unwrap();
    assert!(message.contains("отозван"), "причина не названа: {message}");
}

#[tokio::test]
async fn without_a_master_key_the_answer_is_honest_not_a_panic() {
    // После шага 4 старта это состояние недостижимо. Но недостижимое
    // сегодня становится достижимым при первом рефакторинге старта, и
    // паника в экстракторе — худший способ об этом узнать.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap();
    let app = app_requiring_a_key(AppState::new(store, None, false, 30, false));

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/кто-я?api_key={}", ApiKeyValue::generate()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
```

Добавь в dev-зависимости `serde_json.workspace = true` и `base64.workspace = true`.

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p wakode-api`
Expected: FAIL — `KeyAuth` не существует.

- [ ] **Step 3: Реализовать**

`crates/wakode-api/src/auth/api_key.rs`:

```rust
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use uuid::Uuid;
use wakode_auth::ApiKeyValue;
use wakode_store::{KeyRepo, User, UserRepo};

use crate::error::ApiError;
use crate::state::AppState;

/// Пользователь, опознанный по API-ключу.
#[derive(Debug, Clone)]
pub struct KeyAuth {
    pub user: User,
    pub key_id: Uuid,
}

impl FromRequestParts<AppState> for KeyAuth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let raw = extract_raw_key(parts)
            .ok_or(ApiError::Unauthorized("API-ключ не предъявлен"))?;

        let value = ApiKeyValue::parse(&raw)
            .ok_or(ApiError::Unauthorized("API-ключ имеет неверный формат"))?;

        // Отпечаток считается под мастер-ключом. Его отсутствие — не вина
        // клиента, поэтому 500, а не 401: сервер не в состоянии проверить.
        let Some(master) = state.master_key.as_ref() else {
            tracing::error!("проверка ключа запрошена без мастер-ключа");
            return Err(ApiError::Internal);
        };

        let found = state.store.key_by_lookup(value.lookup(master)).await?;
        let key = found.ok_or(ApiError::Unauthorized("API-ключ не найден"))?;

        if key.revoked_at.is_some() {
            return Err(ApiError::Unauthorized("API-ключ отозван"));
        }

        let user = state
            .store
            .user_by_id(key.user_id)
            .await?
            .ok_or(ApiError::Unauthorized("владелец ключа не найден"))?;

        Ok(KeyAuth {
            user,
            key_id: key.id,
        })
    }
}

/// Достать значение ключа из запроса.
///
/// Три источника: `Basic` (описан спекой WakaTime), `Bearer` (встречается у
/// части плагинов) и query-параметр `api_key`, которым пользуется cli.
/// Префикс `waka_` здесь не трогаем — его срезает `ApiKeyValue::parse`,
/// чтобы знание о формате ключа не размазывалось по двум крейтам.
fn extract_raw_key(parts: &Parts) -> Option<String> {
    if let Some(header) = parts.headers.get(axum::http::header::AUTHORIZATION) {
        let text = header.to_str().ok()?;
        if let Some(encoded) = text.strip_prefix("Basic ") {
            let decoded = STANDARD.decode(encoded.trim()).ok()?;
            let text = String::from_utf8(decoded).ok()?;
            // Basic-схема допускает `логин:пароль`; wakatime-cli шлёт голый
            // ключ, но отрезать хвост после двоеточия дешевле, чем гадать.
            return Some(text.split(':').next().unwrap_or(&text).to_owned());
        }
        if let Some(token) = text.strip_prefix("Bearer ") {
            return Some(token.trim().to_owned());
        }
    }

    let query = parts.uri.query()?;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == "api_key").then(|| value.to_owned())
    })
}
```

`crates/wakode-api/src/auth/mod.rs`:

```rust
pub mod api_key;

pub use api_key::KeyAuth;
```

В `lib.rs` добавь `pub mod auth;`.

- [ ] **Step 4: Прогнать**

Run: `cargo test -p wakode-api`
Expected: PASS, 23 теста (7 от задачи 9 + 16 здесь).

- [ ] **Step 5: Мутационная проверка**

| Мутация | Обязан упасть |
|---|---|
| убрать проверку `key.revoked_at.is_some()` | `a_revoked_key_says_so_instead_of_pretending_it_never_existed` |
| `Unauthorized("API-ключ отозван")` → `Unauthorized("API-ключ не найден")` | тот же |
| разбор игнорирует query-параметр | `a_key_in_the_query_string_works_too` |
| разбор игнорирует заголовок целиком | `a_valid_key_in_the_basic_header_identifies_the_user`, `the_waka_prefix_is_accepted`, `the_authorization_scheme_is_case_insensitive` |
| ветку без мастер-ключа заменить на `panic!` | `without_a_master_key_the_answer_is_honest_not_a_panic` |
| `user_by_id(key.user_id)` → владелец первого ключа | `each_key_identifies_its_own_owner` |
| `key_id: key.id` → `Uuid::nil()` | `key_auth_carries_the_id_of_the_key_that_opened_it` |
| сравнение имени схемы вернуть к регистрозависимому | `the_authorization_scheme_is_case_insensitive` |
| побеждает первый источник (`or_else` вместо списка) | `a_foreign_authorization_header_does_not_cancel_the_key_in_the_query` |
| разбирается только первый кандидат | тот же |
| убрать разбор формы `логин:пароль` | `the_basic_scheme_accepts_the_login_password_form` |
| убрать `trim` перед декодированием base64 | `spaces_around_the_credentials_are_tolerated_in_both_schemes` |
| брать любой query-параметр, а не `api_key` | `only_the_parameter_named_api_key_is_taken` |
| «неверный формат» → «не найден» | `a_malformed_key_says_so_instead_of_pretending_it_was_not_found` |
| убрать различие «не предъявлен» / «неверный формат» | `only_the_parameter_named_api_key_is_taken` |

Не покрыта и покрыта быть не может ветка «у ключа нет владельца»: публичного пути к ней нет (см. про `ON DELETE CASCADE` выше).

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-api
git commit -m "feat(api): опознание пользователя по API-ключу"
```

---

### Task 11: Экстрактор сессии

**Files:**
- Create: `crates/wakode-api/src/auth/session.rs`
- Modify: `crates/wakode-api/src/auth/mod.rs`
- Modify: `crates/wakode-api/tests/api.rs`

**Interfaces:**
- Consumes: `AppState`, `ApiError`, `SessionToken`, `SessionRepo::session_by_token_hash`, `UserRepo::user_by_id`.
- Produces: `SessionAuth { pub user: wakode_store::User, pub session_id: uuid::Uuid }`, реализующий `FromRequestParts<AppState>`; константа `SESSION_COOKIE`.

**Истечение проверяется здесь.** Хранилище отдаёт `expires_at` как есть — доменной валидации в нём нет по построению. Значит, просроченная сессия из базы приходит как обычная, и не проверить её здесь означает пускать по вечным сессиям.

**Граница срока выражается только чистой функцией.** `is_expired(expires_at, now)` вынесена наружу и покрыта юнит-тестом обеих сторон границы и самой границы: попасть запросом ровно в микросекунду `expires_at` через живые часы нельзя, поэтому мутация `<=` → `<` не роняет ни одного интеграционного теста. Равенство решено явно как «истекла».

**Сломанные часы обязаны запирать дверь, а не открывать её.** `now_at(clock)` отдаёт `None` на часах до эпохи (голая VM без RTC, битый контейнер) — и это `500`. Подставить вместо них ноль, как предполагалось изначально, значило бы выключить проверку срока целиком: при `now = 0` не истекла ни одна сессия. Переполнение `i64` насыщается вверх, а не заворачивается через `as i64` в отрицательное — отказ в ту же безопасную сторону.

**Сроки в тестах считаются от настоящего «сейчас», а не константами.** С `Micros::from_secs(4_000_000_000)` набор доказывал лишь то, что часы сервера где-то между 1970 и 2096 годом: часы, замороженные на 2033-м или отставшие на десять лет, проходили весь набор зелёными. На инстансе с уехавшим RTC сессия со сроком в 30 дней при этом не истекает никогда, и украденная cookie живёт вечно.

- [ ] **Step 1: Написать падающие тесты**

```rust
use axum_extra::extract::cookie::Cookie;
use wakode_auth::SessionToken;
use wakode_core::Micros;
use wakode_store::{NewSession, SessionRepo};

fn app_requiring_a_session(state: AppState) -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route(
            "/я",
            get(|auth: wakode_api::auth::SessionAuth| async move { auth.user.login }),
        )
        .with_state(state)
}

/// Завести сессию с заданным сроком и вернуть её токен.
async fn a_session(state: &AppState, user_id: uuid::Uuid, expires_at: Micros) -> SessionToken {
    let token = SessionToken::generate();
    state
        .store
        .create_session(NewSession {
            user_id,
            token_hash: token.hash(),
            user_agent: Some("Firefox".to_owned()),
            expires_at,
        })
        .await
        .unwrap();
    token
}

#[tokio::test]
async fn a_live_session_identifies_the_user() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let token = a_session(&state, user.id, Micros::from_secs(4_000_000_000)).await;

    let app = app_requiring_a_session(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/я")
                .header("cookie", Cookie::new(wakode_api::auth::SESSION_COOKIE, token.to_string()).to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"swrneko");
}

#[tokio::test]
async fn an_expired_session_is_refused() {
    // Хранилище отдаёт `expires_at` как есть — доменной валидации в нём нет
    // по построению. Не проверить срок здесь значит пускать по вечным
    // сессиям.
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let token = a_session(&state, user.id, Micros::from_secs(1)).await;

    let app = app_requiring_a_session(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/я")
                .header("cookie", Cookie::new(wakode_api::auth::SESSION_COOKIE, token.to_string()).to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["error"].as_str().unwrap().contains("истек"));
}

#[tokio::test]
async fn a_revoked_session_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let user = state.store.user_by_login("swrneko").await.unwrap().unwrap();
    let token = a_session(&state, user.id, Micros::from_secs(4_000_000_000)).await;

    let found = state
        .store
        .session_by_token_hash(token.hash())
        .await
        .unwrap()
        .unwrap();
    state.store.revoke_session(found.id).await.unwrap();

    let app = app_requiring_a_session(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/я")
                .header("cookie", Cookie::new(wakode_api::auth::SESSION_COOKIE, token.to_string()).to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["error"].as_str().unwrap().contains("отозв"));
}

#[tokio::test]
async fn no_cookie_is_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let app = app_requiring_a_session(state);

    let response = app
        .oneshot(Request::builder().uri("/я").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_unknown_token_is_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = a_state_with_a_key(&dir).await;
    let stranger = SessionToken::generate();
    let app = app_requiring_a_session(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/я")
                .header("cookie", Cookie::new(wakode_api::auth::SESSION_COOKIE, stranger.to_string()).to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p wakode-api session`
Expected: FAIL — `SessionAuth` не существует.

- [ ] **Step 3: Реализовать**

```rust
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;
use uuid::Uuid;
use wakode_auth::SessionToken;
use wakode_store::{SessionRepo, User, UserRepo};

use crate::error::ApiError;
use crate::state::AppState;

/// Имя cookie с токеном сессии.
pub const SESSION_COOKIE: &str = "wakode_session";

/// Пользователь, опознанный по сессии.
#[derive(Debug, Clone)]
pub struct SessionAuth {
    pub user: User,
    pub session_id: Uuid,
}

impl FromRequestParts<AppState> for SessionAuth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let jar = CookieJar::from_headers(&parts.headers);
        let raw = jar
            .get(SESSION_COOKIE)
            .ok_or(ApiError::Unauthorized("сессия не предъявлена"))?;

        let token = SessionToken::parse(raw.value())
            .ok_or(ApiError::Unauthorized("токен сессии имеет неверный формат"))?;

        let session = state
            .store
            .session_by_token_hash(token.hash())
            .await?
            .ok_or(ApiError::Unauthorized("сессия не найдена"))?;

        if session.revoked_at.is_some() {
            return Err(ApiError::Unauthorized("сессия отозвана"));
        }

        // Срок проверяется здесь: хранилище отдаёт `expires_at` как есть,
        // доменной валидации в нём нет по построению.
        if session.expires_at <= wakode_core::Micros::new(now_micros()) {
            return Err(ApiError::Unauthorized("сессия истекла"));
        }

        let user = state
            .store
            .user_by_id(session.user_id)
            .await?
            .ok_or(ApiError::Unauthorized("владелец сессии не найден"))?;

        Ok(SessionAuth {
            user,
            session_id: session.id,
        })
    }
}

fn now_micros() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}
```

Добавь `pub mod session; pub use session::{SessionAuth, SESSION_COOKIE};` в `auth/mod.rs`.

- [ ] **Step 4: Прогнать**

Run: `cargo test -p wakode-api`
Expected: PASS, 33 интеграционных теста и 2 юнит-теста в `session.rs`.

- [ ] **Step 5: Мутационная проверка**

| Мутация | Обязан упасть |
|---|---|
| убрать проверку срока | `an_expired_session_is_refused` |
| убрать проверку `revoked_at` | `a_revoked_session_says_so`, `a_session_both_revoked_and_expired_says_it_was_revoked` |
| искать сессию по `raw.value()` вместо `token.hash()` | `a_live_session_identifies_the_user` и ещё шесть |
| `<=` заменить на `<` в `is_expired` | `the_boundary_of_expiry_is_pinned` (и только он) |
| `user_by_id(session.user_id)` → владелец первой сессии | `each_session_identifies_its_own_owner` |
| `session_id: session.id` → `Uuid::nil()` | `session_auth_carries_the_id_of_the_session_that_opened_it` |
| «истекла» → «не найдена», «отозвана» → «не найдена» | соответствующие тесты |
| `jar.get(SESSION_COOKIE)` → другое имя | `a_live_session_identifies_the_user` и ещё восемь |
| читать заголовок `cookie` целиком, мимо `CookieJar` | `the_session_cookie_is_found_among_its_neighbours` |
| поменять местами проверки отзыва и срока | `a_session_both_revoked_and_expired_says_it_was_revoked` (и только он) |
| часы до эпохи → `now = 0` вместо `None` | `broken_clocks_lock_the_door_instead_of_opening_it` |
| `as i64` вместо насыщения при переполнении | тот же |
| заморозить часы на далёкой дате / отвести на десять лет назад | живые сессии / `an_expired_session_is_refused` |

Не ловится ничем: переименование самой константы `SESSION_COOKIE` (тесты собирают заголовок из неё же). Закрепить имя как проводной контракт можно будет в плане 3b, когда появится издатель cookie; до тех пор это была бы тавтология.

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-api
git commit -m "feat(api): опознание пользователя по сессии"
```

---

### Task 12: Первичная настройка

**Files:**
- Create: `crates/wakode-api/src/setup.rs`
- Modify: `crates/wakode-api/src/lib.rs`
- Modify: `crates/wakode-api/tests/api.rs`

**Interfaces:**
- Consumes: `AppState`, `ApiError`, `hash_password`, `UserRepo::user_count`, `UserRepo::create_user`.
- Produces: `GET /api/setup/status` → `{"needed": bool}`; `POST /api/setup` с телом `{"login", "password", "timezone"}` → `201` и `{"id"}`.

**Окно, которое надо закрыть.** Пока в базе нет ни одного пользователя, эндпоинт создания администратора открыт — и в это окно чужой может занять инстанс. Защита: настройка принимается только с петлевого адреса, если в конфиге явно не разрешено иначе.

Проверка нужна **отдельно от `listen`**: за обратным прокси адрес прослушивания петлевой, а адрес клиента — нет. Владельцу, ставящему сервер сразу за прокси, придётся один раз написать `setup_from_any_address = true` или создать пользователя через CLI.

**После первого пользователя эндпоинт закрывается навсегда** — независимо от `registration`. Регистрация обычных пользователей появится в 3b; здесь только администратор.

**Адрес проверяется раньше базы, а не наоборот.** Проверка адреса авторизационная: чужой запрос не должен гонять сервер в базу. Побочно это закрывает оракул — чужой получает один и тот же ответ на настроенном и на ненастроенном инстансе. Цена: владелец за прокси на уже настроенном инстансе услышит про адрес, а не про «уже выполнена»; рядом есть публичный `/api/setup/status`, который отвечает на второй вопрос.

**Тело берётся как `Result<Json<SetupRequest>, JsonRejection>`, а не распакованным `Json`.** С распакованным экстрактор отрабатывает до первой строки функции, и это два дефекта сразу: кривое тело уезжает `text/plain`-ом мимо `ApiError`, ломая обещание «тело всегда JSON», а чужой с кривым телом получает `400` про формат вместо `403`. Оба случая были красными до правки.

**Порог пароля считается в символах, а не в байтах.** `len()` пропустил бы кириллический пароль из шести символов — в UTF-8 это двенадцать байт. Логин `trim`-ится и сохраняется обрезанным: логин со случайным пробелом иначе навсегда недостижим с формы входа, а экран настройки к тому моменту уже закрыт.

**Гонка названа, а не закрыта.** Два одновременных запроса оба видят `user_count() == 0`, и администраторов заводится два. Окно узкое и требует петлевого доступа. Закрытие потребовало бы единой транзакции «посчитать и создать», то есть нового метода в `wakode-store`: `create_user` идёт своим соединением мимо очереди записи.

**Настройки состояния — именная структура `AppSettings`, а не пять позиционных аргументов.** `registration` и `setup_from_any_address` — два соседних `bool`, и перестановку их местами компилятор не поймает никогда. Цена такой перестановки у владельца, включившего регистрацию: экран первичной настройки открыт всему интернету, пока в базе нет пользователей.

- [ ] **Step 1: Написать падающие тесты**

```rust
use std::net::SocketAddr;

/// Запрос с подставленным адресом клиента: `ConnectInfo` в тестах не
/// появляется сам — его кладёт слой, которого при `oneshot` нет.
fn with_peer(mut request: Request<Body>, peer: &str) -> Request<Body> {
    let addr: SocketAddr = peer.parse().unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(addr));
    request
}

fn setup_body(login: &str) -> Body {
    Body::from(format!(
        r#"{{"login":"{login}","password":"достаточно длинный","timezone":"Europe/Moscow"}}"#
    ))
}

#[tokio::test]
async fn setup_is_needed_while_the_database_has_no_users() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(Request::builder().uri("/api/setup/status").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["needed"], serde_json::json!(true));
}

#[tokio::test]
async fn setup_from_loopback_creates_the_first_admin() {
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);
    let store = state.store.clone();
    let app = router(state);

    let response = app
        .oneshot(with_peer(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .body(setup_body("swrneko"))
                .unwrap(),
            "127.0.0.1:54321",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    let created = store.user_by_login("swrneko").await.unwrap().unwrap();
    assert!(created.is_admin, "первый пользователь обязан быть администратором");
    assert_ne!(
        created.password_hash, "достаточно длинный",
        "пароль сохранён как есть вместо хеша"
    );
}

#[tokio::test]
async fn setup_from_a_foreign_address_is_refused_by_default() {
    // Окно между стартом и первым входом — это окно, в которое чужой
    // занимает инстанс. Дефолт закрыт.
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(with_peer(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .body(setup_body("чужой"))
                .unwrap(),
            "203.0.113.7:40000",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_foreign_address_is_allowed_when_the_owner_says_so() {
    // Зеркало предыдущего: без него «запрещать всегда» прошло бы проверку
    // на запрет и выглядело бы правильным.
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("wakode.db"), 16).unwrap();
    let app = router(AppState::new(store, None, false, 30, true));

    let response = app
        .oneshot(with_peer(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .body(setup_body("за-прокси"))
                .unwrap(),
            "203.0.113.7:40000",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn setup_closes_forever_after_the_first_user() {
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);
    let app = router(state.clone());

    app.clone()
        .oneshot(with_peer(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .body(setup_body("первый"))
                .unwrap(),
            "127.0.0.1:54321",
        ))
        .await
        .unwrap();

    let second = app
        .clone()
        .oneshot(with_peer(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .body(setup_body("второй"))
                .unwrap(),
            "127.0.0.1:54322",
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::FORBIDDEN);

    let status = app
        .oneshot(Request::builder().uri("/api/setup/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = status.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["needed"], serde_json::json!(false));
}

#[tokio::test]
async fn a_bad_timezone_is_a_bad_request_not_a_500() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(a_state(&dir));

    let response = app
        .oneshot(with_peer(
            Request::builder()
                .method("POST")
                .uri("/api/setup")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"login":"кто","password":"достаточно длинный","timezone":"Марс/Олимп"}"#,
                ))
                .unwrap(),
            "127.0.0.1:54321",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
```

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p wakode-api setup`
Expected: FAIL — маршрутов нет.

- [ ] **Step 3: Реализовать**

```rust
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use wakode_store::{NewUser, UserRepo};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize)]
pub struct SetupStatus {
    /// Нужна ли первичная настройка. Становится `false` навсегда после
    /// появления первого пользователя.
    pub needed: bool,
}

#[derive(Deserialize)]
pub struct SetupRequest {
    pub login: String,
    pub password: String,
    pub timezone: String,
}

#[derive(Serialize)]
pub struct SetupResponse {
    pub id: uuid::Uuid,
}

pub async fn status(State(state): State<AppState>) -> Result<Json<SetupStatus>, ApiError> {
    Ok(Json(SetupStatus {
        needed: state.store.user_count().await? == 0,
    }))
}

pub async fn setup(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(request): Json<SetupRequest>,
) -> Result<(StatusCode, Json<SetupResponse>), ApiError> {
    // Закрыт навсегда после первого пользователя — независимо от того,
    // включена ли регистрация: здесь заводится администратор, а не
    // обычный аккаунт.
    if state.store.user_count().await? > 0 {
        return Err(ApiError::Forbidden("первичная настройка уже выполнена"));
    }

    // Проверка идёт по адресу клиента, а не по адресу прослушивания: за
    // обратным прокси второй петлевой, а первый — нет.
    if !state.setup_from_any_address && !peer.ip().is_loopback() {
        return Err(ApiError::Forbidden(
            "первичная настройка доступна только с локального адреса; \
             разрешите setup_from_any_address или создайте пользователя через CLI",
        ));
    }

    let timezone: chrono_tz::Tz = request
        .timezone
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("неизвестная таймзона: {}", request.timezone)))?;

    let password_hash = wakode_auth::hash_password(&request.password).map_err(|err| {
        tracing::error!(error = %err, "не удалось посчитать хеш пароля");
        ApiError::Internal
    })?;

    let user = state
        .store
        .create_user(NewUser {
            login: request.login,
            email: None,
            password_hash,
            display_name: None,
            timezone,
            timeout_secs: wakode_core::DEFAULT_TIMEOUT_SECS,
            is_admin: true,
        })
        .await?;

    Ok((StatusCode::CREATED, Json(SetupResponse { id: user.id })))
}
```

В `router`:

```rust
        .route("/api/setup/status", get(setup::status))
        .route("/api/setup", axum::routing::post(setup::setup))
```

- [ ] **Step 4: Прогнать**

Run: `cargo test -p wakode-api`
Expected: PASS, 53 интеграционных теста и 2 юнит-теста в `session.rs`.

- [ ] **Step 5: Мутационная проверка**

| Мутация | Обязан упасть |
|---|---|
| убрать проверку `user_count() > 0` | `setup_closes_forever_after_the_first_user`, `setup_closes_even_when_registration_is_open` |
| убрать проверку петлевого адреса | `setup_from_a_foreign_address_is_refused_by_default` и ещё два |
| проверку адреса заменить на «запрещать всегда» | `a_foreign_address_is_allowed_when_the_owner_says_so` |
| игнорировать `setup_from_any_address` | тот же |
| петлевым считать ровно `127.0.0.1` | `an_ipv6_loopback_is_loopback_too` |
| проверять базу раньше адреса (порядок исходного плана) | `the_address_is_checked_before_the_database` |
| `is_admin: true` → `false` | `setup_from_loopback_creates_the_first_admin` |
| `password_hash: request.password` | тот же и `the_created_password_verifies` |
| `password_hash: "мусор"` | `the_created_password_verifies` |
| порог пароля в байтах вместо символов | `the_password_threshold_counts_characters_not_bytes` |
| `<` → `<=` на пороге пароля | `a_password_of_exactly_the_minimum_length_is_accepted` |
| убрать `trim` логина | `an_empty_login_is_refused`, `a_login_is_stored_without_its_stray_spaces` |
| таймзону разбирать через `unwrap_or(Tz::UTC)` | `a_bad_timezone_is_a_bad_request_not_a_500` |
| в текст про таймзону подставить пароль | тот же |
| подменить текст «первичная настройка уже выполнена» | `setup_closes_forever_after_the_first_user` |
| распакованный `Json<SetupRequest>` вместо `Result<..>` | `a_broken_body_is_a_json_error_not_a_bare_400`, `a_foreign_address_is_refused_before_the_body_is_even_read` |
| маршрут `/api/setup` ниже `method_not_allowed_fallback` | `a_wrong_method_on_setup_is_a_json_error_too` |
| `into_make_service()` вместо `into_make_service_with_connect_info` | `setup_over_a_real_socket_sees_the_client_address` — **закрывает парковку задачи 9** |

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-api
git commit -m "feat(api): экран первичной настройки и защита петлевым адресом"
```

---

### Task 13: Перехват паники и журналирование

**Files:**
- Modify: `crates/wakode-api/src/lib.rs`
- Create: `crates/wakode-api/tests/log.rs`
- Modify: `crates/wakode-api/Cargo.toml`, `crates/wakode/Cargo.toml`
- Modify: `crates/wakode/src/main.rs`

**Interfaces:**
- Produces: `wakode_api::with_layers(Router) -> Router`; `router` возвращает `with_layers(...)`; инициализация подписчика `tracing` в бинаре.

**Query-строка действительно утекает — это проверено прогоном, а не документацией.** `TraceLayer::new_for_http()` берёт `DefaultMakeSpan`, и он пишет поле `uri` целиком:

```
INFO request{method=GET uri=/тихо?api_key=waka_0000…4444 version=HTTP/1.1}: tower_http::trace::on_response
```

Поэтому `new_for_http()` в чистом виде брать нельзя: нужен свой `make_span_with`, берущий `request.uri().path()`. Заголовок `Authorization`, наоборот, по умолчанию не пишется (`include_headers` выключен) — но сторож на это всё равно нужен, иначе `include_headers(true)`, добавленный кем-нибудь ради отладки, унесёт ключ в журнал.

**Инвариант, который код проверить не может:** путь не несёт секретов. Сегодня это так, но маршрут вида `/api/keys/{ключ}` уронил бы значение в журнал мимо всех проверок. Записано в докблоке `request_span`.

**`on_response` поднимается до `INFO`.** Умолчание `tower-http` — `DEBUG`, а боевой фильтр в бинаре — `tower_http=info`: с умолчанием журнал запросов был бы пуст при стоящем слое, и обещание «метод, путь, код, длительность журналируются» оказалось бы ложью.

**Тесты с захватом журнала живут в отдельном бинаре.** После этой задачи **все** маршруты идут через `with_layers`, то есть каждый тест в `api.rs` дёргает те же callsite'ы `tracing` — и дёргает их без установленного подписчика. `tracing` кеширует «интерес» к callsite глобально на процесс, поэтому соседи отравляли кеш тем немногим тестам, у которых подписчик есть: набор падал примерно в одном прогоне из четырёх под нагрузкой, без единой правки кода. Мьютекс вокруг `set_default` не помогал — дело не в одновременности подписчиков. В `tests/log.rs` подписчик ровно один и глобальный, а разводятся тесты потоко-локальными буферами; отсечка по уровню делается при разборе накопленного, а не подписчиком.

**Паника в обработчике не должна ронять процесс.** Без перехвата паника уносит задачу соединения; соседние запросы выживают, но клиент получает оборванное соединение вместо ответа, и в логе не остаётся ничего внятного. `CatchPanicLayer` превращает её в `500` с тем же телом, что у остальных ошибок.

**Что журналируется и что нет.** Метод, путь, код ответа, длительность. **Ни query-строка, ни заголовок `Authorization` в лог не идут**: в первой лежит `api_key`, во втором — он же в base64. Это ровно тот класс утечки, который финальное ревью плана 2 нашло у `Debug`, и повторять его через журнал нельзя.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[tokio::test]
async fn a_panicking_handler_becomes_a_500() {
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);

    let app = wakode_api::with_layers(
        axum::Router::new()
            .route("/взрыв", axum::routing::get(|| async { panic!("нарочно") }))
            .with_state(state),
    );

    let response = app
        .oneshot(Request::builder().uri("/взрыв").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn the_process_survives_a_panicking_handler() {
    // Отдельно от предыдущего: 500 могла бы прийти и от упавшей задачи.
    // Здесь проверяется, что после паники сервер продолжает отвечать.
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);

    let app = wakode_api::with_layers(
        axum::Router::new()
            .route("/взрыв", axum::routing::get(|| async { panic!("нарочно") }))
            .route("/жив", axum::routing::get(|| async { "да" }))
            .with_state(state),
    );

    let _ = app
        .clone()
        .oneshot(Request::builder().uri("/взрыв").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let response = app
        .oneshot(Request::builder().uri("/жив").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_panic_response_is_json_like_every_other_error() {
    let dir = tempfile::tempdir().unwrap();
    let state = a_state(&dir);

    let app = wakode_api::with_layers(
        axum::Router::new()
            .route("/взрыв", axum::routing::get(|| async { panic!("нарочно") }))
            .with_state(state),
    );

    let response = app
        .oneshot(Request::builder().uri("/взрыв").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("error").is_some(), "тело не JSON с error: {json}");
    assert!(
        !String::from_utf8_lossy(&body).contains("нарочно"),
        "текст паники уехал клиенту"
    );
}
```

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p wakode-api panic`
Expected: FAIL — `with_layers` не существует.

- [ ] **Step 3: Реализовать**

```rust
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::trace::TraceLayer;

/// Навесить общие слои.
///
/// Вынесено отдельно от `router`, чтобы тесты могли собрать свой маршрут
/// с теми же слоями: проверять перехват паники на настоящем обработчике,
/// который паникует, иначе нечем.
pub fn with_layers(router: Router) -> Router {
    router
        .layer(CatchPanicLayer::custom(handle_panic))
        // Пишутся метод, путь, код и длительность. Query-строка и заголовок
        // `Authorization` не пишутся никогда: в первой лежит `api_key`, во
        // втором — он же в base64.
        .layer(TraceLayer::new_for_http())
}

fn handle_panic(err: Box<dyn std::any::Any + Send + 'static>) -> axum::response::Response {
    let message = err
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| err.downcast_ref::<&str>().copied())
        .unwrap_or("неизвестная паника");

    // Текст паники — в лог, но не клиенту: он содержит подробности кода.
    tracing::error!(panic = message, "паника в обработчике");
    ApiError::Internal.into_response()
}
```

`router` теперь возвращает `with_layers(...)`.

В `crates/wakode/src/main.rs` — инициализация подписчика:

```rust
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "wakode=info,wakode_api=info,tower_http=info".into()),
        )
        .init();
```

Добавь `tracing-subscriber` с фичей `env-filter` в `crates/wakode/Cargo.toml`.

**Проверь руками и запиши в отчёт:** попадает ли query-строка в вывод `TraceLayer` по умолчанию. Если да — замени `new_for_http()` на конфигурацию, где `make_span` берёт только метод и путь без query. Это не косметика: `api_key` в логе равносилен ключу в открытом виде.

- [ ] **Step 4: Прогнать**

Run: `cargo test -p wakode-api`
Expected: PASS — 53 теста в `api.rs`, 10 в `log.rs`, 2 юнита в `session.rs`.

- [ ] **Step 5: Мутационная проверка**

| Мутация | Обязан упасть |
|---|---|
| убрать `CatchPanicLayer` | все пять тестов паники |
| `handle_panic` возвращает текст паники в теле | `the_panic_response_is_json_like_every_other_error` |
| `handle_panic` ничего не пишет в лог | `a_panic_message_reaches_the_log_but_not_the_client`, `a_panicking_request_is_journalled_as_a_completed_500` |
| `handle_panic` паникует на чужой нагрузке | `an_unusual_panic_payload_is_survived_and_named` |
| убрать ветку `&str` / ветку `String` в `handle_panic` | `a_panic_message_reaches_the_log_but_not_the_client` |
| вернуть `DefaultMakeSpan` (`new_for_http()`) | `the_query_string_never_reaches_the_log`, `the_real_router_journals_a_request_without_its_query` |
| писать `uri` вместо `path` | те же |
| `include_headers(true)` в span | `the_authorization_header_never_reaches_the_log` |
| `on_response` вернуть к умолчанию `DEBUG`, `info_span!` → `debug_span!` | `a_finished_request_is_journalled_at_info` |
| переставить слои (`TraceLayer` внутрь) | `a_panicking_request_is_journalled_as_a_completed_500` |
| `router` возвращает голый маршрутизатор без `with_layers` | `the_real_router_journals_a_request_without_its_query` — и **только он**: тесты паники собирают свой маршрут через `with_layers` напрямую, поэтому проводку `router` нужно сторожить отдельным тестом |

- [ ] **Step 6: Коммит**

```bash
git add crates/wakode-api crates/wakode
git commit -m "feat(api): перехват паники и журналирование без секретов"
```

---

### Task 14: Подкоманды CLI

**Files:**
- Create: `crates/wakode/src/cli/mod.rs`
- Create: `crates/wakode/src/cli/user.rs`
- Create: `crates/wakode/src/cli/key.rs`
- Create: `crates/wakode/src/cli/backup.rs`
- Modify: `crates/wakode/src/main.rs`
- Create: `crates/wakode/tests/cli.rs`

**Interfaces:**
- Consumes: `Config`, `start`, `MasterKey`, `ApiKeyValue`, `hash_password`, репозиторные трейты.
- Produces: подкоманды `serve`, `migrate`, `master-key generate`, `user create`, `user list`, `key issue`, `key revoke`, `backup`.

**Все подкоманды читают тот же конфиг, что и сервер** — иначе `wakode user create` пошёл бы не в ту базу. Флаг `--config` объявлен на верхнем уровне, а не у каждой.

**Пароль флагом не принимается.** Он остался бы в истории оболочки и в выводе `ps`, где его увидит любой процесс того же пользователя. Спрашивается интерактивно; для неинтерактивного сценария есть чтение из stdin.

**Значение ключа печатается один раз.** Расшифровать его потом можно — за этим и хранится шифротекст, — но подсматривать через CLI незачем: для показа есть настройки в интерфейсе.

**Порог длины пароля живёт в `wakode-auth`, внутри `hash_password`.** Проверка у входа повторяется столько раз, сколько входов — экран первичной настройки, `wakode user create`, регистрация и смена пароля в 3b, — и одного забытого хватает, чтобы инварианта не стало. Это не гипотеза: CLI заводил администратора с паролем «1», пока HTTP требовал восьми символов. Дверь к хешу одна, проверка стоит в ней; `setup.rs` дублирует её только ради внятного `400` вместо `500` и читает ту же константу.

**`revoke_key` возвращает три исхода, а не `()`.** «Ключа нет» и «ключ уже отозван» — разные события: второе обычное (ретрай, двойной клик), первое почти всегда опечатка в идентификаторе. Пока они были неразличимы, `key revoke` отвечал «отозван» на опечатку, и владелец, отзывающий утёкший ключ, считал инцидент закрытым, пока ключ продолжал работать. Второй запрос делается только на редком пути — когда `UPDATE` не тронул ни строки.

**Журнал уходит в stderr.** Stdout подкоманд — это данные: значение выданного ключа, список пользователей, идентификатор заведённого. Строка журнала в том же потоке уехала бы в `wakode user list | …` наравне с данными.

**Подпись `AppState::new` изменилась** относительно первоначального текста этого раздела: `AppState::new(store, master_key, AppSettings { … })`. Проводка вынесена в `fn app_settings(&Config) -> AppSettings` и покрыта юнит-тестом с несовпадающими значениями — с одинаковыми перестановка двух соседних `bool` неразличима.

- [ ] **Step 1: Написать падающие тесты**

`crates/wakode/tests/cli.rs`:

```rust
use std::process::Command;

fn wakode() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wakode"))
}

/// Конфиг во временной папке плюс мастер-ключ.
fn a_setup() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("wakode.toml");
    std::fs::write(
        &config,
        format!(
            "[database]\npath = {:?}\n",
            dir.path().join("wakode.db").to_str().unwrap()
        ),
    )
    .unwrap();

    let master = String::from_utf8(
        wakode()
            .args(["master-key", "generate"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();

    (dir, config, master)
}

#[test]
fn master_key_generate_prints_a_usable_key() {
    let output = wakode().args(["master-key", "generate"]).output().unwrap();
    assert!(output.status.success());

    let printed = String::from_utf8(output.stdout).unwrap();
    assert!(
        wakode_auth::MasterKey::from_base64(printed.trim()).is_ok(),
        "напечатано не то: {printed}"
    );
    // Дважды подряд — разные ключи.
    let again = String::from_utf8(
        wakode().args(["master-key", "generate"]).output().unwrap().stdout,
    )
    .unwrap();
    assert_ne!(printed, again);
}

#[test]
fn migrate_creates_the_schema() {
    let (dir, config, _) = a_setup();

    let status = wakode()
        .args(["--config", config.to_str().unwrap(), "migrate"])
        .status()
        .unwrap();
    assert!(status.success());

    let conn = wakode_store::open(&dir.path().join("wakode.db")).unwrap();
    assert_eq!(wakode_store::schema_version(&conn).unwrap(), 1);
}

#[test]
fn user_create_reads_the_password_from_stdin_not_a_flag() {
    // Пароль флагом остался бы в истории оболочки и в выводе `ps`.
    let (_dir, config, _) = a_setup();

    let rejected = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "user",
            "create",
            "--login",
            "swrneko",
            "--password",
            "секрет",
        ])
        .output()
        .unwrap();
    assert!(
        !rejected.status.success(),
        "флаг --password принят, а не должен был"
    );
}

#[test]
fn user_create_then_list_shows_the_user() {
    let (_dir, config, _) = a_setup();

    let created = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "user",
            "create",
            "--login",
            "swrneko",
            "--admin",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(b"достаточно длинный\n")?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(created.status.success(), "{:?}", String::from_utf8_lossy(&created.stderr));

    let listed = wakode()
        .args(["--config", config.to_str().unwrap(), "user", "list"])
        .output()
        .unwrap();
    let text = String::from_utf8(listed.stdout).unwrap();
    assert!(text.contains("swrneko"), "в списке нет пользователя: {text}");
    assert!(
        !text.contains("$argon2id$"),
        "хеш пароля попал в вывод списка: {text}"
    );
}

#[test]
fn key_issue_prints_the_value_once_and_it_authenticates() {
    let (_dir, config, master) = a_setup();

    // Пользователь нужен, чтобы было кому выдавать.
    let mut child = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "user",
            "create",
            "--login",
            "swrneko",
        ])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child.stdin.as_mut().unwrap().write_all(b"пароль подлиннее\n").unwrap();
    }
    assert!(child.wait().unwrap().success());

    let issued = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "key",
            "issue",
            "--user",
            "swrneko",
            "--name",
            "ноутбук",
        ])
        .env("WAKODE_MASTER_KEY", &master)
        .output()
        .unwrap();
    assert!(issued.status.success(), "{}", String::from_utf8_lossy(&issued.stderr));

    let printed = String::from_utf8(issued.stdout).unwrap();
    let value = printed
        .lines()
        .find_map(|line| wakode_auth::ApiKeyValue::parse(line.trim()))
        .expect(&format!("значение ключа не напечатано: {printed}"));

    // Ключ действительно лежит в базе и зашифрован тем самым мастер-ключом.
    // Проверка через расшифровку сильнее сравнения отпечатков: она
    // доказывает и что ключ сохранён, и что сохранён он под правильным
    // мастер-ключом — то есть что `key issue` не перепутал ключи местами.
    let conn = wakode_store::open(&_dir.path().join("wakode.db")).unwrap();
    let stored = wakode_store::first_api_key(&conn).unwrap().unwrap();
    let master_key = wakode_auth::MasterKey::from_base64(&master).unwrap();
    let decrypted = wakode_auth::ApiKeyValue::decrypt(
        &wakode_auth::EncryptedKey::from_bytes(stored.key_encrypted),
        &master_key,
    )
    .unwrap();
    assert_eq!(decrypted, value);
}

#[test]
fn key_issue_without_a_master_key_fails_loudly() {
    // Без мастер-ключа шифровать нечем. Молчаливая выдача незашифрованного
    // ключа была бы худшим исходом.
    let (_dir, config, _) = a_setup();

    let issued = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "key",
            "issue",
            "--user",
            "кого-нет",
            "--name",
            "ноутбук",
        ])
        .env_remove("WAKODE_MASTER_KEY")
        .output()
        .unwrap();

    assert!(!issued.status.success());
    let stderr = String::from_utf8(issued.stderr).unwrap();
    assert!(stderr.contains("WAKODE_MASTER_KEY"), "причина не названа: {stderr}");
}

#[test]
fn backup_produces_a_readable_copy() {
    let (dir, config, _) = a_setup();
    assert!(wakode()
        .args(["--config", config.to_str().unwrap(), "migrate"])
        .status()
        .unwrap()
        .success());

    let dest = dir.path().join("копия.db");
    let status = wakode()
        .args([
            "--config",
            config.to_str().unwrap(),
            "backup",
            "--to",
            dest.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());

    let conn = wakode_store::open(&dest).unwrap();
    assert_eq!(wakode_store::schema_version(&conn).unwrap(), 1);
}
```

**Замечание про `ApiKey`.** Поля `key_lookup` в возвращаемой структуре нет — хранилище отпечаток наружу не отдаёт, он нужен только для поиска. Поэтому проверка идёт через расшифровку `key_encrypted`, а не через сравнение отпечатков.

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p wakode --test cli`
Expected: FAIL — подкоманд нет.

- [ ] **Step 3: Объявить подкоманды**

`crates/wakode/src/cli/mod.rs`:

```rust
use std::path::PathBuf;

use clap::{Parser, Subcommand};

pub mod backup;
pub mod key;
pub mod user;

#[derive(Debug, Parser)]
#[command(name = "wakode", about = "Selfhosted-трекер времени, совместимый с WakaTime")]
pub struct Cli {
    /// Путь к файлу конфигурации. По умолчанию ./wakode.toml.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Поднять сервер (подразумевается, если подкоманда не указана).
    Serve,
    /// Применить миграции и выйти.
    Migrate,
    /// Операции с мастер-ключом.
    #[command(subcommand, name = "master-key")]
    MasterKey(MasterKeyCommand),
    /// Операции с пользователями.
    #[command(subcommand)]
    User(UserCommand),
    /// Операции с API-ключами.
    #[command(subcommand)]
    Key(KeyCommand),
    /// Консистентный снимок базы.
    Backup {
        #[arg(long)]
        to: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum MasterKeyCommand {
    /// Напечатать новый мастер-ключ в base64.
    Generate,
}

#[derive(Debug, Subcommand)]
pub enum UserCommand {
    /// Создать пользователя. Пароль спрашивается интерактивно: флагом он
    /// остался бы в истории оболочки и в выводе `ps`.
    Create {
        #[arg(long)]
        login: String,
        #[arg(long)]
        admin: bool,
        #[arg(long, default_value = "UTC")]
        timezone: String,
    },
    /// Показать пользователей.
    List,
}

#[derive(Debug, Subcommand)]
pub enum KeyCommand {
    /// Выдать ключ. Значение печатается один раз.
    Issue {
        #[arg(long)]
        user: String,
        #[arg(long)]
        name: String,
    },
    /// Отозвать ключ.
    Revoke {
        #[arg(long)]
        id: uuid::Uuid,
    },
}
```

- [ ] **Step 4: Реализовать подкоманды**

`crates/wakode/src/cli/user.rs`:

```rust
use std::io::BufRead;

use wakode_store::{NewUser, SqliteStore, UserRepo};

/// Прочитать пароль.
///
/// Флагом пароль не принимается: он остался бы в истории оболочки и в
/// выводе `ps`, где его увидит любой процесс того же пользователя.
fn read_password() -> std::io::Result<String> {
    eprint!("Пароль: ");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\n', '\r']).to_owned())
}

pub async fn create(
    store: &SqliteStore,
    login: String,
    admin: bool,
    timezone: String,
) -> anyhow::Result<()> {
    let timezone: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| anyhow::anyhow!("неизвестная таймзона: {timezone}"))?;

    let password = read_password()?;
    if password.is_empty() {
        anyhow::bail!("пароль пуст");
    }

    let user = store
        .create_user(NewUser {
            login,
            email: None,
            password_hash: wakode_auth::hash_password(&password)?,
            display_name: None,
            timezone,
            timeout_secs: wakode_core::DEFAULT_TIMEOUT_SECS,
            is_admin: admin,
        })
        .await?;

    println!("{} {}", user.id, user.login);
    Ok(())
}
```

Для `list` нужно чтение всех пользователей, которого в хранилище нет. Добавь в `crates/wakode-store/src/users.rs`:

```rust
/// Все пользователи, от самого раннего к позднему.
///
/// Порядок по `created_at` делает вывод `wakode user list` устойчивым:
/// список, меняющий порядок между запусками, нельзя ни сравнить глазами,
/// ни зафиксировать тестом.
pub fn list_users(conn: &Connection) -> StoreResult<Vec<User>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, login, email, password_hash, display_name, timezone,
                timeout_secs, is_admin, created_at, updated_at
         FROM users ORDER BY created_at, id",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, i64>(9)?,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (id, login, email, password_hash, display_name, tz, timeout, admin, created, updated) =
            row?;
        out.push(User {
            id: blob_to_uuid(&id)?,
            login,
            email,
            password_hash,
            display_name,
            timezone: tz
                .parse()
                .map_err(|_| StoreError::Corrupt(format!("таймзона {tz}")))?,
            timeout_secs: timeout,
            is_admin: admin != 0,
            created_at: Micros::new(created),
            updated_at: Micros::new(updated),
        });
    }
    Ok(out)
}
```

Метод трейта в `repo.rs`:

```rust
// в trait UserRepo
    fn list_users(&self) -> impl std::future::Future<Output = StoreResult<Vec<User>>> + Send;

// в impl UserRepo for SqliteStore
    async fn list_users(&self) -> StoreResult<Vec<User>> {
        on_own_connection(self, |conn| crate::list_users(&conn)).await
    }
```

Тест в `crates/wakode-store/tests/repository.rs`:

```rust
#[test]
fn users_are_listed_oldest_first() {
    let mut conn = open_in_memory().unwrap();
    migrate(&mut conn).unwrap();

    let first = insert_user(&conn, &a_user("первый")).unwrap();
    let second = insert_user(&conn, &a_user("второй")).unwrap();

    let listed = list_users(&conn).unwrap();
    let logins: Vec<&str> = listed.iter().map(|u| u.login.as_str()).collect();
    assert_eq!(logins, vec!["первый", "второй"]);
    assert_eq!(listed[0].id, first.id);
    assert_eq!(listed[1].id, second.id);
}
```

Сам вывод подкоманды:

```rust
pub async fn list(store: &SqliteStore) -> anyhow::Result<()> {
    for user in store.list_users().await? {
        // Хеш пароля не печатается: вывод CLI уезжает в терминал, в
        // историю и в чужие скриншоты. Это закреплено тестом.
        println!(
            "{}\t{}\t{}",
            user.id,
            user.login,
            if user.is_admin { "админ" } else { "" }
        );
    }
    Ok(())
}
```

`crates/wakode/src/cli/key.rs`:

```rust
use wakode_auth::{ApiKeyValue, MasterKey};
use wakode_store::{KeyRepo, NewApiKey, SqliteStore, UserRepo};

pub async fn issue(
    store: &SqliteStore,
    master: Option<&MasterKey>,
    login: String,
    name: String,
) -> anyhow::Result<()> {
    // Без мастер-ключа шифровать нечем. Выдать незашифрованный ключ молча
    // было бы худшим исходом: база выглядела бы защищённой, не будучи ею.
    let master = master.ok_or_else(|| {
        anyhow::anyhow!("для выдачи ключа нужна переменная WAKODE_MASTER_KEY")
    })?;

    let user = store
        .user_by_login(&login)
        .await?
        .ok_or_else(|| anyhow::anyhow!("нет пользователя {login}"))?;

    let value = ApiKeyValue::generate();
    store
        .create_key(NewApiKey {
            user_id: user.id,
            name,
            key_encrypted: value.encrypt(master)?.as_bytes().to_vec(),
            key_lookup: value.lookup(master),
        })
        .await?;

    // Печатается один раз: подсматривать значение через CLI незачем, для
    // показа есть настройки в интерфейсе.
    println!("{value}");
    Ok(())
}

pub async fn revoke(store: &SqliteStore, id: uuid::Uuid) -> anyhow::Result<()> {
    store.revoke_key(id).await?;
    println!("отозван {id}");
    Ok(())
}
```

`crates/wakode/src/cli/backup.rs`:

```rust
use std::path::Path;

use wakode_store::SqliteStore;

pub async fn backup(store: &SqliteStore, to: &Path) -> anyhow::Result<()> {
    // Отказывает, если файл уже есть: `VACUUM INTO` не перезаписывает, и
    // ротацию имён решает вызывающий, а не эта подкоманда.
    store.backup(to).await?;
    println!("снимок записан в {}", to.display());
    Ok(())
}
```

`crates/wakode/src/main.rs`:

```rust
mod cli;
mod config;
mod startup;

use clap::Parser;

use crate::cli::{Cli, Command, KeyCommand, MasterKeyCommand, UserCommand};
use crate::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "wakode=info,wakode_api=info,tower_http=info".into()),
        )
        .init();

    let args = Cli::parse();

    // Единственная подкоманда, которой не нужны ни конфиг, ни база:
    // мастер-ключ генерируют до того, как что-либо существует.
    if let Some(Command::MasterKey(MasterKeyCommand::Generate)) = args.command {
        println!("{}", wakode_auth::MasterKey::generate().to_base64());
        return Ok(());
    }

    let config = Config::load(args.config.as_deref())?;
    tracing::info!(
        config = ?args.config.as_deref().unwrap_or(std::path::Path::new(config::DEFAULT_CONFIG_PATH)),
        database = ?config.database.path,
        "конфигурация прочитана"
    );

    let started = startup::start(config, std::env::var("WAKODE_MASTER_KEY").ok()).await?;

    match args.command.unwrap_or(Command::Serve) {
        Command::MasterKey(_) => unreachable!("обработано выше"),
        Command::Migrate => {
            // Миграции уже применил `start`; сюда мы попадаем, только если
            // они прошли, поэтому остаётся сообщить об этом и выйти.
            println!("миграции применены");
        }
        Command::User(UserCommand::Create { login, admin, timezone }) => {
            cli::user::create(&started.store, login, admin, timezone).await?;
        }
        Command::User(UserCommand::List) => cli::user::list(&started.store).await?,
        Command::Key(KeyCommand::Issue { user, name }) => {
            cli::key::issue(&started.store, started.master_key.as_ref(), user, name).await?;
        }
        Command::Key(KeyCommand::Revoke { id }) => cli::key::revoke(&started.store, id).await?,
        Command::Backup { to } => cli::backup::backup(&started.store, &to).await?,
        Command::Serve => serve(started).await?,
    }

    // Останов писателя нужен всем путям, а не только `serve`: подкоманда,
    // завершившаяся, не дождавшись коммита, потеряла бы принятое.
    started_shutdown(&started).await;
    Ok(())
}

async fn serve(started: &startup::Startup) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(&started.config.server.listen).await?;
    tracing::info!(listen = %started.config.server.listen, "сервер поднят");

    let state = wakode_api::AppState::new(
        started.store.clone(),
        started.master_key.clone(),
        started.config.auth.registration,
        started.config.auth.session_ttl_days,
        started.config.auth.setup_from_any_address,
    );

    wakode_api::serve(listener, state).await?;
    Ok(())
}

async fn started_shutdown(started: &startup::Startup) {
    if let Err(err) = started.store.shutdown().await {
        tracing::warn!(error = %err, "останов писателя завершился с ошибкой");
    }
}
```

**Осторожно с владением.** В наброске выше `started` используется и в `match`, и после него — сверься с компилятором: `Command::Serve => serve(started)` требует ссылки, а не значения. Если проще передать `&started` во все ветки, сделай так; форма важна меньше, чем два свойства: `serve` получает клон состояния, а `shutdown` зовётся на всех путях, включая ошибочные. Если добиться второго без `Drop`-обёртки не выходит — скажи об этом в отчёте, не подгоняй молча.

Добавь `anyhow` в зависимости `wakode`; версию возьми из `cargo search anyhow`.

- [ ] **Step 5: Прогнать**

Run: `cargo test --workspace`
Expected: PASS. Предупреждений нет ни на этапе компиляции, ни в выводе тестов.

- [ ] **Step 6: Мутационная проверка**

| Мутация | Обязан упасть |
|---|---|
| `user create` принимает `--password` | `user_create_reads_the_password_from_stdin_not_a_flag` |
| `list` печатает `password_hash` | `user_create_then_list_shows_the_user` |
| журнал пишется в stdout | тот же |
| `user create` кладёт в `password_hash` мусор | `user_create_stores_a_hash_that_opens` |
| порог длины пароля снят / считается в байтах | `a_short_password_is_refused_by_the_cli_too`, `the_threshold_counts_characters_not_bytes` |
| `key issue` при отсутствии мастер-ключа пишет пустой `key_encrypted` | `key_issue_without_a_master_key_fails_loudly` |
| `key issue` шифрует, но не печатает значение | `key_issue_prints_the_value_once_and_it_authenticates` |
| `revoke_key` не различает «нет такого» и «уже отозван» | `revoking_a_key_that_does_not_exist_is_a_failure_not_a_shrug`, `revoking_tells_the_three_cases_apart` |
| `master-key generate` печатает константу | `master_key_generate_prints_a_usable_key` |
| `app_settings` меняет местами два `bool` | `the_config_reaches_the_state_without_swapping_its_flags` |
| `serve` не зовёт `wakode_api::serve` / биндит не тот адрес | `serve_comes_up_and_answers` |
| `unwrap_or(Command::Serve)` заменён другой веткой | `no_subcommand_means_serve` |
| `--config` игнорируется | 13 тестов из 19 |
| `--timezone` игнорируется | `user_create_stores_the_timezone_it_was_given` |
| `list_users` без `ORDER BY created_at, id` | `list_users_orders_by_created_at_not_by_insertion` — и **только он**: `users_are_listed_oldest_first` остаётся зелёным, потому что `users` объявлена `WITHOUT ROWID` и обход по кластерному UUIDv7 совпадает с порядком вставки |

Не ловится ничем: подмена флага прямо в `serve` (`AppSettings { setup_from_any_address: true, ..app_settings(&config) }`). Закрыть сегодня нечем — `registration` в `wakode-api` нигде не читается, а `setup_from_any_address` различим только по не-петлевому адресу клиента, которого на том же хосте не взять.

- [ ] **Step 7: Коммит**

```bash
git add crates/wakode crates/wakode-store
git commit -m "feat(cli): подкоманды пользователей, ключей и бэкапа"
```

---

## Что этот план сознательно не делает

- **Ни одного прикладного эндпоинта.** Шесть совместимых и внутренний API — план 3b. Здесь есть только `/healthz` и первичная настройка.
- **Ни одной страницы интерфейса.** Экран первичной настройки — это эндпоинт; HTML к нему рисует план 4.
- **Ни входа, ни выхода по паролю.** `POST /api/auth/login`, создающий сессию, относится к 3b: здесь построен механизм проверки сессии, но не её выдача. Единственный способ получить пользователя в 3a — первичная настройка или CLI.
- **Ни регистрации обычных пользователей.** Флаг `registration` прочитан и лежит в состоянии, но эндпоинта, который бы на него смотрел, ещё нет.
- **Ни ротации мастер-ключа.** Перешифровка всех ключей под новым мастер-ключом — отдельная подкоманда, и она понадобится не раньше, чем появится первый повод её звать.

## Долги, передаваемые в 3b

1. `"unknown"` → `null` перед отдачей совместимому клиенту.
2. `Micros` на проводе — дробные секунды, а не целое.
3. `dependencies` — список, а не строка через запятую; в `Copy`-структуру `Attrs` он не помещается.
4. `summaries` обязан отдавать пустые дни, которых разбиение по локальным дням не возвращает.
5. Точная форма `statusbar/today` — снимается с живого ответа, документации нет.
6. **Golden-фикстуры** с живого аккаунта WakaTime. Внешняя зависимость: без них совместимость непроверяема, и начинать 3b до их появления бессмысленно.
7. `IncomingHeartbeat` печатает путь к файлу производным `Debug`, а `WriteJob` несёт целый батч.

**Расхождение со спекой, зафиксированное намеренно.** §9 дизайна называет этот долг обязательным для 3a. План его откладывает, и вот почему: решать надо не «прятать или не прятать», а «что писать в лог на горячем пути приёма отметок», а горячего пути в 3a нет — эндпоинт ingest'а появляется только в 3b. Решать вслепую значило бы либо спрятать то, что при отладке нужнее всего, либо оставить производный `Debug` на основании «пока некому его позвать». Обе половины решения принимаются там, где виден потребитель.

Что 3a при этом делает: заводит правило и подпирает его тестами на каждом типе, который несёт секрет (`MasterKey`, `ApiKeyValue`, `EncryptedKey`, `SessionToken`, `AppState`), и не даёт `TraceLayer` писать query-строку и `Authorization`. Долг остаётся открытым, а не закрытым — не считай его выполненным.

Второй долг из §9, шов управляемого завершения писателя, закрыт задачей 8 полностью: `WriterGone` стал достижимым состоянием и покрыт тестом, а молчаливая смерть потока при панике перестала существовать как режим.
