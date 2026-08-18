use std::io::BufRead;

use wakode_store::{NewUser, SqliteStore, UserRepo};

/// Прочитать пароль со стандартного ввода.
///
/// Флагом пароль не принимается: он остался бы в истории оболочки и в
/// выводе `ps`, где его увидит любой процесс того же пользователя.
///
/// Приглашение уходит в **stderr**, а не в stdout: вывод подкоманд —
/// данные (`wakode user list | …`), и приглашение, попавшее в конвейер,
/// сломало бы разбор, а в неинтерактивном сценарии осталось бы в файле.
///
/// Эха при вводе не отключается: для этого нужен доступ к терминалу
/// (`termios`), то есть ещё одна зависимость. Пароль виден на экране —
/// это хуже, чем невидимый, но лучше, чем пароль в истории оболочки, и
/// решается это отдельно, когда появится повод трогать терминал.
fn read_password(input: &mut impl BufRead) -> std::io::Result<String> {
    eprint!("Пароль: ");
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim_end_matches(['\n', '\r']).to_owned())
}

pub async fn create(
    store: &SqliteStore,
    login: String,
    admin: bool,
    timezone: String,
    // Из `[durations]`, а не из `wakode_core::DEFAULT_TIMEOUT_SECS`: пока
    // константа была прошита здесь и в экране первичной настройки, секция
    // конфига не читалась вообще, и `timeout_secs = 300` не значил ничего.
    timeout_secs: i64,
) -> anyhow::Result<()> {
    // Таймзона разбирается до того, как спрошен пароль: опечатка в ней
    // иначе всплывала бы после ввода, и вводить пришлось бы заново.
    let timezone: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| anyhow::anyhow!("неизвестная таймзона: {timezone}"))?;

    let password = read_password(&mut std::io::stdin().lock())?;
    if password.is_empty() {
        // Пустая строка — это не «пароль по умолчанию», а неинтерактивный
        // сценарий, забывший его передать. Завести такого пользователя
        // значило бы открыть вход по пустому паролю.
        anyhow::bail!("пароль пуст");
    }

    let user = store
        .create_user(NewUser {
            login,
            email: None,
            password_hash: wakode_auth::hash_password(&password)?,
            display_name: None,
            timezone,
            timeout_secs,
            is_admin: admin,
        })
        .await?;

    println!("{} {}", user.id, user.login);
    Ok(())
}

pub async fn list(store: &SqliteStore) -> anyhow::Result<()> {
    for user in store.list_users().await? {
        // Хеш пароля не печатается: вывод CLI уезжает в терминал, в
        // историю и в чужие скриншоты. Это закреплено тестом.
        println!(
            "{}\t{}\t{}",
            user.id,
            user.login,
            if user.is_admin { "админ" } else { "" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trailing_newline_is_not_part_of_the_password() {
        // Пароль, к которому приклеился `\n`, захешируется и сохранится, а
        // войти с ним будет нельзя ничем, кроме этой же подкоманды: форму
        // ввода в вебе никто не заставит дописать перевод строки.
        assert_eq!(
            read_password(&mut "секрет\n".as_bytes()).unwrap(),
            "секрет"
        );
        // CRLF — тот же случай на выводе оболочки Windows.
        assert_eq!(
            read_password(&mut "секрет\r\n".as_bytes()).unwrap(),
            "секрет"
        );
        // А вот пробелы внутри и по краям — часть пароля: обрезать их
        // значило бы принять один пароль, а сохранить другой.
        assert_eq!(
            read_password(&mut " с пробелами \n".as_bytes()).unwrap(),
            " с пробелами "
        );
    }

    #[test]
    fn an_empty_line_reads_as_an_empty_password() {
        // Отличать «пусто» от «ничего не введено» здесь нечем и незачем:
        // вызывающий отказывает и на то, и на другое.
        assert_eq!(read_password(&mut "\n".as_bytes()).unwrap(), "");
        assert_eq!(read_password(&mut "".as_bytes()).unwrap(), "");
    }
}
