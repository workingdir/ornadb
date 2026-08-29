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
    sync::Arc,
};

use orna_core::{
    CatalogueRevisionId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
    canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
    catalogue::{CatalogueSnapshot, QualifiedSemanticName, SchemaDefinition},
    physical::PhysicalMigrationArtifact,
    revision::{
        ActiveDatabaseRevision, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
        DeployableRevision, RevisionPair, Sha256Digest, SourceOrigin, StoredSourceRevision,
        StoredSourceUnit,
    },
};
use orna_storage::{
    BootstrapRevision, MigrationLedgerEntry, MigrationLedgerEntryError, RevisionStore, StorageError,
};
use tokio::sync::Mutex;
use turso::{Builder, Connection, Value};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS orna_active_revision (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    source_revision_id BLOB NOT NULL UNIQUE CHECK (length(source_revision_id) = 16),
    source_parent_revision_id BLOB CHECK (
        source_parent_revision_id IS NULL OR length(source_parent_revision_id) = 16
    ),
    catalogue_revision_id BLOB NOT NULL UNIQUE CHECK (length(catalogue_revision_id) = 16),
    source_bundle_id BLOB NOT NULL CHECK (length(source_bundle_id) = 16),
    source_bundle_hash BLOB NOT NULL CHECK (length(source_bundle_hash) = 32),
    source_revision_hash BLOB NOT NULL CHECK (length(source_revision_hash) = 32),
    catalogue_hash BLOB NOT NULL CHECK (length(catalogue_hash) = 32)
);
CREATE TABLE IF NOT EXISTS orna_source_revisions (
    source_revision_id BLOB NOT NULL PRIMARY KEY CHECK (length(source_revision_id) = 16),
    source_parent_revision_id BLOB CHECK (
        source_parent_revision_id IS NULL OR length(source_parent_revision_id) = 16
    ),
    source_bundle_id BLOB NOT NULL CHECK (length(source_bundle_id) = 16),
    source_bundle_hash BLOB NOT NULL CHECK (length(source_bundle_hash) = 32),
    source_revision_hash BLOB NOT NULL CHECK (length(source_revision_hash) = 32)
);
CREATE TABLE IF NOT EXISTS orna_catalogue_revisions (
    catalogue_revision_id BLOB NOT NULL PRIMARY KEY CHECK (length(catalogue_revision_id) = 16),
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
);
CREATE TABLE IF NOT EXISTS orna_application_migrations (
    ordinal INTEGER PRIMARY KEY CHECK (ordinal >= 0),
    format TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    expected_source_revision_id BLOB NOT NULL CHECK (length(expected_source_revision_id) = 16),
    expected_catalogue_revision_id BLOB NOT NULL CHECK (length(expected_catalogue_revision_id) = 16),
    candidate_source_revision_id BLOB NOT NULL CHECK (length(candidate_source_revision_id) = 16),
    candidate_catalogue_revision_id BLOB NOT NULL CHECK (length(candidate_catalogue_revision_id) = 16),
    canonical_bytes BLOB NOT NULL CHECK (length(canonical_bytes) > 0),
    digest BLOB NOT NULL CHECK (length(digest) = 32),
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (candidate_source_revision_id, candidate_catalogue_revision_id)
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
                f.write_str("SQLite adapter does not yet support applying non-schema revisions")
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
    connection: Arc<Mutex<Connection>>,
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
        let mut connection = database.connect()?;
        ensure_schema(&mut connection).await?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    async fn seed_pair(&self) -> Result<BootstrapRevision, SqliteError> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut *connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await?;
        let result = seed_pair_in_transaction(&transaction).await;
        match result {
            Ok(pair) => {
                transaction.commit().await?;
                Ok(pair)
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }

    /// Recovers the active durable revision after validating all ledger rows.
    pub async fn recover(&self) -> Result<ActiveDatabaseRevision, StorageError<SqliteError>> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut *connection,
            turso::transaction::TransactionBehavior::Deferred,
        )
        .await
        .map_err(SqliteError::from)
        .map_err(StorageError::Backend)?;

        let result = async {
            let ledger = load_ledger_from(&*transaction)
                .await
                .map_err(StorageError::Backend)?;
            let active = load_active_from(&*transaction)
                .await
                .map_err(StorageError::Backend)?;
            if let Some(last) = ledger.last()
                && last.candidate_pair() != active.pair()
            {
                return Err(StorageError::Backend(SqliteError::InvalidPersistedData(
                    "migration ledger does not end at active revision",
                )));
            }
            Ok(active)
        }
        .await;

        match result {
            Ok(active) => {
                transaction
                    .commit()
                    .await
                    .map_err(SqliteError::from)
                    .map_err(StorageError::Backend)?;
                Ok(active)
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }

    /// Reads the durable migration ledger oldest-first.
    pub async fn read_ledger(
        &self,
    ) -> Result<Vec<MigrationLedgerEntry>, StorageError<SqliteError>> {
        let connection = self.connection.lock().await;
        load_ledger_from(&connection)
            .await
            .map_err(StorageError::Backend)
    }

    /// Compatibility entry point that plans the exact artifact before applying.
    pub async fn apply(
        &self,
        candidate: &DeployableRevision,
    ) -> Result<ActiveDatabaseRevision, StorageError<SqliteError>> {
        let active = self.recover().await?;
        let artifact =
            PhysicalMigrationArtifact::from_revisions(&active, candidate).map_err(|error| {
                StorageError::InvalidRequest(MigrationLedgerEntryError::PhysicalArtifact(error))
            })?;
        self.apply_with_artifact(candidate, &artifact).await
    }

    async fn apply_with_artifact(
        &self,
        candidate: &DeployableRevision,
        artifact: &PhysicalMigrationArtifact,
    ) -> Result<ActiveDatabaseRevision, StorageError<SqliteError>> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut *connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await
        .map_err(SqliteError::from)
        .map_err(StorageError::Backend)?;

        let result = apply_in_transaction(&transaction, candidate, artifact).await;
        match result {
            Ok(()) => {
                let active = match load_active_from(&*transaction).await {
                    Ok(active) => active,
                    Err(error) => {
                        let _ = transaction.rollback().await;
                        return Err(StorageError::Backend(error));
                    }
                };
                transaction
                    .commit()
                    .await
                    .map_err(SqliteError::from)
                    .map_err(StorageError::Backend)?;
                Ok(active)
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }
}

async fn ensure_schema(connection: &mut Connection) -> Result<(), SqliteError> {
    connection.execute_batch(SCHEMA).await?;

    let transaction = turso::transaction::Transaction::new(
        connection,
        turso::transaction::TransactionBehavior::Immediate,
    )
    .await?;
    let result = async {
        // Legacy databases predate source_parent_revision_id. Their active source
        // lineage is therefore recoverable only as a NULL parent; backfill validates
        // the stored source content against that identity before creating registries.
        let mut columns = transaction
            .query("PRAGMA table_info(orna_active_revision)", ())
            .await?;
        let mut has_source_parent = false;
        while let Some(column) = columns.next().await? {
            if column.get::<String>(1)? == "source_parent_revision_id" {
                has_source_parent = true;
                break;
            }
        }
        let legacy_active_schema = !has_source_parent;
        drop(columns);
        if legacy_active_schema {
            transaction
                .execute(
                    "ALTER TABLE orna_active_revision ADD COLUMN source_parent_revision_id BLOB",
                    (),
                )
                .await?;
        }
        backfill_active_identity_registries(&*transaction, legacy_active_schema).await
    }
    .await;
    match result {
        Ok(()) => {
            transaction.commit().await?;
            Ok(())
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct SourceRevisionIdentity {
    parent: Option<SourceRevisionId>,
    bundle: SourceBundleId,
    bundle_hash: Sha256Digest,
    revision_hash: Sha256Digest,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct CatalogueRevisionIdentity {
    hash: Sha256Digest,
}

#[derive(Clone, Copy)]
struct ActiveIdentityMetadata {
    source_id: SourceRevisionId,
    source: SourceRevisionIdentity,
    catalogue_id: CatalogueRevisionId,
    catalogue: CatalogueRevisionIdentity,
}

async fn load_active_identity_metadata(
    connection: &Connection,
) -> Result<Option<ActiveIdentityMetadata>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT source_revision_id, source_parent_revision_id,
                    catalogue_revision_id, source_bundle_id, source_bundle_hash,
                    source_revision_hash, catalogue_hash
             FROM orna_active_revision WHERE singleton = 1",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(ActiveIdentityMetadata {
        source_id: SourceRevisionId::from_bytes(id16(
            row.get::<Vec<u8>>(0)?,
            "source revision id",
        )?),
        source: SourceRevisionIdentity {
            parent: optional_source_revision_id(row.get_value(1)?, "source parent revision id")?,
            bundle: SourceBundleId::from_bytes(id16(row.get::<Vec<u8>>(3)?, "source bundle id")?),
            bundle_hash: digest32(row.get::<Vec<u8>>(4)?, "source bundle hash")?,
            revision_hash: digest32(row.get::<Vec<u8>>(5)?, "source revision hash")?,
        },
        catalogue_id: CatalogueRevisionId::from_bytes(id16(
            row.get::<Vec<u8>>(2)?,
            "catalogue revision id",
        )?),
        catalogue: CatalogueRevisionIdentity {
            hash: digest32(row.get::<Vec<u8>>(6)?, "catalogue hash")?,
        },
    }))
}

async fn load_source_revision_registry(
    connection: &Connection,
    revision: SourceRevisionId,
) -> Result<Option<SourceRevisionIdentity>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT source_parent_revision_id, source_bundle_id,
                    source_bundle_hash, source_revision_hash
             FROM orna_source_revisions
             WHERE source_revision_id = ?1",
            [Value::Blob(revision.to_bytes().to_vec())],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(SourceRevisionIdentity {
        parent: optional_source_revision_id(
            row.get_value(0)?,
            "source revision registry parent revision id",
        )?,
        bundle: SourceBundleId::from_bytes(id16(
            row.get::<Vec<u8>>(1)?,
            "source revision registry bundle id",
        )?),
        bundle_hash: digest32(
            row.get::<Vec<u8>>(2)?,
            "source revision registry bundle hash",
        )?,
        revision_hash: digest32(row.get::<Vec<u8>>(3)?, "source revision registry hash")?,
    }))
}

