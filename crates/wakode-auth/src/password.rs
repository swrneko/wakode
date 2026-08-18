use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::{Algorithm, Argon2, Params, Version};

use crate::error::{AuthError, AuthResult};

/// Стоимость хеширования пароля.
///
/// Объявлена здесь, а не взята из `Argon2::default()`, намеренно.
/// Умолчания крейта — это «как решит крейт»: `argon2 = "0.5"` допускает
/// обновление внутри мажорной версии, и `cargo update` способен сдвинуть
/// их, не изменив ни строки нашего кода. Решение о стоимости принимаем мы,
/// а не список зависимостей.
///
/// Память — рекомендованный OWASP минимум для argon2id. Проходов три, а не
/// два: во-первых, это строгое усиление ценой примерно половины лишнего
/// такта на вход, что для трекера времени неощутимо; во-вторых, так
/// значения расходятся с умолчаниями крейта, и тест наконец способен
/// отличить «мы задали параметры» от «мы взяли что дали». Пока они
/// совпадали, эту разницу не ловило ничто.
const ARGON2_MEMORY_KIB: u32 = 19_456;
const ARGON2_ITERATIONS: u32 = 3;
const ARGON2_PARALLELISM: u32 = 1;

// Нижний порог — рекомендованный OWASP минимум для argon2id, числа
// литеральные намеренно. Проверка, читающая те же константы, что и код,
// двигалась бы вместе с ними и ослабление пропустила. Порог, а не
// равенство: усиление параметров — правильное изменение.
const _: () = assert!(ARGON2_MEMORY_KIB >= 19_456, "память ослаблена");
const _: () = assert!(ARGON2_ITERATIONS >= 2, "число проходов ослаблено");
const _: () = assert!(ARGON2_PARALLELISM >= 1);

/// Настроенный хешер.
fn hasher() -> Argon2<'static> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        None,
    )
    .expect("параметры argon2 заданы константами и заведомо допустимы");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Нижняя граница длины пароля, в символах.
///
/// Восемь — не гигиена ради гигиены: этим паролем открывается учётная
/// запись на инстансе, который смотрит наружу. Верхней границы нет: argon2
/// длинный ввод переваривает, а на HTTP-входе длину тела и так ограничивает
/// axum.
///
/// Считаются **символы, а не байты**: порог обещан пользователю в символах,
/// и на кириллическом пароле байтовая длина вдвое больше — граница уехала
/// бы туда, где её никто не ждёт.
pub const MIN_PASSWORD_CHARS: usize = 8;

