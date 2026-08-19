//! Одноразовый токен первичной настройки.

use base64::Engine as _;
use rand::RngCore as _;
use subtle::ConstantTimeEq as _;

use crate::REDACTED;

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
/// Живёт только в памяти процесса. Выдаётся, только если на старте
/// пользователей в базе не было; после появления администратора в этом же
/// процессе значение из памяти не исчезает, но эндпоинт настройки закрыт
/// проверкой `user_count() > 0`, а не отсутствием токена — секрет, для
/// которого окно уже закрыто, просто больше никого не пускает. Перезапуск
/// выдаёт новый токен, прежний перестаёт существовать вместе с процессом.
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
        f.debug_tuple("SetupToken").field(&REDACTED).finish()
    }
}

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
        assert_eq!(format!("{token:?}"), format!("SetupToken({REDACTED:?})"));
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
