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
    //
    // Проверка идёт до поиска пользователя намеренно: без ключа отказ
    // будет в любом случае, и сообщать сначала «нет такого пользователя»
    // значило бы отправить владельца чинить не то.
    let master = master
        .ok_or_else(|| anyhow::anyhow!("для выдачи ключа нужна переменная WAKODE_MASTER_KEY"))?;

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

    // Печатается один раз и только здесь: подсматривать значение через CLI
    // незачем, для показа есть настройки в интерфейсе. Строка отдельная и
    // без подписи — её копируют в конфиг плагина.
    println!("{value}");
    Ok(())
}

pub async fn revoke(store: &SqliteStore, id: uuid::Uuid) -> anyhow::Result<()> {
    // Отзыв несуществующего ключа отказом не считается: `revoke_key`
    // обновляет строку по идентификатору, и ноль затронутых строк здесь
    // неотличим от повторного отзыва. Различать их пришлось бы ради
    // сообщения, которое ничего не меняет.
    store.revoke_key(id).await?;
    println!("отозван {id}");
    Ok(())
}
