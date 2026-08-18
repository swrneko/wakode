//! Разбор командной строки.
//!
//! Все подкоманды, кроме `master-key generate`, читают тот же конфиг, что и
//! сервер: иначе `wakode user create` завёл бы пользователя в одной базе, а
//! сервер искал бы его в другой. Ради этого `--config` объявлен на верхнем
//! уровне, а не у каждой подкоманды.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

pub mod backup;
pub mod key;
pub mod user;

#[derive(Debug, Parser)]
#[command(
    name = "wakode",
    about = "Selfhosted-трекер времени, совместимый с WakaTime"
)]
pub struct Cli {
    /// Путь к файлу конфигурации. По умолчанию ./wakode.toml.
    ///
    /// `global`, потому что флаг принимается и до подкоманды, и после неё:
    /// `wakode --config … user list` и `wakode user list --config …`
    /// набирают одинаково охотно.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Поднять сервер (подразумевается, если подкоманда не указана).
    Serve,
    /// Применить миграции и выйти.
    Migrate,
    /// Операции с мастер-ключом.
    #[command(subcommand, name = "master-key")]
    MasterKey(MasterKeyCommand),
    /// Операции с пользователями.
    #[command(subcommand)]
    User(UserCommand),
    /// Операции с API-ключами.
    #[command(subcommand)]
    Key(KeyCommand),
    /// Консистентный снимок базы.
    Backup {
        #[arg(long)]
        to: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum MasterKeyCommand {
    /// Напечатать новый мастер-ключ в base64.
    Generate,
}

#[derive(Debug, Subcommand)]
pub enum UserCommand {
    /// Создать пользователя.
    ///
    /// Пароля среди флагов нет намеренно: он остался бы в истории оболочки
    /// и в выводе `ps`, где его увидит любой процесс того же пользователя.
    /// Спрашивается со stdin.
    Create {
        #[arg(long)]
        login: String,
        #[arg(long)]
        admin: bool,
        #[arg(long, default_value = "UTC")]
        timezone: String,
    },
    /// Показать пользователей.
    List,
}

#[derive(Debug, Subcommand)]
pub enum KeyCommand {
    /// Выдать ключ. Значение печатается один раз.
    Issue {
        #[arg(long)]
        user: String,
        #[arg(long)]
        name: String,
    },
    /// Отозвать ключ.
    Revoke {
        #[arg(long)]
        id: uuid::Uuid,
    },
}
