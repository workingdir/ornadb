//! Local Turso SQLite adapter for the backend-neutral Orna revision lifecycle.
//!
//! This slice persists source units and schemas from compiler-produced
//! candidates. Other catalogue categories and semantic artifacts remain
//! explicitly fail-closed until their durable codecs are implemented.

use std::{
    error::Error,
    fmt,
    future::Future,
    path::{Path, PathBuf},
};

use orna_core::{
    CatalogueRevisionId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
    canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
    catalogue::{CatalogueSnapshot, QualifiedSemanticName, SchemaDefinition},
    revision::{
        ActiveDatabaseRevision, DeployableRevision, RevisionPair, StoredSourceRevision,
        StoredSourceUnit,
    },
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
);
CREATE TABLE IF NOT EXISTS orna_source_units (
    source_revision_id BLOB NOT NULL,
    source_unit_id BLOB NOT NULL CHECK (length(source_unit_id) = 16),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    logical_path TEXT NOT NULL,
    content TEXT NOT NULL,
    content_hash BLOB NOT NULL CHECK (length(content_hash) = 32),
    PRIMARY KEY (source_revision_id, source_unit_id),
    UNIQUE (source_revision_id, ordinal),
    UNIQUE (source_revision_id, logical_path)
);
CREATE TABLE IF NOT EXISTS orna_catalogue_schemas (
    catalogue_revision_id BLOB NOT NULL,
    schema_id BLOB NOT NULL CHECK (length(schema_id) = 16),
    name_parts TEXT NOT NULL,
    source_unit_id BLOB NOT NULL CHECK (length(source_unit_id) = 16),
    source_start INTEGER NOT NULL CHECK (source_start >= 0),
    source_end INTEGER NOT NULL CHECK (source_end >= source_start),
    PRIMARY KEY (catalogue_revision_id, schema_id)
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
            Self::InvalidPersistedData(message) => {
                write!(f, "invalid persisted SQLite data: {message}")
            }
            Self::UnsupportedApply => {
                f.write_str("SQLite adapter does not yet support applying non-empty revisions")
            }
            Self::Domain(message) => write!(f, "Orna domain error: {message}"),
        }
    }
}
impl Error for SqliteError {}
impl From<turso::Error> for SqliteError {
    fn from(error: turso::Error) -> Self {
        Self::Backend(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteConfig {
    path: PathBuf,
}
impl SqliteConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Clone)]
pub struct SqliteRevisionStore {
    connection: Connection,
}

impl SqliteRevisionStore {
    pub async fn open(config: &SqliteConfig) -> Result<Self, SqliteError> {
        let path = config
            .path
            .to_str()
            .ok_or(SqliteError::InvalidPersistedData(
                "database path is not UTF-8",
            ))?;
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
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let source: [u8; 16] = row.get::<Vec<u8>>(0)?.try_into().map_err(|_| {
            SqliteError::InvalidPersistedData("source revision id must be 16 bytes")
        })?;
        let catalogue: [u8; 16] = row.get::<Vec<u8>>(1)?.try_into().map_err(|_| {
            SqliteError::InvalidPersistedData("catalogue revision id must be 16 bytes")
        })?;
        Ok(Some(BootstrapRevision::new(
            SourceRevisionId::from_bytes(source),
            CatalogueRevisionId::from_bytes(catalogue),
        )))
    }

    async fn seed_pair(&self) -> Result<BootstrapRevision, SqliteError> {
        if let Some(pair) = self.load_pair().await? {
            return Ok(pair);
        }
        let pair = BootstrapRevision::new(SourceRevisionId::new(), CatalogueRevisionId::new());
        let bundle = SourceBundleId::new();
        let source_bundle_hash =
            source_bundle_digest(&[]).map_err(|e| SqliteError::Domain(e.to_string()))?;
        let source_hash = source_revision_record_digest(bundle, None, source_bundle_hash)
            .map_err(|e| SqliteError::Domain(e.to_string()))?;
        let catalogue = CatalogueSnapshot::new(pair.catalogue(), Vec::new(), Vec::new())
            .map_err(|e| SqliteError::Domain(e.to_string()))?;
        let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[])
            .map_err(|e| SqliteError::Domain(e.to_string()))?;
        self.connection
            .execute(
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
            )
            .await?;
        self.load_pair()
            .await?
            .ok_or(SqliteError::InvalidPersistedData(
                "bootstrap row disappeared",
            ))
    }

