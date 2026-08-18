//! Первичная настройка: создание единственного администратора.

use std::net::SocketAddr;

use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use wakode_store::{NewUser, UserRepo};

use crate::error::ApiError;
use crate::state::AppState;

/// Нижняя граница длины пароля — та же, что у всех остальных входов.
///
/// Берётся из `wakode-auth`, а не объявляется здесь: своя копия разошлась
/// бы с чужой молча. Так уже было — CLI заводил администратора с паролем
/// «1», пока этот файл требовал восьми символов. Настоящая проверка стоит
/// внутри `hash_password`, эта — ради внятного `400` вместо `500`:
/// «пароль короче восьми символов» отличается от «внутренняя ошибка» ровно
/// тем, что говорит владельцу, что делать.
use wakode_auth::MIN_PASSWORD_CHARS;

#[derive(Serialize)]
pub struct SetupStatus {
    /// Нужна ли первичная настройка. Становится `false` навсегда после
    /// появления первого пользователя.
    pub needed: bool,
}

/// Нужна ли первичная настройка.
///
/// Отвечает всем без разбора адреса: экран настройки — первое, что
/// открывает браузер, и до создания администратора предъявлять нечего.
/// Единственный факт, который отсюда узнаёт чужой, — «инстанс уже занят»;
/// заводить на него секретность бессмысленно, потому что этот же факт
/// виден по любому другому отказу.
pub async fn status(State(state): State<AppState>) -> Result<Json<SetupStatus>, ApiError> {
    Ok(Json(SetupStatus {
        needed: state.store.user_count().await? == 0,
    }))
}

/// Заголовки, наличие которых означает «запрос пришёл через посредника».
///
/// Список закрытый; содержимое заголовков не читается — см. обоснование
/// в `setup`. Расширять его безопасно: каждая новая строка только сужает
/// то, что проходит.
///
/// Кто их ставит: `x-forwarded-for` — Caddy, Traefik, Apache mod_proxy,
/// Cloudflare и почти любой готовый nginx-рецепт; `forwarded` — RFC 7239,
/// его умеют Traefik и Envoy; `x-real-ip` — nginx-рецепты; `x-forwarded-proto`
/// и `x-forwarded-host` — всё, что терминирует TLS; `via` — RFC 9110,
/// его ставят кеширующие прокси.
const PROXY_HEADERS: [&str; 6] = [
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-proto",
    "x-forwarded-host",
    "x-real-ip",
    "via",
];

const SETUP_IS_LOCAL_ONLY: &str = "первичная настройка доступна только с локального адреса; \
     разрешите setup_from_any_address или создайте пользователя через `wakode user create`";

const SETUP_THROUGH_A_PROXY: &str =
    "запрос пришёл через обратный прокси, и адрес клиента отсюда неизвестен; \
     разрешите setup_from_any_address или создайте пользователя через `wakode user create`";

