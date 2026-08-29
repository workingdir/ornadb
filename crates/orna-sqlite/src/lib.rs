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

const SCHEMA: &str = include_str!("../migrations/0001_revision_store.sql");

/// A candidate capability that the SQLite revision store does not persist.
///
/// The checks use a fixed precedence: catalogue categories first (with
/// semantic records before their owning categories), then non-schema origins,
/// and finally the catalogue hash context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqliteCapability {
    /// A non-schema object type definition.
    ObjectType,
    /// A by-value catalogue type definition.
    ValueType,
    /// An enum catalogue type definition.
    EnumType,
    /// A record-value catalogue type definition.
    RecordValueType,
    /// A catalogue type-name binding.
    TypeBinding,
    /// A compiled expression artifact.
    Expression,
    /// A semantic definition reference.
    Reference,
    /// A newly installed function revision.
    FunctionRevision,
    /// A catalogue function definition.
    Function,
    /// A non-schema declaration origin.
    NonSchemaOrigin,
    /// A catalogue hash context other than the SQLite-supported version 1.
    CatalogueHashVersion,
}

impl fmt::Display for SqliteCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::ObjectType => "object type",
            Self::ValueType => "value type",
            Self::EnumType => "enum type",
            Self::RecordValueType => "record value type",
            Self::TypeBinding => "type binding",
            Self::Expression => "expression",
            Self::Reference => "reference",
            Self::FunctionRevision => "function revision",
            Self::Function => "function",
            Self::NonSchemaOrigin => "non-schema origin",
            Self::CatalogueHashVersion => "catalogue hash version",
        };
        f.write_str(name)
    }
}

#[derive(Debug)]
pub enum SqliteError {
    Backend(turso::Error),
    InvalidPersistedData(&'static str),
    UnsupportedCapability(SqliteCapability),
    Domain(String),
}

impl fmt::Display for SqliteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(error) => write!(f, "Turso backend error: {error}"),
            Self::InvalidPersistedData(message) => {
                write!(f, "invalid persisted SQLite data: {message}")
            }
            Self::UnsupportedCapability(capability) => {
                write!(f, "SQLite adapter does not support applying {capability}")
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
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(SqliteError::from(rollback)),
            },
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
            let active = load_active_from(&*transaction)
                .await
                .map_err(StorageError::Backend)?;
            let ledger = load_ledger_from(&*transaction)
                .await
                .map_err(StorageError::Backend)?;
            validate_ledger_active_pair(&ledger, &active).map_err(StorageError::Backend)?;
            validate_active_catalogue_lineage(&*transaction, &active, &ledger)
                .await
                .map_err(StorageError::Backend)?;
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
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(StorageError::Backend(SqliteError::from(rollback))),
            },
        }
    }

    /// Reads the durable migration ledger oldest-first.
    pub async fn read_ledger(
        &self,
    ) -> Result<Vec<MigrationLedgerEntry>, StorageError<SqliteError>> {
        let connection = self.connection.lock().await;
        let active = load_active_from(&connection)
            .await
            .map_err(StorageError::Backend)?;
        let ledger = load_ledger_from(&connection)
            .await
            .map_err(StorageError::Backend)?;
        validate_ledger_active_pair(&ledger, &active).map_err(StorageError::Backend)?;
        validate_active_catalogue_lineage(&connection, &active, &ledger)
            .await
            .map_err(StorageError::Backend)?;
        Ok(ledger)
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
                    Err(error) => match transaction.rollback().await {
                        Ok(()) => return Err(StorageError::Backend(error)),
                        Err(rollback) => {
                            return Err(StorageError::Backend(SqliteError::from(rollback)));
                        }
                    },
                };
                transaction
                    .commit()
                    .await
                    .map_err(SqliteError::from)
                    .map_err(StorageError::Backend)?;
                Ok(active)
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(StorageError::Backend(SqliteError::from(rollback))),
            },
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
        ensure_catalogue_revision_lineage_schema(&*transaction).await?;
        // Source-unit identities are immutable globally, not just within one
        // revision. Creating this index also hardens legacy databases; a
        // duplicate legacy identity fails the transaction and therefore keeps
        // the database unopened rather than silently accepting ambiguous rows.
        transaction
            .execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS
                 orna_source_units_source_unit_id_idx
                 ON orna_source_units (source_unit_id)",
                (),
            )
            .await?;
        // A source bundle is likewise immutable and globally identified.
        transaction
            .execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS
                 orna_source_revisions_source_bundle_id_idx
                 ON orna_source_revisions (source_bundle_id)",
                (),
            )
            .await?;

        backfill_active_identity_registries(&*transaction, legacy_active_schema).await?;
        backfill_catalogue_revision_lineage(&*transaction).await
    }
    .await;
    match result {
        Ok(()) => {
            transaction.commit().await?;
            Ok(())
        }
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(SqliteError::from(rollback)),
        },
    }
}

async fn ensure_catalogue_revision_lineage_schema(
    connection: &Connection,
) -> Result<(), SqliteError> {
    let mut columns = connection
        .query("PRAGMA table_info(orna_catalogue_revisions)", ())
        .await?;
    let mut has_source_revision = false;
    let mut has_parent_catalogue_revision = false;
    while let Some(column) = columns.next().await? {
        match column.get::<String>(1)?.as_str() {
            "source_revision_id" => has_source_revision = true,
            "parent_catalogue_revision_id" => has_parent_catalogue_revision = true,
            _ => {}
        }
    }
    drop(columns);
    if !has_source_revision {
        connection
            .execute(
                "ALTER TABLE orna_catalogue_revisions
                 ADD COLUMN source_revision_id BLOB",
                (),
            )
            .await?;
    }
    if !has_parent_catalogue_revision {
        connection
            .execute(
                "ALTER TABLE orna_catalogue_revisions
                 ADD COLUMN parent_catalogue_revision_id BLOB",
                (),
            )
            .await?;
    }
    Ok(())
}

