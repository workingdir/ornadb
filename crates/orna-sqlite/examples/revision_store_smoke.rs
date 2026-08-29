use std::{
    env,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use orna_sqlite::{SqliteConfig, SqliteRevisionStore};
use orna_storage::RevisionStore;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (path, remove_after) = match env::args_os().nth(1) {
        Some(path) => (PathBuf::from(path), false),
        None => {
            let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let path = env::temp_dir().join(format!(
                "orna-sqlite-revision-store-smoke-{}-{nonce}.db",
                std::process::id()
            ));
            (path, true)
        }
    };
    let store = SqliteRevisionStore::open(&SqliteConfig::new(&path)).await?;
    let bootstrapped = store.bootstrap().await?;
    let recovered = store.recover().await?;
    let ledger = store.read_ledger().await?;
    println!(
        "SQLite revision store ready: path={} source={:?} catalogue={:?} recovered={:?} ledger_entries={}",
        path.display(),
        bootstrapped.source(),
        bootstrapped.catalogue(),
        recovered.pair(),
        ledger.len()
    );
    if remove_after {
        std::fs::remove_file(path)?;
    }
    Ok(())
}