/// Есть ли в запросе заголовок с таким именем.
fn parts_have(headers: &HeaderMap, name: &str) -> bool {
    headers.contains_key(name)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupRequest {
    pub login: String,
    pub password: String,
    pub timezone: String,
}

#[derive(Serialize)]
pub struct SetupResponse {
    pub id: uuid::Uuid,
}

/// Завести первого администратора.
pub async fn setup(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    // Тело берётся `Result`ом, а не распакованным `Json`: экстрактор в
    // сигнатуре отрабатывает до первой строки функции, и его собственный
    // отказ уехал бы клиенту как `text/plain` мимо `ApiError` — то самое
    // «пустое тело выглядит сломанным сервером», ради которого заведён
    // `fallback`. Заодно это ставит разбор тела после адресной проверки:
    // чужому незачем слышать про формат JSON.
    request: Result<Json<SetupRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<SetupResponse>), ApiError> {
    if !state.setup_from_any_address {
        // Смотрим на адрес клиента, а не на адрес прослушивания: проверка
        // по `listen` открыла бы настройку всему интернету на любом
        // инстансе, слушающем `0.0.0.0`.
        if !peer.ip().is_loopback() {
            return Err(ApiError::Forbidden(SETUP_IS_LOCAL_ONLY));
        }

        // Но одного петлевого пира мало, и это не педантизм. При штатной
        // установке — nginx на том же хосте, `proxy_pass
        // http://127.0.0.1:9000` — TCP-пиром всегда оказывается сам
        // прокси, то есть `127.0.0.1`. Проверка выше в такой установке
        // истинна для кого угодно из интернета, и экран первичной
        // настройки, который она обязана закрывать, стоит открытым до
        // первого постучавшегося.
        //
        // Заголовок здесь используется **по факту наличия, а не по
        // содержимому**. Доверять тому, что в нём написано, нельзя: его
        // подделает кто угодно. А вот его присутствие — надёжное
        // свидетельство, что запрос прошёл через посредника и адрес пира
        // не говорит о клиенте ничего. Отказ в эту сторону безопасен:
        // подделав заголовок, чужой добьётся только собственного отказа.
        //
        // Владелец за прокси ставит `setup_from_any_address = true` или
        // заводит администратора через `wakode user create` — ровно то,
        // что спека и обещала ему сделать один раз.
        //
        // Чего это НЕ закрывает, названо прямо: голый
        // `proxy_pass http://127.0.0.1:9000;` без единого
        // `proxy_set_header` — это минимальный рабочий конфиг nginx, и он
        // не добавляет ни одного из перечисленных заголовков. Такая
        // установка по-прежнему оставляет экран открытым, и проверено это
        // живым прогоном, а не рассуждением.
        //
        // Настоящее решение — белый список доверенных посредников или
        // одноразовый токен настройки, печатаемый в журнал при первом
        // старте; и то, и другое относится к плану 3b. Здесь закрыто
        // подмножество, и закрыто в безопасную сторону: список заголовков
        // можно только расширять, каждая новая строка сужает проходящее.
        if let Some(header) = PROXY_HEADERS
            .iter()
            .find(|name| parts_have(&headers, name))
        {
            tracing::warn!(
                header = %header,
                "первичная настройка запрошена через посредника — адрес клиента неизвестен"
            );
            return Err(ApiError::Forbidden(SETUP_THROUGH_A_PROXY));
        }
    }

    // Закрыт навсегда после первого пользователя — независимо от того,
    // включена ли регистрация: здесь заводится администратор, а не
    // обычный аккаунт.
    //
    // Проверка идёт **после** адресной, а не до неё, как в плане. Порядок
    // выбран так, потому что адресная проверка — авторизационная: чужой
    // запрос не должен заставлять сервер ходить в базу. Секрета порядок не
    // прячет и не открывает: «инстанс уже настроен» и так лежит публично
    // в `/api/setup/status`. Цена — владелец за прокси, забывший
    // `setup_from_any_address`, на уже настроенном инстансе услышит про
    // адрес, а не про «уже настроено»; это сообщение ведёт к настоящей
    // причине отказа, так что цена приемлемая. Порядок держится тестом
    // `the_address_is_checked_before_the_database`.
    //
    // Остаточный риск, закрывать который тут нечем: два одновременных
    // запроса оба видят ноль и заводят двух администраторов. Окно — время
    // между `user_count` и `create_user`, и требует оно петлевого доступа
    // (или явно открытого `setup_from_any_address`). Единая транзакция
    // «посчитать и создать» потребовала бы нового метода в `wakode-store`:
    // `create_user` уходит своим соединением мимо очереди записи, и
    // счётчик с вставкой сегодня физически в разных транзакциях. Риск
    // назван осознанно, а не забыт.
    if state.store.user_count().await? > 0 {
        return Err(ApiError::Forbidden("первичная настройка уже выполнена"));
    }

    let Json(request) = request.map_err(|err| ApiError::BadRequest(err.body_text()))?;

    // Пробелы по краям срезаются, а не отвергаются: логин со случайным
    // пробелом иначе навсегда остался бы недостижимым с формы входа —
    // экран настройки к тому моменту уже закрыт.
    let login = request.login.trim();
    if login.is_empty() {
        return Err(ApiError::BadRequest("логин пуст".to_owned()));
    }

    // Считаем символы, а не байты: порог обещан пользователю в символах, и
    // на кириллическом пароле байтовая длина вдвое больше — граница уехала
    // бы туда, где её никто не ждёт.
    if request.password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(ApiError::BadRequest(format!(
            "пароль короче {MIN_PASSWORD_CHARS} символов"
        )));
    }

    let timezone: chrono_tz::Tz = request.timezone.parse().map_err(|_| {
        ApiError::BadRequest(format!("неизвестная таймзона: {}", request.timezone))
    })?;

    let password_hash = wakode_auth::hash_password(&request.password).map_err(|err| {
        tracing::error!(error = %err, "не удалось посчитать хеш пароля");
        ApiError::Internal
    })?;

    let user = state
        .store
        .create_user(NewUser {
            login: login.to_owned(),
            email: None,
            password_hash,
            display_name: None,
            timezone,
            timeout_secs: state.default_timeout_secs,
            is_admin: true,
        })
        .await?;

    Ok((StatusCode::CREATED, Json(SetupResponse { id: user.id })))
}
