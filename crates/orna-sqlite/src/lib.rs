//! Local Turso SQLite adapter for the backend-neutral Orna revision lifecycle.
//!
//! The first SQLite slice intentionally persists only the empty bootstrap
//! revision. Applying a non-empty compiler candidate is rejected explicitly;
//! no fields are silently discarded.

use std::{error::Error, fmt, future::Future, path::{Path, PathBuf}};

use orna_core::{
    CatalogueRevisionId, SourceBundleId, SourceRevisionId,
    canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
    catalogue::CatalogueSnapshot,
    revision::{ActiveDatabaseRevision, DeployableRevision, Sha256Digest, StoredSourceRevision},
};
use orna_storage::{BootstrapRevision, RevisionStore, StorageError};
use turso::{Builder, Connection, Value};

#[derive(Debug)]
pub enum SqliteError {
    Backend(turso::Error),
    InvalidPersistedData(&'static str),
    UnsupportedApply,
    Domain(String),
}

impl fmt::Display for SqliteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(error) => write!(f, "Turso backend error: {error}"),
            Self::InvalidPersistedData(message) => write!(f, "invalid persisted SQLite data: {message}"),
            Self::UnsupportedApply => f.write_str("SQLite adapter does not yet support applying non-empty revisions"),
            Self::Domain(message) => write!(f, "Orna domain error: {message}"),
        }
    }
}
impl Error for SqliteError {}
impl From<turso::Error> for SqliteError { fn from(error: turso::Error) -> Self { Self::Backend(error) } }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteConfig { path: PathBuf }
impl SqliteConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self { Self { path: path.into() } }
    pub fn path(&self) -> &Path { &self.path }
}

#[derive(Clone)]
pub struct SqliteRevisionStore { connection: Connection }

impl SqliteRevisionStore {
    pub async fn open(config: &SqliteConfig) -> Result<Self, SqliteError> {
        let path = config.path.to_str().ok_or(SqliteError::InvalidPersistedData("database path is not UTF-8"))?;
        let database = Builder::new_local(path).build().await?;
        let connection = database.connect()?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS orna_active_revision (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                source_revision_id BLOB NOT NULL UNIQUE CHECK (length(source_revision_id) = 16),
                catalogue_revision_id BLOB NOT NULL UNIQUE CHECK (length(catalogue_revision_id) = 16)
            );",
        ).await?;
        Ok(Self { connection })
    }

    async fn load_pair(&self) -> Result<Option<BootstrapRevision>, SqliteError> {
        let mut rows = self.connection.query(
            "SELECT source_revision_id, catalogue_revision_id FROM orna_active_revision WHERE singleton = 1",
            (),
        ).await?;
        let Some(row) = rows.next().await? else { return Ok(None) };
        let source: Vec<u8> = row.get(0)?;
        let catalogue: Vec<u8> = row.get(1)?;
        let source: [u8; 16] = source.try_into().map_err(|_| SqliteError::InvalidPersistedData("source revision id must be 16 bytes"))?;
        let catalogue: [u8; 16] = catalogue.try_into().map_err(|_| SqliteError::InvalidPersistedData("catalogue revision id must be 16 bytes"))?;
        Ok(Some(BootstrapRevision::new(SourceRevisionId::from_bytes(source), CatalogueRevisionId::from_bytes(catalogue))))
    }

    async fn seed_pair(&self) -> Result<BootstrapRevision, SqliteError> {
        if let Some(pair) = self.load_pair().await? { return Ok(pair); }
        let pair = BootstrapRevision::new(SourceRevisionId::new(), CatalogueRevisionId::new());
        self.connection.execute(
            "INSERT INTO orna_active_revision (singleton, source_revision_id, catalogue_revision_id) VALUES (1, ?1, ?2)",
            [Value::Blob(pair.source().to_bytes().to_vec()), Value::Blob(pair.catalogue().to_bytes().to_vec())],
        ).await?;
        Ok(self.load_pair().await?.ok_or(SqliteError::InvalidPersistedData("bootstrap row disappeared"))?)
    }

    pub async fn bootstrap(&self) -> Result<BootstrapRevision, StorageError<SqliteError>> {
        self.seed_pair().await.map_err(StorageError::Backend)
    }

    pub async fn recover(&self) -> Result<ActiveDatabaseRevision, StorageError<SqliteError>> {
        let pair = self.load_pair().await.map_err(StorageError::Backend)?.ok_or_else(|| StorageError::Backend(SqliteError::InvalidPersistedData("database has not been bootstrapped")))?;
        let bundle = SourceBundleId::new();
        let empty_units = Vec::new();
        let source_bundle_hash = source_bundle_digest(&empty_units).map_err(|e| StorageError::Backend(SqliteError::Domain(e.to_string())))?;
        let source_hash = source_revision_record_digest(bundle, None, source_bundle_hash).map_err(|e| StorageError::Backend(SqliteError::Domain(e.to_string())))?;
        let source = StoredSourceRevision::new(bundle, pair.source(), None, empty_units, source_bundle_hash, source_hash)
            .map_err(|e| StorageError::Backend(SqliteError::Domain(e.to_string())))?;
        let catalogue = CatalogueSnapshot::new(pair.catalogue(), Vec::new(), Vec::new())
            .map_err(|e| StorageError::Backend(SqliteError::Domain(e.to_string())))?;
        let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[])
            .map_err(|e| StorageError::Backend(SqliteError::Domain(e.to_string())))?;
        ActiveDatabaseRevision::new(pair_to_revision_pair(pair), source, catalogue, catalogue_hash, Vec::new(), Vec::new(), Vec::new(), Vec::new())
            .map_err(|e| StorageError::Backend(SqliteError::Domain(e.to_string())))
    }

    pub async fn apply(&self, _candidate: &DeployableRevision) -> Result<ActiveDatabaseRevision, StorageError<SqliteError>> {
        Err(StorageError::Backend(SqliteError::UnsupportedApply))
    }
}

fn pair_to_revision_pair(pair: BootstrapRevision) -> orna_core::revision::RevisionPair {
    orna_core::revision::RevisionPair::new(pair.source(), pair.catalogue())
}

impl RevisionStore for SqliteRevisionStore {
    type Error = SqliteError;
    fn bootstrap(&self) -> impl Future<Output = Result<BootstrapRevision, StorageError<Self::Error>>> + Send { self.bootstrap() }
    fn recover(&self) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send { self.recover() }
    fn apply(&self, candidate: &DeployableRevision) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send { self.apply(candidate) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path() -> PathBuf {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("orna-sqlite-{nonce}.db"))
    }

    #[tokio::test]
    async fn opens_local_file_bootstraps_idempotently_and_recovers_empty_revision() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path)).await.unwrap();
        let first = store.bootstrap().await.unwrap();
        let second = store.bootstrap().await.unwrap();
        assert_eq!(first, second);
        let recovered = store.recover().await.unwrap();
        assert_eq!(recovered.pair().source(), first.source());
        assert_eq!(recovered.pair().catalogue(), first.catalogue());
        assert!(recovered.source().units().is_empty());
        assert!(recovered.catalogue().schemas().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn apply_fails_closed_until_full_revision_representation_exists() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path)).await.unwrap();
        let error = store.apply(unsafe { std::mem::MaybeUninit::zeroed().assume_init() }).await.unwrap_err();
        assert!(matches!(error, StorageError::Backend(SqliteError::UnsupportedApply)));
        let _ = std::fs::remove_file(path);
    }
}