    async fn load_active(&self) -> Result<ActiveDatabaseRevision, SqliteError> {
        let mut rows = self.connection.query(
            "SELECT source_revision_id, catalogue_revision_id, source_bundle_id, source_bundle_hash,
                    source_revision_hash, catalogue_hash FROM orna_active_revision WHERE singleton = 1", (),
        ).await?;
        let Some(row) = rows.next().await? else {
            return Err(SqliteError::InvalidPersistedData(
                "database has not been bootstrapped",
            ));
        };
        let source_id =
            SourceRevisionId::from_bytes(id16(row.get::<Vec<u8>>(0)?, "source revision id")?);
        let catalogue_id =
            CatalogueRevisionId::from_bytes(id16(row.get::<Vec<u8>>(1)?, "catalogue revision id")?);
        let bundle = SourceBundleId::from_bytes(id16(row.get::<Vec<u8>>(2)?, "source bundle id")?);
        let bundle_hash = digest32(row.get::<Vec<u8>>(3)?, "source bundle hash")?;
        let source_hash = digest32(row.get::<Vec<u8>>(4)?, "source revision hash")?;
        let catalogue_hash = digest32(row.get::<Vec<u8>>(5)?, "catalogue hash")?;
        let mut unit_rows = self.connection.query(
            "SELECT source_unit_id, ordinal, logical_path, content, content_hash FROM orna_source_units WHERE source_revision_id = ?1 ORDER BY ordinal",
            [Value::Blob(source_id.to_bytes().to_vec())],
        ).await?;
        let mut units = Vec::new();
        while let Some(unit) = unit_rows.next().await? {
            units.push(
                StoredSourceUnit::new(
                    SourceUnitId::from_bytes(id16(unit.get::<Vec<u8>>(0)?, "source unit id")?),
                    u32::try_from(unit.get::<i64>(1)?).map_err(|_| {
                        SqliteError::InvalidPersistedData("source unit ordinal must fit u32")
                    })?,
                    unit.get::<String>(2)?,
                    unit.get::<String>(3)?,
                    digest32(unit.get::<Vec<u8>>(4)?, "source unit hash")?,
                )
                .map_err(|e| SqliteError::Domain(e.to_string()))?,
            );
        }
        if source_bundle_digest(&units).map_err(|e| SqliteError::Domain(e.to_string()))?
            != bundle_hash
        {
            return Err(SqliteError::InvalidPersistedData(
                "source bundle hash mismatch",
            ));
        }
        let source =
            StoredSourceRevision::new(bundle, source_id, None, units, bundle_hash, source_hash)
                .map_err(|e| SqliteError::Domain(e.to_string()))?;
        let mut schema_rows = self.connection.query(
            "SELECT schema_id, name_parts FROM orna_catalogue_schemas WHERE catalogue_revision_id = ?1 ORDER BY rowid",
            [Value::Blob(catalogue_id.to_bytes().to_vec())],
        ).await?;
        let mut schemas = Vec::new();
        while let Some(schema) = schema_rows.next().await? {
            let parts = schema
                .get::<String>(1)?
                .split('\u{1f}')
                .map(str::to_owned)
                .collect::<Vec<_>>();
            schemas.push(SchemaDefinition::new(
                SchemaId::from_bytes(id16(schema.get::<Vec<u8>>(0)?, "schema id")?),
                QualifiedSemanticName::new(parts)
                    .map_err(|e| SqliteError::Domain(e.to_string()))?,
            ));
        }
        let catalogue = CatalogueSnapshot::new(catalogue_id, schemas, Vec::new())
            .map_err(|e| SqliteError::Domain(e.to_string()))?;
        ActiveDatabaseRevision::new(
            RevisionPair::new(source_id, catalogue_id),
            source,
            catalogue,
            catalogue_hash,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .map_err(|e| SqliteError::Domain(e.to_string()))
    }

    pub async fn recover(&self) -> Result<ActiveDatabaseRevision, StorageError<SqliteError>> {
        self.load_active().await.map_err(StorageError::Backend)
    }

    pub async fn apply(
        &self,
        candidate: &DeployableRevision,
    ) -> Result<ActiveDatabaseRevision, StorageError<SqliteError>> {
        if !candidate.source().units().is_empty() && candidate.source().units().len() != 1
            || !candidate.candidate().object_types().is_empty()
            || !candidate.candidate().value_types().is_empty()
            || !candidate.candidate().enum_types().is_empty()
            || !candidate.candidate().record_value_types().is_empty()
            || !candidate.candidate().type_bindings().is_empty()
            || !candidate.candidate().functions().is_empty()
            || !candidate.origins().iter().all(|origin| {
                matches!(
                    origin.identity(),
                    orna_core::revision::DefinitionIdentity::Schema(_)
                )
            })
            || !candidate.expressions().is_empty()
            || !candidate.new_function_revisions().is_empty()
            || !candidate.references().is_empty()
            || candidate.catalogue_hash_context().version()
                != orna_core::revision::CatalogueHashVersion::Version1
        {
            return Err(StorageError::Backend(SqliteError::UnsupportedApply));
        }
        let active = self
            .load_pair()
            .await
            .map_err(StorageError::Backend)?
            .ok_or(SqliteError::InvalidPersistedData(
                "database has not been bootstrapped",
            ))
            .map_err(StorageError::Backend)?;
        if candidate.expected_base() != RevisionPair::new(active.source(), active.catalogue()) {
            return Err(StorageError::Backend(SqliteError::InvalidPersistedData(
                "candidate base does not match active revision",
            )));
        }
        let tx = turso::transaction::Transaction::new_unchecked(
            &self.connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await
        .map_err(|e| StorageError::Backend(SqliteError::from(e)))?;
        let source = candidate.source();
        let pair = candidate.candidate_pair();
        for unit in source.units() {
            tx.execute("INSERT INTO orna_source_units (source_revision_id, source_unit_id, ordinal, logical_path, content, content_hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", [Value::Blob(source.id().to_bytes().to_vec()), Value::Blob(unit.id().to_bytes().to_vec()), Value::Integer(i64::from(unit.ordinal())), Value::Text(unit.logical_path().to_owned()), Value::Text(unit.content().to_owned()), Value::Blob(unit.content_hash().to_bytes().to_vec())]).await.map_err(|e| StorageError::Backend(SqliteError::from(e)))?;
        }
        for origin in candidate.origins() {
            let schema_id = match origin.identity() {
                orna_core::revision::DefinitionIdentity::Schema(id) => id,
                _ => unreachable!(),
            };
            let schema = candidate
                .candidate()
                .schema_by_id(schema_id)
                .ok_or(SqliteError::InvalidPersistedData(
                    "schema origin has no candidate schema",
                ))
                .map_err(StorageError::Backend)?;
            let parts = schema.name().parts().join("\u{1f}");
            tx.execute("INSERT INTO orna_catalogue_schemas (catalogue_revision_id, schema_id, name_parts, source_unit_id, source_start, source_end) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", [Value::Blob(pair.catalogue().to_bytes().to_vec()), Value::Blob(schema.id().to_bytes().to_vec()), Value::Text(parts), Value::Blob(origin.source().source_unit().to_bytes().to_vec()), Value::Integer(i64::from(origin.source().byte_start())), Value::Integer(i64::from(origin.source().byte_end()))]).await.map_err(|e| StorageError::Backend(SqliteError::from(e)))?;
        }
        tx.execute("UPDATE orna_active_revision SET source_revision_id=?1, catalogue_revision_id=?2, source_bundle_id=?3, source_bundle_hash=?4, source_revision_hash=?5, catalogue_hash=?6 WHERE singleton=1", [Value::Blob(pair.source().to_bytes().to_vec()), Value::Blob(pair.catalogue().to_bytes().to_vec()), Value::Blob(source.bundle().to_bytes().to_vec()), Value::Blob(source.bundle_hash().to_bytes().to_vec()), Value::Blob(source.revision_hash().to_bytes().to_vec()), Value::Blob(candidate.catalogue_hash().to_bytes().to_vec())]).await.map_err(|e| StorageError::Backend(SqliteError::from(e)))?;
        tx.commit()
            .await
            .map_err(|e| StorageError::Backend(SqliteError::from(e)))?;
        self.load_active().await.map_err(StorageError::Backend)
    }
}

fn id16(value: Vec<u8>, field: &'static str) -> Result<[u8; 16], SqliteError> {
    value
        .try_into()
        .map_err(|_| SqliteError::InvalidPersistedData(field))
}
fn digest32(
    value: Vec<u8>,
    field: &'static str,
) -> Result<orna_core::revision::Sha256Digest, SqliteError> {
    Ok(orna_core::revision::Sha256Digest::from_bytes(
        value
            .try_into()
            .map_err(|_| SqliteError::InvalidPersistedData(field))?,
    ))
}

impl RevisionStore for SqliteRevisionStore {
    type Error = SqliteError;
    async fn bootstrap(
        &self,
    ) -> Result<BootstrapRevision, StorageError<Self::Error>> {
        self.seed_pair().await.map_err(StorageError::Backend)
    }
    fn recover(
        &self,
    ) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send
    {
        SqliteRevisionStore::recover(self)
    }
    fn apply(
        &self,
        candidate: &DeployableRevision,
    ) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send
    {
        SqliteRevisionStore::apply(self, candidate)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("orna-sqlite-{nonce}.db"))
    }

    #[tokio::test]
    async fn opens_local_file_bootstraps_idempotently_and_recovers_empty_revision() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
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