async fn backfill_catalogue_revision_lineage(connection: &Connection) -> Result<(), SqliteError> {
    let active = load_active_identity_metadata(connection).await?;
    let mut rows = connection
        .query(
            "SELECT expected_source_revision_id, expected_catalogue_revision_id,
                    candidate_source_revision_id, candidate_catalogue_revision_id
             FROM orna_application_migrations ORDER BY ordinal ASC",
            (),
        )
        .await?;
    let mut edges = Vec::new();
    while let Some(row) = rows.next().await? {
        edges.push((
            SourceRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(0)?,
                "migration expected source revision id",
            )?),
            CatalogueRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(1)?,
                "migration expected catalogue revision id",
            )?),
            SourceRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(2)?,
                "migration candidate source revision id",
            )?),
            CatalogueRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(3)?,
                "migration candidate catalogue revision id",
            )?),
        ));
    }
    drop(rows);

    let mut desired = Vec::with_capacity(edges.len().saturating_add(1));
    if let Some((source, catalogue, _, _)) = edges.first().copied() {
        desired.push((catalogue, source, None));
    } else if let Some(active) = active {
        desired.push((active.catalogue_id, active.source_id, None));
    }
    desired.extend(edges.iter().map(
        |(_, expected_catalogue, candidate_source, candidate_catalogue)| {
            (
                *candidate_catalogue,
                *candidate_source,
                Some(*expected_catalogue),
            )
        },
    ));

    for (catalogue, source, parent) in desired {
        connection
            .execute(
                "UPDATE orna_catalogue_revisions
                 SET source_revision_id = ?1,
                     parent_catalogue_revision_id = ?2
                 WHERE catalogue_revision_id = ?3
                   AND source_revision_id IS NULL",
                [
                    Value::Blob(source.to_bytes().to_vec()),
                    parent.map_or(Value::Null, |parent| {
                        Value::Blob(parent.to_bytes().to_vec())
                    }),
                    Value::Blob(catalogue.to_bytes().to_vec()),
                ],
            )
            .await?;
        let Some(actual) = load_catalogue_revision_lineage(connection, catalogue).await? else {
            return Err(SqliteError::InvalidPersistedData(
                "catalogue revision has no registry record",
            ));
        };
        if actual.source != Some(source) || actual.parent != parent {
            return Err(SqliteError::InvalidPersistedData(
                "catalogue revision lineage does not match migration history",
            ));
        }
    }

    let mut rows = connection
        .query(
            "SELECT COUNT(*) FROM orna_catalogue_revisions
             WHERE source_revision_id IS NULL",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(SqliteError::InvalidPersistedData(
            "catalogue lineage completeness query returned no row",
        ));
    };
    if row.get::<i64>(0)? != 0 {
        return Err(SqliteError::InvalidPersistedData(
            "catalogue revision lineage is incomplete",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceRevisionIdentity {
    parent: Option<SourceRevisionId>,
    bundle: SourceBundleId,
    bundle_hash: Sha256Digest,
    revision_hash: Sha256Digest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CatalogueRevisionIdentity {
    hash: Sha256Digest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CatalogueRevisionLineage {
    source: Option<SourceRevisionId>,
    parent: Option<CatalogueRevisionId>,
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

async fn load_catalogue_revision_lineage(
    connection: &Connection,
    revision: CatalogueRevisionId,
) -> Result<Option<CatalogueRevisionLineage>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT source_revision_id, parent_catalogue_revision_id
             FROM orna_catalogue_revisions
             WHERE catalogue_revision_id = ?1",
            [Value::Blob(revision.to_bytes().to_vec())],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(CatalogueRevisionLineage {
        source: optional_source_revision_id(row.get_value(0)?, "catalogue source revision id")?,
        parent: optional_catalogue_revision_id(row.get_value(1)?, "catalogue parent revision id")?,
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
    source: SourceRevisionId,
    parent: Option<CatalogueRevisionId>,
    identity: CatalogueRevisionIdentity,
) -> Result<(), SqliteError> {
    let inserted = connection
        .execute(
            "INSERT INTO orna_catalogue_revisions
             (catalogue_revision_id, source_revision_id,
              parent_catalogue_revision_id, catalogue_hash)
             VALUES (?1, ?2, ?3, ?4)",
            [
                Value::Blob(revision.to_bytes().to_vec()),
                Value::Blob(source.to_bytes().to_vec()),
                parent.map_or(Value::Null, |parent| {
                    Value::Blob(parent.to_bytes().to_vec())
                }),
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

async fn infer_catalogue_parent(
    connection: &Connection,
    catalogue: CatalogueRevisionId,
) -> Result<Option<CatalogueRevisionId>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT expected_catalogue_revision_id
             FROM orna_application_migrations
             WHERE candidate_catalogue_revision_id = ?1
             ORDER BY ordinal ASC LIMIT 1",
            [Value::Blob(catalogue.to_bytes().to_vec())],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(CatalogueRevisionId::from_bytes(id16(
        row.get::<Vec<u8>>(0)?,
        "migration expected catalogue revision id",
    )?)))
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
            let parent = infer_catalogue_parent(connection, active.catalogue_id).await?;
            insert_catalogue_revision_registry(
                connection,
                active.catalogue_id,
                active.source_id,
                parent,
                active.catalogue,
            )
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
    if let Some(parent) = active.source.parent {
        if load_source_revision_registry(connection, parent)
            .await?
            .is_none()
        {
            return Err(SqliteError::InvalidPersistedData(
                "active source parent revision has no registry record",
            ));
        }
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
    if load_active_identity_metadata(transaction).await?.is_some() {
        let active = load_active_from(&*transaction).await?;
        let ledger = load_ledger_from(&*transaction).await?;
        validate_ledger_active_pair(&ledger, &active)?;
        validate_active_catalogue_lineage(&*transaction, &active, &ledger).await?;
        return Ok(BootstrapRevision::new(
            active.pair().source(),
            active.pair().catalogue(),
        ));
    }

    let durable_rows = {
        let mut rows = transaction
            .query(
                "SELECT
                    (SELECT COUNT(*) FROM orna_source_revisions),
                    (SELECT COUNT(*) FROM orna_catalogue_revisions),
                    (SELECT COUNT(*) FROM orna_source_units),
                    (SELECT COUNT(*) FROM orna_catalogue_schemas),
                    (SELECT COUNT(*) FROM orna_application_migrations)",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(SqliteError::InvalidPersistedData(
                "durable row count query returned no row",
            ));
        };
        let has_rows = row.get::<i64>(0)? != 0
            || row.get::<i64>(1)? != 0
            || row.get::<i64>(2)? != 0
            || row.get::<i64>(3)? != 0
            || row.get::<i64>(4)? != 0;
        drop(rows);
        has_rows
    };
    if durable_rows {
        return Err(SqliteError::InvalidPersistedData(
            "durable revisions exist without an active revision",
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
        pair.source(),
        None,
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
async fn load_source_revision_from(
    connection: &Connection,
    revision: SourceRevisionId,
    identity: SourceRevisionIdentity,
) -> Result<StoredSourceRevision, SqliteError> {
    let mut unit_rows = connection
        .query(
            "SELECT source_unit_id, ordinal, logical_path, content, content_hash
             FROM orna_source_units
             WHERE source_revision_id = ?1 ORDER BY ordinal ASC",
            [Value::Blob(revision.to_bytes().to_vec())],
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
    drop(unit_rows);

    let computed_bundle_hash =
        source_bundle_digest(&units).map_err(|error| SqliteError::Domain(error.to_string()))?;
    if computed_bundle_hash != identity.bundle_hash {
        return Err(SqliteError::InvalidPersistedData(
            "source bundle hash mismatch",
        ));
    }
    let computed_source_hash =
        source_revision_record_digest(identity.bundle, identity.parent, identity.bundle_hash)
            .map_err(|error| SqliteError::Domain(error.to_string()))?;
    if computed_source_hash != identity.revision_hash {
        return Err(SqliteError::InvalidPersistedData(
            "source revision hash mismatch",
        ));
    }

    StoredSourceRevision::new(
        identity.bundle,
        revision,
        identity.parent,
        units,
        identity.bundle_hash,
        identity.revision_hash,
    )
    .map_err(|error| SqliteError::Domain(error.to_string()))
}

async fn load_active_from(connection: &Connection) -> Result<ActiveDatabaseRevision, SqliteError> {
    let active = load_active_identity_metadata(connection).await?.ok_or(
        SqliteError::InvalidPersistedData("database has not been bootstrapped"),
    )?;

    let source_id = active.source_id;
    let catalogue_id = active.catalogue_id;
    let catalogue_hash = active.catalogue.hash;
    let source = load_source_revision_from(connection, source_id, active.source).await?;
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
        let encoded_name = schema.get::<String>(1)?;
        let name = decode_qualified_semantic_name(&encoded_name)?;
        schemas.push(SchemaDefinition::new(schema_id, name));
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
    drop(rows);
    validate_loaded_ledger_chain(connection, &entries).await?;
    Ok(entries)
}

fn validate_ledger_active_pair(
    ledger: &[MigrationLedgerEntry],
    active: &ActiveDatabaseRevision,
) -> Result<(), SqliteError> {
    if let Some(last) = ledger.last()
        && last.candidate_pair() != active.pair()
    {
        return Err(SqliteError::InvalidPersistedData(
            "migration ledger does not end at active revision",
        ));
    }
    Ok(())
}

async fn validate_active_catalogue_lineage(
    connection: &Connection,
    active: &ActiveDatabaseRevision,
    ledger: &[MigrationLedgerEntry],
) -> Result<(), SqliteError> {
    let lineage = load_catalogue_revision_lineage(connection, active.pair().catalogue())
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "active catalogue revision has no lineage record",
        ))?;
    let expected_parent = ledger.last().map(|entry| entry.expected_base().catalogue());
    if lineage.source != Some(active.pair().source()) {
        return Err(SqliteError::InvalidPersistedData(
            "active catalogue revision source does not match active source",
        ));
    }
    if lineage.parent != expected_parent {
        return Err(SqliteError::InvalidPersistedData(
            "active catalogue revision parent does not match migration ledger",
        ));
    }
    Ok(())
}

async fn validate_catalogue_revision(
    connection: &Connection,
    revision: CatalogueRevisionId,
    source: &StoredSourceRevision,
    expected_parent: Option<CatalogueRevisionId>,
) -> Result<(), SqliteError> {
    let registry = load_catalogue_revision_registry(connection, revision)
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "catalogue revision has no registry record",
        ))?;
    let lineage = load_catalogue_revision_lineage(connection, revision)
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "catalogue revision has no lineage record",
        ))?;
    if lineage.source != Some(source.id()) {
        return Err(SqliteError::InvalidPersistedData(
            "catalogue revision source does not match its source revision",
        ));
    }
    if lineage.parent != expected_parent {
        return Err(SqliteError::InvalidPersistedData(
            "catalogue revision parent does not match migration history",
        ));
    }
    let mut rows = connection
        .query(
            "SELECT schema_id, name_parts, source_unit_id, source_start, source_end
             FROM orna_catalogue_schemas
             WHERE catalogue_revision_id = ?1 ORDER BY rowid ASC",
            [Value::Blob(revision.to_bytes().to_vec())],
        )
        .await?;
    let mut schemas = Vec::new();
    let mut origins = Vec::new();
    while let Some(row) = rows.next().await? {
        let schema_id = SchemaId::from_bytes(id16(row.get::<Vec<u8>>(0)?, "schema id")?);
        let name = decode_qualified_semantic_name(&row.get::<String>(1)?)?;
        let source_origin = SourceOrigin::new(
            SourceUnitId::from_bytes(id16(row.get::<Vec<u8>>(2)?, "schema source unit id")?),
            u32::try_from(row.get::<i64>(3)?).map_err(|_| {
                SqliteError::InvalidPersistedData("schema source start must fit u32")
            })?,
            u32::try_from(row.get::<i64>(4)?)
                .map_err(|_| SqliteError::InvalidPersistedData("schema source end must fit u32"))?,
        )
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
        if !source
            .units()
            .iter()
            .any(|unit| unit.id() == source_origin.source_unit())
        {
            return Err(SqliteError::InvalidPersistedData(
                "catalogue schema source unit is not in its source revision",
            ));
        }
        schemas.push(SchemaDefinition::new(schema_id, name));
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema_id),
            source_origin,
        ));
    }
    drop(rows);
    let catalogue = CatalogueSnapshot::new(revision, schemas, Vec::new())
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    let computed_hash = catalogue_digest(&catalogue, &[], &[], &origins, &[])
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    if computed_hash != registry.hash {
        return Err(SqliteError::InvalidPersistedData(
            "historical catalogue hash mismatch",
        ));
    }
    Ok(())
}

async fn validate_loaded_ledger_chain(
    connection: &Connection,
    entries: &[MigrationLedgerEntry],
) -> Result<(), SqliteError> {
    let Some(first) = entries.first() else {
        if let Some(active) = load_active_identity_metadata(connection).await?
            && active.source.parent.is_some()
        {
            return Err(SqliteError::InvalidPersistedData(
                "migration ledger is empty but active source revision has a parent",
            ));
        }
        return Ok(());
    };

    let first_source = load_source_revision_registry(connection, first.expected_base().source())
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "migration ledger first expected source revision has no registry record",
        ))?;
    if first_source.parent.is_some() {
        return Err(SqliteError::InvalidPersistedData(
            "migration ledger first expected source revision is not a root source",
        ));
    }
    let first_source_revision =
        load_source_revision_from(connection, first.expected_base().source(), first_source).await?;
    validate_catalogue_revision(
        connection,
        first.expected_base().catalogue(),
        &first_source_revision,
        None,
    )
    .await?;

    for (ordinal, entry) in entries.iter().enumerate() {
        if ordinal > 0 {
            let previous = &entries[ordinal - 1];
            if entry.expected_base() != previous.candidate_pair() {
                return Err(SqliteError::Domain(format!(
                    "invalid migration ledger chain at ordinal {ordinal}: \
                     expected base does not match previous candidate pair",
                )));
            }

            let expected_source =
                load_source_revision_registry(connection, entry.expected_base().source())
                    .await?
                    .ok_or(SqliteError::InvalidPersistedData(
                        "migration ledger expected source revision has no registry record",
                    ))?;
            load_source_revision_from(connection, entry.expected_base().source(), expected_source)
                .await?;
        }

        let candidate_source =
            load_source_revision_registry(connection, entry.candidate_pair().source())
                .await?
                .ok_or(SqliteError::InvalidPersistedData(
                    "migration ledger candidate source revision has no registry record",
                ))?;
        if candidate_source.parent != Some(entry.expected_base().source()) {
            return Err(SqliteError::InvalidPersistedData(
                "migration ledger candidate source parent does not match expected source revision",
            ));
        }
        let candidate_source_revision = load_source_revision_from(
            connection,
            entry.candidate_pair().source(),
            candidate_source,
        )
        .await?;
        validate_catalogue_revision(
            connection,
            entry.candidate_pair().catalogue(),
            &candidate_source_revision,
            Some(entry.expected_base().catalogue()),
        )
        .await?;
    }

    Ok(())
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
    validate_ledger_active_pair(&ledger, &active).map_err(StorageError::Backend)?;
    validate_active_catalogue_lineage(transaction, &active, &ledger)
        .await
        .map_err(StorageError::Backend)?;
    let ordinal = next_ledger_ordinal(transaction)
        .await
        .map_err(StorageError::Backend)?;
    persist_candidate(transaction, candidate, &entry, ordinal)
        .await
        .map_err(StorageError::Backend)
}

fn ensure_supported_candidate(candidate: &DeployableRevision) -> Result<(), SqliteError> {
    let catalogue = candidate.candidate();

    if !catalogue.object_types().is_empty() {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::ObjectType,
        ));
    }
    if !catalogue.record_value_types().is_empty() {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::RecordValueType,
        ));
    }
    if !catalogue.type_bindings().is_empty() {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::TypeBinding,
        ));
    }
    if !catalogue.value_types().is_empty() {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::ValueType,
        ));
    }
    if !catalogue.enum_types().is_empty() {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::EnumType,
        ));
    }
    if !candidate.expressions().is_empty() {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::Expression,
        ));
    }
    if !candidate.references().is_empty() {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::Reference,
        ));
    }
    if !candidate.new_function_revisions().is_empty() {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::FunctionRevision,
        ));
    }
    if !catalogue.functions().is_empty() {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::Function,
        ));
    }
    if candidate
        .origins()
        .iter()
        .any(|origin| !matches!(origin.identity(), DefinitionIdentity::Schema(_)))
    {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::NonSchemaOrigin,
        ));
    }
    if candidate.catalogue_hash_context().version() != CatalogueHashVersion::Version1 {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::CatalogueHashVersion,
        ));
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
    let catalogue_pair = candidate.candidate_pair();
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
    insert_catalogue_revision_registry(
        transaction,
        catalogue_pair.catalogue(),
        source.id(),
        Some(candidate.parent_catalogue()),
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
            return Err(SqliteError::UnsupportedCapability(
                SqliteCapability::NonSchemaOrigin,
            ));
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
                    Value::Text(encode_qualified_semantic_name(schema.name())),
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

fn optional_catalogue_revision_id(
    value: Value,
    field: &'static str,
) -> Result<Option<CatalogueRevisionId>, SqliteError> {
    match value {
        Value::Null => Ok(None),
        Value::Blob(value) => Ok(Some(CatalogueRevisionId::from_bytes(id16(value, field)?))),
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

fn encode_qualified_semantic_name(name: &QualifiedSemanticName) -> String {
    let separator = '\u{1f}';
    let capacity = name
        .parts()
        .iter()
        .map(String::len)
        .sum::<usize>()
        .saturating_add(name.parts().len().saturating_sub(1));
    let mut encoded = String::with_capacity(capacity);
    for (index, part) in name.parts().iter().enumerate() {
        if index > 0 {
            encoded.push(separator);
        }
        for character in part.chars() {
            encoded.push(character);
            if character == separator {
                encoded.push(separator);
            }
        }
    }
    encoded
}

fn decode_qualified_semantic_name(encoded: &str) -> Result<QualifiedSemanticName, SqliteError> {
    let separator = '\u{1f}';
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut characters = encoded.chars().peekable();
    while let Some(character) = characters.next() {
        if character != separator {
            part.push(character);
            continue;
        }
        if characters.peek() == Some(&separator) {
            characters.next();
            part.push(separator);
            continue;
        }
        if part.is_empty() {
            return Err(SqliteError::InvalidPersistedData(
                "schema name contains an empty part",
            ));
        }
        parts.push(std::mem::take(&mut part));
    }
    if part.is_empty() {
        return Err(SqliteError::InvalidPersistedData(
            "schema name contains an empty part",
        ));
    }
    parts.push(part);
    QualifiedSemanticName::new(parts).map_err(|_| {
        SqliteError::InvalidPersistedData("schema name parts must form one exact semantic name")
    })
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
    use orna_core::{
        ExpressionId, FieldId, FunctionId, FunctionRevisionId, StandardLibraryRevisionId, TypeId,
        canonical_hash::{
            artifact_payload_digest, calculate_standard_library_digest,
            verify_standard_library_v2_snapshot,
        },
        catalogue::{
            EnumTypeDefinition, FunctionDefinition, FunctionDomain, FunctionReturn,
            FunctionSecurity, FunctionTransaction, FunctionVolatility, ObjectTypeDefinition,
            RecordValueFieldDefinition, RecordValueTypeDefinition, TypeBinding,
            ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
        },
        revision::{
            CatalogueHashContext, DefinitionReference, DefinitionReferenceKind,
            DefinitionReferenceTarget, DeployableRevisionContent, DeployableRevisionInput,
            ExecutableArtifact, ExecutableArtifactKind, FunctionRevisionRecord,
            StandardLibraryDigestVersion, StandardLibrarySnapshot,
        },
        types::{ResolvedType, StandardScalar, TypeDescriptor},
    };
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

    const UNSUPPORTED_SOURCE_CONTENT: &str = "CREATE SCHEMA candidate;\n";

    fn unsupported_source_origin(source_byte: u8) -> SourceOrigin {
        SourceOrigin::new(
            SourceUnitId::from_bytes([source_byte; 16]),
            0,
            UNSUPPORTED_SOURCE_CONTENT.len() as u32,
        )
        .unwrap()
    }

    fn unsupported_candidate(
        active: &ActiveDatabaseRevision,
        source_byte: u8,
        catalogue: CatalogueSnapshot,
        identities: impl IntoIterator<Item = DefinitionIdentity>,
        expressions: Vec<orna_core::revision::ExpressionArtifact>,
        new_function_revisions: Vec<FunctionRevisionRecord>,
        references: Vec<DefinitionReference>,
        context: CatalogueHashContext,
    ) -> DeployableRevision {
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([source_byte; 16]),
            0,
            format!("candidate-{source_byte}.orna"),
            UNSUPPORTED_SOURCE_CONTENT,
            source_unit_content_digest(UNSUPPORTED_SOURCE_CONTENT).unwrap(),
        )
        .unwrap();
        let bundle = SourceBundleId::from_bytes([source_byte.wrapping_add(1); 16]);
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let source_hash =
            source_revision_record_digest(bundle, Some(active.pair().source()), bundle_hash)
                .unwrap();
        let source = StoredSourceRevision::new(
            bundle,
            SourceRevisionId::from_bytes([source_byte; 16]),
            Some(active.pair().source()),
            vec![unit],
            bundle_hash,
            source_hash,
        )
        .unwrap();
        let origins = identities
            .into_iter()
            .map(|identity| DefinitionOrigin::new(identity, unsupported_source_origin(source_byte)))
            .collect::<Vec<_>>();
        let candidate_hash = Sha256Digest::from_bytes([0; 32]);
        let expected_base = active.pair();

        if context.version() == CatalogueHashVersion::Version1 {
            return DeployableRevision::new(
                expected_base,
                source,
                expected_base.catalogue(),
                catalogue,
                candidate_hash,
                origins,
                expressions,
                new_function_revisions,
                references,
            )
            .unwrap();
        }

        let current_function_revisions = new_function_revisions.clone();
        let content = DeployableRevisionContent::new(
            origins,
            expressions,
            new_function_revisions,
            references,
        )
        .with_current_function_revisions(current_function_revisions);
        let input = DeployableRevisionInput::new(
            expected_base,
            source,
            expected_base.catalogue(),
            catalogue,
            candidate_hash,
            content,
        );
        DeployableRevision::new_with_catalogue_hash_context_and_parent(input, context, None)
            .unwrap()
    }

    fn empty_version_two_context() -> CatalogueHashContext {
        let content = "standard source\n";
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0xe0; 16]),
            0,
            "standard.orna",
            content,
            source_unit_content_digest(content).unwrap(),
        )
        .unwrap();
        let bundle = SourceBundleId::from_bytes([0xe2; 16]);
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let source_parent = SourceRevisionId::from_bytes([0xe1; 16]);
        let source_hash =
            source_revision_record_digest(bundle, Some(source_parent), bundle_hash).unwrap();
        let source = StoredSourceRevision::new(
            bundle,
            SourceRevisionId::from_bytes([0xe0; 16]),
            Some(source_parent),
            vec![unit],
            bundle_hash,
            source_hash,
        )
        .unwrap();
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0xe3; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let make_snapshot = |digest| {
            StandardLibrarySnapshot::new(
                StandardLibraryRevisionId::from_bytes([0xe4; 16]),
                StandardLibraryDigestVersion::Version2,
                source.clone(),
                "orna.language/2",
                catalogue.clone(),
                Vec::new(),
                digest,
            )
            .unwrap()
        };
        let unchecked = make_snapshot(Sha256Digest::from_bytes([0; 32]));
        let digest = calculate_standard_library_digest(&unchecked).unwrap();
        let verified = verify_standard_library_v2_snapshot(make_snapshot(digest)).unwrap();
        CatalogueHashContext::version_two(verified)
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct PersistedRowCounts {
        active: i64,
        source_revisions: i64,
        catalogue_revisions: i64,
        source_units: i64,
        schemas: i64,
        ledger: i64,
    }

    async fn persisted_row_counts(store: &SqliteRevisionStore) -> PersistedRowCounts {
        let connection = store.connection.lock().await;
        PersistedRowCounts {
            active: row_count(&connection, "SELECT COUNT(*) FROM orna_active_revision").await,
            source_revisions: row_count(&connection, "SELECT COUNT(*) FROM orna_source_revisions")
                .await,
            catalogue_revisions: row_count(
                &connection,
                "SELECT COUNT(*) FROM orna_catalogue_revisions",
            )
            .await,
            source_units: row_count(&connection, "SELECT COUNT(*) FROM orna_source_units").await,
            schemas: row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_schemas").await,
            ledger: row_count(
                &connection,
                "SELECT COUNT(*) FROM orna_application_migrations",
            )
            .await,
        }
    }

    async fn assert_unsupported_candidate_without_mutation(
        store: &SqliteRevisionStore,
        candidate: &DeployableRevision,
        artifact: &PhysicalMigrationArtifact,
        expected: SqliteCapability,
    ) {
        let before = persisted_row_counts(store).await;
        let error = RevisionStore::apply(store, candidate, artifact)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::UnsupportedCapability(actual))
                if actual == expected
        ));
        assert_eq!(persisted_row_counts(store).await, before);
        assert_eq!(
            store.recover().await.unwrap().pair(),
            candidate.expected_base()
        );
    }

    fn unsupported_schema(schema_byte: u8) -> (SchemaId, SchemaDefinition) {
        let id = SchemaId::from_bytes([schema_byte; 16]);
        let schema = SchemaDefinition::new(id, QualifiedSemanticName::new(["schema"]).unwrap());
        (id, schema)
    }

    async fn row_count(connection: &Connection, query: &str) -> i64 {
        let mut rows = connection.query(query, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    #[test]
    fn qualified_name_serialization_escapes_delimiters_and_round_trips() {
        let name = QualifiedSemanticName::new(vec![
            "catalog\u{1f}part".to_owned(),
            "schema".to_owned(),
            "tail\u{1f}".to_owned(),
        ])
        .unwrap();
        let encoded = encode_qualified_semantic_name(&name);
        assert_eq!(
            encoded,
            "catalog\u{1f}\u{1f}part\u{1f}schema\u{1f}tail\u{1f}\u{1f}"
        );
        assert_eq!(decode_qualified_semantic_name(&encoded).unwrap(), name);
    }

    #[test]
    fn qualified_name_decoder_preserves_legacy_rows_and_rejects_empty_parts() {
        let legacy = decode_qualified_semantic_name("catalog\u{1f}schema").unwrap();
        assert_eq!(
            legacy,
            QualifiedSemanticName::new(["catalog", "schema"]).unwrap()
        );

        for encoded in [
            "",
            "\u{1f}",
            "leading\u{1f}",
            "\u{1f}trailing",
            "a\u{1f}\u{1f}\u{1f}",
        ] {
            assert!(matches!(
                decode_qualified_semantic_name(encoded),
                Err(SqliteError::InvalidPersistedData(
                    "schema name contains an empty part"
                ))
            ));
        }
    }

    #[tokio::test]
    async fn rejects_unsupported_capabilities_without_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let active = store.recover().await.unwrap();

        let (schema_id, schema) = unsupported_schema(0x01);
        let object_id = TypeId::from_bytes([0x02; 16]);
        let object_catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x03; 16]),
            vec![schema],
            vec![ObjectTypeDefinition::new(
                object_id,
                QualifiedSemanticName::new(["schema", "object"]).unwrap(),
                Vec::new(),
            )],
        )
        .unwrap();
        let object_candidate = unsupported_candidate(
            &active,
            0x04,
            object_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::ObjectType(object_id),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            CatalogueHashContext::version_one(),
        );

        let (schema_id, schema) = unsupported_schema(0x11);
        let value_id = TypeId::from_bytes([0x12; 16]);
        let binary_value = ValueTypeDefinition::primitive(
            value_id,
            QualifiedSemanticName::new(["schema", "binary"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.binary-large-object@1",
        );
        let binary_catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0x13; 16]),
            vec![schema],
            Vec::new(),
            vec![binary_value],
            Vec::new(),
        )
        .unwrap();
        let binary_value_candidate = unsupported_candidate(
            &active,
            0x14,
            binary_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::ValueType(value_id),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            empty_version_two_context(),
        );

        let (schema_id, schema) = unsupported_schema(0x21);
        let array_like_id = TypeId::from_bytes([0x22; 16]);
        let array_like_value = ValueTypeDefinition::opaque(
            array_like_id,
            QualifiedSemanticName::new(["schema", "array"]).unwrap(),
            "orna.kernel.value.array@1",
        );
        let array_like_catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0x23; 16]),
            vec![schema],
            Vec::new(),
            vec![array_like_value],
            Vec::new(),
        )
        .unwrap();
        let array_like_value_candidate = unsupported_candidate(
            &active,
            0x24,
            array_like_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::ValueType(array_like_id),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            empty_version_two_context(),
        );

        let (schema_id, schema) = unsupported_schema(0x31);
        let binding_value_id = TypeId::from_bytes([0x32; 16]);
        let binding = TypeBinding::qualified(
            QualifiedSemanticName::new(["schema", "bound"]).unwrap(),
            binding_value_id,
        )
        .unwrap();
        let binding_id = binding.id();
        let binding_catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0x33; 16]),
            vec![schema],
            Vec::new(),
            vec![ValueTypeDefinition::primitive(
                binding_value_id,
                QualifiedSemanticName::new(["schema", "value"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                "orna.kernel.value.boolean@1",
            )],
            vec![binding],
        )
        .unwrap();
        let binding_candidate = unsupported_candidate(
            &active,
            0x34,
            binding_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::ValueType(binding_value_id),
                DefinitionIdentity::TypeBinding(binding_id),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            empty_version_two_context(),
        );

        let (schema_id, schema) = unsupported_schema(0x41);
        let enum_id = TypeId::from_bytes([0x42; 16]);
        let enum_catalogue = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([0x43; 16]),
            vec![schema],
            Vec::new(),
            Vec::new(),
            vec![EnumTypeDefinition::new(
                enum_id,
                QualifiedSemanticName::new(["schema", "status"]).unwrap(),
                ["active", "closed"],
            )],
            Vec::new(),
        )
        .unwrap();
        let enum_candidate = unsupported_candidate(
            &active,
            0x44,
            enum_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::ValueType(enum_id),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            empty_version_two_context(),
        );

        let (schema_id, schema) = unsupported_schema(0x51);
        let record_enum_id = TypeId::from_bytes([0x52; 16]);
        let record_id = TypeId::from_bytes([0x53; 16]);
        let record_field_id = FieldId::from_bytes([0x54; 16]);
        let record_field = RecordValueFieldDefinition::try_new_descriptor(
            record_field_id,
            "status",
            0,
            TypeDescriptor::named(record_enum_id),
        )
        .unwrap();
        let record_catalogue = CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes([0x55; 16]),
            vec![schema],
            Vec::new(),
            Vec::new(),
            vec![EnumTypeDefinition::new(
                record_enum_id,
                QualifiedSemanticName::new(["schema", "record_status"]).unwrap(),
                ["active", "closed"],
            )],
            vec![RecordValueTypeDefinition::new(
                record_id,
                QualifiedSemanticName::new(["schema", "status_record"]).unwrap(),
                vec![record_field],
            )],
            Vec::new(),
        )
        .unwrap();
        let record_candidate = unsupported_candidate(
            &active,
            0x56,
            record_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::ValueType(record_enum_id),
                DefinitionIdentity::ValueType(record_id),
                DefinitionIdentity::Field {
                    owner: record_id,
                    field: record_field_id,
                },
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            empty_version_two_context(),
        );

        let (schema_id, schema) = unsupported_schema(0x61);
        let function_id = FunctionId::from_bytes([0x62; 16]);
        let function_revision_id = FunctionRevisionId::from_bytes([0x63; 16]);
        let function_catalogue = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([0x64; 16]),
            vec![schema],
            Vec::new(),
            vec![FunctionDefinition::new(
                function_id,
                QualifiedSemanticName::new(["schema", "calculate"]).unwrap(),
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
                function_revision_id,
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Immutable,
            )],
        )
        .unwrap();
        let function_candidate = unsupported_candidate(
            &active,
            0x65,
            function_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::Function(function_id),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            CatalogueHashContext::version_one(),
        );

        let (schema_id, schema) = unsupported_schema(0x71);
        let revision_function_id = FunctionId::from_bytes([0x72; 16]);
        let revision_id = FunctionRevisionId::from_bytes([0x73; 16]);
        let revision_origin = unsupported_source_origin(0x7a);
        let executable_payload = vec![0x76];
        let executable = ExecutableArtifact::new(
            ExecutableArtifactKind::Server,
            "test.function",
            1,
            executable_payload.clone(),
            artifact_payload_digest(&executable_payload).unwrap(),
        )
        .unwrap();
        let function_revision = FunctionRevisionRecord::new(
            revision_function_id,
            revision_id,
            1,
            revision_origin,
            Sha256Digest::from_bytes([0x77; 32]),
            Sha256Digest::from_bytes([0x78; 32]),
            "orna.language/1",
            executable,
        )
        .unwrap();
        let revision_catalogue = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([0x79; 16]),
            vec![schema],
            Vec::new(),
            vec![FunctionDefinition::new(
                revision_function_id,
                QualifiedSemanticName::new(["schema", "versioned"]).unwrap(),
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
                revision_id,
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Immutable,
            )],
        )
        .unwrap();
        let revision_candidate = unsupported_candidate(
            &active,
            0x7a,
            revision_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::Function(revision_function_id),
            ],
            Vec::new(),
            vec![function_revision],
            Vec::new(),
            CatalogueHashContext::version_one(),
        );

        let (schema_id, schema) = unsupported_schema(0x81);
        let reference_function_id = FunctionId::from_bytes([0x82; 16]);
        let reference_revision_id = FunctionRevisionId::from_bytes([0x83; 16]);
        let reference_origin = unsupported_source_origin(0x8a);
        let reference_payload = vec![0x86];
        let reference_executable = ExecutableArtifact::new(
            ExecutableArtifactKind::Server,
            "test.reference",
            1,
            reference_payload.clone(),
            artifact_payload_digest(&reference_payload).unwrap(),
        )
        .unwrap();
        let reference_revision = FunctionRevisionRecord::new(
            reference_function_id,
            reference_revision_id,
            1,
            reference_origin,
            Sha256Digest::from_bytes([0x87; 32]),
            Sha256Digest::from_bytes([0x88; 32]),
            "orna.language/1",
            reference_executable,
        )
        .unwrap();
        let reference_catalogue = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([0x89; 16]),
            vec![schema],
            Vec::new(),
            vec![FunctionDefinition::new(
                reference_function_id,
                QualifiedSemanticName::new(["schema", "referencing"]).unwrap(),
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
                reference_revision_id,
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Immutable,
            )],
        )
        .unwrap();
        let reference = DefinitionReference::new(
            reference_function_id,
            reference_revision_id,
            0,
            DefinitionReferenceTarget::Function(reference_function_id),
            DefinitionReferenceKind::FunctionCall,
            reference_origin,
        );
        let reference_candidate = unsupported_candidate(
            &active,
            0x8a,
            reference_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::Function(reference_function_id),
            ],
            Vec::new(),
            vec![reference_revision],
            vec![reference],
            CatalogueHashContext::version_one(),
        );

        let (schema_id, schema) = unsupported_schema(0x91);
        let expression_id = ExpressionId::from_bytes([0x92; 16]);
        let expression_payload = vec![0x93];
        let expression = orna_core::revision::ExpressionArtifact::new(
            expression_id,
            "test.expression",
            1,
            expression_payload.clone(),
            artifact_payload_digest(&expression_payload).unwrap(),
        )
        .unwrap();
        let expression_catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x94; 16]),
            vec![schema],
            Vec::new(),
        )
        .unwrap();
        let expression_candidate = unsupported_candidate(
            &active,
            0x95,
            expression_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::Expression(expression_id),
            ],
            vec![expression],
            Vec::new(),
            Vec::new(),
            CatalogueHashContext::version_one(),
        );

        let (schema_id, schema) = unsupported_schema(0xa1);
        let origin_object_id = TypeId::from_bytes([0xa2; 16]);
        let origin_catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0xa3; 16]),
            vec![schema],
            vec![ObjectTypeDefinition::new(
                origin_object_id,
                QualifiedSemanticName::new(["schema", "origin_object"]).unwrap(),
                Vec::new(),
            )],
        )
        .unwrap();
        let origin_candidate = unsupported_candidate(
            &active,
            0xa4,
            origin_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::ObjectType(origin_object_id),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            CatalogueHashContext::version_one(),
        );

        let (schema_id, schema) = unsupported_schema(0xb1);
        let hash_catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0xb2; 16]),
            vec![schema],
            Vec::new(),
        )
        .unwrap();
        let hash_candidate = unsupported_candidate(
            &active,
            0xb3,
            hash_catalogue,
            [DefinitionIdentity::Schema(schema_id)],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            empty_version_two_context(),
        );

        let cases = [
            (SqliteCapability::ObjectType, object_candidate),
            (SqliteCapability::ValueType, binary_value_candidate),
            (SqliteCapability::ValueType, array_like_value_candidate),
            (SqliteCapability::TypeBinding, binding_candidate),
            (SqliteCapability::EnumType, enum_candidate),
            (SqliteCapability::RecordValueType, record_candidate),
            (SqliteCapability::Function, function_candidate),
            (SqliteCapability::FunctionRevision, revision_candidate),
            (SqliteCapability::Reference, reference_candidate),
            (SqliteCapability::Expression, expression_candidate),
            (SqliteCapability::ObjectType, origin_candidate),
            (SqliteCapability::CatalogueHashVersion, hash_candidate),
        ];
        for (expected, candidate) in cases {
            let artifact = PhysicalMigrationArtifact::from_revisions(&active, &candidate).unwrap();
            assert_unsupported_candidate_without_mutation(&store, &candidate, &artifact, expected)
                .await;
        }
        let _ = std::fs::remove_file(path);
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
    async fn rejects_active_pointer_divergence_when_reading_ledger() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let initial_metadata = {
            let connection = store.connection.lock().await;
            load_active_identity_metadata(&connection)
                .await
                .unwrap()
                .unwrap()
        };

        let candidate = schema_candidate(&initial, 0x31, 0x32, 0x33);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();
        RevisionStore::apply(&store, &candidate, &artifact)
            .await
            .unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
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
                    Value::Blob(initial_metadata.source_id.to_bytes().to_vec()),
                    Value::Null,
                    Value::Blob(initial_metadata.catalogue_id.to_bytes().to_vec()),
                    Value::Blob(initial_metadata.source.bundle.to_bytes().to_vec()),
                    Value::Blob(initial_metadata.source.bundle_hash.to_bytes().to_vec()),
                    Value::Blob(initial_metadata.source.revision_hash.to_bytes().to_vec()),
                    Value::Blob(initial_metadata.catalogue.hash.to_bytes().to_vec()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        assert!(matches!(
            store.read_ledger().await.unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger does not end at active revision"
            ))
        ));
        assert!(matches!(
            store.recover().await.unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger does not end at active revision"
            ))
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_tampered_active_catalogue_lineage_before_apply() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let candidate = schema_candidate(&initial, 0x81, 0x82, 0x83);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_catalogue_revisions
                 SET source_revision_id = ?1
                 WHERE catalogue_revision_id = ?2",
                [
                    Value::Blob(vec![0x99; 16]),
                    Value::Blob(initial.pair().catalogue().to_bytes().to_vec()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        assert!(matches!(
            RevisionStore::apply(&store, &candidate, &artifact)
                .await
                .unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "active catalogue revision source does not match active source"
            ))
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_tampered_historical_catalogue_when_reading_ledger() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let first_candidate = schema_candidate(&initial, 0x41, 0x42, 0x43);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let first = RevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();
        let second_candidate = schema_candidate(&first, 0x51, 0x52, 0x53);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&first, &second_candidate).unwrap();
        RevisionStore::apply(&store, &second_candidate, &second_artifact)
            .await
            .unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_catalogue_revisions
                 SET catalogue_hash = ?1
                 WHERE catalogue_revision_id = ?2",
                [
                    Value::Blob(vec![0; 32]),
                    Value::Blob(
                        first_candidate
                            .candidate_pair()
                            .catalogue()
                            .to_bytes()
                            .to_vec(),
                    ),
                ],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        assert!(matches!(
            store.read_ledger().await.unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "historical catalogue hash mismatch"
            ))
        ));
        assert!(matches!(
            store.recover().await.unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "historical catalogue hash mismatch"
            ))
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_tampered_historical_catalogue_parent_when_reading_ledger() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let first_candidate = schema_candidate(&initial, 0x61, 0x62, 0x63);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let first = RevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();
        let second_candidate = schema_candidate(&first, 0x71, 0x72, 0x73);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&first, &second_candidate).unwrap();
        RevisionStore::apply(&store, &second_candidate, &second_artifact)
            .await
            .unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_catalogue_revisions
                 SET parent_catalogue_revision_id = NULL
                 WHERE catalogue_revision_id = ?1",
                [Value::Blob(
                    first_candidate
                        .candidate_pair()
                        .catalogue()
                        .to_bytes()
                        .to_vec(),
                )],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        assert!(matches!(
            store.read_ledger().await.unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "catalogue revision parent does not match migration history"
            ))
        ));
        assert!(matches!(
            store.recover().await.unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "catalogue revision parent does not match migration history"
            ))
        ));
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

    #[tokio::test]
    async fn concurrent_independent_applies_commit_exactly_once() {
        let path = temp_path();
        let store_a = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store_a.bootstrap().await.unwrap();
        let store_b = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        let initial = store_a.recover().await.unwrap();
        let candidate = schema_candidate(&initial, 0xb1, 0xb2, 0xb3);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();

        let (result_a, result_b) = tokio::join!(
            RevisionStore::apply(&store_a, &candidate, &artifact),
            RevisionStore::apply(&store_b, &candidate, &artifact),
        );

        let mut successes = 0;
        let mut failures = 0;
        for result in [result_a, result_b] {
            match result {
                Ok(active) => {
                    successes += 1;
                    assert_eq!(active.pair(), candidate.candidate_pair());
                }
                Err(error) => {
                    failures += 1;
                    match error {
                        StorageError::InvalidRequest(
                            MigrationLedgerEntryError::ActiveBaseMismatch { expected, actual },
                        ) => {
                            assert_eq!(expected, initial.pair());
                            assert_eq!(actual, candidate.candidate_pair());
                        }
                        StorageError::Backend(SqliteError::Backend(turso::Error::Busy(_)))
                        | StorageError::Backend(SqliteError::Backend(
                            turso::Error::BusySnapshot(_),
                        )) => {}
                        StorageError::Backend(SqliteError::Backend(turso::Error::Error(
                            message,
                        ))) if message.to_ascii_lowercase().contains("busy")
                            || message.to_ascii_lowercase().contains("locked") => {}
                        other => panic!("unexpected concurrent apply result: {other:?}"),
                    }
                }
            }
        }
        assert_eq!(successes, 1);
        assert_eq!(failures, 1);

        assert_eq!(
            persisted_row_counts(&store_a).await,
            PersistedRowCounts {
                active: 1,
                source_revisions: 2,
                catalogue_revisions: 2,
                source_units: 1,
                schemas: 1,
                ledger: 1,
            }
        );
        assert_eq!(
            store_a.read_ledger().await.unwrap(),
            vec![MigrationLedgerEntry::from_artifact(&artifact)]
        );
        assert_eq!(
            store_a.recover().await.unwrap().pair(),
            candidate.candidate_pair()
        );

        drop(store_b);
        drop(store_a);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_non_contiguous_ledger_before_applying_candidate() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let first_candidate = schema_candidate(&initial, 0xc1, 0xc2, 0xc3);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let active_before = RevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();
        let counts_before = persisted_row_counts(&store).await;

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_application_migrations
                 SET ordinal = 1
                 WHERE ordinal = 0",
                (),
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        let second_candidate = schema_candidate(&active_before, 0xd1, 0xd2, 0xd3);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&active_before, &second_candidate).unwrap();
        let error = RevisionStore::apply(&store, &second_candidate, &second_artifact)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::Domain(message))
                if message == "invalid migration ledger ordinal 1: expected 0"
        ));

        assert_eq!(persisted_row_counts(&store).await, counts_before);
        let active_after = {
            let connection = store.connection.lock().await;
            load_active_from(&connection).await.unwrap()
        };
        assert_eq!(active_after.pair(), active_before.pair());
        assert_eq!(
            active_after.source().revision_hash(),
            active_before.source().revision_hash()
        );
        assert_eq!(
            active_after.catalogue_hash(),
            active_before.catalogue_hash()
        );
        let ordinal = {
            let connection = store.connection.lock().await;
            let mut rows = connection
                .query(
                    "SELECT ordinal
                     FROM orna_application_migrations",
                    (),
                )
                .await
                .unwrap();
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
        };
        assert_eq!(ordinal, 1);

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_deleted_and_renumbered_first_ledger_entry_without_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let first_candidate = schema_candidate(&initial, 0xe1, 0xe2, 0xe3);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let first_active = RevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();

        let second_candidate = schema_candidate(&first_active, 0xe4, 0xe5, 0xe6);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&first_active, &second_candidate).unwrap();
        let second_active = RevisionStore::apply(&store, &second_candidate, &second_artifact)
            .await
            .unwrap();

        let third_candidate = schema_candidate(&second_active, 0xe7, 0xe8, 0xe9);
        let third_artifact =
            PhysicalMigrationArtifact::from_revisions(&second_active, &third_candidate).unwrap();

        let connection = store.connection.lock().await;
        let deleted = connection
            .execute(
                "DELETE FROM orna_application_migrations
                 WHERE ordinal = 0",
                (),
            )
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        let renumbered = connection
            .execute(
                "UPDATE orna_application_migrations
                 SET ordinal = 0
                 WHERE ordinal = 1",
                (),
            )
            .await
            .unwrap();
        assert_eq!(renumbered, 1);
        drop(connection);

        let counts_after_corruption = persisted_row_counts(&store).await;
        let error = store.read_ledger().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger first expected source revision is not a root source"
            ))
        ));

        let error = store.recover().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger first expected source revision is not a root source"
            ))
        ));

        let error = RevisionStore::apply(&store, &third_candidate, &third_artifact)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger first expected source revision is not a root source"
            ))
        ));
        assert_eq!(persisted_row_counts(&store).await, counts_after_corruption);

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_empty_ledger_after_non_root_apply_without_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let candidate = schema_candidate(&initial, 0xea, 0xeb, 0xec);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();
        let active = RevisionStore::apply(&store, &candidate, &artifact)
            .await
            .unwrap();
        let next_candidate = schema_candidate(&active, 0xed, 0xee, 0xef);
        let next_artifact =
            PhysicalMigrationArtifact::from_revisions(&active, &next_candidate).unwrap();

        let connection = store.connection.lock().await;
        let deleted = connection
            .execute("DELETE FROM orna_application_migrations", ())
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        drop(connection);

        let counts_after_corruption = persisted_row_counts(&store).await;
        let error = store.read_ledger().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger is empty but active source revision has a parent"
            ))
        ));

        let error = store.recover().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger is empty but active source revision has a parent"
            ))
        ));

        let error = RevisionStore::apply(&store, &next_candidate, &next_artifact)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger is empty but active source revision has a parent"
            ))
        ));
        assert_eq!(persisted_row_counts(&store).await, counts_after_corruption);

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_recovery_when_active_source_parent_registry_is_missing() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let candidate = schema_candidate(&initial, 0xe1, 0xe2, 0xe3);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();
        let active = RevisionStore::apply(&store, &candidate, &artifact)
            .await
            .unwrap();
        let counts_before_delete = persisted_row_counts(&store).await;
        let metadata_before = {
            let connection = store.connection.lock().await;
            load_active_identity_metadata(&connection)
                .await
                .unwrap()
                .unwrap()
        };
        let parent = active
            .source()
            .parent()
            .expect("applied candidate must retain its active source parent");

        let connection = store.connection.lock().await;
        let deleted = connection
            .execute(
                "DELETE FROM orna_source_revisions
                 WHERE source_revision_id = ?1",
                [Value::Blob(parent.to_bytes().to_vec())],
            )
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        drop(connection);

        let error = store.recover().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "active source parent revision has no registry record"
            ))
        ));
        assert_eq!(
            persisted_row_counts(&store).await,
            PersistedRowCounts {
                source_revisions: counts_before_delete.source_revisions - 1,
                ..counts_before_delete
            }
        );
        let metadata_after = {
            let connection = store.connection.lock().await;
            load_active_identity_metadata(&connection)
                .await
                .unwrap()
                .unwrap()
        };
        assert_eq!(metadata_after.source_id, metadata_before.source_id);
        assert_eq!(metadata_after.source, metadata_before.source);
        assert_eq!(metadata_after.catalogue_id, metadata_before.catalogue_id);
        assert_eq!(metadata_after.catalogue, metadata_before.catalogue);

        drop(store);
        let _ = std::fs::remove_file(path);
    }
    #[tokio::test]
    async fn rejects_tampered_historical_source_units_without_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let first_candidate = schema_candidate(&initial, 0x31, 0x32, 0x33);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let first_active = RevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();

        let second_candidate = schema_candidate(&first_active, 0x41, 0x42, 0x43);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&first_active, &second_candidate).unwrap();
        let second_active = RevisionStore::apply(&store, &second_candidate, &second_artifact)
            .await
            .unwrap();
        assert_ne!(
            first_candidate.source().id(),
            second_active.pair().source(),
            "the tampered source must be historical rather than active"
        );

        let counts_before_tamper = persisted_row_counts(&store).await;
        let metadata_before_tamper = {
            let connection = store.connection.lock().await;
            load_active_identity_metadata(&connection)
                .await
                .unwrap()
                .unwrap()
        };
        let third_candidate = schema_candidate(&second_active, 0x51, 0x52, 0x53);
        let third_artifact =
            PhysicalMigrationArtifact::from_revisions(&second_active, &third_candidate).unwrap();

        let tampered_content = "tampered historical source\n";
        let tampered_hash = source_unit_content_digest(tampered_content).unwrap();
        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_source_units
                 SET content = ?1, content_hash = ?2
                 WHERE source_revision_id = ?3",
                [
                    Value::Text(tampered_content.to_owned()),
                    Value::Blob(tampered_hash.to_bytes().to_vec()),
                    Value::Blob(first_candidate.source().id().to_bytes().to_vec()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        let error = store.read_ledger().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "source bundle hash mismatch"
            ))
        ));

        let error = store.recover().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "source bundle hash mismatch"
            ))
        ));

        let error = RevisionStore::apply(&store, &third_candidate, &third_artifact)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "source bundle hash mismatch"
            ))
        ));

        assert_eq!(persisted_row_counts(&store).await, counts_before_tamper);
        let metadata_after_tamper = {
            let connection = store.connection.lock().await;
            load_active_identity_metadata(&connection)
                .await
                .unwrap()
                .unwrap()
        };
        assert_eq!(
            metadata_after_tamper.source_id,
            metadata_before_tamper.source_id
        );
        assert_eq!(metadata_after_tamper.source, metadata_before_tamper.source);
        assert_eq!(
            metadata_after_tamper.catalogue_id,
            metadata_before_tamper.catalogue_id
        );
        assert_eq!(
            metadata_after_tamper.catalogue,
            metadata_before_tamper.catalogue
        );

        drop(store);
        let _ = std::fs::remove_file(path);
    }
}
