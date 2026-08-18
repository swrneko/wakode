use std::path::Path;

use wakode_store::SqliteStore;

pub async fn backup(store: &SqliteStore, to: &Path) -> anyhow::Result<()> {
    // Отказывает, если файл уже есть: `VACUUM INTO` не перезаписывает, и
    // ротацию имён решает вызывающий, а не эта подкоманда.
    store.backup(to).await?;
    println!("снимок записан в {}", to.display());
    Ok(())
}
