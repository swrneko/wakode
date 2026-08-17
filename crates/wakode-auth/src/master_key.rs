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
        let short = STANDARD.encode([0u8; 31]);
        assert!(matches!(
            MasterKey::from_base64(&short),
            Err(AuthError::MasterKeyLength { got: 31 })
        ));

        let long = STANDARD.encode([0u8; 33]);
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
        let key = MasterKey::generate();
        let dump = format!("{key:?}");
        assert!(!dump.contains(&key.to_base64()), "ключ утёк в Debug: {dump}");
        // В брифе вторая проверка была `!dump.contains(&hex_первого_байта) ||
        // dump.len() < 32`: заглушка `<скрыт>` короче 32 байт всегда, поэтому
        // OR был истинным независимо от содержимого — проверка ничего не
        // доказывала. Хуже того: замена условия на честный hex-чек всё равно
        // не ловит производный `Debug` — тот печатает массив байт в
        // десятичном виде (`[18, 52, ...]`), а не в hex и не в base64, так
        // что ни один из вариантов «искать подстроку» утечку не находит.
        // Единственный надёжный способ — сравнить с точным ожидаемым выводом
        // заглушки: тогда любой другой вывод (в частности, производный)
        // тест провалит.
        assert_eq!(dump, format!("MasterKey({REDACTED:?})"));
    }
}