/// Посчитать хеш пароля.
///
/// Возвращается PHC-строка: она самоописывающая — соль и параметры лежат
/// внутри неё, поэтому хранилищу достаточно одной колонки, а смена
/// параметров не требует миграции.
///
/// **Порог длины проверяется здесь, а не у вызывающих.** Проверка у входа
/// повторяется столько раз, сколько входов: экран первичной настройки,
/// `wakode user create`, смена пароля в 3b, регистрация в 3b. Одного
/// забытого входа хватает, чтобы инвариант перестал существовать — и это
/// уже случилось: до этой правки CLI заводил администратора с паролем «1»,
/// пока HTTP требовал восьми символов. Единственная дверь к хешу одна, и
/// проверка стоит в ней.
pub fn hash_password(password: &str) -> AuthResult<String> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(AuthError::PasswordTooShort {
            minimum: MIN_PASSWORD_CHARS,
        });
    }

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
///
/// Проверка идёт по параметрам **из самой строки**, а не по нашим
/// константам: иначе после их изменения перестали бы открываться все
/// прежние хеши. Ровно за этим PHC и самоописывающий.
pub fn verify_password(password: &str, hash: &str) -> AuthResult<bool> {
    let parsed = PasswordHash::new(hash).map_err(|_| AuthError::PasswordHashMalformed)?;

    // PHC-строка без поля дайджеста разбирается успешно, но сверять с ней
    // нечего: `verify_password` вернул бы `Ok(false)`, то есть «пароль не
    // подошёл» — а на деле такой хеш не откроется ничем и никогда.
    if parsed.hash.is_none() {
        return Err(AuthError::PasswordHashMalformed);
    }

    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

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
        let hash = hash_password("любой пароль подлиннее").unwrap();
        assert!(hash.starts_with("$argon2id$"), "получили {hash}");

        // Стоимость проверяется на двух уровнях, и оба нужны.
        //
        // Первый: то, что уехало в хеш, совпадает с нашими константами.
        // Сторожит проводку — что `hasher()` действительно применяет то,
        // что объявлено, а не умолчания крейта.
        assert!(
            hash.contains(&format!(
                "$v=19$m={ARGON2_MEMORY_KIB},t={ARGON2_ITERATIONS},p={ARGON2_PARALLELISM}$"
            )),
            "параметры стоимости разошлись с константами: {hash}"
        );

        // Второй: сами константы не ниже рекомендованного OWASP минимума
        // для argon2id. Числа здесь литеральные намеренно — проверка,
        // читающая те же константы, что и код, двигается вместе с ними и
        // ослабление пропускает. Порог, а не равенство: усиление параметров
        // — правильное изменение, и ронять на нём тест незачем.
        // Порог проверяется на этапе компиляции (см. `const _` рядом с
        // константами): assert! на константах clippy справедливо считает
        // бессмыслицей — условие известно до запуска тестов.

        // Длина соли — та же категория тихого ослабления. Крейт даёт 16
        // байт по умолчанию, и это тоже его решение, а не наше.
        let salt = argon2::password_hash::PasswordHash::new(&hash)
            .unwrap()
            .salt
            .unwrap();
        assert_eq!(salt.decode_b64(&mut [0u8; 64]).unwrap().len(), 16);
    }

    #[test]
    fn a_hash_made_with_other_parameters_still_opens() {
        // Докстринг обещает, что проверка идёт по параметрам из самой
        // строки, а не по нашим константам. Обещание не держалось ничем:
        // «упрощающий» рефактор, пересчитывающий хеш через `hasher()` и
        // сравнивающий дайджесты, проходил все тесты — и закрыл бы вход
        // всем, чей хеш посчитан прежними параметрами.
        //
        // Это не гипотеза: `ARGON2_ITERATIONS` уже поднимали с 2 до 3, и
        // хеши с `t=2` в базах есть.
        let salt = SaltString::generate(&mut OsRng);
        let weaker = Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            Params::new(19_456, 2, 1, None).unwrap(),
        )
        .hash_password("прежний пароль".as_bytes(), &salt)
        .unwrap()
        .to_string();

        assert!(weaker.contains("t=2"), "вектор собран не теми параметрами");
        assert!(verify_password("прежний пароль", &weaker).unwrap());
        assert!(!verify_password("другой", &weaker).unwrap());
    }

    #[test]
    fn a_hash_without_a_digest_is_malformed_not_a_wrong_password() {
        // PHC-строка, обрезанная ровно перед дайджестом, разбирается без
        // ошибки. Сверять с ней нечего: такой хеш не откроется ничем и
        // никогда, и отвечать на него «пароль не подошёл» значит прятать
        // поломку данных под обычное событие.
        let full = hash_password("любой пароль подлиннее").unwrap();
        let without_digest = &full[..full.rfind('$').unwrap()];

        assert!(matches!(
            verify_password("любой пароль подлиннее", without_digest),
            Err(AuthError::PasswordHashMalformed)
        ));
    }

    #[test]
    fn a_short_password_is_refused_before_it_is_hashed() {
        // Порог живёт здесь, а не у вызывающих: входов к хешу несколько
        // (экран первичной настройки, `wakode user create`, регистрация и
        // смена пароля в 3b), и одного забытого хватает, чтобы инварианта
        // не стало. Это не гипотеза: CLI заводил администратора с паролем
        // «1», пока HTTP требовал восьми символов.
        assert!(matches!(
            hash_password("1234567"),
            Err(AuthError::PasswordTooShort { minimum: 8 })
        ));
        assert!(hash_password("12345678").is_ok(), "ровно порог — годится");
    }

    #[test]
    fn the_threshold_counts_characters_not_bytes() {
        // Восемь кириллических символов — шестнадцать байт. Проверка по
        // `len()` пропустила бы пароль вдвое короче обещанного, и граница
        // уехала бы туда, где её никто не ждёт.
        let eight_cyrillic = "паролище";
        assert_eq!(eight_cyrillic.chars().count(), 8);
        assert_eq!(eight_cyrillic.len(), 16);
        assert!(hash_password(eight_cyrillic).is_ok());

        let seven_cyrillic = "паролищ";
        assert_eq!(seven_cyrillic.chars().count(), 7);
        assert!(hash_password(seven_cyrillic).is_err(), "семь символов приняты");
    }

    #[test]
    fn the_threshold_is_pinned_by_a_literal() {
        // Литерал намеренно: проверка, читающая ту же константу, что и код,
        // двигалась бы вместе с ней и ослабление пропустила. Порог, а не
        // равенство — усиление правильное изменение.
        assert!(MIN_PASSWORD_CHARS >= 8, "порог длины пароля ослаблен");
    }
}