async fn load_catalogue_revision_registry(
    connection: &Connection,
    revision: CatalogueRevisionId,
) -> Result<Option<CatalogueRevisionIdentity>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT catalogue_hash
             FROM orna_catalogue_revisions
             WHERE catalogue_revision_id = ?1",
            [Value::Blob(revision.to_bytes().to_vec())],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(CatalogueRevisionIdentity {
        hash: digest32(row.get::<Vec<u8>>(0)?, "catalogue revision registry hash")?,
    }))
}

async fn insert_source_revision_registry(
    connection: &Connection,
    revision: SourceRevisionId,
    identity: SourceRevisionIdentity,
) -> Result<(), SqliteError> {
    let inserted = connection
        .execute(
            "INSERT INTO orna_source_revisions
             (source_revision_id, source_parent_revision_id, source_bundle_id,
              source_bundle_hash, source_revision_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            [
                Value::Blob(revision.to_bytes().to_vec()),
                identity.parent.map_or(Value::Null, |parent| {
                    Value::Blob(parent.to_bytes().to_vec())
                }),
                Value::Blob(identity.bundle.to_bytes().to_vec()),
                Value::Blob(identity.bundle_hash.to_bytes().to_vec()),
                Value::Blob(identity.revision_hash.to_bytes().to_vec()),
            ],
        )
        .await?;
    if inserted != 1 {
        return Err(SqliteError::InvalidPersistedData(
            "source revision registry insert affected an unexpected number of rows",
        ));
    }
    Ok(())
}

async fn insert_catalogue_revision_registry(
    connection: &Connection,
    revision: CatalogueRevisionId,
    identity: CatalogueRevisionIdentity,
) -> Result<(), SqliteError> {
    let inserted = connection
        .execute(
            "INSERT INTO orna_catalogue_revisions
             (catalogue_revision_id, catalogue_hash)
             VALUES (?1, ?2)",
            [
                Value::Blob(revision.to_bytes().to_vec()),
                Value::Blob(identity.hash.to_bytes().to_vec()),
            ],
        )
        .await?;
    if inserted != 1 {
        return Err(SqliteError::InvalidPersistedData(
            "catalogue revision registry insert affected an unexpected number of rows",
        ));
    }
    Ok(())
}

async fn backfill_active_identity_registries(
    connection: &Connection,
    legacy_active_schema: bool,
) -> Result<(), SqliteError> {
    let Some(active) = load_active_identity_metadata(connection).await? else {
        return Ok(());
    };

    if legacy_active_schema {
        validate_legacy_active_source(connection, active).await?;
    }
    match load_source_revision_registry(connection, active.source_id).await? {
        Some(existing) if existing != active.source => {
            return Err(SqliteError::InvalidPersistedData(
                "source revision registry conflicts with active metadata",
            ));
        }
        Some(_) => {}
        None => {
            insert_source_revision_registry(connection, active.source_id, active.source).await?;
        }
    }

    match load_catalogue_revision_registry(connection, active.catalogue_id).await? {
        Some(existing) if existing != active.catalogue => {
            return Err(SqliteError::InvalidPersistedData(
                "catalogue revision registry conflicts with active metadata",
            ));
        }
        Some(_) => {}
        None => {
            insert_catalogue_revision_registry(connection, active.catalogue_id, active.catalogue)
                .await?;
        }
    }
    Ok(())
}

async fn validate_legacy_active_source(
    connection: &Connection,
    active: ActiveIdentityMetadata,
) -> Result<(), SqliteError> {
    let mut rows = connection
        .query(
            "SELECT source_unit_id, ordinal, logical_path, content, content_hash
             FROM orna_source_units
             WHERE source_revision_id = ?1 ORDER BY ordinal ASC",
            [Value::Blob(active.source_id.to_bytes().to_vec())],
        )
        .await?;
    let mut units = Vec::new();
    while let Some(unit) = rows.next().await? {
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
            .map_err(|_| {
                SqliteError::InvalidPersistedData(
                    "legacy active source content/hash is not valid for a NULL parent",
                )
            })?,
        );
    }

    let computed_bundle_hash = source_bundle_digest(&units).map_err(|_| {
        SqliteError::InvalidPersistedData(
            "legacy active source content/hash is not valid for a NULL parent",
        )
    })?;
    if computed_bundle_hash != active.source.bundle_hash {
        return Err(SqliteError::InvalidPersistedData(
            "legacy active source content/hash is not valid for a NULL parent",
        ));
    }

    let computed_source_hash =
        source_revision_record_digest(active.source.bundle, None, active.source.bundle_hash)
            .map_err(|_| {
                SqliteError::InvalidPersistedData(
                    "legacy active source content/hash is not valid for a NULL parent",
                )
            })?;
    if computed_source_hash != active.source.revision_hash {
        return Err(SqliteError::InvalidPersistedData(
            "legacy active source content/hash is not valid for a NULL parent",
        ));
    }
    Ok(())
}

