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
        let value = ApiKeyValue::generate();
        let parsed = uuid::Uuid::parse_str(&value.to_string()).unwrap();
        assert_eq!(parsed.get_version_num(), 4);
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
