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
        let hash = hash_password("любой").unwrap();
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
        assert!(ARGON2_MEMORY_KIB >= 19_456, "память ослаблена");
        assert!(ARGON2_ITERATIONS >= 2, "число проходов ослаблено");
        assert!(ARGON2_PARALLELISM >= 1);

        // Длина соли — та же категория тихого ослабления. Крейт даёт 16
        // байт по умолчанию, и это тоже его решение, а не наше.
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