async fn validate_active_identity_registries(
    connection: &Connection,
    active: ActiveIdentityMetadata,
) -> Result<(), SqliteError> {
    let source = load_source_revision_registry(connection, active.source_id)
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "active source revision has no registry record",
        ))?;
    if source != active.source {
        return Err(SqliteError::InvalidPersistedData(
            "active source revision registry does not match active metadata",
        ));
    }

    let catalogue = load_catalogue_revision_registry(connection, active.catalogue_id)
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "active catalogue revision has no registry record",
        ))?;
    if catalogue != active.catalogue {
        return Err(SqliteError::InvalidPersistedData(
            "active catalogue revision registry does not match active metadata",
        ));
    }
    Ok(())
}

async fn require_source_revision_registry(
    connection: &Connection,
    revision: SourceRevisionId,
) -> Result<(), SqliteError> {
    if load_source_revision_registry(connection, revision)
        .await?
        .is_none()
    {
        return Err(SqliteError::InvalidPersistedData(
            "candidate source parent revision is not registered",
        ));
    }
    Ok(())
}

async fn require_catalogue_revision_registry(
    connection: &Connection,
    revision: CatalogueRevisionId,
) -> Result<(), SqliteError> {
    if load_catalogue_revision_registry(connection, revision)
        .await?
        .is_none()
    {
        return Err(SqliteError::InvalidPersistedData(
            "candidate catalogue parent revision is not registered",
        ));
    }
    Ok(())
}

