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
        // Без `trim`: токен приезжает из cookie, где HTTP-слой уже отрезал
        // разделители. У `ApiKeyValue` обрезка есть и обоснована — там
        // значение приходит из конфига редактора с переводом строки; здесь
        // такой причины нет, а расширять множество принимаемых входов без
        // причины не надо.
        let bytes = URL_SAFE_NO_PAD.decode(raw).ok()?;
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
        // Сравнение через `String::from_utf8_lossy` тут не годится: лосс-
        // декодирование 32 сырых байт даёт строку не длиннее 32 символов
        // (каждый невалидный байт превращается максимум в один символ
        // замены), а base64-текст токена — 43 символа. Более короткая
        // строка не может содержать более длинную ни при какой
        // реализации `hash`, так что такая проверка прошла бы даже на
        // мутации «hash возвращает байты токена как есть» — она в
        // принципе ничего не доказывает. Сравниваем байты напрямую.
        let token = SessionToken::generate();
        let raw_bytes = URL_SAFE_NO_PAD.decode(token.to_string()).unwrap();
        assert_ne!(token.hash(), raw_bytes);
    }

    #[test]
    fn garbage_does_not_parse() {
        assert!(SessionToken::parse("короткий").is_none());
        assert!(SessionToken::parse("").is_none());
    }

    #[test]
    fn debug_does_not_print_the_token() {
        // Проверка из брифа (`!dump.contains(&token.to_string())`) не ловит
        // производный `Debug`: он печатает массив байт в десятичном виде
        // (`SessionToken([12, 34, ...])`), а искомая подстрока —
        // base64-текст токена, и они не пересекаются ни при какой
        // реализации. Тот же капкан уже был в задаче 1 для `MasterKey`
        // (см. `master_key.rs`); лечится точным сравнением с ожидаемым
        // выводом заглушки — тогда любой другой вывод, включая
        // производный, тест провалит.
        let token = SessionToken::generate();
        let dump = format!("{token:?}");
        assert_eq!(dump, format!("SessionToken({REDACTED:?})"));
    }

    #[test]
    fn the_base64_alphabet_is_pinned() {
        // Токен уезжает в cookie и приезжает обратно. Алфавит здесь
        // URL-safe и без паддинга — иначе `+`, `/` и `=` пришлось бы
        // экранировать. Байты подобраны так, что в стандартном алфавите
        // они дали бы `+` и `/`: без этого смена алфавита разлогинила бы
        // всех разом, а круговой обход остался бы зелёным.
        const RAW: [u8; 32] = [
            251, 255, 191, 251, 255, 191, 251, 255, 191, 251, 255, 191, 251, 255, 191, 251, 255,
            191, 251, 255, 191, 251, 255, 191, 251, 255, 191, 251, 255, 191, 251, 255,
        ];
        const ENCODED: &str = "-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_8";

        let token = SessionToken::parse(ENCODED).expect("URL-safe алфавит без паддинга");
        assert_eq!(token.to_string(), ENCODED);
        assert_eq!(token.hash(), <sha2::Sha256 as sha2::Digest>::digest(RAW).to_vec());
    }

    #[test]
    fn the_hash_algorithm_is_pinned_to_a_fixed_vector() {
        // Хеш ложится в уникальный индекс sessions.token_hash. Смена
        // алгоритма или его усечение — тихая поломка: она разлогинит
        // всех разом, а тест, сравнивающий хеш сам с собой, её не видит.
        let token = SessionToken::parse("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8").unwrap();
        assert_eq!(
            token.hash(),
            vec![
                99, 13, 205, 41, 102, 196, 51, 102, 145, 18, 84, 72, 187, 178, 91, 79, 244, 18,
                164, 156, 115, 45, 178, 200, 171, 193, 184, 88, 27, 215, 16, 221
            ]
        );
    }
}
