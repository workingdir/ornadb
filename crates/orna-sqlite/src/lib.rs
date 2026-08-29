//! Local Turso SQLite adapter for the backend-neutral Orna revision lifecycle.
//!
//! The first SQLite slice intentionally persists only the empty bootstrap
//! revision. Applying a non-empty compiler candidate is rejected explicitly;
//! no fields are silently discarded.

use std::{
    error::Error,
    fmt,
    future::Future,
    path::{Path, PathBuf},
};

use orna_core::{
    CatalogueRevisionId, SourceBundleId, SourceRevisionId,
    canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
    catalogue::CatalogueSnapshot,
    revision::{ActiveDatabaseRevision, DeployableRevision, RevisionPair, StoredSourceRevision},
};
use orna_storage::{BootstrapRevision, RevisionStore, StorageError};
use turso::{Builder, Connection, Value};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS orna_active_revision (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    source_revision_id BLOB NOT NULL UNIQUE CHECK (length(source_revision_id) = 16),
    catalogue_revision_id BLOB NOT NULL UNIQUE CHECK (length(catalogue_revision_id) = 16),
    source_bundle_id BLOB NOT NULL CHECK (length(source_bundle_id) = 16),
    source_bundle_hash BLOB NOT NULL CHECK (length(source_bundle_hash) = 32),
    source_revision_hash BLOB NOT NULL CHECK (length(source_revision_hash) = 32),
    catalogue_hash BLOB NOT NULL CHECK (length(catalogue_hash) = 32)
);";

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
        connection.execute_batch(SCHEMA).await?;
        Ok(Self { connection })
    }

    async fn load_pair(&self) -> Result<Option<BootstrapRevision>, SqliteError> {
        let mut rows = self.connection.query(
            "SELECT source_revision_id, catalogue_revision_id FROM orna_active_revision WHERE singleton = 1",
            (),
        ).await?;
        let Some(row) = rows.next().await? else { return Ok(None) };
        let source: [u8; 16] = row.get::<Vec<u8>>(0)?.try_into()
            .map_err(|_| SqliteError::InvalidPersistedData("source revision id must be 16 bytes"))?;
        let catalogue: [u8; 16] = row.get::<Vec<u8>>(1)?.try_into()
            .map_err(|_| SqliteError::InvalidPersistedData("catalogue revision id must be 16 bytes"))?;
        Ok(Some(BootstrapRevision::new(SourceRevisionId::from_bytes(source), CatalogueRevisionId::from_bytes(catalogue))))
    }

    async fn seed_pair(&self) -> Result<BootstrapRevision, SqliteError> {
        if let Some(pair) = self.load_pair().await? {
            return Ok(pair);
        }
        let pair = BootstrapRevision::new(SourceRevisionId::new(), CatalogueRevisionId::new());
        let bundle = SourceBundleId::new();
        let source_bundle_hash = source_bundle_digest(&[]).map_err(|e| SqliteError::Domain(e.to_string()))?;
        let source_hash = source_revision_record_digest(bundle, None, source_bundle_hash)
            .map_err(|e| SqliteError::Domain(e.to_string()))?;
        let catalogue = CatalogueSnapshot::new(pair.catalogue(), Vec::new(), Vec::new())
            .map_err(|e| SqliteError::Domain(e.to_string()))?;
        let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[])
            .map_err(|e| SqliteError::Domain(e.to_string()))?;
        self.connection.execute(
            "INSERT INTO orna_active_revision
             (singleton, source_revision_id, catalogue_revision_id, source_bundle_id,
              source_bundle_hash, source_revision_hash, catalogue_hash)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
            [
                Value::Blob(pair.source().to_bytes().to_vec()),
                Value::Blob(pair.catalogue().to_bytes().to_vec()),
                Value::Blob(bundle.to_bytes().to_vec()),
                Value::Blob(source_bundle_hash.to_bytes().to_vec()),
                Value::Blob(source_hash.to_bytes().to_vec()),
                Value::Blob(catalogue_hash.to_bytes().to_vec()),
            ],
        ).await?;
        Ok(self.load_pair().await?.ok_or(SqliteError::InvalidPersistedData("bootstrap row disappeared"))?)
    }

    async fn load_active(&self) -> Result<ActiveDatabaseRevision, SqliteError> {
        let pair = self.load_pair().await?.ok_or(SqliteError::InvalidPersistedData("database has not been bootstrapped"))?;
        let bundle = SourceBundleId::new();
        let source_bundle_hash = source_bundle_digest(&[]).map_err(|e| SqliteError::Domain(e.to_string()))?;
        let source_hash = source_revision_record_digest(bundle, None, source_bundle_hash).map_err(|e| SqliteError::Domain(e.to_string()))?;
        let source = StoredSourceRevision::new(bundle, pair.source(), None, Vec::new(), source_bundle_hash, source_hash)
            .map_err(|e| SqliteError::Domain(e.to_string()))?;
        let catalogue = CatalogueSnapshot::new(pair.catalogue(), Vec::new(), Vec::new())
            .map_err(|e| SqliteError::Domain(e.to_string()))?;
        let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[])
            .map_err(|e| SqliteError::Domain(e.to_string()))?;
        ActiveDatabaseRevision::new(RevisionPair::new(pair.source(), pair.catalogue()), source, catalogue, catalogue_hash, Vec::new(), Vec::new(), Vec::new(), Vec::new())
            .map_err(|e| SqliteError::Domain(e.to_string()))
    }

    pub async fn recover(&self) -> Result<ActiveDatabaseRevision, StorageError<SqliteError>> {
        self.load_active().await.map_err(StorageError::Backend)
    }

    pub async fn apply(&self, candidate: &DeployableRevision) -> Result<ActiveDatabaseRevision, StorageError<SqliteError>> {
        if !candidate.source().units().is_empty()
            || !candidate.candidate().schemas().is_empty()
            || !candidate.candidate().enum_types().is_empty()
            || !candidate.candidate().object_types().is_empty()
            || !candidate.candidate().value_types().is_empty()
            || !candidate.origins().is_empty()
            || !candidate.expressions().is_empty()
            || !candidate.new_function_revisions().is_empty()
            || !candidate.references().is_empty()
        {
            return Err(StorageError::Backend(SqliteError::UnsupportedApply));
        }
        let active = self.load_pair().await.map_err(|e| StorageError::Backend(e))?
            .ok_or(SqliteError::InvalidPersistedData("database has not been bootstrapped"))
            .map_err(StorageError::Backend)?;
        if candidate.expected_base() != RevisionPair::new(active.source(), active.catalogue()) {
            return Err(StorageError::Backend(SqliteError::InvalidPersistedData("candidate base does not match active revision")));
        }
        let tx = self.connection.unchecked_transaction().await.map_err(|e| StorageError::Backend(SqliteError::from(e)))?;
        let pair = candidate.candidate_pair();
        let source = candidate.source();
        tx.execute(
            "UPDATE orna_active_revision SET source_revision_id=?1, catalogue_revision_id=?2,
             source_bundle_id=?3, source_bundle_hash=?4, source_revision_hash=?5, catalogue_hash=?6
             WHERE singleton=1",
            [
                Value::Blob(pair.source().to_bytes().to_vec()),
                Value::Blob(pair.catalogue().to_bytes().to_vec()),
                Value::Blob(source.bundle().to_bytes().to_vec()),
                Value::Blob(source.bundle_hash().to_bytes().to_vec()),
                Value::Blob(source.revision_hash().to_bytes().to_vec()),
                Value::Blob(candidate.catalogue_hash().to_bytes().to_vec()),
            ],
        ).await.map_err(|e| StorageError::Backend(SqliteError::from(e)))?;
        tx.commit().await.map_err(|e| StorageError::Backend(SqliteError::from(e)))?;
        self.load_active().await.map_err(StorageError::Backend)
    }

}

fn pair_to_revision_pair(pair: BootstrapRevision) -> orna_core::revision::RevisionPair {
    orna_core::revision::RevisionPair::new(pair.source(), pair.catalogue())
}

impl RevisionStore for SqliteRevisionStore {
    type Error = SqliteError;
    fn bootstrap(&self) -> impl Future<Output = Result<BootstrapRevision, StorageError<Self::Error>>> + Send { async move { self.seed_pair().await.map_err(StorageError::Backend) } }
    fn recover(&self) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send { SqliteRevisionStore::recover(self) }
    fn apply(&self, candidate: &DeployableRevision) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send { SqliteRevisionStore::apply(self, candidate) }
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
}