async fn seed_pair_in_transaction(
    transaction: &turso::transaction::Transaction<'_>,
) -> Result<BootstrapRevision, SqliteError> {
    if let Some(active) = load_active_identity_metadata(transaction).await? {
        validate_active_identity_registries(transaction, active).await?;
        return Ok(BootstrapRevision::new(
            active.source_id,
            active.catalogue_id,
        ));
    }

    let pair = BootstrapRevision::new(SourceRevisionId::new(), CatalogueRevisionId::new());
    let bundle = SourceBundleId::new();
    let source_bundle_hash =
        source_bundle_digest(&[]).map_err(|error| SqliteError::Domain(error.to_string()))?;
    let source_hash = source_revision_record_digest(bundle, None, source_bundle_hash)
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    let catalogue = CatalogueSnapshot::new(pair.catalogue(), Vec::new(), Vec::new())
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[])
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    insert_source_revision_registry(
        transaction,
        pair.source(),
        SourceRevisionIdentity {
            parent: None,
            bundle,
            bundle_hash: source_bundle_hash,
            revision_hash: source_hash,
        },
    )
    .await?;
    insert_catalogue_revision_registry(
        transaction,
        pair.catalogue(),
        CatalogueRevisionIdentity {
            hash: catalogue_hash,
        },
    )
    .await?;
    let inserted = transaction
        .execute(
            "INSERT INTO orna_active_revision
             (singleton, source_revision_id, source_parent_revision_id,
              catalogue_revision_id, source_bundle_id, source_bundle_hash,
              source_revision_hash, catalogue_hash)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            [
                Value::Blob(pair.source().to_bytes().to_vec()),
                Value::Null,
                Value::Blob(pair.catalogue().to_bytes().to_vec()),
                Value::Blob(bundle.to_bytes().to_vec()),
                Value::Blob(source_bundle_hash.to_bytes().to_vec()),
                Value::Blob(source_hash.to_bytes().to_vec()),
                Value::Blob(catalogue_hash.to_bytes().to_vec()),
            ],
        )
        .await?;
    if inserted != 1 {
        return Err(SqliteError::InvalidPersistedData(
            "bootstrap active row insert affected an unexpected number of rows",
        ));
    }
    load_pair_from(transaction)
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "bootstrap row disappeared",
        ))
}

async fn load_pair_from(connection: &Connection) -> Result<Option<BootstrapRevision>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT source_revision_id, catalogue_revision_id
             FROM orna_active_revision WHERE singleton = 1",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let source = SourceRevisionId::from_bytes(id16(row.get::<Vec<u8>>(0)?, "source revision id")?);
    let catalogue =
        CatalogueRevisionId::from_bytes(id16(row.get::<Vec<u8>>(1)?, "catalogue revision id")?);
    Ok(Some(BootstrapRevision::new(source, catalogue)))
}

async fn load_active_from(connection: &Connection) -> Result<ActiveDatabaseRevision, SqliteError> {
    let active = load_active_identity_metadata(connection).await?.ok_or(
        SqliteError::InvalidPersistedData("database has not been bootstrapped"),
    )?;

    let source_id = active.source_id;
    let source_parent = active.source.parent;
    let bundle = active.source.bundle;
    let bundle_hash = active.source.bundle_hash;
    let source_hash = active.source.revision_hash;
    let catalogue_id = active.catalogue_id;
    let catalogue_hash = active.catalogue.hash;

    let mut unit_rows = connection
        .query(
            "SELECT source_unit_id, ordinal, logical_path, content, content_hash
             FROM orna_source_units
             WHERE source_revision_id = ?1 ORDER BY ordinal ASC",
            [Value::Blob(source_id.to_bytes().to_vec())],
        )
        .await?;
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
            .map_err(|error| SqliteError::Domain(error.to_string()))?,
        );
    }
    let computed_bundle_hash =
        source_bundle_digest(&units).map_err(|error| SqliteError::Domain(error.to_string()))?;
    if computed_bundle_hash != bundle_hash {
        return Err(SqliteError::InvalidPersistedData(
            "source bundle hash mismatch",
        ));
    }
    let computed_source_hash = source_revision_record_digest(bundle, source_parent, bundle_hash)
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    if computed_source_hash != source_hash {
        return Err(SqliteError::InvalidPersistedData(
            "source revision hash mismatch",
        ));
    }
    let source = StoredSourceRevision::new(
        bundle,
        source_id,
        source_parent,
        units,
        bundle_hash,
        source_hash,
    )
    .map_err(|error| SqliteError::Domain(error.to_string()))?;

    let mut schema_rows = connection
        .query(
            "SELECT schema_id, name_parts, source_unit_id, source_start, source_end
             FROM orna_catalogue_schemas
             WHERE catalogue_revision_id = ?1 ORDER BY rowid ASC",
            [Value::Blob(catalogue_id.to_bytes().to_vec())],
        )
        .await?;
    let mut schemas = Vec::new();
    let mut origins = Vec::new();
    while let Some(schema) = schema_rows.next().await? {
        let schema_id = SchemaId::from_bytes(id16(schema.get::<Vec<u8>>(0)?, "schema id")?);
        let parts = schema
            .get::<String>(1)?
            .split('\u{1f}')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        schemas.push(SchemaDefinition::new(
            schema_id,
            QualifiedSemanticName::new(parts)
                .map_err(|error| SqliteError::Domain(error.to_string()))?,
        ));
        let source_origin = SourceOrigin::new(
            SourceUnitId::from_bytes(id16(schema.get::<Vec<u8>>(2)?, "schema source unit id")?),
            u32::try_from(schema.get::<i64>(3)?).map_err(|_| {
                SqliteError::InvalidPersistedData("schema source start must fit u32")
            })?,
            u32::try_from(schema.get::<i64>(4)?)
                .map_err(|_| SqliteError::InvalidPersistedData("schema source end must fit u32"))?,
        )
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema_id),
            source_origin,
        ));
    }
    let catalogue = CatalogueSnapshot::new(catalogue_id, schemas, Vec::new())
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    let computed_catalogue_hash = catalogue_digest(&catalogue, &[], &[], &origins, &[])
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    if computed_catalogue_hash != catalogue_hash {
        return Err(SqliteError::InvalidPersistedData("catalogue hash mismatch"));
    }
    validate_active_identity_registries(connection, active).await?;

    ActiveDatabaseRevision::new(
        RevisionPair::new(source_id, catalogue_id),
        source,
        catalogue,
        catalogue_hash,
        Vec::new(),
        Vec::new(),
        origins,
        Vec::new(),
    )
    .map_err(|error| SqliteError::Domain(error.to_string()))
}

