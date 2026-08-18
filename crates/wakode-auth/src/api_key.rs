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

        // Та же проверка глазами плагина: тринадцатый символ — версия,
        // семнадцатый — вариант.
        let text = value.to_string();
        let chars: Vec<char> = text.chars().collect();
        assert_eq!(chars[14], '4', "версия не на месте: {text}");
        assert!(
            matches!(chars[19], '8' | '9' | 'a' | 'b'),
            "вариант не на месте: {text}"
        );
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
    fn a_short_or_empty_ciphertext_is_refused_not_panicked() {
        // `EncryptedKey::from_bytes` зовут на байтах из базы. Усечённая или
        // занулённая строка — частичная запись, ручная правка, миграция —
        // не должна превращать отказ авторизации в панику потока: без
        // проверки длины `split_at` паникует прямо на первом же байте.
        let master = MasterKey::generate();
        let full = ApiKeyValue::generate().encrypt(&master).unwrap();

        for bytes in [
            Vec::new(),
            vec![0u8; 1],
            // Половина настоящего шифротекста: длина берётся от него, а не
            // от константы, которую читает реализация.
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
        // реализация, склеивающая nonce с открытым текстом, прошла бы его
        // целиком.
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
        // здесь: круговой обход внутри одного запуска согласован сам с
        // собой и такую поломку не видит.
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
    fn debug_prints_neither_the_value_nor_the_ciphertext() {
        let master = MasterKey::generate();
        let value = ApiKeyValue::generate();
        let encrypted = value.encrypt(&master).unwrap();

        // Сверка с точной ожидаемой строкой, как в `master_key` и
        // `session`: поиск подстроки в этом крейте трижды оказывался
        // зелёным на утёкшем секрете, и правило «как проверяется
        // отсутствие утечки» должно быть одно на все четыре модуля.
        assert_eq!(format!("{value:?}"), format!("ApiKeyValue({REDACTED:?})"));
        assert_eq!(
            format!("{encrypted:?}"),
            format!("EncryptedKey {{ bytes: {} }}", encrypted.as_bytes().len())
        );
    }
}
