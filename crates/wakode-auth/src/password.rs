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
    Argon2::default()
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

        // Стоимость пришпилена, а не только вариант алгоритма. Умолчания
        // крейта сегодня совпадают с рекомендованным OWASP минимумом для
        // argon2id, но умолчания меняются молча — а ослабление памяти с
        // 19 МиБ до, скажем, 8 КиБ не изменит ни одной строки нашего кода
        // и не уронит ни одной проверки формата.
        assert!(
            hash.contains("$v=19$m=19456,t=2,p=1$"),
            "параметры стоимости изменились: {hash}"
        );
    }
}