async fn load_ledger_from(
    connection: &Connection,
) -> Result<Vec<MigrationLedgerEntry>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT ordinal, format, version,
                    expected_source_revision_id, expected_catalogue_revision_id,
                    candidate_source_revision_id, candidate_catalogue_revision_id,
                    canonical_bytes, digest
             FROM orna_application_migrations ORDER BY ordinal ASC",
            (),
        )
        .await?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next().await? {
        let ordinal = row.get::<i64>(0)?;
        if ordinal < 0 {
            return Err(SqliteError::InvalidPersistedData(
                "migration ledger ordinal must be non-negative",
            ));
        }
        let expected_ordinal = i64::try_from(entries.len())
            .map_err(|_| SqliteError::InvalidPersistedData("migration ledger ordinal overflow"))?;
        if ordinal != expected_ordinal {
            return Err(SqliteError::Domain(format!(
                "invalid migration ledger ordinal {ordinal}: expected {expected_ordinal}",
            )));
        }
        let version = u32::try_from(row.get::<i64>(2)?).map_err(|_| {
            SqliteError::InvalidPersistedData("migration ledger version must fit u32")
        })?;
        let expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(3)?,
                "migration expected source revision id",
            )?),
            CatalogueRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(4)?,
                "migration expected catalogue revision id",
            )?),
        );
        let candidate_pair = RevisionPair::new(
            SourceRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(5)?,
                "migration candidate source revision id",
            )?),
            CatalogueRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(6)?,
                "migration candidate catalogue revision id",
            )?),
        );
        let entry = MigrationLedgerEntry::from_parts(
            row.get::<String>(1)?,
            version,
            expected_base,
            candidate_pair,
            row.get::<Vec<u8>>(7)?,
            digest32(row.get::<Vec<u8>>(8)?, "migration digest")?,
        )
        .map_err(|error| {
            SqliteError::Domain(format!(
                "invalid migration ledger entry at ordinal {ordinal}: {error}"
            ))
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

async fn next_ledger_ordinal(connection: &Connection) -> Result<i64, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT COALESCE(MAX(ordinal), -1) FROM orna_application_migrations",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(SqliteError::InvalidPersistedData(
            "migration ledger ordinal query returned no row",
        ));
    };
    let max = row.get::<i64>(0)?;
    if max < -1 {
        return Err(SqliteError::InvalidPersistedData(
            "migration ledger ordinal must be non-negative",
        ));
    }
    max.checked_add(1).ok_or(SqliteError::InvalidPersistedData(
        "migration ledger ordinal overflow",
    ))
}

async fn apply_in_transaction(
    transaction: &turso::transaction::Transaction<'_>,
    candidate: &DeployableRevision,
    artifact: &PhysicalMigrationArtifact,
) -> Result<(), StorageError<SqliteError>> {
    let active = load_active_from(transaction)
        .await
        .map_err(StorageError::Backend)?;
    if candidate.expected_base() != active.pair() {
        return Err(StorageError::InvalidRequest(
            MigrationLedgerEntryError::ActiveBaseMismatch {
                expected: candidate.expected_base(),
                actual: active.pair(),
            },
        ));
    }

    let entry = MigrationLedgerEntry::from_artifact(artifact);
    entry
        .validate(&active, candidate)
        .map_err(StorageError::InvalidRequest)?;

    if let Err(error) = ensure_supported_candidate(candidate) {
        return Err(StorageError::Backend(error));
    }
    validate_candidate_records(candidate).map_err(StorageError::InvalidRequest)?;
    validate_candidate_parent_registries(transaction, candidate, active.pair())
        .await
        .map_err(StorageError::Backend)?;

    let ledger = load_ledger_from(transaction)
        .await
        .map_err(StorageError::Backend)?;
    if let Some(last) = ledger.last()
        && last.candidate_pair() != active.pair()
    {
        return Err(StorageError::Backend(SqliteError::InvalidPersistedData(
            "migration ledger does not end at active revision",
        )));
    }
    let ordinal = next_ledger_ordinal(transaction)
        .await
        .map_err(StorageError::Backend)?;
    persist_candidate(transaction, candidate, &entry, ordinal)
        .await
        .map_err(StorageError::Backend)
}

fn ensure_supported_candidate(candidate: &DeployableRevision) -> Result<(), SqliteError> {
    let catalogue = candidate.candidate();
    if !catalogue.object_types().is_empty()
        || !catalogue.value_types().is_empty()
        || !catalogue.enum_types().is_empty()
        || !catalogue.record_value_types().is_empty()
        || !catalogue.type_bindings().is_empty()
        || !catalogue.functions().is_empty()
        || !candidate
            .origins()
            .iter()
            .all(|origin| matches!(origin.identity(), DefinitionIdentity::Schema(_)))
        || !candidate.expressions().is_empty()
        || !candidate.new_function_revisions().is_empty()
        || !candidate.references().is_empty()
        || candidate.catalogue_hash_context().version() != CatalogueHashVersion::Version1
    {
        return Err(SqliteError::UnsupportedApply);
    }
    Ok(())
}

fn validate_candidate_records(
    candidate: &DeployableRevision,
) -> Result<(), MigrationLedgerEntryError> {
    let source = candidate.source();
    let expected_bundle_hash =
        source_bundle_digest(source.units()).map_err(MigrationLedgerEntryError::CanonicalHash)?;
    if expected_bundle_hash != source.bundle_hash() {
        return Err(MigrationLedgerEntryError::DigestMismatch {
            expected: expected_bundle_hash,
            actual: source.bundle_hash(),
        });
    }
    let expected_source_hash =
        source_revision_record_digest(source.bundle(), source.parent(), source.bundle_hash())
            .map_err(MigrationLedgerEntryError::CanonicalHash)?;
    if expected_source_hash != source.revision_hash() {
        return Err(MigrationLedgerEntryError::DigestMismatch {
            expected: expected_source_hash,
            actual: source.revision_hash(),
        });
    }
    let expected_catalogue_hash =
        catalogue_digest(candidate.candidate(), &[], &[], candidate.origins(), &[])
            .map_err(MigrationLedgerEntryError::CanonicalHash)?;
    if expected_catalogue_hash != candidate.catalogue_hash() {
        return Err(MigrationLedgerEntryError::DigestMismatch {
            expected: expected_catalogue_hash,
            actual: candidate.catalogue_hash(),
        });
    }
    Ok(())
}

async fn validate_candidate_parent_registries(
    connection: &Connection,
    candidate: &DeployableRevision,
    active: RevisionPair,
) -> Result<(), SqliteError> {
    if candidate.source().parent() != Some(active.source()) {
        return Err(SqliteError::InvalidPersistedData(
            "candidate source parent does not match active source revision",
        ));
    }
    require_source_revision_registry(connection, active.source()).await?;

    if candidate.parent_catalogue() != active.catalogue() {
        return Err(SqliteError::InvalidPersistedData(
            "candidate catalogue parent does not match active catalogue revision",
        ));
    }
    require_catalogue_revision_registry(connection, active.catalogue()).await?;
    Ok(())
}

async fn persist_candidate(
    transaction: &turso::transaction::Transaction<'_>,
    candidate: &DeployableRevision,
    entry: &MigrationLedgerEntry,
    ordinal: i64,
) -> Result<(), SqliteError> {
    let source = candidate.source();
    insert_source_revision_registry(
        transaction,
        source.id(),
        SourceRevisionIdentity {
            parent: source.parent(),
            bundle: source.bundle(),
            bundle_hash: source.bundle_hash(),
            revision_hash: source.revision_hash(),
        },
    )
    .await?;
    let catalogue_pair = candidate.candidate_pair();
    insert_catalogue_revision_registry(
        transaction,
        catalogue_pair.catalogue(),
        CatalogueRevisionIdentity {
            hash: candidate.catalogue_hash(),
        },
    )
    .await?;

    for unit in source.units() {
        let inserted = transaction
            .execute(
                "INSERT INTO orna_source_units
                 (source_revision_id, source_unit_id, ordinal, logical_path, content, content_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                [
                    Value::Blob(source.id().to_bytes().to_vec()),
                    Value::Blob(unit.id().to_bytes().to_vec()),
                    Value::Integer(i64::from(unit.ordinal())),
                    Value::Text(unit.logical_path().to_owned()),
                    Value::Text(unit.content().to_owned()),
                    Value::Blob(unit.content_hash().to_bytes().to_vec()),
                ],
            )
            .await?;
        if inserted != 1 {
            return Err(SqliteError::InvalidPersistedData(
                "source unit insert affected an unexpected number of rows",
            ));
        }
    }

    for origin in candidate.origins() {
        let DefinitionIdentity::Schema(schema_id) = origin.identity() else {
            return Err(SqliteError::UnsupportedApply);
        };
        let schema = candidate.candidate().schema_by_id(schema_id).ok_or(
            SqliteError::InvalidPersistedData("schema origin has no candidate schema"),
        )?;
        let inserted = transaction
            .execute(
                "INSERT INTO orna_catalogue_schemas
                 (catalogue_revision_id, schema_id, name_parts, source_unit_id, source_start, source_end)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                [
                    Value::Blob(catalogue_pair.catalogue().to_bytes().to_vec()),
                    Value::Blob(schema.id().to_bytes().to_vec()),
                    Value::Text(schema.name().parts().join("\u{1f}")),
                    Value::Blob(origin.source().source_unit().to_bytes().to_vec()),
                    Value::Integer(i64::from(origin.source().byte_start())),
                    Value::Integer(i64::from(origin.source().byte_end())),
                ],
            )
            .await?;
        if inserted != 1 {
            return Err(SqliteError::InvalidPersistedData(
                "schema insert affected an unexpected number of rows",
            ));
        }
    }

    let next_ordinal = ordinal;
    let inserted = transaction
        .execute(
            "INSERT INTO orna_application_migrations
             (ordinal, format, version,
              expected_source_revision_id, expected_catalogue_revision_id,
              candidate_source_revision_id, candidate_catalogue_revision_id,
              canonical_bytes, digest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            [
                Value::Integer(next_ordinal),
                Value::Text(entry.format().to_owned()),
                Value::Integer(i64::from(entry.version())),
                Value::Blob(entry.expected_base().source().to_bytes().to_vec()),
                Value::Blob(entry.expected_base().catalogue().to_bytes().to_vec()),
                Value::Blob(entry.candidate_pair().source().to_bytes().to_vec()),
                Value::Blob(entry.candidate_pair().catalogue().to_bytes().to_vec()),
                Value::Blob(entry.canonical_bytes().to_owned()),
                Value::Blob(entry.digest().to_bytes().to_vec()),
            ],
        )
        .await?;
    if inserted != 1 {
        return Err(SqliteError::InvalidPersistedData(
            "migration ledger insert affected an unexpected number of rows",
        ));
    }

    let source_parent = source.parent().map_or(Value::Null, |parent| {
        Value::Blob(parent.to_bytes().to_vec())
    });
    let updated = transaction
        .execute(
            "UPDATE orna_active_revision
             SET source_revision_id = ?1,
                 source_parent_revision_id = ?2,
                 catalogue_revision_id = ?3,
                 source_bundle_id = ?4,
                 source_bundle_hash = ?5,
                 source_revision_hash = ?6,
                 catalogue_hash = ?7
             WHERE singleton = 1",
            [
                Value::Blob(source.id().to_bytes().to_vec()),
                source_parent,
                Value::Blob(catalogue_pair.catalogue().to_bytes().to_vec()),
                Value::Blob(source.bundle().to_bytes().to_vec()),
                Value::Blob(source.bundle_hash().to_bytes().to_vec()),
                Value::Blob(source.revision_hash().to_bytes().to_vec()),
                Value::Blob(candidate.catalogue_hash().to_bytes().to_vec()),
            ],
        )
        .await?;
    if updated != 1 {
        return Err(SqliteError::InvalidPersistedData(
            "active pointer update affected an unexpected number of rows",
        ));
    }
    Ok(())
}

fn optional_source_revision_id(
    value: Value,
    field: &'static str,
) -> Result<Option<SourceRevisionId>, SqliteError> {
    match value {
        Value::Null => Ok(None),
        Value::Blob(value) => Ok(Some(SourceRevisionId::from_bytes(id16(value, field)?))),
        _ => Err(SqliteError::InvalidPersistedData(field)),
    }
}

fn id16(value: Vec<u8>, field: &'static str) -> Result<[u8; 16], SqliteError> {
    value
        .try_into()
        .map_err(|_| SqliteError::InvalidPersistedData(field))
}

fn digest32(value: Vec<u8>, field: &'static str) -> Result<Sha256Digest, SqliteError> {
    Ok(Sha256Digest::from_bytes(
        value
            .try_into()
            .map_err(|_| SqliteError::InvalidPersistedData(field))?,
    ))
}

impl RevisionStore for SqliteRevisionStore {
    type Error = SqliteError;

    fn bootstrap(
        &self,
    ) -> impl Future<Output = Result<BootstrapRevision, StorageError<Self::Error>>> + Send {
        async move { self.seed_pair().await.map_err(StorageError::Backend) }
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
        artifact: &PhysicalMigrationArtifact,
    ) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send
    {
        self.apply_with_artifact(candidate, artifact)
    }

    fn read_ledger(
        &self,
    ) -> impl Future<Output = Result<Vec<MigrationLedgerEntry>, StorageError<Self::Error>>> + Send
    {
        SqliteRevisionStore::read_ledger(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::canonical_hash::source_unit_content_digest;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("orna-sqlite-{nonce}.db"))
    }

    fn schema_candidate(
        active: &ActiveDatabaseRevision,
        source_byte: u8,
        catalogue_byte: u8,
        schema_byte: u8,
    ) -> DeployableRevision {
        schema_candidate_with_revision_ids(
            active,
            SourceRevisionId::from_bytes([source_byte; 16]),
            CatalogueRevisionId::from_bytes([catalogue_byte; 16]),
            source_byte,
            schema_byte,
        )
    }

    fn schema_candidate_with_revision_ids(
        active: &ActiveDatabaseRevision,
        source_id: SourceRevisionId,
        catalogue_id: CatalogueRevisionId,
        source_byte: u8,
        schema_byte: u8,
    ) -> DeployableRevision {
        let content = format!("CREATE SCHEMA schema_{schema_byte};\n");
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([source_byte; 16]),
            0,
            format!("schema_{schema_byte}.orna"),
            content.clone(),
            source_unit_content_digest(&content).unwrap(),
        )
        .unwrap();
        let bundle = SourceBundleId::from_bytes([source_byte.wrapping_add(1); 16]);
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let source_hash =
            source_revision_record_digest(bundle, Some(active.pair().source()), bundle_hash)
                .unwrap();
        let source = StoredSourceRevision::new(
            bundle,
            source_id,
            Some(active.pair().source()),
            vec![unit.clone()],
            bundle_hash,
            source_hash,
        )
        .unwrap();
        let schema_id = SchemaId::from_bytes([schema_byte; 16]);
        let schema = SchemaDefinition::new(
            schema_id,
            QualifiedSemanticName::new(vec!["schema".to_owned(), schema_byte.to_string()]).unwrap(),
        );
        let catalogue = CatalogueSnapshot::new(catalogue_id, vec![schema], Vec::new()).unwrap();
        let origin = DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema_id),
            SourceOrigin::new(unit.id(), 0, content.len() as u32).unwrap(),
        );
        let catalogue_hash =
            catalogue_digest(&catalogue, &[], &[], std::slice::from_ref(&origin), &[]).unwrap();
        DeployableRevision::new(
            active.pair(),
            source,
            active.pair().catalogue(),
            catalogue,
            catalogue_hash,
            vec![origin],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    async fn row_count(connection: &Connection, query: &str) -> i64 {
        let mut rows = connection.query(query, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
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
        assert!(store.read_ledger().await.unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_malformed_source_hash_with_null_parent_on_recovery() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_active_revision
                 SET source_revision_hash = ?1
                 WHERE singleton = 1",
                [Value::Blob(vec![0; 32])],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        let error = store.recover().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "source revision hash mismatch"
            ))
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_tampered_active_source_registry_on_recovery() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_source_revisions
                 SET source_bundle_hash = ?1",
                [Value::Blob(vec![0; 32])],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        let error = store.recover().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "active source revision registry does not match active metadata"
            ))
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn applies_exact_artifacts_in_order_and_recovers_schema_records() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let first_candidate = schema_candidate(&initial, 0x11, 0x12, 0x13);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let first = RevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();

        let second_candidate = schema_candidate(&first, 0x21, 0x22, 0x23);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&first, &second_candidate).unwrap();
        RevisionStore::apply(&store, &second_candidate, &second_artifact)
            .await
            .unwrap();

        let ledger = RevisionStore::read_ledger(&store).await.unwrap();
        assert_eq!(ledger.len(), 2);
        assert_eq!(
            ledger[0],
            MigrationLedgerEntry::from_artifact(&first_artifact)
        );
        assert_eq!(
            ledger[1],
            MigrationLedgerEntry::from_artifact(&second_artifact)
        );
        assert_eq!(ledger[0].candidate_pair(), first_candidate.candidate_pair());
        assert_eq!(
            ledger[1].candidate_pair(),
            second_candidate.candidate_pair()
        );

        let reopened = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        let recovered = reopened.recover().await.unwrap();
        assert_eq!(recovered.pair(), second_candidate.candidate_pair());
        assert_eq!(recovered.source().units().len(), 1);
        assert_eq!(recovered.catalogue().schemas().len(), 1);
        assert_eq!(recovered.origins().len(), 1);
        assert_eq!(reopened.read_ledger().await.unwrap(), ledger);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_reused_source_revision_identity_without_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let first_candidate = schema_candidate(&initial, 0x71, 0x72, 0x73);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let active = RevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();

        let ledger_before = store.read_ledger().await.unwrap();
        let connection = store.connection.lock().await;
        let source_units_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_units").await;
        let schemas_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_schemas").await;
        let source_revisions_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_revisions").await;
        let catalogue_revisions_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_revisions").await;
        drop(connection);

        let candidate = schema_candidate_with_revision_ids(
            &active,
            initial.pair().source(),
            CatalogueRevisionId::from_bytes([0x82; 16]),
            0x81,
            0x83,
        );
        let artifact = PhysicalMigrationArtifact::from_revisions(&active, &candidate).unwrap();
        let error = RevisionStore::apply(&store, &candidate, &artifact)
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::Backend(_)));

        assert_eq!(store.read_ledger().await.unwrap(), ledger_before);
        let recovered = store.recover().await.unwrap();
        assert_eq!(recovered.pair(), active.pair());
        assert_eq!(
            recovered.source().revision_hash(),
            active.source().revision_hash()
        );
        assert_eq!(recovered.catalogue_hash(), active.catalogue_hash());
        let connection = store.connection.lock().await;
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_units").await,
            source_units_before
        );
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_schemas").await,
            schemas_before
        );
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_revisions").await,
            source_revisions_before
        );
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_revisions").await,
            catalogue_revisions_before
        );
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_reused_catalogue_revision_identity_without_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let first_candidate = schema_candidate(&initial, 0x91, 0x92, 0x93);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let active = RevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();

        let ledger_before = store.read_ledger().await.unwrap();
        let connection = store.connection.lock().await;
        let source_units_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_units").await;
        let schemas_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_schemas").await;
        let source_revisions_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_revisions").await;
        let catalogue_revisions_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_revisions").await;
        drop(connection);

        let candidate = schema_candidate_with_revision_ids(
            &active,
            SourceRevisionId::from_bytes([0xa1; 16]),
            initial.pair().catalogue(),
            0xa1,
            0xa3,
        );
        let artifact = PhysicalMigrationArtifact::from_revisions(&active, &candidate).unwrap();
        let error = RevisionStore::apply(&store, &candidate, &artifact)
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::Backend(_)));

        assert_eq!(store.read_ledger().await.unwrap(), ledger_before);
        let recovered = store.recover().await.unwrap();
        assert_eq!(recovered.pair(), active.pair());
        assert_eq!(
            recovered.source().revision_hash(),
            active.source().revision_hash()
        );
        assert_eq!(recovered.catalogue_hash(), active.catalogue_hash());
        let connection = store.connection.lock().await;
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_units").await,
            source_units_before
        );
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_schemas").await,
            schemas_before
        );
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_revisions").await,
            source_revisions_before
        );
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_revisions").await,
            catalogue_revisions_before
        );
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_non_contiguous_migration_ledger_ordinals() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let first_candidate = schema_candidate(&initial, 0x51, 0x52, 0x53);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let first = RevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();

        let second_candidate = schema_candidate(&first, 0x61, 0x62, 0x63);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&first, &second_candidate).unwrap();
        RevisionStore::apply(&store, &second_candidate, &second_artifact)
            .await
            .unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_application_migrations
                 SET ordinal = 2
                 WHERE ordinal = 1",
                (),
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        let error = store.read_ledger().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::Domain(message))
                if message.contains("ordinal 2") && message.contains("expected 1")
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_mismatched_artifact_without_visible_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let candidate = schema_candidate(&initial, 0x31, 0x32, 0x33);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();
        let wrong_candidate = schema_candidate(&initial, 0x41, 0x42, 0x43);

        let before = store.read_ledger().await.unwrap();
        let error = RevisionStore::apply(&store, &wrong_candidate, &artifact)
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::InvalidRequest(_)));
        assert_eq!(store.read_ledger().await.unwrap(), before);
        assert_eq!(store.recover().await.unwrap().pair(), initial.pair());
        let _ = std::fs::remove_file(path);
    }
}
