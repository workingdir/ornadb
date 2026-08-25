use std::collections::{BTreeMap, BTreeSet, HashSet};

#[path = "recovery/functions.rs"]
mod functions;

#[cfg(feature = "test-hooks")]
use orna_core::canonical_hash::{
    verify_standard_library_snapshot as verify_structural_standard_library_snapshot,
    verify_standard_library_v2_snapshot as verify_structural_standard_library_v2_snapshot,
};
use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId, StandardLibraryRevisionId,
    TypeBindingId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest_with_context, source_bundle_digest,
        source_revision_digest, source_revision_record_digest, source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, EnumTypeDefinition, FieldDefinition, FunctionDefinition, FunctionDomain,
        FunctionReturn, FunctionSecurity, FunctionTransaction, FunctionVolatility,
        ObjectTypeDefinition, OnDeleteAction, ParameterDefinition, PreludeTypeName,
        QualifiedSemanticName, RecordValueFieldDefinition, RecordValueTypeDefinition,
        SchemaDefinition, TypeBinding, TypeBindingKind, ValueTypeDefinition, ValueTypeKind,
        ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
        DefinitionReference, DefinitionReferenceKind, DefinitionReferenceTarget,
        ExecutableArtifact, ExecutableArtifactKind, ExpressionArtifact, FunctionRevisionRecord,
        FunctionSemanticHashVersion, RevisionPair, Sha256Digest, SourceOrigin, StandardExecutable,
        StandardLibraryDigestVersion, StandardLibrarySnapshot, StoredSourceRevision,
        StoredSourceUnit, VerifiedStandardLibrarySnapshot,
    },
    system::SYS_INSPECT_INVOCATION_TYPE_ID,
    types::{ResolvedType, StandardScalar, TypeDescriptor},
};

use orna_standard::{
    STANDARD_LIBRARY_REVISION_ID, STANDARD_LIBRARY_V2_REVISION_ID, STANDARD_LIBRARY_V3_REVISION_ID,
    STANDARD_LIBRARY_V4_REVISION_ID, STANDARD_LIBRARY_V5_REVISION_ID,
    STANDARD_LIBRARY_V6_REVISION_ID, verify_standard_library_snapshot,
    verify_standard_library_v2_snapshot, verify_standard_library_v3_snapshot,
    verify_standard_library_v4_snapshot, verify_standard_library_v5_snapshot,
    verify_standard_library_v6_snapshot,
};
use tokio_postgres::{Client, IsolationLevel, Row, Transaction};

use crate::{
    PostgresKernel, PostgresKernelError,
    bootstrap::require_current_migrations,
    decode::{
        DurableRecord, digest_bytes, exact_enum, identity_bytes, optional_identity_bytes,
        u32_from_i64, u64_from_i64,
    },
    is_sealed_inspect_type_id,
    physical::{establish_trusted_search_path, verify_physical_catalogue},
};

use self::functions::{
    RecoveredFunctionState, load_catalogue_current_revisions, load_catalogue_functions,
    load_function_state, load_references, validate_reference_sources,
};

const ACTIVE_RELATION: &str = "_orna_kernel.active_revision";
const SOURCE_UNIT_RELATION: &str = "_orna_kernel.source_units";
const SOURCE_REVISION_RELATION: &str = "_orna_kernel.source_revisions";
const CATALOGUE_REVISION_RELATION: &str = "_orna_kernel.catalogue_revisions";

/// One immutable source/catalogue revision pair retained by the kernel.
///
/// Entries expose only revision identities and their stored parent links. The
/// pair whose identities match the durable active marker is reported by
/// [`RevisionPairHistoryEntry::is_active`]. Returned entries are ordered by ascending source then
/// catalogue identity, using the canonical ordering of the opaque IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionPairHistoryEntry {
    source_revision_id: SourceRevisionId,
    source_parent_revision_id: Option<SourceRevisionId>,
    catalogue_revision_id: CatalogueRevisionId,
    catalogue_parent_revision_id: Option<CatalogueRevisionId>,
    is_active: bool,
}

impl RevisionPairHistoryEntry {
    /// Returns the source revision identity.
    pub const fn source_revision_id(self) -> SourceRevisionId {
        self.source_revision_id
    }

    /// Returns the optional parent source revision identity.
    pub const fn source_parent_revision_id(self) -> Option<SourceRevisionId> {
        self.source_parent_revision_id
    }

    /// Returns the catalogue revision identity.
    pub const fn catalogue_revision_id(self) -> CatalogueRevisionId {
        self.catalogue_revision_id
    }

    /// Returns the optional parent catalogue revision identity.
    pub const fn catalogue_parent_revision_id(self) -> Option<CatalogueRevisionId> {
        self.catalogue_parent_revision_id
    }

    /// Returns whether this pair is the durable active revision pair.
    pub const fn is_active(self) -> bool {
        self.is_active
    }
}

#[derive(Clone, Copy)]
enum HashAlgorithm {
    Sha256,
}

#[derive(Clone, Copy)]
enum TextEncoding {
    Utf8,
}

struct RecoveredRevisionHeader {
    bundle: SourceBundleId,
    source: SourceRevisionId,
    source_parent: Option<SourceRevisionId>,
    catalogue: CatalogueRevisionId,
    bundle_hash: Sha256Digest,
    source_hash: Sha256Digest,
    catalogue_hash: Sha256Digest,
    catalogue_hash_version: CatalogueHashVersion,
    standard_library_revision: Option<StandardLibraryRevisionId>,
}

struct RecoveredStandardHeader {
    revision: StandardLibraryRevisionId,
    bundle: SourceBundleId,
    source: SourceRevisionId,
    source_parent: Option<SourceRevisionId>,
    catalogue: CatalogueRevisionId,
    digest_version: StandardLibraryDigestVersion,
    language_version: String,
    bundle_hash: Sha256Digest,
    source_hash: Sha256Digest,
    digest: Sha256Digest,
}

struct RecoveredStandardSchema {
    definition: SchemaDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredStandardValueType {
    schema: SchemaId,
    definition: ValueTypeDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredStandardEnumType {
    schema: SchemaId,
    definition: EnumTypeDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredStandardTypeBinding {
    binding: TypeBinding,
    origin: DefinitionOrigin,
}

struct RecoveredStandardFunction {
    schema: SchemaId,
    id: FunctionId,
    name: QualifiedSemanticName,
    domain: FunctionDomain,
    security: FunctionSecurity,
    transaction: Option<FunctionTransaction>,
    volatility: FunctionVolatility,
    return_type: FunctionReturn,
    current_revision: FunctionRevisionId,
    origin: DefinitionOrigin,
}

#[derive(Clone)]
struct RecoveredStandardParameter {
    function: FunctionId,
    definition: ParameterDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredSchema {
    definition: SchemaDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredObjectType {
    id: TypeId,
    schema: SchemaId,
    name: QualifiedSemanticName,
    origin: DefinitionOrigin,
}

struct RecoveredEnumType {
    schema: SchemaId,
    definition: EnumTypeDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredRecordValueType {
    id: TypeId,
    schema: SchemaId,
    name: QualifiedSemanticName,
    origin: DefinitionOrigin,
}

struct RecoveredRecordValueField {
    owner: TypeId,
    definition: RecordValueFieldDefinition,
    origin: DefinitionOrigin,
}

struct RecordValueFieldTypeTuple {
    kind: Option<String>,
    value_type: Option<TypeId>,
    value_standard_library_revision: Option<StandardLibraryRevisionId>,
    application_enum_type: Option<TypeId>,
    enum_standard_library_revision: Option<StandardLibraryRevisionId>,
    standard_enum_type: Option<TypeId>,
    application_record_type: Option<TypeId>,
}

struct RecoveredField {
    owner: TypeId,
    definition: FieldDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredExpression {
    artifact: ExpressionArtifact,
    origin: DefinitionOrigin,
}

struct RecoveredCatalogueSemantics {
    catalogue: CatalogueSnapshot,
    expressions: Vec<ExpressionArtifact>,
    origins: Vec<DefinitionOrigin>,
}

impl PostgresKernel {
    /// Lists every immutable source/catalogue revision pair retained by the
    /// kernel, marking the pair selected by the durable active marker.
    ///
    /// The listing runs in one read-only repeatable-read transaction after the
    /// trusted search path and current migration registry have been checked.
    /// Entries are ordered by ascending source then catalogue identity. This
    /// method only reads revision metadata; it does not activate or reconstruct
    /// a historical revision.
    pub async fn list_revision_pairs(
        &self,
    ) -> Result<Vec<RevisionPairHistoryEntry>, PostgresKernelError> {
        let mut session = self.open().await?;
        let listing_result = list_revision_pairs_client(&mut session.client).await;
        let shutdown_result = session.shutdown().await;

        match (listing_result, shutdown_result) {
            (Ok(entries), Ok(())) => Ok(entries),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    /// Reconstructs and validates the complete active durable database revision.
    ///
    /// This recovery slice supports schemas, object and record value types,
    /// fields, expression artifacts, compiler-deployable functions, immutable
    /// function history, and active definition references. It fails closed on
    /// any semantic, source, hash-chain, or physical-layout state it cannot
    /// prove complete.
    pub async fn recover(&self) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
        let mut session = self.open().await?;
        let recovery_result = recover_client(&mut session.client)
            .await
            .map_err(super::map_recovery_client_error);
        let shutdown_result = session.shutdown_for_source_apply().await;

        match (recovery_result, shutdown_result) {
            (Ok(active), Ok(())) => Ok(active),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
}

async fn list_revision_pairs_client(
    client: &mut Client,
) -> Result<Vec<RevisionPairHistoryEntry>, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;

    establish_trusted_search_path(&transaction).await?;
    require_current_migrations(&transaction).await?;
    let active_pair = load_active_revision_pair(&transaction).await?;
    let rows = transaction
        .query(
            "SELECT
                catalogue.source_revision_id AS source_id,
                source.parent_source_revision_id AS source_parent_id,
                catalogue.id AS catalogue_id,
                catalogue.parent_catalogue_revision_id AS catalogue_parent_id,
                source.id IS NOT NULL AS source_exists
             FROM _orna_kernel.catalogue_revisions AS catalogue
             LEFT JOIN _orna_kernel.source_revisions AS source
               ON source.id = catalogue.source_revision_id
             ORDER BY catalogue.source_revision_id ASC, catalogue.id ASC",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    let entries = rows
        .iter()
        .enumerate()
        .map(|(index, row)| decode_revision_pair_row(row, index, active_pair))
        .collect::<Result<Vec<_>, _>>()?;
    validate_revision_pair_listing(&entries)?;

    transaction
        .commit()
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(entries)
}

async fn load_active_revision_pair(
    transaction: &Transaction<'_>,
) -> Result<RevisionPair, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT singleton,
                    source_revision_id AS active_source_id,
                    catalogue_revision_id AS active_catalogue_id
             FROM _orna_kernel.active_revision",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let record = DurableRecord::new(ACTIVE_RELATION, "singleton=true");
    if rows.len() != 1 {
        return Err(
            record.invariant("exactly one active source and catalogue revision pair must exist")
        );
    }

    let row = &rows[0];
    if !record.column::<bool>(
        row,
        "singleton",
        "active revision singleton flag must be true",
    )? {
        return Err(record.invariant("active revision singleton flag must be true"));
    }
    let source = SourceRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "active_source_id",
            "active source revision identity must be 16 bytes",
        )?,
        &record,
        "active source revision identity must be 16 bytes",
    )?);
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "active_catalogue_id",
            "active catalogue revision identity must be 16 bytes",
        )?,
        &record,
        "active catalogue revision identity must be 16 bytes",
    )?);
    Ok(RevisionPair::new(source, catalogue))
}

fn decode_revision_pair_row(
    row: &Row,
    row_index: usize,
    active_pair: RevisionPair,
) -> Result<RevisionPairHistoryEntry, PostgresKernelError> {
    let source_record = DurableRecord::new(SOURCE_REVISION_RELATION, format!("row={row_index}"));
    let catalogue_record =
        DurableRecord::new(CATALOGUE_REVISION_RELATION, format!("row={row_index}"));
    if !catalogue_record.column::<bool>(
        row,
        "source_exists",
        "catalogue source join must identify a source row",
    )? {
        return Err(catalogue_record
            .invariant("each catalogue revision must have a matching source revision"));
    }
    decode_revision_pair_values(
        source_record.column(
            row,
            "source_id",
            "source revision identity must be 16 bytes",
        )?,
        source_record.column(
            row,
            "source_parent_id",
            "source parent identity must be null or 16 bytes",
        )?,
        catalogue_record.column(
            row,
            "catalogue_id",
            "catalogue revision identity must be 16 bytes",
        )?,
        catalogue_record.column(
            row,
            "catalogue_parent_id",
            "catalogue parent identity must be null or 16 bytes",
        )?,
        active_pair,
        &source_record,
        &catalogue_record,
    )
}

fn decode_revision_pair_values(
    source_id: Vec<u8>,
    source_parent_id: Option<Vec<u8>>,
    catalogue_id: Vec<u8>,
    catalogue_parent_id: Option<Vec<u8>>,
    active_pair: RevisionPair,
    source_record: &DurableRecord,
    catalogue_record: &DurableRecord,
) -> Result<RevisionPairHistoryEntry, PostgresKernelError> {
    let source_revision_id = SourceRevisionId::from_bytes(identity_bytes(
        source_id,
        source_record,
        "source revision identity must be 16 bytes",
    )?);
    let source_parent_revision_id = optional_identity_bytes(
        source_parent_id,
        source_record,
        "source parent identity must be null or 16 bytes",
    )?
    .map(SourceRevisionId::from_bytes);
    let catalogue_revision_id = CatalogueRevisionId::from_bytes(identity_bytes(
        catalogue_id,
        catalogue_record,
        "catalogue revision identity must be 16 bytes",
    )?);
    let catalogue_parent_revision_id = optional_identity_bytes(
        catalogue_parent_id,
        catalogue_record,
        "catalogue parent identity must be null or 16 bytes",
    )?
    .map(CatalogueRevisionId::from_bytes);
    if source_parent_revision_id.is_some() != catalogue_parent_revision_id.is_some() {
        return Err(catalogue_record.invariant(
            "source and catalogue parent identities must both be null or both be present",
        ));
    }

    Ok(RevisionPairHistoryEntry {
        source_revision_id,
        source_parent_revision_id,
        catalogue_revision_id,
        catalogue_parent_revision_id,
        is_active: RevisionPair::new(source_revision_id, catalogue_revision_id) == active_pair,
    })
}

fn validate_revision_pair_listing(
    entries: &[RevisionPairHistoryEntry],
) -> Result<(), PostgresKernelError> {
    let mut by_catalogue = BTreeMap::new();
    let mut source_ids = BTreeSet::new();
    for entry in entries {
        let catalogue = entry.catalogue_revision_id();
        let catalogue_record =
            DurableRecord::new(CATALOGUE_REVISION_RELATION, catalogue.canonical());
        if by_catalogue.insert(catalogue, *entry).is_some() {
            return Err(catalogue_record.invariant("catalogue revision identities must be unique"));
        }
        if !source_ids.insert(entry.source_revision_id()) {
            return Err(DurableRecord::new(
                SOURCE_REVISION_RELATION,
                entry.source_revision_id().canonical(),
            )
            .invariant("source revision identities must be unique"));
        }
        if entry.source_parent_revision_id().is_some()
            != entry.catalogue_parent_revision_id().is_some()
        {
            return Err(catalogue_record.invariant(
                "source and catalogue parent identities must both be null or both be present",
            ));
        }
    }

    for entry in entries {
        let Some(parent_catalogue) = entry.catalogue_parent_revision_id() else {
            continue;
        };
        let Some(parent_source) = entry.source_parent_revision_id() else {
            return Err(DurableRecord::new(
                CATALOGUE_REVISION_RELATION,
                entry.catalogue_revision_id().canonical(),
            )
            .invariant(
                "each catalogue parent must exist and identify the corresponding source parent",
            ));
        };
        let Some(parent_entry) = by_catalogue.get(&parent_catalogue) else {
            return Err(DurableRecord::new(
                CATALOGUE_REVISION_RELATION,
                parent_catalogue.canonical(),
            )
            .invariant(
                "each catalogue parent must exist and identify the corresponding source parent",
            ));
        };
        if parent_entry.source_revision_id() != parent_source {
            return Err(DurableRecord::new(
                CATALOGUE_REVISION_RELATION,
                entry.catalogue_revision_id().canonical(),
            )
            .invariant(
                "each catalogue parent must exist and identify the corresponding source parent",
            ));
        }
    }

    let mut state = BTreeMap::new();
    for entry in entries {
        let mut current = entry.catalogue_revision_id();
        let mut path = Vec::new();
        loop {
            match state.get(&current).copied().unwrap_or(0) {
                0 => {
                    state.insert(current, 1);
                    path.push(current);
                    let Some(current_entry) = by_catalogue.get(&current) else {
                        return Err(DurableRecord::new(
                            CATALOGUE_REVISION_RELATION,
                            current.canonical(),
                        )
                        .invariant(
                            "each catalogue parent must exist and identify the corresponding source parent",
                        ));
                    };
                    let Some(parent) = current_entry.catalogue_parent_revision_id() else {
                        break;
                    };
                    current = parent;
                }
                1 => {
                    return Err(DurableRecord::new(
                        CATALOGUE_REVISION_RELATION,
                        current.canonical(),
                    )
                    .invariant(
                        "catalogue and source revision ancestry must terminate without repeated identities",
                    ));
                }
                2 => break,
                _ => unreachable!("revision listing state has only three values"),
            }
        }
        for catalogue in path {
            state.insert(catalogue, 2);
        }
    }

    if entries.iter().filter(|entry| entry.is_active()).count() != 1 {
        return Err(DurableRecord::new(ACTIVE_RELATION, "singleton=true")
            .invariant("exactly one listed revision pair must match the active marker"));
    }
    Ok(())
}

async fn recover_client(
    client: &mut Client,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;

    let active = recover_active_revision(&transaction).await?;
    crate::security::recover_invocation_audit_events(&transaction, &active).await?;
    crate::inspect::recover_inspect_relations(&transaction, &active).await?;

    transaction
        .commit()
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(active)
}

pub(crate) async fn recover_active_revision(
    transaction: &Transaction<'_>,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    establish_trusted_search_path(transaction).await?;
    require_current_migrations(transaction).await?;
    let header = load_active_header(transaction).await?;
    let catalogue_hash_context = load_active_catalogue_hash_context(transaction, &header).await?;
    let active_ancestry =
        validate_revision_ancestry(transaction, header.catalogue, header.source).await?;
    validate_source_ancestry(transaction, &active_ancestry).await?;
    let units = load_source_units(transaction, header.bundle).await?;
    let mut function_state = load_function_state(
        transaction,
        header.catalogue,
        &active_ancestry,
        &catalogue_hash_context,
    )
    .await?;
    validate_catalogue_ancestry(transaction, header.catalogue, &active_ancestry).await?;
    let functions = std::mem::take(&mut function_state.functions);
    let function_origins = std::mem::take(&mut function_state.origins);
    let semantics = load_catalogue_semantics(
        transaction,
        header.catalogue,
        functions,
        function_origins,
        &catalogue_hash_context,
    )
    .await?;
    let active = assemble_revision(
        header,
        units,
        semantics,
        function_state,
        catalogue_hash_context,
    )?;
    verify_physical_catalogue(transaction, &active).await?;

    Ok(active)
}

async fn validate_revision_ancestry(
    transaction: &Transaction<'_>,
    active_catalogue: CatalogueRevisionId,
    active_source: SourceRevisionId,
) -> Result<BTreeSet<(CatalogueRevisionId, SourceRevisionId)>, PostgresKernelError> {
    let mut catalogue = active_catalogue;
    let mut source = active_source;
    let mut seen_catalogues = HashSet::new();
    let mut seen_sources = HashSet::new();
    let mut ancestry = BTreeSet::new();

    loop {
        let catalogue_record =
            DurableRecord::new("_orna_kernel.catalogue_revisions", catalogue.canonical());
        if !seen_catalogues.insert(catalogue) || !seen_sources.insert(source) {
            return Err(catalogue_record.invariant(
                "catalogue and source revision ancestry must terminate without repeated identities",
            ));
        }
        ancestry.insert((catalogue, source));

        let rows = transaction
            .query(
                "SELECT
                    catalogue.parent_catalogue_revision_id AS catalogue_parent_id,
                    source.parent_source_revision_id AS source_parent_id,
                    parent_catalogue.source_revision_id AS parent_catalogue_source_id
                 FROM _orna_kernel.catalogue_revisions AS catalogue
                 JOIN _orna_kernel.source_revisions AS source
                   ON source.id = catalogue.source_revision_id
                 LEFT JOIN _orna_kernel.catalogue_revisions AS parent_catalogue
                   ON parent_catalogue.id = catalogue.parent_catalogue_revision_id
                 WHERE catalogue.id = $1
                   AND source.id = $2",
                &[&catalogue.to_bytes().to_vec(), &source.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if rows.len() != 1 {
            return Err(catalogue_record.invariant(
                "each catalogue ancestor must join exactly one corresponding source revision",
            ));
        }

        let row = &rows[0];
        let catalogue_parent = optional_identity_bytes(
            catalogue_record.column(
                row,
                "catalogue_parent_id",
                "catalogue parent identity must be null or 16 bytes",
            )?,
            &catalogue_record,
            "catalogue parent identity must be null or 16 bytes",
        )?
        .map(CatalogueRevisionId::from_bytes);
        let source_record = DurableRecord::new("_orna_kernel.source_revisions", source.canonical());
        let source_parent = optional_identity_bytes(
            source_record.column(
                row,
                "source_parent_id",
                "source parent identity must be null or 16 bytes",
            )?,
            &source_record,
            "source parent identity must be null or 16 bytes",
        )?
        .map(SourceRevisionId::from_bytes);
        let parent_catalogue_source = optional_identity_bytes(
            catalogue_record.column(
                row,
                "parent_catalogue_source_id",
                "parent catalogue source identity must be null or 16 bytes",
            )?,
            &catalogue_record,
            "parent catalogue source identity must be null or 16 bytes",
        )?
        .map(SourceRevisionId::from_bytes);

        match (catalogue_parent, source_parent, parent_catalogue_source) {
            (None, None, None) => return Ok(ancestry),
            (Some(parent_catalogue), Some(parent_source), Some(joined_parent_source))
                if parent_source == joined_parent_source =>
            {
                catalogue = parent_catalogue;
                source = parent_source;
            }
            _ => {
                return Err(catalogue_record.invariant(
                    "each catalogue parent must exist and identify the corresponding source parent",
                ));
            }
        }
    }
}

async fn validate_source_ancestry(
    transaction: &Transaction<'_>,
    ancestry: &BTreeSet<(CatalogueRevisionId, SourceRevisionId)>,
) -> Result<(), PostgresKernelError> {
    let source_revisions = ancestry
        .iter()
        .map(|(_, source)| *source)
        .collect::<BTreeSet<_>>();
    for source in source_revisions {
        validate_source_revision(transaction, source).await?;
    }
    Ok(())
}

async fn validate_catalogue_ancestry(
    transaction: &Transaction<'_>,
    active_catalogue: CatalogueRevisionId,
    active_ancestry: &BTreeSet<(CatalogueRevisionId, SourceRevisionId)>,
) -> Result<(), PostgresKernelError> {
    for (catalogue, _) in active_ancestry {
        if *catalogue == active_catalogue {
            continue;
        }

        let record = DurableRecord::new(CATALOGUE_REVISION_RELATION, catalogue.canonical());
        let rows = transaction
            .query(
                "SELECT content_hash,
                        hash_algorithm,
                        hash_contract_version,
                        canonical_hash_version,
                        standard_library_revision_id
                 FROM _orna_kernel.catalogue_revisions
                 WHERE id = $1",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if rows.len() != 1 {
            return Err(record.invariant(
                "each reachable catalogue ancestor must have exactly one durable revision row",
            ));
        }

        let row = &rows[0];
        require_hash_contract(
            row,
            &record,
            "hash_algorithm",
            "hash_contract_version",
            "catalogue hash algorithm must be sha256",
            "catalogue hash contract version must be 1",
        )?;
        let hash_version = decode_catalogue_hash_version(
            record.column(
                row,
                "canonical_hash_version",
                "catalogue canonical hash version must be a supported smallint",
            )?,
            &record,
        )?;
        let standard_library_revision = optional_identity_bytes(
            record.column(
                row,
                "standard_library_revision_id",
                "catalogue standard library revision identity must be null or 16 bytes",
            )?,
            &record,
            "catalogue standard library revision identity must be null or 16 bytes",
        )?
        .map(StandardLibraryRevisionId::from_bytes);
        let verified_standard = match standard_library_revision {
            Some(revision) => Some(load_verified_standard_library(transaction, revision).await?),
            None => None,
        };
        let catalogue_hash_context = catalogue_hash_context_for(
            hash_version,
            standard_library_revision,
            verified_standard.as_ref(),
            &record,
        )?;
        let stored_hash = Sha256Digest::from_bytes(digest_bytes(
            record.column(
                row,
                "content_hash",
                "catalogue revision digest must be 32 bytes",
            )?,
            &record,
            "catalogue revision digest must be 32 bytes",
        )?);

        let (functions, origins) =
            load_catalogue_functions(transaction, *catalogue, &catalogue_hash_context).await?;
        let references = load_references(
            transaction,
            *catalogue,
            catalogue_hash_context
                .standard()
                .map(|standard| standard.revision()),
        )
        .await?;
        validate_reference_sources(&functions, &references)?;
        let semantics = load_catalogue_semantics(
            transaction,
            *catalogue,
            functions,
            origins,
            &catalogue_hash_context,
        )
        .await?;

        let revisions = load_catalogue_current_revisions(transaction, *catalogue).await?;
        let mut current_revisions = Vec::with_capacity(semantics.catalogue.functions().len());
        for function in semantics.catalogue.functions() {
            let revision = revisions
                .get(&function.current_revision())
                .ok_or_else(|| {
                    DurableRecord::new(
                        "_orna_kernel.function_revisions",
                        function.current_revision().canonical(),
                    )
                        .invariant(
                            "every reachable catalogue function must resolve its immutable current revision",
                        )
                })?;
            if revision.function() != function.id() {
                return Err(DurableRecord::new(
                    "_orna_kernel.function_revisions",
                    revision.id().canonical(),
                )
                .invariant(
                    "reachable catalogue current revision must belong to its exact function",
                ));
            }
            current_revisions.push(revision.clone());
        }

        let computed_hash = catalogue_digest_with_context(
            &catalogue_hash_context,
            &semantics.catalogue,
            &current_revisions,
            &semantics.expressions,
            &semantics.origins,
            &references,
        )
        .map_err(PostgresKernelError::CanonicalHash)?;
        if computed_hash != stored_hash {
            return Err(record.invariant(
                "catalogue revision digest must match its complete recovered semantic catalogue",
            ));
        }
    }
    Ok(())
}

async fn validate_source_revision(
    transaction: &Transaction<'_>,
    source: SourceRevisionId,
) -> Result<(), PostgresKernelError> {
    let source_record = DurableRecord::new(SOURCE_REVISION_RELATION, source.canonical());
    let rows = transaction
        .query(
            "SELECT
                source.parent_source_revision_id AS source_parent_id,
                source.bundle_id AS source_bundle_id,
                source.content_hash AS source_hash,
                source.hash_algorithm AS source_algorithm,
                source.hash_contract_version AS source_contract_version,
                bundle.id AS bundle_id,
                bundle.content_hash AS bundle_hash,
                bundle.hash_algorithm AS bundle_algorithm,
                bundle.hash_contract_version AS bundle_contract_version
             FROM _orna_kernel.source_revisions AS source
             JOIN _orna_kernel.source_bundles AS bundle
               ON bundle.id = source.bundle_id
             WHERE source.id = $1",
            &[&source.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    if rows.len() != 1 {
        return Err(source_record
            .invariant("each retained source revision must join exactly one source bundle"));
    }

    let row = &rows[0];
    let parent = optional_identity_bytes(
        source_record.column(
            row,
            "source_parent_id",
            "source parent identity must be null or 16 bytes",
        )?,
        &source_record,
        "source parent identity must be null or 16 bytes",
    )?
    .map(SourceRevisionId::from_bytes);
    let bundle = SourceBundleId::from_bytes(identity_bytes(
        source_record.column(
            row,
            "source_bundle_id",
            "source bundle identity must be 16 bytes",
        )?,
        &source_record,
        "source bundle identity must be 16 bytes",
    )?);
    let bundle_record = DurableRecord::new("_orna_kernel.source_bundles", bundle.canonical());
    let joined_bundle = SourceBundleId::from_bytes(identity_bytes(
        bundle_record.column(row, "bundle_id", "source bundle identity must be 16 bytes")?,
        &bundle_record,
        "source bundle identity must be 16 bytes",
    )?);
    if joined_bundle != bundle {
        return Err(
            source_record.invariant("source revision must join its exact source bundle identity")
        );
    }
    let source_hash = Sha256Digest::from_bytes(digest_bytes(
        source_record.column(
            row,
            "source_hash",
            "source revision digest must be 32 bytes",
        )?,
        &source_record,
        "source revision digest must be 32 bytes",
    )?);
    let bundle_hash = Sha256Digest::from_bytes(digest_bytes(
        bundle_record.column(row, "bundle_hash", "source bundle digest must be 32 bytes")?,
        &bundle_record,
        "source bundle digest must be 32 bytes",
    )?);

    require_hash_contract(
        row,
        &source_record,
        "source_algorithm",
        "source_contract_version",
        "source revision hash algorithm must be sha256",
        "source revision hash contract version must be 1",
    )?;
    require_hash_contract(
        row,
        &bundle_record,
        "bundle_algorithm",
        "bundle_contract_version",
        "source bundle hash algorithm must be sha256",
        "source bundle hash contract version must be 1",
    )?;

    let units = load_source_units(transaction, bundle).await?;
    let computed_bundle_hash =
        source_bundle_digest(&units).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_bundle_hash != bundle_hash {
        return Err(bundle_record
            .invariant("source bundle digest must match the ordered source unit records"));
    }
    let computed_source_hash = source_revision_record_digest(bundle, parent, bundle_hash)
        .map_err(PostgresKernelError::CanonicalHash)?;
    if computed_source_hash != source_hash {
        return Err(source_record
            .invariant("source revision digest must match its bundle, parent, and bundle digest"));
    }
    Ok(())
}

async fn load_active_header(
    transaction: &Transaction<'_>,
) -> Result<RecoveredRevisionHeader, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT
                active.singleton,
                active.source_revision_id AS active_source_id,
                active.catalogue_revision_id AS active_catalogue_id,
                catalogue.id AS catalogue_id,
                catalogue.source_revision_id AS catalogue_source_id,
                catalogue.parent_catalogue_revision_id AS catalogue_parent_id,
                catalogue.content_hash AS catalogue_hash,
                catalogue.hash_algorithm AS catalogue_algorithm,
                catalogue.hash_contract_version AS catalogue_contract_version,
                catalogue.canonical_hash_version AS catalogue_canonical_hash_version,
                catalogue.standard_library_revision_id AS catalogue_standard_library_revision_id,
                parent_catalogue.source_revision_id AS parent_catalogue_source_id,
                source.id AS source_id,
                source.parent_source_revision_id AS source_parent_id,
                source.bundle_id AS source_bundle_id,
                source.content_hash AS source_hash,
                source.hash_algorithm AS source_algorithm,
                source.hash_contract_version AS source_contract_version,
                bundle.id AS bundle_id,
                bundle.content_hash AS bundle_hash,
                bundle.hash_algorithm AS bundle_algorithm,
                bundle.hash_contract_version AS bundle_contract_version
             FROM _orna_kernel.active_revision AS active
             JOIN _orna_kernel.catalogue_revisions AS catalogue
               ON catalogue.id = active.catalogue_revision_id
              AND catalogue.source_revision_id = active.source_revision_id
             JOIN _orna_kernel.source_revisions AS source
               ON source.id = active.source_revision_id
             JOIN _orna_kernel.source_bundles AS bundle
               ON bundle.id = source.bundle_id
             LEFT JOIN _orna_kernel.catalogue_revisions AS parent_catalogue
               ON parent_catalogue.id = catalogue.parent_catalogue_revision_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    if rows.len() != 1 {
        return Err(PostgresKernelError::DurableInvariant {
            relation: ACTIVE_RELATION,
            record: "singleton=true".into(),
            rule: "exactly one active catalogue, source revision, and source bundle join must exist",
        });
    }

    decode_active_header(&rows[0])
}

fn decode_active_header(row: &Row) -> Result<RecoveredRevisionHeader, PostgresKernelError> {
    let active_record = DurableRecord::new(ACTIVE_RELATION, "singleton=true");
    let singleton: bool = active_record.column(
        row,
        "singleton",
        "the active revision singleton flag must be true",
    )?;
    if !singleton {
        return Err(active_record.invariant("the active revision singleton flag must be true"));
    }

    let active_source = SourceRevisionId::from_bytes(identity_bytes(
        active_record.column(
            row,
            "active_source_id",
            "active source revision identity must be 16 bytes",
        )?,
        &active_record,
        "active source revision identity must be 16 bytes",
    )?);
    let active_catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        active_record.column(
            row,
            "active_catalogue_id",
            "active catalogue revision identity must be 16 bytes",
        )?,
        &active_record,
        "active catalogue revision identity must be 16 bytes",
    )?);
    let catalogue_record = DurableRecord::new(
        "_orna_kernel.catalogue_revisions",
        active_catalogue.canonical(),
    );
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        catalogue_record.column(
            row,
            "catalogue_id",
            "joined catalogue revision identity must be 16 bytes",
        )?,
        &catalogue_record,
        "joined catalogue revision identity must be 16 bytes",
    )?);
    let catalogue_source = SourceRevisionId::from_bytes(identity_bytes(
        catalogue_record.column(
            row,
            "catalogue_source_id",
            "catalogue source revision identity must be 16 bytes",
        )?,
        &catalogue_record,
        "catalogue source revision identity must be 16 bytes",
    )?);
    let source_record =
        DurableRecord::new("_orna_kernel.source_revisions", active_source.canonical());
    let source = SourceRevisionId::from_bytes(identity_bytes(
        source_record.column(
            row,
            "source_id",
            "joined source revision identity must be 16 bytes",
        )?,
        &source_record,
        "joined source revision identity must be 16 bytes",
    )?);
    let source_bundle = SourceBundleId::from_bytes(identity_bytes(
        source_record.column(
            row,
            "source_bundle_id",
            "source revision bundle identity must be 16 bytes",
        )?,
        &source_record,
        "source revision bundle identity must be 16 bytes",
    )?);
    let bundle_record =
        DurableRecord::new("_orna_kernel.source_bundles", source_bundle.canonical());
    let bundle = SourceBundleId::from_bytes(identity_bytes(
        bundle_record.column(
            row,
            "bundle_id",
            "joined source bundle identity must be 16 bytes",
        )?,
        &bundle_record,
        "joined source bundle identity must be 16 bytes",
    )?);

    if active_source != source || catalogue_source != source {
        return Err(active_record.invariant(
            "active and catalogue source links must identify the joined source revision",
        ));
    }
    if active_catalogue != catalogue {
        return Err(active_record
            .invariant("the active catalogue link must identify the joined catalogue revision"));
    }
    if source_bundle != bundle {
        return Err(source_record
            .invariant("the source revision bundle link must identify the joined source bundle"));
    }

    let source_parent = optional_identity_bytes(
        source_record.column(
            row,
            "source_parent_id",
            "source parent identity must be null or 16 bytes",
        )?,
        &source_record,
        "source parent identity must be null or 16 bytes",
    )?
    .map(SourceRevisionId::from_bytes);
    let catalogue_parent = optional_identity_bytes(
        catalogue_record.column(
            row,
            "catalogue_parent_id",
            "catalogue parent identity must be null or 16 bytes",
        )?,
        &catalogue_record,
        "catalogue parent identity must be null or 16 bytes",
    )?
    .map(CatalogueRevisionId::from_bytes);
    let parent_catalogue_source = optional_identity_bytes(
        catalogue_record.column(
            row,
            "parent_catalogue_source_id",
            "parent catalogue source identity must be null or 16 bytes",
        )?,
        &catalogue_record,
        "parent catalogue source identity must be null or 16 bytes",
    )?
    .map(SourceRevisionId::from_bytes);

    if catalogue_parent == Some(catalogue) {
        return Err(catalogue_record.invariant("the catalogue revision must not be its own parent"));
    }
    match (catalogue_parent, parent_catalogue_source) {
        (None, None) if source_parent.is_none() => {}
        (Some(_), Some(parent_source)) if source_parent == Some(parent_source) => {}
        _ => {
            return Err(catalogue_record.invariant(
                "the parent catalogue source link must equal the active source parent link",
            ));
        }
    }

    require_hash_contract(
        row,
        &catalogue_record,
        "catalogue_algorithm",
        "catalogue_contract_version",
        "catalogue hash algorithm must be sha256",
        "catalogue hash contract version must be 1",
    )?;
    require_hash_contract(
        row,
        &source_record,
        "source_algorithm",
        "source_contract_version",
        "source revision hash algorithm must be sha256",
        "source revision hash contract version must be 1",
    )?;
    require_hash_contract(
        row,
        &bundle_record,
        "bundle_algorithm",
        "bundle_contract_version",
        "source bundle hash algorithm must be sha256",
        "source bundle hash contract version must be 1",
    )?;

    let catalogue_hash_version = decode_catalogue_hash_version(
        catalogue_record.column(
            row,
            "catalogue_canonical_hash_version",
            "catalogue canonical hash version must be a supported smallint",
        )?,
        &catalogue_record,
    )?;
    let standard_library_revision = optional_identity_bytes(
        catalogue_record.column(
            row,
            "catalogue_standard_library_revision_id",
            "catalogue standard library revision identity must be null or 16 bytes",
        )?,
        &catalogue_record,
        "catalogue standard library revision identity must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    match (catalogue_hash_version, standard_library_revision) {
        (CatalogueHashVersion::Version1, None) | (CatalogueHashVersion::Version2, Some(_)) => {}
        _ => {
            return Err(catalogue_record.invariant(
                "catalogue hash version and standard library revision must form one exact context",
            ));
        }
    }

    Ok(RecoveredRevisionHeader {
        bundle,
        source,
        source_parent,
        catalogue,
        bundle_hash: Sha256Digest::from_bytes(digest_bytes(
            bundle_record.column(row, "bundle_hash", "source bundle digest must be 32 bytes")?,
            &bundle_record,
            "source bundle digest must be 32 bytes",
        )?),
        source_hash: Sha256Digest::from_bytes(digest_bytes(
            source_record.column(
                row,
                "source_hash",
                "source revision digest must be 32 bytes",
            )?,
            &source_record,
            "source revision digest must be 32 bytes",
        )?),
        catalogue_hash: Sha256Digest::from_bytes(digest_bytes(
            catalogue_record.column(
                row,
                "catalogue_hash",
                "catalogue revision digest must be 32 bytes",
            )?,
            &catalogue_record,
            "catalogue revision digest must be 32 bytes",
        )?),
        catalogue_hash_version,
        standard_library_revision,
    })
}

fn decode_catalogue_hash_version(
    value: i16,
    record: &DurableRecord,
) -> Result<CatalogueHashVersion, PostgresKernelError> {
    let value = decode_durable_version(
        value,
        record,
        "catalogue canonical hash version must be a supported smallint",
    )?;
    CatalogueHashVersion::try_from(value)
        .map_err(|_| record.invariant("catalogue canonical hash version must be 1 or 2"))
}

pub(super) fn decode_durable_version(
    value: i16,
    record: &DurableRecord,
    smallint_rule: &'static str,
) -> Result<u32, PostgresKernelError> {
    u32_from_i64(i64::from(value), record, smallint_rule)
}

async fn load_active_catalogue_hash_context(
    transaction: &Transaction<'_>,
    header: &RecoveredRevisionHeader,
) -> Result<CatalogueHashContext, PostgresKernelError> {
    let standard = match header.standard_library_revision {
        Some(revision) => Some(load_verified_standard_library(transaction, revision).await?),
        None => None,
    };
    let record = DurableRecord::new(
        "_orna_kernel.catalogue_revisions",
        header.catalogue.canonical(),
    );
    catalogue_hash_context_for(
        header.catalogue_hash_version,
        header.standard_library_revision,
        standard.as_ref(),
        &record,
    )
}

pub(super) fn catalogue_hash_context_for(
    version: CatalogueHashVersion,
    standard_revision: Option<StandardLibraryRevisionId>,
    verified_standard: Option<&VerifiedStandardLibrarySnapshot>,
    record: &DurableRecord,
) -> Result<CatalogueHashContext, PostgresKernelError> {
    match (version, standard_revision, verified_standard) {
        (CatalogueHashVersion::Version1, None, _) => Ok(CatalogueHashContext::version_one()),
        (CatalogueHashVersion::Version2, Some(revision), Some(standard))
            if standard.revision() == revision =>
        {
            Ok(CatalogueHashContext::version_two(standard.clone()))
        }
        (CatalogueHashVersion::Version1, Some(_), _) | (CatalogueHashVersion::Version2, None, _) => Err(record.invariant(
            "catalogue hash version and standard library revision must form one exact context",
        )),
        (CatalogueHashVersion::Version2, Some(_), None) => Err(record.invariant(
            "version 2 catalogue standard library revision must be recovered and verified",
        )),
        (CatalogueHashVersion::Version2, Some(_), Some(_)) => Err(record.invariant(
            "version 2 catalogue standard library revision must equal the recovered standard revision",
        )),
        _ => Err(record.invariant("catalogue hash version is unsupported")),
    }
}

pub(crate) async fn load_verified_standard_library(
    transaction: &Transaction<'_>,
    expected_revision: StandardLibraryRevisionId,
) -> Result<VerifiedStandardLibrarySnapshot, PostgresKernelError> {
    let header = load_standard_header(transaction, expected_revision).await?;
    let units = load_source_units(transaction, header.bundle).await?;
    let source = StoredSourceRevision::new(
        header.bundle,
        header.source,
        header.source_parent,
        units,
        header.bundle_hash,
        header.source_hash,
    )
    .map_err(PostgresKernelError::RevisionInvariant)?;
    let bundle_record =
        DurableRecord::new("_orna_kernel.source_bundles", header.bundle.canonical());
    let computed_bundle_hash =
        source_bundle_digest(source.units()).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_bundle_hash != header.bundle_hash {
        return Err(bundle_record.invariant(
            "standard source bundle digest must match the ordered source unit records",
        ));
    }
    let source_record =
        DurableRecord::new("_orna_kernel.source_revisions", header.source.canonical());
    let computed_source_hash =
        source_revision_digest(&source).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_source_hash != header.source_hash {
        return Err(source_record.invariant(
            "standard source revision digest must match its bundle, parent, and bundle digest",
        ));
    }

    let (catalogue, origins) = load_standard_catalogue(transaction, &header).await?;
    let snapshot = match header.digest_version {
        StandardLibraryDigestVersion::Version1 => {
            require_no_standard_executable_rows(transaction, header.revision).await?;
            StandardLibrarySnapshot::new(
                header.revision,
                header.digest_version,
                source,
                header.language_version,
                catalogue,
                origins,
                header.digest,
            )
            .map_err(PostgresKernelError::RevisionInvariant)?
        }
        StandardLibraryDigestVersion::Version2 => {
            let executables =
                load_standard_executable_facts(transaction, header.revision, &catalogue).await?;
            StandardLibrarySnapshot::new_with_executables(
                header.revision,
                header.digest_version,
                source,
                header.language_version,
                catalogue,
                executables,
                origins,
                header.digest,
            )
            .map_err(PostgresKernelError::RevisionInvariant)?
        }
        _ => {
            return Err(DurableRecord::new(
                "_orna_kernel.standard_library_revisions",
                header.revision.canonical(),
            )
            .invariant("standard library digest version is unsupported"));
        }
    };
    #[cfg(feature = "test-hooks")]
    {
        return verify_recovered_standard_snapshot_for_test_hooks(snapshot);
    }
    #[cfg(not(feature = "test-hooks"))]
    {
        verify_recovered_standard_snapshot(snapshot)
    }
}

fn verify_recovered_standard_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, PostgresKernelError> {
    let revision = snapshot.revision();
    let result = match revision {
        STANDARD_LIBRARY_REVISION_ID => verify_standard_library_snapshot(snapshot),
        STANDARD_LIBRARY_V2_REVISION_ID => verify_standard_library_v2_snapshot(snapshot),
        STANDARD_LIBRARY_V3_REVISION_ID => verify_standard_library_v3_snapshot(snapshot),
        STANDARD_LIBRARY_V4_REVISION_ID => verify_standard_library_v4_snapshot(snapshot),
        STANDARD_LIBRARY_V5_REVISION_ID => verify_standard_library_v5_snapshot(snapshot),
        STANDARD_LIBRARY_V6_REVISION_ID => verify_standard_library_v6_snapshot(snapshot),
        _ => {
            return Err(DurableRecord::new(
                "_orna_kernel.standard_library_revisions",
                revision.canonical(),
            )
            .invariant("standard library revision identity is not an accepted retained revision"));
        }
    };
    result.map_err(|error| map_recovered_standard_verifier_error(error, revision))
}

fn map_recovered_standard_verifier_error(
    error: orna_standard::StandardLibraryError,
    revision: StandardLibraryRevisionId,
) -> PostgresKernelError {
    match error {
        orna_standard::StandardLibraryError::CanonicalHash { source } => {
            PostgresKernelError::CanonicalHash(source)
        }
        orna_standard::StandardLibraryError::Revision { source } => {
            PostgresKernelError::RevisionInvariant(source)
        }
        _ => DurableRecord::new(
            "_orna_kernel.standard_library_revisions",
            revision.canonical(),
        )
        .invariant("standard library retained verifier rejected the recovered snapshot"),
    }
}

#[cfg(feature = "test-hooks")]
fn verify_recovered_standard_snapshot_for_test_hooks(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, PostgresKernelError> {
    let revision = snapshot.revision();
    if matches!(
        revision,
        STANDARD_LIBRARY_REVISION_ID
            | STANDARD_LIBRARY_V2_REVISION_ID
            | STANDARD_LIBRARY_V3_REVISION_ID
            | STANDARD_LIBRARY_V4_REVISION_ID
            | STANDARD_LIBRARY_V5_REVISION_ID
            | STANDARD_LIBRARY_V6_REVISION_ID
    ) {
        return verify_recovered_standard_snapshot(snapshot);
    }

    let result = match snapshot.digest_version() {
        StandardLibraryDigestVersion::Version1 => {
            verify_structural_standard_library_snapshot(snapshot)
        }
        StandardLibraryDigestVersion::Version2 => {
            verify_structural_standard_library_v2_snapshot(snapshot)
        }
        _ => {
            return Err(DurableRecord::new(
                "_orna_kernel.standard_library_revisions",
                revision.canonical(),
            )
            .invariant("standard library test fixture digest version is unsupported"));
        }
    };
    result.map_err(PostgresKernelError::CanonicalHash)
}

async fn load_standard_header(
    transaction: &Transaction<'_>,
    expected_revision: StandardLibraryRevisionId,
) -> Result<RecoveredStandardHeader, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_library_revisions";
    let rows = transaction
        .query(
            "SELECT
                standard.id AS standard_id,
                standard.source_revision_id AS standard_source_id,
                standard.catalogue_revision_id AS standard_catalogue_id,
                standard.digest_version AS standard_digest_version,
                standard.language_version AS standard_language_version,
                standard.content_hash AS standard_digest,
                standard.hash_algorithm AS standard_algorithm,
                source.id AS source_id,
                source.parent_source_revision_id AS source_parent_id,
                source.bundle_id AS source_bundle_id,
                source.content_hash AS source_hash,
                source.hash_algorithm AS source_algorithm,
                source.hash_contract_version AS source_contract_version,
                bundle.id AS bundle_id,
                bundle.content_hash AS bundle_hash,
                bundle.hash_algorithm AS bundle_algorithm,
                bundle.hash_contract_version AS bundle_contract_version
             FROM _orna_kernel.standard_library_revisions AS standard
             JOIN _orna_kernel.source_revisions AS source
               ON source.id = standard.source_revision_id
             JOIN _orna_kernel.source_bundles AS bundle
               ON bundle.id = source.bundle_id
             WHERE standard.id = $1",
            &[&expected_revision.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    if rows.len() != 1 {
        return Err(DurableRecord::new(RELATION, expected_revision.canonical()).invariant(
            "each version 2 catalogue pin must join exactly one standard source revision and bundle",
        ));
    }
    decode_standard_header(&rows[0], expected_revision)
}

fn decode_standard_header(
    row: &Row,
    expected_revision: StandardLibraryRevisionId,
) -> Result<RecoveredStandardHeader, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_library_revisions";
    let row_record = DurableRecord::new(RELATION, expected_revision.canonical());
    let revision = StandardLibraryRevisionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "standard_id",
            "standard library revision identity must be 16 bytes",
        )?,
        &row_record,
        "standard library revision identity must be 16 bytes",
    )?);
    if revision != expected_revision {
        return Err(row_record.invariant(
            "selected standard library revision must identify the joined standard record",
        ));
    }
    let record = DurableRecord::new(RELATION, revision.canonical());
    let standard_source = SourceRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "standard_source_id",
            "standard source revision identity must be 16 bytes",
        )?,
        &record,
        "standard source revision identity must be 16 bytes",
    )?);
    let source = SourceRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_id",
            "joined standard source identity must be 16 bytes",
        )?,
        &record,
        "joined standard source identity must be 16 bytes",
    )?);
    if standard_source != source {
        return Err(record
            .invariant("standard library source link must identify the joined source revision"));
    }
    let bundle = SourceBundleId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_bundle_id",
            "standard source bundle identity must be 16 bytes",
        )?,
        &record,
        "standard source bundle identity must be 16 bytes",
    )?);
    let joined_bundle = SourceBundleId::from_bytes(identity_bytes(
        record.column(
            row,
            "bundle_id",
            "joined standard bundle identity must be 16 bytes",
        )?,
        &record,
        "joined standard bundle identity must be 16 bytes",
    )?);
    if bundle != joined_bundle {
        return Err(
            record.invariant("standard source bundle link must identify the joined source bundle")
        );
    }
    let source_parent = optional_identity_bytes(
        record.column(
            row,
            "source_parent_id",
            "standard source parent identity must be null or 16 bytes",
        )?,
        &record,
        "standard source parent identity must be null or 16 bytes",
    )?
    .map(SourceRevisionId::from_bytes);
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "standard_catalogue_id",
            "standard catalogue revision identity must be 16 bytes",
        )?,
        &record,
        "standard catalogue revision identity must be 16 bytes",
    )?);
    let digest_version = decode_standard_library_digest_version(
        record.column(
            row,
            "standard_digest_version",
            "standard library digest version must be a supported smallint",
        )?,
        &record,
    )?;
    let language_version: String = record.column(
        row,
        "standard_language_version",
        "standard library language version must be PostgreSQL text",
    )?;
    if language_version.is_empty() {
        return Err(record.invariant("standard library language version must not be empty"));
    }
    let standard_algorithm: String = record.column(
        row,
        "standard_algorithm",
        "standard library hash algorithm must be sha256",
    )?;
    exact_enum(
        &standard_algorithm,
        &[("sha256", HashAlgorithm::Sha256)],
        &record,
        "standard library hash algorithm must be sha256",
    )?;
    let source_record = DurableRecord::new("_orna_kernel.source_revisions", source.canonical());
    let bundle_record = DurableRecord::new("_orna_kernel.source_bundles", bundle.canonical());
    require_hash_contract(
        row,
        &source_record,
        "source_algorithm",
        "source_contract_version",
        "standard source hash algorithm must be sha256",
        "standard source hash contract version must be 1",
    )?;
    require_hash_contract(
        row,
        &bundle_record,
        "bundle_algorithm",
        "bundle_contract_version",
        "standard bundle hash algorithm must be sha256",
        "standard bundle hash contract version must be 1",
    )?;

    Ok(RecoveredStandardHeader {
        revision,
        bundle,
        source,
        source_parent,
        catalogue,
        digest_version,
        language_version,
        bundle_hash: Sha256Digest::from_bytes(digest_bytes(
            bundle_record.column(
                row,
                "bundle_hash",
                "standard bundle digest must be 32 bytes",
            )?,
            &bundle_record,
            "standard bundle digest must be 32 bytes",
        )?),
        source_hash: Sha256Digest::from_bytes(digest_bytes(
            source_record.column(
                row,
                "source_hash",
                "standard source digest must be 32 bytes",
            )?,
            &source_record,
            "standard source digest must be 32 bytes",
        )?),
        digest: Sha256Digest::from_bytes(digest_bytes(
            record.column(
                row,
                "standard_digest",
                "standard library digest must be 32 bytes",
            )?,
            &record,
            "standard library digest must be 32 bytes",
        )?),
    })
}

fn decode_standard_library_digest_version(
    value: i16,
    record: &DurableRecord,
) -> Result<StandardLibraryDigestVersion, PostgresKernelError> {
    let value = decode_durable_version(
        value,
        record,
        "standard library digest version must be a supported smallint",
    )?;
    StandardLibraryDigestVersion::try_from(value)
        .map_err(|_| record.invariant("standard library digest version must be 1"))
}

async fn load_standard_catalogue(
    transaction: &Transaction<'_>,
    header: &RecoveredStandardHeader,
) -> Result<(CatalogueSnapshot, Vec<DefinitionOrigin>), PostgresKernelError> {
    let schemas = load_standard_schemas(transaction, header.revision).await?;
    let value_types = load_standard_value_types(transaction, header.revision).await?;
    let value_type_ids = value_types
        .iter()
        .map(|value_type| value_type.definition.id())
        .collect::<HashSet<_>>();
    let enum_types = load_standard_enum_types(transaction, header.revision).await?;
    let bindings = load_standard_type_bindings(transaction, header.revision).await?;
    let functions = load_standard_functions(transaction, header.revision, &value_type_ids).await?;
    let parameters =
        load_standard_parameters(transaction, header.revision, &value_type_ids).await?;

    let schema_names = schemas
        .iter()
        .map(|schema| (schema.definition.id(), schema.definition.name().clone()))
        .collect::<BTreeMap<_, _>>();
    let mut origins = Vec::with_capacity(
        schemas.len()
            + value_types.len()
            + enum_types.len()
            + bindings.len()
            + functions.len()
            + parameters.len(),
    );
    let schemas = schemas
        .into_iter()
        .map(|schema| {
            origins.push(schema.origin);
            schema.definition
        })
        .collect::<Vec<_>>();
    let mut definitions = Vec::with_capacity(value_types.len());
    for value_type in value_types {
        let record = DurableRecord::new(
            "_orna_kernel.standard_catalogue_value_types",
            value_type.definition.id().canonical(),
        );
        require_standard_definition_schema(
            &record,
            &schema_names,
            value_type.schema,
            value_type.definition.name(),
            "standard value type schema identity must identify a recovered schema",
            "standard value type qualified name must contain a schema namespace",
            "standard value type schema identity must equal the schema named by its namespace",
        )?;
        origins.push(value_type.origin);
        definitions.push(value_type.definition);
    }
    let mut enum_definitions = Vec::with_capacity(enum_types.len());
    for enum_type in enum_types {
        let record = DurableRecord::new(
            "_orna_kernel.standard_catalogue_enum_types",
            enum_type.definition.id().canonical(),
        );
        require_standard_definition_schema(
            &record,
            &schema_names,
            enum_type.schema,
            enum_type.definition.name(),
            "standard enum schema identity must identify a recovered schema",
            "standard enum qualified name must contain a schema namespace",
            "standard enum schema identity must equal the schema named by its namespace",
        )?;
        origins.push(enum_type.origin);
        enum_definitions.push(enum_type.definition);
    }
    let bindings = bindings
        .into_iter()
        .map(|binding| {
            origins.push(binding.origin);
            binding.binding
        })
        .collect::<Vec<_>>();
    let function_definitions = functions
        .into_iter()
        .map(|function| {
            let record = DurableRecord::new(
                "_orna_kernel.standard_catalogue_functions",
                function.id.canonical(),
            );
            require_standard_definition_schema(
                &record,
                &schema_names,
                function.schema,
                &function.name,
                "standard function schema identity must identify a recovered schema",
                "standard function qualified name must contain a schema namespace",
                "standard function schema identity must equal the schema named by its namespace",
            )?;
            let recovered_parameters = parameters.get(&function.id).cloned().unwrap_or_default();
            let definition = FunctionDefinition::new(
                function.id,
                function.name,
                function.domain,
                recovered_parameters
                    .iter()
                    .map(|parameter| parameter.definition.clone())
                    .collect(),
                function.return_type,
                function.current_revision,
                function.security,
                function.transaction,
                function.volatility,
            );
            origins.push(function.origin);
            origins.extend(
                recovered_parameters
                    .into_iter()
                    .map(|parameter| parameter.origin),
            );
            Ok(definition)
        })
        .collect::<Result<Vec<_>, PostgresKernelError>>()?;
    let catalogue = CatalogueSnapshot::new_with_functions_and_enum_types(
        header.catalogue,
        schemas,
        Vec::new(),
        definitions,
        enum_definitions,
        bindings,
        function_definitions,
    )
    .map_err(PostgresKernelError::CatalogueSnapshot)?;
    Ok((catalogue, origins))
}

#[allow(clippy::too_many_arguments)]
fn require_standard_definition_schema(
    record: &DurableRecord,
    schema_names: &BTreeMap<SchemaId, QualifiedSemanticName>,
    schema: SchemaId,
    name: &QualifiedSemanticName,
    missing_schema_rule: &'static str,
    missing_namespace_rule: &'static str,
    mismatch_rule: &'static str,
) -> Result<(), PostgresKernelError> {
    let schema_name = schema_names
        .get(&schema)
        .ok_or_else(|| record.invariant(missing_schema_rule))?;
    let name_parts = name.parts();
    let namespace = name_parts
        .get(..name_parts.len().saturating_sub(1))
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| record.invariant(missing_namespace_rule))?;
    if namespace != schema_name.parts() {
        return Err(record.invariant(mismatch_rule));
    }
    Ok(())
}

/// A version-one standard revision must have no row in any new executable
/// relation. The version-one digest contract covers no executable fact, so
/// stray executable rows would otherwise survive recovery unverified.
async fn require_no_standard_executable_rows(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
) -> Result<(), PostgresKernelError> {
    const EXECUTABLE_RELATIONS: &[&str] = &[
        "standard_catalogue_functions",
        "standard_catalogue_function_parameters",
        "standard_function_revisions",
        "standard_function_artifacts",
        "standard_definition_references",
    ];
    for relation in EXECUTABLE_RELATIONS {
        let rows = transaction
            .query(
                &format!(
                    "SELECT 1 FROM _orna_kernel.{relation}
                     WHERE standard_library_revision_id = $1 LIMIT 1"
                ),
                &[&standard.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if !rows.is_empty() {
            return Err(DurableRecord::new(relation, standard.canonical())
                .invariant("a version-one standard revision must have no executable rows"));
        }
    }
    Ok(())
}

async fn load_standard_functions(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
    value_type_ids: &HashSet<TypeId>,
) -> Result<Vec<RecoveredStandardFunction>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_functions";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, function_id, schema_id, name_parts,
                    domain, security_mode, transaction_mode, volatility, return_shape,
                    return_type_kind, return_scalar_type, return_value_type_id,
                    current_function_revision_id, source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_functions
             WHERE standard_library_revision_id = $1
             ORDER BY function_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut functions = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        functions.push(decode_standard_function(
            row,
            index,
            standard,
            RELATION,
            value_type_ids,
        )?);
    }
    Ok(functions)
}

fn decode_standard_function(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
    value_type_ids: &HashSet<TypeId>,
) -> Result<RecoveredStandardFunction, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "function")?;
    let id = FunctionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_id",
            "standard function identity must be 16 bytes",
        )?,
        &row_record,
        "standard function identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(
            row,
            "schema_id",
            "standard function schema identity must be 16 bytes",
        )?,
        &record,
        "standard function schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "standard function name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts).map_err(|_| {
        record.invariant("standard function name parts must form one exact semantic name")
    })?;
    let domain_name: String =
        record.column(row, "domain", "standard function domain must decode")?;
    let domain = exact_enum(
        &domain_name,
        &[
            ("server", FunctionDomain::Server),
            ("client", FunctionDomain::Client),
        ],
        &record,
        "standard function domain must be server or client",
    )?;
    let security_name: String = record.column(
        row,
        "security_mode",
        "standard function security must decode",
    )?;
    let security = exact_enum(
        &security_name,
        &[
            ("invoker", FunctionSecurity::Invoker),
            ("definer", FunctionSecurity::Definer),
        ],
        &record,
        "standard function security must be invoker or definer",
    )?;
    let transaction_name: Option<String> = record.column(
        row,
        "transaction_mode",
        "standard function transaction mode must decode",
    )?;
    let transaction = transaction_name
        .map(|name| {
            exact_enum(
                &name,
                &[
                    ("atomic", FunctionTransaction::Atomic),
                    ("read_only", FunctionTransaction::ReadOnly),
                ],
                &record,
                "standard function transaction mode must be atomic or read_only",
            )
        })
        .transpose()?;
    let volatility_name: String = record.column(
        row,
        "volatility",
        "standard function volatility must decode",
    )?;
    let volatility = exact_enum(
        &volatility_name,
        &[
            ("immutable", FunctionVolatility::Immutable),
            ("stable", FunctionVolatility::Stable),
            ("volatile", FunctionVolatility::Volatile),
        ],
        &record,
        "standard function volatility must be immutable, stable, or volatile",
    )?;
    let return_type = decode_standard_function_return(row, &record, value_type_ids)?;
    let current_revision = FunctionRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "current_function_revision_id",
            "standard function current revision identity must be 16 bytes",
        )?,
        &record,
        "standard function current revision identity must be 16 bytes",
    )?);
    let origin = decode_origin(row, &record, DefinitionIdentity::Function(id))?;
    Ok(RecoveredStandardFunction {
        schema,
        id,
        name,
        domain,
        security,
        transaction,
        volatility,
        return_type,
        current_revision,
        origin,
    })
}

fn decode_standard_function_return(
    row: &Row,
    record: &DurableRecord,
    value_type_ids: &HashSet<TypeId>,
) -> Result<FunctionReturn, PostgresKernelError> {
    let shape: String = record.column(
        row,
        "return_shape",
        "standard function return shape must decode",
    )?;
    if shape != "single" {
        return Err(record.invariant(
            "standard catalogue functions with ROWS results are not supported by standard persistence",
        ));
    }
    let kind: Option<String> = record.column(
        row,
        "return_type_kind",
        "standard function return type kind must decode",
    )?;
    let scalar: Option<String> = record.column(
        row,
        "return_scalar_type",
        "standard function return scalar type must decode",
    )?;
    let value_type: Option<Vec<u8>> = record.column(
        row,
        "return_value_type_id",
        "standard function return value type identity must be null or exact bytes",
    )?;
    let resolved = decode_standard_resolved_type(
        kind,
        scalar,
        value_type,
        Some(value_type_ids),
        true,
        record,
    )?;
    Ok(FunctionReturn::Single(resolved))
}

/// Decodes the closed scalar-or-value resolved type persisted for standard
/// catalogue functions and parameters. `value_type_ids` is required for the
/// value shape so the type must identify one standard value type.
fn decode_standard_resolved_type(
    kind: Option<String>,
    scalar_name: Option<String>,
    value_type: Option<Vec<u8>>,
    value_type_ids: Option<&HashSet<TypeId>>,
    allow_void: bool,
    record: &DurableRecord,
) -> Result<ResolvedType, PostgresKernelError> {
    match kind.as_deref() {
        Some("scalar") => {
            if value_type.is_some() {
                return Err(record.invariant(
                    "standard resolved type columns must form one exact scalar or value tuple",
                ));
            }
            let Some(scalar_name) = scalar_name else {
                return Err(record.invariant(
                    "standard resolved type columns must form one exact scalar or value tuple",
                ));
            };
            let scalar = exact_enum(
                &scalar_name,
                &[
                    ("boolean", StandardScalar::Boolean),
                    ("integer", StandardScalar::Integer),
                    ("bigint", StandardScalar::BigInt),
                    ("float", StandardScalar::Float),
                    ("decimal", StandardScalar::Decimal),
                    (
                        "character_large_object",
                        StandardScalar::CharacterLargeObject,
                    ),
                    ("binary_large_object", StandardScalar::BinaryLargeObject),
                    ("uuid", StandardScalar::Uuid),
                    ("date", StandardScalar::Date),
                    ("time", StandardScalar::Time),
                    ("timestamp", StandardScalar::Timestamp),
                    ("duration", StandardScalar::Duration),
                    ("void", StandardScalar::Void),
                ],
                record,
                "standard resolved scalar type must be one exact supported scalar",
            )?;
            if scalar == StandardScalar::Void && !allow_void {
                return Err(record.invariant(
                    "void is valid only as a SINGLE function return, never as a parameter",
                ));
            }
            Ok(ResolvedType::scalar(scalar))
        }
        Some("value") => {
            if scalar_name.is_some() {
                return Err(record.invariant(
                    "standard resolved type columns must form one exact scalar or value tuple",
                ));
            }
            let Some(bytes) = value_type else {
                return Err(record.invariant(
                    "standard resolved type columns must form one exact scalar or value tuple",
                ));
            };
            let id = TypeId::from_bytes(identity_bytes(
                bytes,
                record,
                "standard resolved value type identity must be 16 bytes",
            )?);
            if value_type_ids.is_none_or(|value_type_ids| !value_type_ids.contains(&id)) {
                return Err(record.invariant(
                    "standard resolved value type must identify one standard catalogue value type",
                ));
            }
            Ok(ResolvedType::value(id))
        }
        _ => Err(record.invariant("standard resolved type kind must be scalar or value")),
    }
}

async fn load_standard_parameters(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
    value_type_ids: &HashSet<TypeId>,
) -> Result<BTreeMap<FunctionId, Vec<RecoveredStandardParameter>>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_function_parameters";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, function_id, parameter_id, name, ordinal,
                    type_kind, scalar_type, value_type_id,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_function_parameters
             WHERE standard_library_revision_id = $1
             ORDER BY function_id, ordinal, parameter_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut parameters = BTreeMap::<FunctionId, Vec<RecoveredStandardParameter>>::new();
    for (index, row) in rows.iter().enumerate() {
        let parameter = decode_standard_parameter(row, index, standard, RELATION, value_type_ids)?;
        parameters
            .entry(parameter.function)
            .or_default()
            .push(parameter);
    }
    Ok(parameters)
}

fn decode_standard_parameter(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
    value_type_ids: &HashSet<TypeId>,
) -> Result<RecoveredStandardParameter, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "parameter")?;
    let function = FunctionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_id",
            "standard parameter owner identity must be 16 bytes",
        )?,
        &row_record,
        "standard parameter owner identity must be 16 bytes",
    )?);
    let id = ParameterId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "parameter_id",
            "standard parameter identity must be 16 bytes",
        )?,
        &row_record,
        "standard parameter identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(
        relation,
        format!(
            "function={} parameter={}",
            function.canonical(),
            id.canonical()
        ),
    );
    let name: String = record.column(
        row,
        "name",
        "standard parameter name must be PostgreSQL text",
    )?;
    if name.is_empty() {
        return Err(record.invariant("standard parameter name must not be empty"));
    }
    let ordinal = u32_from_i64(
        record.column(row, "ordinal", "standard parameter ordinal must fit u32")?,
        &record,
        "standard parameter ordinal must fit u32",
    )?;
    let kind: Option<String> =
        record.column(row, "type_kind", "standard parameter type kind must decode")?;
    let scalar: Option<String> = record.column(
        row,
        "scalar_type",
        "standard parameter scalar type must decode",
    )?;
    let value_type: Option<Vec<u8>> = record.column(
        row,
        "value_type_id",
        "standard parameter value type identity must be null or exact bytes",
    )?;
    let resolved = decode_standard_resolved_type(
        kind,
        scalar,
        value_type,
        Some(value_type_ids),
        false,
        &record,
    )?;
    let origin = decode_origin(
        row,
        &record,
        DefinitionIdentity::Parameter {
            owner: function,
            parameter: id,
        },
    )?;
    Ok(RecoveredStandardParameter {
        function,
        definition: ParameterDefinition::new(id, name, ordinal, resolved, None),
        origin,
    })
}

/// Reconstructs the complete version-2 standard executable sequence: the
/// immutable function revisions with their artifacts and the ordered
/// definition references, aligned one-to-one with the catalogue functions.
async fn load_standard_executable_facts(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<StandardExecutable>, PostgresKernelError> {
    let artifacts = load_standard_artifacts(transaction, standard).await?;
    let revisions = load_standard_revisions(transaction, standard, &artifacts).await?;
    let references = load_standard_references(transaction, standard, &revisions).await?;
    build_standard_executables(catalogue, revisions, references)
}

async fn load_standard_artifacts(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
) -> Result<BTreeMap<FunctionRevisionId, Vec<ExecutableArtifact>>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_function_artifacts";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, function_revision_id, artifact_kind,
                    format, format_version::bigint AS format_version, payload, content_hash,
                    hash_algorithm, hash_contract_version
             FROM _orna_kernel.standard_function_artifacts
             WHERE standard_library_revision_id = $1
             ORDER BY function_revision_id, artifact_kind",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut artifacts = BTreeMap::<FunctionRevisionId, Vec<ExecutableArtifact>>::new();
    for (index, row) in rows.iter().enumerate() {
        let (revision, artifact) = decode_standard_artifact(row, index, standard, RELATION)?;
        artifacts.entry(revision).or_default().push(artifact);
    }
    Ok(artifacts)
}

fn decode_standard_artifact(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
) -> Result<(FunctionRevisionId, ExecutableArtifact), PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "function artifact")?;
    let revision = FunctionRevisionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_revision_id",
            "standard artifact function revision identity must be 16 bytes",
        )?,
        &row_record,
        "standard artifact function revision identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, revision.canonical());
    require_hash_contract(
        row,
        &record,
        "hash_algorithm",
        "hash_contract_version",
        "standard function artifact hash algorithm must be sha256",
        "standard function artifact hash contract version must be 1",
    )?;
    let kind_name: String =
        record.column(row, "artifact_kind", "standard artifact kind must decode")?;
    let kind = exact_enum(
        &kind_name,
        &[
            ("server_plan", ExecutableArtifactKind::Server),
            ("client_bytecode", ExecutableArtifactKind::Client),
        ],
        &record,
        "standard artifact kind must be server_plan or client_bytecode",
    )?;
    let format: String = record.column(row, "format", "standard artifact format must be text")?;
    let version = u32_from_i64(
        record.column(
            row,
            "format_version",
            "standard artifact format version must fit u32",
        )?,
        &record,
        "standard artifact format version must fit u32",
    )?;
    let payload: Vec<u8> = record.column(
        row,
        "payload",
        "standard artifact payload must be exact bytes",
    )?;
    let content_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(
            row,
            "content_hash",
            "standard artifact content hash must be 32 bytes",
        )?,
        &record,
        "standard artifact content hash must be 32 bytes",
    )?);
    let computed = artifact_payload_digest(&payload).map_err(PostgresKernelError::CanonicalHash)?;
    if computed != content_hash {
        return Err(record.invariant("standard artifact digest must match its exact payload"));
    }
    let artifact = ExecutableArtifact::new(kind, format, version, payload, content_hash)
        .map_err(PostgresKernelError::RevisionInvariant)?;
    Ok((revision, artifact))
}

async fn load_standard_revisions(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
    artifacts: &BTreeMap<FunctionRevisionId, Vec<ExecutableArtifact>>,
) -> Result<Vec<FunctionRevisionRecord>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_function_revisions";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, function_revision_id, function_id,
                    revision_number, declaration_source_unit_id, declaration_source_start,
                    declaration_source_end, declaration_content_hash, semantic_hash,
                    semantic_hash_version, language_version, hash_contract_version
             FROM _orna_kernel.standard_function_revisions
             WHERE standard_library_revision_id = $1
             ORDER BY function_revision_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut revisions = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        revisions.push(decode_standard_revision(
            row, index, standard, RELATION, artifacts,
        )?);
    }
    Ok(revisions)
}

fn decode_standard_revision(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
    artifacts: &BTreeMap<FunctionRevisionId, Vec<ExecutableArtifact>>,
) -> Result<FunctionRevisionRecord, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "function revision")?;
    let id = FunctionRevisionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_revision_id",
            "standard function revision identity must be 16 bytes",
        )?,
        &row_record,
        "standard function revision identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, id.canonical());
    let contract_version: i16 = record.column(
        row,
        "hash_contract_version",
        "standard function revision hash contract version must be 1",
    )?;
    if contract_version != 1 {
        return Err(record.invariant("standard function revision hash contract version must be 1"));
    }
    let function = FunctionId::from_bytes(identity_bytes(
        record.column(
            row,
            "function_id",
            "standard function revision owner identity must be 16 bytes",
        )?,
        &record,
        "standard function revision owner identity must be 16 bytes",
    )?);
    let revision_number = u64_from_i64(
        record.column(
            row,
            "revision_number",
            "standard function revision number must be a positive bigint",
        )?,
        &record,
        "standard function revision number must be a positive u64",
    )?;
    if revision_number == 0 {
        return Err(record.invariant("standard function revision number must be positive"));
    }
    let declaration_origin = decode_required_source_origin_columns(row, &record)?;
    let declaration_content_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(
            row,
            "declaration_content_hash",
            "standard function declaration hash must be 32 bytes",
        )?,
        &record,
        "standard function declaration hash must be 32 bytes",
    )?);
    let semantic_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(
            row,
            "semantic_hash",
            "standard function semantic hash must be 32 bytes",
        )?,
        &record,
        "standard function semantic hash must be 32 bytes",
    )?);
    let semantic_hash_version = decode_durable_version(
        record.column(
            row,
            "semantic_hash_version",
            "standard function semantic hash version must be a supported smallint",
        )?,
        &record,
        "standard function semantic hash version must be a supported smallint",
    )?;
    let semantic_hash_version = FunctionSemanticHashVersion::try_from(semantic_hash_version)
        .map_err(|_| record.invariant("standard function semantic hash version must be 1 or 2"))?;
    let language_version: String = record.column(
        row,
        "language_version",
        "standard function language version must be PostgreSQL text",
    )?;
    let revision_artifacts = artifacts.get(&id).ok_or_else(|| {
        record.invariant("standard function revision must own exactly one artifact")
    })?;
    if revision_artifacts.len() != 1 {
        return Err(record.invariant("standard function revision must own exactly one artifact"));
    }
    FunctionRevisionRecord::new(
        function,
        id,
        revision_number,
        declaration_origin,
        declaration_content_hash,
        semantic_hash,
        language_version,
        revision_artifacts[0].clone(),
    )
    .map_err(PostgresKernelError::RevisionInvariant)
    .map(|revision| revision.with_semantic_hash_version(semantic_hash_version))
}

fn decode_required_source_origin_columns(
    row: &Row,
    record: &DurableRecord,
) -> Result<SourceOrigin, PostgresKernelError> {
    let unit = SourceUnitId::from_bytes(identity_bytes(
        record.column(
            row,
            "declaration_source_unit_id",
            "standard function declaration source unit identity must be 16 bytes",
        )?,
        record,
        "standard function declaration source unit identity must be 16 bytes",
    )?);
    let start = u32_from_i64(
        record.column(
            row,
            "declaration_source_start",
            "standard function declaration source start must fit u32",
        )?,
        record,
        "standard function declaration source start must fit u32",
    )?;
    let end = u32_from_i64(
        record.column(
            row,
            "declaration_source_end",
            "standard function declaration source end must fit u32",
        )?,
        record,
        "standard function declaration source end must fit u32",
    )?;
    SourceOrigin::new(unit, start, end).map_err(PostgresKernelError::RevisionInvariant)
}

async fn load_standard_references(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
    revisions: &[FunctionRevisionRecord],
) -> Result<Vec<DefinitionReference>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_definition_references";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, function_revision_id, ordinal,
                    target_definition_id, target_kind, target_owner_type_id,
                    target_owner_function_id, target_standard_library_revision_id,
                    reference_kind, source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_definition_references
             WHERE standard_library_revision_id = $1
             ORDER BY function_revision_id, ordinal",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut references = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        references.push(decode_standard_reference(
            row, index, standard, RELATION, revisions,
        )?);
    }
    Ok(references)
}

fn decode_standard_reference(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
    revisions: &[FunctionRevisionRecord],
) -> Result<DefinitionReference, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "definition reference")?;
    let source_revision = FunctionRevisionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_revision_id",
            "standard reference source revision identity must be 16 bytes",
        )?,
        &row_record,
        "standard reference source revision identity must be 16 bytes",
    )?);
    let ordinal = u32_from_i64(
        row_record.column(row, "ordinal", "standard reference ordinal must fit u32")?,
        &row_record,
        "standard reference ordinal must fit u32",
    )?;
    let record = DurableRecord::new(
        relation,
        format!("revision={} ordinal={ordinal}", source_revision.canonical()),
    );
    let source_function = revisions
        .iter()
        .find(|revision| revision.id() == source_revision)
        .map(FunctionRevisionRecord::function)
        .ok_or_else(|| {
            record.invariant(
                "standard reference source revision must identify one recovered function revision",
            )
        })?;
    let target_bytes = identity_bytes(
        record.column(
            row,
            "target_definition_id",
            "standard reference target identity must be 16 bytes",
        )?,
        &record,
        "standard reference target identity must be 16 bytes",
    )?;
    let owner_type = optional_identity_bytes(
        record.column(
            row,
            "target_owner_type_id",
            "standard reference target type owner must be null or 16 bytes",
        )?,
        &record,
        "standard reference target type owner must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let owner_function = optional_identity_bytes(
        record.column(
            row,
            "target_owner_function_id",
            "standard reference target function owner must be null or 16 bytes",
        )?,
        &record,
        "standard reference target function owner must be null or 16 bytes",
    )?
    .map(FunctionId::from_bytes);
    let target_standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "target_standard_library_revision_id",
            "standard reference target standard library revision identity must be null or 16 bytes",
        )?,
        &record,
        "standard reference target standard library revision identity must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    let target_kind: String = record.column(
        row,
        "target_kind",
        "standard reference target kind must decode",
    )?;
    let target = match (
        target_kind.as_str(),
        owner_type,
        owner_function,
        target_standard_library_revision,
    ) {
        ("object_type", None, None, None) => {
            DefinitionReferenceTarget::ObjectType(TypeId::from_bytes(target_bytes))
        }
        ("field", Some(owner), None, None) => DefinitionReferenceTarget::Field {
            owner,
            field: FieldId::from_bytes(target_bytes),
        },
        ("function", None, None, None) => {
            DefinitionReferenceTarget::Function(FunctionId::from_bytes(target_bytes))
        }
        ("parameter", None, Some(owner), None) => DefinitionReferenceTarget::Parameter {
            owner,
            parameter: ParameterId::from_bytes(target_bytes),
        },
        ("value_type", None, None, Some(revision)) if revision == expected_standard => {
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes(target_bytes))
        }
        ("expression", None, None, None) => {
            DefinitionReferenceTarget::Expression(ExpressionId::from_bytes(target_bytes))
        }
        _ => {
            return Err(record.invariant(
                "standard reference target kind and owner columns must form one exact owner-qualified target",
            ));
        }
    };
    let kind_name: String =
        record.column(row, "reference_kind", "standard reference kind must decode")?;
    let kind = decode_standard_reference_kind(&kind_name, &record)?;
    if !standard_reference_kind_matches_target(kind, target) {
        return Err(record
            .invariant("standard reference kind must be compatible with its exact target kind"));
    }
    let source_origin = decode_standard_reference_origin(row, &record)?;
    Ok(DefinitionReference::new(
        source_function,
        source_revision,
        ordinal,
        target,
        kind,
        source_origin,
    ))
}

fn decode_standard_reference_kind(
    name: &str,
    record: &DurableRecord,
) -> Result<DefinitionReferenceKind, PostgresKernelError> {
    exact_enum(
        name,
        STANDARD_REFERENCE_KINDS,
        record,
        "standard reference kind must be one exact supported semantic relation",
    )
}

const STANDARD_REFERENCE_KINDS: &[(&str, DefinitionReferenceKind)] = &[
    ("function_call", DefinitionReferenceKind::FunctionCall),
    ("named_type", DefinitionReferenceKind::NamedType),
    ("object_reference", DefinitionReferenceKind::ObjectReference),
    ("parameter_read", DefinitionReferenceKind::ParameterRead),
    ("query_object", DefinitionReferenceKind::QueryObject),
    ("query_field", DefinitionReferenceKind::QueryField),
    ("expression", DefinitionReferenceKind::Expression),
    ("write_object", DefinitionReferenceKind::WriteObject),
    ("write_field", DefinitionReferenceKind::WriteField),
];

const fn standard_reference_kind_matches_target(
    kind: DefinitionReferenceKind,
    target: DefinitionReferenceTarget,
) -> bool {
    matches!(
        (kind, target),
        (
            DefinitionReferenceKind::FunctionCall,
            DefinitionReferenceTarget::Function(_)
        ) | (
            DefinitionReferenceKind::NamedType
                | DefinitionReferenceKind::ObjectReference
                | DefinitionReferenceKind::QueryObject,
            DefinitionReferenceTarget::ObjectType(_)
        ) | (
            DefinitionReferenceKind::NamedType,
            DefinitionReferenceTarget::ValueType(_)
        ) | (
            DefinitionReferenceKind::ObjectReference
                | DefinitionReferenceKind::QueryObject
                | DefinitionReferenceKind::QueryField,
            DefinitionReferenceTarget::Field { .. }
        ) | (
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter { .. }
        ) | (
            DefinitionReferenceKind::Expression,
            DefinitionReferenceTarget::Expression(_)
        ) | (
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(_)
        ) | (
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Field { .. }
        )
    )
}

fn decode_standard_reference_origin(
    row: &Row,
    record: &DurableRecord,
) -> Result<SourceOrigin, PostgresKernelError> {
    let unit = SourceUnitId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_unit_id",
            "standard reference source unit identity must be 16 bytes",
        )?,
        record,
        "standard reference source unit identity must be 16 bytes",
    )?);
    let start = u32_from_i64(
        record.column(
            row,
            "source_start",
            "standard reference source start must fit u32",
        )?,
        record,
        "standard reference source start must fit u32",
    )?;
    let end = u32_from_i64(
        record.column(
            row,
            "source_end",
            "standard reference source end must fit u32",
        )?,
        record,
        "standard reference source end must fit u32",
    )?;
    SourceOrigin::new(unit, start, end).map_err(PostgresKernelError::RevisionInvariant)
}

/// Aligns the recovered immutable revisions and references one-to-one with the
/// recovered standard catalogue functions and validates the complete
/// executable sequence before the canonical digest is verified.
fn build_standard_executables(
    catalogue: &CatalogueSnapshot,
    revisions: Vec<FunctionRevisionRecord>,
    references: Vec<DefinitionReference>,
) -> Result<Vec<StandardExecutable>, PostgresKernelError> {
    let relation = "_orna_kernel.standard_function_revisions";
    for revision in &revisions {
        if catalogue.function_by_id(revision.function()).is_none() {
            return Err(
                DurableRecord::new(relation, revision.id().canonical()).invariant(
                    "standard function revision must identify one standard catalogue function",
                ),
            );
        }
    }
    let mut executables = Vec::with_capacity(catalogue.functions().len());
    let mut consumed_references = 0usize;
    for function in catalogue.functions() {
        let mut owned = revisions
            .iter()
            .filter(|revision| revision.function() == function.id());
        let revision = owned.next().ok_or_else(|| {
            DurableRecord::new(relation, function.id().canonical()).invariant(
                "standard catalogue function must own exactly one current function revision",
            )
        })?;
        if owned.next().is_some() {
            return Err(
                DurableRecord::new(relation, function.id().canonical()).invariant(
                    "standard catalogue function must own exactly one current function revision",
                ),
            );
        }
        if revision.id() != function.current_revision() {
            return Err(DurableRecord::new(relation, revision.id().canonical()).invariant(
                "standard catalogue function current revision must equal its recovered revision",
            ));
        }
        let mut owned_references = references
            .iter()
            .filter(|reference| {
                reference.source_function() == function.id()
                    && reference.source_revision() == revision.id()
            })
            .cloned()
            .collect::<Vec<_>>();
        owned_references.sort_by_key(|reference| reference.ordinal());
        for (index, reference) in owned_references.iter().enumerate() {
            let expected = u32::try_from(index).map_err(|_| {
                DurableRecord::new(relation, revision.id().canonical())
                    .invariant("standard executable reference ordinal count must fit u32")
            })?;
            if reference.ordinal() != expected {
                return Err(DurableRecord::new(
                    "_orna_kernel.standard_definition_references",
                    revision.id().canonical(),
                )
                .invariant("standard executable reference ordinals must be contiguous from zero"));
            }
        }
        consumed_references += owned_references.len();
        executables.push(
            StandardExecutable::new(function.id(), revision.clone(), owned_references)
                .map_err(PostgresKernelError::RevisionInvariant)?,
        );
    }
    if consumed_references != references.len() {
        return Err(DurableRecord::new(
            "_orna_kernel.standard_definition_references",
            "unowned".to_owned(),
        )
        .invariant(
            "standard definition reference must belong to one recovered executable revision",
        ));
    }
    if executables.is_empty() {
        return Err(DurableRecord::new(
            "_orna_kernel.standard_library_revisions",
            catalogue.revision().canonical(),
        )
        .invariant("version-two standard snapshot must carry at least one executable"));
    }
    Ok(executables)
}

async fn load_standard_schemas(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
) -> Result<Vec<RecoveredStandardSchema>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_schemas";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, schema_id, name_parts,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_schemas
             WHERE standard_library_revision_id = $1
             ORDER BY schema_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_standard_schema(row, index, standard, RELATION))
        .collect()
}

fn decode_standard_schema(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
) -> Result<RecoveredStandardSchema, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "schema")?;
    let id = SchemaId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "schema_id",
            "standard schema identity must be 16 bytes",
        )?,
        &row_record,
        "standard schema identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, id.canonical());
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "standard schema name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts).map_err(|_| {
        record.invariant("standard schema name parts must form one exact semantic name")
    })?;
    let origin = decode_origin(row, &record, DefinitionIdentity::Schema(id))?;
    Ok(RecoveredStandardSchema {
        definition: SchemaDefinition::new(id, name),
        origin,
    })
}

async fn load_standard_value_types(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
) -> Result<Vec<RecoveredStandardValueType>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_value_types";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, type_id, schema_id, name_parts,
                    value_kind, mutability, persistence, representation_contract,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_value_types
             WHERE standard_library_revision_id = $1
             ORDER BY type_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_standard_value_type(row, index, standard, RELATION))
        .collect()
}

fn decode_standard_value_type(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
) -> Result<RecoveredStandardValueType, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "value type")?;
    let id = TypeId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "type_id",
            "standard value type identity must be 16 bytes",
        )?,
        &row_record,
        "standard value type identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(
            row,
            "schema_id",
            "standard value type schema identity must be 16 bytes",
        )?,
        &record,
        "standard value type schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "standard value type name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts).map_err(|_| {
        record.invariant("standard value type name parts must form one exact semantic name")
    })?;
    let value_kind: String = record.column(
        row,
        "value_kind",
        "standard value type kind must be primitive or opaque",
    )?;
    let kind = exact_enum(
        &value_kind,
        &[
            ("primitive", ValueTypeKind::Primitive),
            ("opaque", ValueTypeKind::Opaque),
        ],
        &record,
        "standard value type kind must be primitive or opaque",
    )?;
    let mutability: String = record.column(
        row,
        "mutability",
        "standard value type mutability must be immutable",
    )?;
    exact_enum(
        &mutability,
        &[("immutable", ValueTypeMutability::Immutable)],
        &record,
        "standard value type mutability must be immutable",
    )?;
    let persistence_name: String = record.column(
        row,
        "persistence",
        "standard value type persistence must be persistable or transient",
    )?;
    let persistence = exact_enum(
        &persistence_name,
        &[
            ("persistable", ValueTypePersistence::Persistable),
            ("transient", ValueTypePersistence::Transient),
        ],
        &record,
        "standard value type persistence must be persistable or transient",
    )?;
    let representation_contract: String = record.column(
        row,
        "representation_contract",
        "standard value type representation contract must be PostgreSQL text",
    )?;
    let origin = decode_origin(row, &record, DefinitionIdentity::ValueType(id))?;
    Ok(RecoveredStandardValueType {
        schema,
        definition: recovered_standard_value_definition(
            &record,
            id,
            name,
            kind,
            persistence,
            representation_contract,
        )?,
        origin,
    })
}

fn recovered_standard_value_definition(
    record: &DurableRecord,
    id: TypeId,
    name: QualifiedSemanticName,
    kind: ValueTypeKind,
    persistence: ValueTypePersistence,
    representation_contract: String,
) -> Result<ValueTypeDefinition, PostgresKernelError> {
    if representation_contract.is_empty() {
        return Err(
            record.invariant("standard value type representation contract must not be empty")
        );
    }
    match kind {
        ValueTypeKind::Primitive => Ok(ValueTypeDefinition::primitive(
            id,
            name,
            ValueTypeMutability::Immutable,
            persistence,
            representation_contract,
        )),
        ValueTypeKind::Opaque => {
            if persistence != ValueTypePersistence::Transient {
                return Err(record.invariant("standard opaque value type must be transient"));
            }
            if representation_contract.len() > 128
                || !representation_contract
                    .bytes()
                    .all(|byte| (0x20..=0x7e).contains(&byte))
            {
                return Err(record.invariant(
                    "standard opaque value type contract must be 1 to 128 printable ASCII bytes",
                ));
            }
            Ok(ValueTypeDefinition::opaque(
                id,
                name,
                representation_contract,
            ))
        }
        _ => Err(record.invariant("standard value type kind is not recoverable")),
    }
}

async fn load_standard_enum_types(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
) -> Result<Vec<RecoveredStandardEnumType>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_enum_types";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, type_id, schema_id, name_parts, labels,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_enum_types
             WHERE standard_library_revision_id = $1
             ORDER BY type_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_standard_enum_type(row, index, standard, RELATION))
        .collect()
}

fn decode_standard_enum_type(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
) -> Result<RecoveredStandardEnumType, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "enum type")?;
    let id = TypeId::from_bytes(identity_bytes(
        row_record.column(row, "type_id", "standard enum identity must be 16 bytes")?,
        &row_record,
        "standard enum identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(
            row,
            "schema_id",
            "standard enum schema identity must be 16 bytes",
        )?,
        &record,
        "standard enum schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "standard enum name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts).map_err(|_| {
        record.invariant("standard enum name parts must form one exact semantic name")
    })?;
    let labels: Vec<String> = record.column(
        row,
        "labels",
        "standard enum labels must be one exact PostgreSQL text array",
    )?;
    let origin = decode_origin(row, &record, DefinitionIdentity::ValueType(id))?;
    Ok(RecoveredStandardEnumType {
        schema,
        definition: EnumTypeDefinition::new(id, name, labels),
        origin,
    })
}

async fn load_standard_type_bindings(
    transaction: &Transaction<'_>,
    standard: StandardLibraryRevisionId,
) -> Result<Vec<RecoveredStandardTypeBinding>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.standard_catalogue_type_bindings";
    let rows = transaction
        .query(
            "SELECT standard_library_revision_id, type_binding_id, kind, name_parts,
                    target_type_kind, target_type_id, target_enum_type_id,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.standard_catalogue_type_bindings
             WHERE standard_library_revision_id = $1
             ORDER BY type_binding_id",
            &[&standard.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_standard_type_binding(row, index, standard, RELATION))
        .collect()
}

fn decode_standard_type_binding(
    row: &Row,
    index: usize,
    expected_standard: StandardLibraryRevisionId,
    relation: &'static str,
) -> Result<RecoveredStandardTypeBinding, PostgresKernelError> {
    let row_record = DurableRecord::new(relation, format!("row={index}"));
    require_standard_library_revision(row, &row_record, expected_standard, "type binding")?;
    let id = TypeBindingId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "type_binding_id",
            "standard type binding identity must be 16 bytes",
        )?,
        &row_record,
        "standard type binding identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(relation, id.canonical());
    let kind_name: String = record.column(
        row,
        "kind",
        "standard type binding kind must be qualified or prelude",
    )?;
    let kind = exact_enum(
        &kind_name,
        &[
            ("qualified", TypeBindingKind::Qualified),
            ("prelude", TypeBindingKind::Prelude),
        ],
        &record,
        "standard type binding kind must be qualified or prelude",
    )?;
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "standard type binding name parts must be an exact PostgreSQL text array",
    )?;
    let target_kind: String = record.column(
        row,
        "target_type_kind",
        "standard type binding target kind must be value or enum",
    )?;
    let value_target = optional_identity_bytes(
        record.column(
            row,
            "target_type_id",
            "standard type binding value target must be null or 16 bytes",
        )?,
        &record,
        "standard type binding value target must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let enum_target = optional_identity_bytes(
        record.column(
            row,
            "target_enum_type_id",
            "standard type binding enum target must be null or 16 bytes",
        )?,
        &record,
        "standard type binding enum target must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let target = decode_standard_binding_target(&target_kind, value_target, enum_target, &record)?;
    let binding = match kind {
        TypeBindingKind::Qualified => {
            let name = QualifiedSemanticName::new(name_parts).map_err(|_| {
                record.invariant(
                    "qualified standard type binding name must form one exact semantic name",
                )
            })?;
            TypeBinding::qualified(name, target).map_err(|_| {
                record.invariant("qualified standard type binding name must include a schema")
            })?
        }
        TypeBindingKind::Prelude => {
            let name = PreludeTypeName::new(name_parts).map_err(|_| {
                record.invariant("prelude standard type binding name must form exact keyword words")
            })?;
            TypeBinding::prelude(name, target).map_err(|_| {
                record.invariant(
                    "prelude standard type binding name must derive one binding identity",
                )
            })?
        }
        _ => {
            return Err(record.invariant("standard type binding kind must be qualified or prelude"));
        }
    };
    if binding.id() != id {
        return Err(record.invariant(
            "standard type binding identity must equal the identity derived from its kind and name",
        ));
    }
    let origin = decode_origin(row, &record, DefinitionIdentity::TypeBinding(id))?;
    Ok(RecoveredStandardTypeBinding { binding, origin })
}

fn decode_standard_binding_target(
    kind: &str,
    value_target: Option<TypeId>,
    enum_target: Option<TypeId>,
    record: &DurableRecord,
) -> Result<TypeId, PostgresKernelError> {
    match (kind, value_target, enum_target) {
        ("value", Some(target), None) | ("enum", None, Some(target)) => Ok(target),
        _ => Err(record.invariant(
            "standard type binding target kind and identities must form one exact value or enum tuple",
        )),
    }
}

fn require_standard_library_revision(
    row: &Row,
    record: &DurableRecord,
    expected: StandardLibraryRevisionId,
    member: &'static str,
) -> Result<(), PostgresKernelError> {
    let standard = StandardLibraryRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "standard_library_revision_id",
            "standard catalogue member revision identity must be 16 bytes",
        )?,
        record,
        "standard catalogue member revision identity must be 16 bytes",
    )?);
    if standard != expected {
        return Err(record.invariant(match member {
            "schema" => "standard schema must belong to the selected standard library revision",
            "value type" => {
                "standard value type must belong to the selected standard library revision"
            }
            "enum type" => {
                "standard enum type must belong to the selected standard library revision"
            }
            "function" => {
                "standard function must belong to the selected standard library revision"
            }
            "parameter" => {
                "standard parameter must belong to the selected standard library revision"
            }
            "function revision" => {
                "standard function revision must belong to the selected standard library revision"
            }
            "function artifact" => {
                "standard function artifact must belong to the selected standard library revision"
            }
            "definition reference" => {
                "standard definition reference must belong to the selected standard library revision"
            }
            _ => "standard type binding must belong to the selected standard library revision",
        }));
    }
    Ok(())
}

fn require_hash_contract(
    row: &Row,
    record: &DurableRecord,
    algorithm_column: &'static str,
    version_column: &'static str,
    algorithm_rule: &'static str,
    version_rule: &'static str,
) -> Result<(), PostgresKernelError> {
    let algorithm: String = record.column(row, algorithm_column, algorithm_rule)?;
    exact_enum(
        &algorithm,
        &[("sha256", HashAlgorithm::Sha256)],
        record,
        algorithm_rule,
    )?;
    let version: i16 = record.column(row, version_column, version_rule)?;
    if version != 1 {
        return Err(record.invariant(version_rule));
    }
    Ok(())
}

async fn load_schemas(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
) -> Result<Vec<RecoveredSchema>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT
                catalogue_revision_id,
                schema_id,
                name_parts,
                source_unit_id,
                source_start,
                source_end
             FROM _orna_kernel.catalogue_schemas
             WHERE catalogue_revision_id = $1
             ORDER BY schema_id",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_schema(row, index, catalogue))
        .collect()
}

fn decode_schema(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
) -> Result<RecoveredSchema, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_schemas";
    let record = DurableRecord::new(RELATION, format!("row={row_index}"));
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "catalogue_revision_id",
            "schema catalogue revision identity must be 16 bytes",
        )?,
        &record,
        "schema catalogue revision identity must be 16 bytes",
    )?);
    if catalogue != expected_catalogue {
        return Err(record.invariant("schema must belong to the selected catalogue revision"));
    }

    let id = SchemaId::from_bytes(identity_bytes(
        record.column(row, "schema_id", "schema identity must be 16 bytes")?,
        &record,
        "schema identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(RELATION, id.canonical());
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "schema name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts)
        .map_err(|_| record.invariant("schema name parts must form one exact semantic name"))?;

    let source_unit: Option<Vec<u8>> = record.column(
        row,
        "source_unit_id",
        "schema source origin must contain a source unit identity",
    )?;
    let source_start: Option<i64> = record.column(
        row,
        "source_start",
        "schema source origin start must be a non-negative bigint",
    )?;
    let source_end: Option<i64> = record.column(
        row,
        "source_end",
        "schema source origin end must be a non-negative bigint",
    )?;
    let (source_unit, source_start, source_end) = match (source_unit, source_start, source_end) {
        (Some(source_unit), Some(source_start), Some(source_end)) => {
            (source_unit, source_start, source_end)
        }
        _ => {
            return Err(record.invariant(
                "schema source origin must contain source unit, start, and end values",
            ));
        }
    };
    let source_unit = SourceUnitId::from_bytes(identity_bytes(
        source_unit,
        &record,
        "schema source unit identity must be 16 bytes",
    )?);
    let source_start = u32_from_i64(
        source_start,
        &record,
        "schema source origin start must fit u32",
    )?;
    let source_end = u32_from_i64(source_end, &record, "schema source origin end must fit u32")?;
    let origin = SourceOrigin::new(source_unit, source_start, source_end)
        .map_err(PostgresKernelError::RevisionInvariant)?;

    Ok(RecoveredSchema {
        definition: SchemaDefinition::new(id, name),
        origin: DefinitionOrigin::new(DefinitionIdentity::Schema(id), origin),
    })
}

async fn load_object_types(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
) -> Result<Vec<RecoveredObjectType>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT catalogue_revision_id, type_id, schema_id, name_parts,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.catalogue_object_types
             WHERE catalogue_revision_id = $1
             ORDER BY type_id",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_object_type(row, index, catalogue))
        .collect()
}

fn decode_object_type(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
) -> Result<RecoveredObjectType, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_object_types";
    let row_record = DurableRecord::new(RELATION, format!("row={row_index}"));
    require_catalogue_identity(row, &row_record, expected_catalogue, "object type")?;
    let id = TypeId::from_bytes(identity_bytes(
        row_record.column(row, "type_id", "object type identity must be 16 bytes")?,
        &row_record,
        "object type identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(RELATION, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(row, "schema_id", "object schema identity must be 16 bytes")?,
        &record,
        "object schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "object name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts)
        .map_err(|_| record.invariant("object name parts must form one exact semantic name"))?;
    let origin = decode_origin(row, &record, DefinitionIdentity::ObjectType(id))?;

    Ok(RecoveredObjectType {
        id,
        schema,
        name,
        origin,
    })
}

async fn load_enum_types(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
) -> Result<Vec<RecoveredEnumType>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT catalogue_revision_id, type_id, schema_id, name_parts, labels,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.catalogue_enum_types
             WHERE catalogue_revision_id = $1
             ORDER BY type_id",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_enum_type(row, index, catalogue))
        .collect()
}

fn decode_enum_type(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
) -> Result<RecoveredEnumType, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_enum_types";
    let row_record = DurableRecord::new(RELATION, format!("row={row_index}"));
    require_catalogue_identity(row, &row_record, expected_catalogue, "enum type")?;
    let id = TypeId::from_bytes(identity_bytes(
        row_record.column(row, "type_id", "enum type identity must be 16 bytes")?,
        &row_record,
        "enum type identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(RELATION, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(row, "schema_id", "enum schema identity must be 16 bytes")?,
        &record,
        "enum schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "enum name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts)
        .map_err(|_| record.invariant("enum name parts must form one exact semantic name"))?;
    let labels: Vec<String> = record.column(
        row,
        "labels",
        "enum labels must be one exact PostgreSQL text array",
    )?;
    let origin = decode_origin(row, &record, DefinitionIdentity::ValueType(id))?;

    Ok(RecoveredEnumType {
        schema,
        definition: EnumTypeDefinition::new(id, name, labels),
        origin,
    })
}

async fn load_record_value_types(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
) -> Result<Vec<RecoveredRecordValueType>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT catalogue_revision_id, type_id, schema_id, name_parts,
                    value_kind, mutability, persistence,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.catalogue_record_value_types
             WHERE catalogue_revision_id = $1
             ORDER BY type_id",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_record_value_type(row, index, catalogue))
        .collect()
}

fn decode_record_value_type(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
) -> Result<RecoveredRecordValueType, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_record_value_types";
    let row_record = DurableRecord::new(RELATION, format!("row={row_index}"));
    require_catalogue_identity(row, &row_record, expected_catalogue, "record value type")?;
    let id = TypeId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "type_id",
            "record value type identity must be 16 bytes",
        )?,
        &row_record,
        "record value type identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(RELATION, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(
            row,
            "schema_id",
            "record value schema identity must be 16 bytes",
        )?,
        &record,
        "record value schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "record value name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts).map_err(|_| {
        record.invariant("record value name parts must form one exact semantic name")
    })?;
    for (column, expected, rule) in [
        ("value_kind", "record", "record value kind must be record"),
        (
            "mutability",
            "immutable",
            "record value mutability must be immutable",
        ),
        (
            "persistence",
            "persistable",
            "record value persistence must be persistable",
        ),
    ] {
        let actual: String = record.column(row, column, rule)?;
        if actual != expected {
            return Err(record.invariant(rule));
        }
    }
    let origin = decode_origin(row, &record, DefinitionIdentity::ValueType(id))?;

    Ok(RecoveredRecordValueType {
        id,
        schema,
        name,
        origin,
    })
}

async fn load_record_value_fields(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<BTreeMap<TypeId, Vec<RecoveredRecordValueField>>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                    type_kind, value_type_id, value_standard_library_revision_id,
                    enum_type_id, enum_standard_library_revision_id,
                    standard_enum_type_id, record_type_id,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.catalogue_record_value_fields
             WHERE catalogue_revision_id = $1
             ORDER BY owner_type_id, ordinal, field_id",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    let mut fields = BTreeMap::<TypeId, Vec<RecoveredRecordValueField>>::new();
    for (index, row) in rows.iter().enumerate() {
        let field = decode_record_value_field(row, index, catalogue, catalogue_hash_context)?;
        fields.entry(field.owner).or_default().push(field);
    }
    Ok(fields)
}

fn decode_record_value_field(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<RecoveredRecordValueField, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_record_value_fields";
    let row_record = DurableRecord::new(RELATION, format!("row={row_index}"));
    require_catalogue_identity(row, &row_record, expected_catalogue, "record value field")?;
    let owner = TypeId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "owner_type_id",
            "record value field owner identity must be 16 bytes",
        )?,
        &row_record,
        "record value field owner identity must be 16 bytes",
    )?);
    let id = FieldId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "field_id",
            "record value field identity must be 16 bytes",
        )?,
        &row_record,
        "record value field identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(
        RELATION,
        format!("owner={} field={}", owner.canonical(), id.canonical()),
    );
    let name: String = record.column(row, "name", "record value field name must be text")?;
    if name.is_empty() {
        return Err(record.invariant("record value field name must not be empty"));
    }
    let ordinal = u32_from_i64(
        record.column(row, "ordinal", "record value field ordinal must fit u32")?,
        &record,
        "record value field ordinal must fit u32",
    )?;
    let kind: Option<String> = record.column(
        row,
        "type_kind",
        "record value field kind must be value, enum, or record",
    )?;
    let value_type = optional_identity_bytes(
        record.column(
            row,
            "value_type_id",
            "record value field standard type identity must be null or 16 bytes",
        )?,
        &record,
        "record value field standard type identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "value_standard_library_revision_id",
            "record value field standard revision must be null or 16 bytes",
        )?,
        &record,
        "record value field standard revision must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    let enum_type = optional_identity_bytes(
        record.column(
            row,
            "enum_type_id",
            "record value field enum identity must be null or 16 bytes",
        )?,
        &record,
        "record value field enum identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let enum_standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "enum_standard_library_revision_id",
            "record value field standard enum revision must be null or 16 bytes",
        )?,
        &record,
        "record value field standard enum revision must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    let standard_enum_type = optional_identity_bytes(
        record.column(
            row,
            "standard_enum_type_id",
            "record value field standard enum identity must be null or 16 bytes",
        )?,
        &record,
        "record value field standard enum identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let record_type = optional_identity_bytes(
        record.column(
            row,
            "record_type_id",
            "record value field record identity must be null or 16 bytes",
        )?,
        &record,
        "record value field record identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let descriptor = decode_record_value_field_descriptor(
        RecordValueFieldTypeTuple {
            kind,
            value_type,
            value_standard_library_revision: standard_library_revision,
            application_enum_type: enum_type,
            enum_standard_library_revision,
            standard_enum_type,
            application_record_type: record_type,
        },
        catalogue_hash_context,
        &record,
    )?;
    let origin = decode_origin(row, &record, DefinitionIdentity::Field { owner, field: id })?;
    let definition = RecordValueFieldDefinition::try_new_descriptor(id, name, ordinal, descriptor)
        .map_err(|_| record.invariant("record value field tuple must use one flat descriptor"))?;

    Ok(RecoveredRecordValueField {
        owner,
        definition,
        origin,
    })
}

fn decode_record_value_field_descriptor(
    tuple: RecordValueFieldTypeTuple,
    catalogue_hash_context: &CatalogueHashContext,
    record: &DurableRecord,
) -> Result<TypeDescriptor, PostgresKernelError> {
    if tuple.enum_standard_library_revision.is_some() || tuple.standard_enum_type.is_some() {
        let (Some(standard_library_revision), Some(enum_type)) = (
            tuple.enum_standard_library_revision,
            tuple.standard_enum_type,
        ) else {
            return Err(record.invariant(
                "record value field type columns must form one exact pinned standard value, application enum, pinned standard enum, or application record tuple",
            ));
        };
        if tuple.kind.as_deref() != Some("enum")
            || tuple.value_type.is_some()
            || tuple.value_standard_library_revision.is_some()
            || tuple.application_enum_type.is_some()
            || tuple.application_record_type.is_some()
        {
            return Err(record.invariant(
                "record value field type columns must form one exact pinned standard value, application enum, pinned standard enum, or application record tuple",
            ));
        }
        let standard = catalogue_hash_context.standard().ok_or_else(|| {
            record.invariant(
                "record value field standard enum requires a version 2 catalogue context",
            )
        })?;
        if standard_library_revision != standard.revision() {
            return Err(record.invariant(
                "record value field standard enum revision must equal the selected catalogue pin",
            ));
        }
        if standard.catalogue().enum_type_by_id(enum_type).is_none() {
            return Err(record.invariant(
                "record value field standard enum must identify one enum in the selected pinned standard library",
            ));
        }
        return Ok(TypeDescriptor::named(enum_type));
    }

    let resolved_type = decode_resolved_type_tuple(
        ResolvedTypeTuple {
            kind: tuple.kind,
            scalar: None,
            target: None,
            value_type: tuple.value_type,
            standard_library_revision: tuple.value_standard_library_revision,
            enum_type: tuple.application_enum_type,
            record_type: tuple.application_record_type,
        },
        catalogue_hash_context,
        record,
        LegacyResolvedTypeTupleMember::Field,
    )?;
    match resolved_type {
        ResolvedType::Named(type_id) | ResolvedType::Value(type_id) => {
            Ok(TypeDescriptor::named(type_id))
        }
        ResolvedType::Scalar(_) | ResolvedType::Reference { .. } => Err(record
            .invariant("record value field tuple must decode to one named descriptor identity")),
    }
}

async fn load_fields(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<BTreeMap<TypeId, Vec<RecoveredField>>, PostgresKernelError> {
    let rows = if catalogue_hash_context.standard().is_some() {
        transaction
            .query(
                "SELECT catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                        type_kind, scalar_type, target_type_id,
                        value_type_id, value_standard_library_revision_id,
                        enum_type_id, record_type_id,
                        nullable, is_unique, default_expression_id, on_delete,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_fields
                 WHERE catalogue_revision_id = $1
                 ORDER BY owner_type_id, ordinal, field_id",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    } else {
        transaction
            .query(
                "SELECT catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                        type_kind, scalar_type, target_type_id, nullable, is_unique,
                        default_expression_id, on_delete,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_fields
                 WHERE catalogue_revision_id = $1
                 ORDER BY owner_type_id, ordinal, field_id",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    };

    let mut fields = BTreeMap::<TypeId, Vec<RecoveredField>>::new();
    for (index, row) in rows.iter().enumerate() {
        let field = decode_field(row, index, catalogue, catalogue_hash_context)?;
        fields.entry(field.owner).or_default().push(field);
    }
    Ok(fields)
}

/// One current SQL tuple member that stores a legacy resolved type.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum LegacyResolvedTypeTupleMember {
    Field,
    Parameter,
    ReturnColumn,
    SingleReturn,
    StreamReturn,
}

impl LegacyResolvedTypeTupleMember {
    pub(super) const fn tuple_rule(self) -> &'static str {
        match self {
            Self::Field => {
                "field type kind, scalar type, and target identity must form one exact supported tuple"
            }
            Self::Parameter => "parameter type columns must form one exact resolved type tuple",
            Self::ReturnColumn => {
                "return column type columns must form one exact resolved type tuple"
            }
            Self::SingleReturn => {
                "function return type columns must form one exact resolved type tuple"
            }
            Self::StreamReturn => {
                "stream item type columns must form one exact resolved type tuple"
            }
        }
    }

    const fn value_tuple_rule(self) -> &'static str {
        match self {
            Self::Field => {
                "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple"
            }
            Self::Parameter => {
                "parameter type columns must form one exact supported scalar, object, value, enum, or record tuple"
            }
            Self::ReturnColumn => {
                "return column type columns must form one exact supported scalar, object, value, enum, or record tuple"
            }
            Self::SingleReturn => {
                "function return type columns must form one exact supported scalar, object, value, enum, or record tuple"
            }
            Self::StreamReturn => {
                "stream item type columns must form one exact supported scalar, object, value, enum, or record tuple"
            }
        }
    }

    const fn scalar_rule(self) -> &'static str {
        match self {
            Self::Field => "field scalar type must be an exact standard scalar name",
            Self::Parameter | Self::ReturnColumn | Self::SingleReturn | Self::StreamReturn => {
                "resolved scalar type must be an exact standard scalar name"
            }
        }
    }

    const fn allows_void(self) -> bool {
        matches!(self, Self::Field | Self::SingleReturn)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum LegacyResolvedTypeTupleKind {
    Scalar,
    Named,
    Reference,
}

/// The stored columns that describe one version-2 resolved type.
///
/// This is the only recovery projection that combines legacy type columns with
/// a standard value identity and its standard-library revision pin.
pub(super) struct ResolvedTypeTuple {
    pub(super) kind: Option<String>,
    pub(super) scalar: Option<String>,
    pub(super) target: Option<TypeId>,
    pub(super) value_type: Option<TypeId>,
    pub(super) standard_library_revision: Option<StandardLibraryRevisionId>,
    pub(super) enum_type: Option<TypeId>,
    pub(super) record_type: Option<TypeId>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum LegacyResolvedTypeTuple {
    Scalar(StandardScalar),
    Named(TypeId),
    Reference(TypeId),
}

impl LegacyResolvedTypeTuple {
    fn into_resolved_type(self) -> ResolvedType {
        match self {
            Self::Scalar(scalar) => ResolvedType::scalar(scalar),
            Self::Named(target) => ResolvedType::named(target),
            Self::Reference(target) => ResolvedType::reference(target),
        }
    }
}

/// Decodes the current scalar, named, or reference SQL kind before tuple data.
pub(super) fn decode_legacy_resolved_type_tuple_kind(
    value: Option<&str>,
    record: &DurableRecord,
    member: LegacyResolvedTypeTupleMember,
) -> Result<LegacyResolvedTypeTupleKind, PostgresKernelError> {
    let rule = if member == LegacyResolvedTypeTupleMember::Field {
        "field type kind must be scalar, named, or reference"
    } else {
        member.tuple_rule()
    };
    let value = value.ok_or_else(|| record.invariant(rule))?;
    exact_enum(
        value,
        &[
            ("scalar", LegacyResolvedTypeTupleKind::Scalar),
            ("named", LegacyResolvedTypeTupleKind::Named),
            ("reference", LegacyResolvedTypeTupleKind::Reference),
        ],
        record,
        rule,
    )
}

/// Decodes and projects one current legacy SQL resolved-type tuple.
///
/// The later value-tuple decoder remains separate. This decoder rejects every
/// value shape until that later recovery row explicitly enables it.
pub(super) fn decode_legacy_resolved_type_tuple(
    kind: LegacyResolvedTypeTupleKind,
    scalar: Option<&str>,
    target: Option<TypeId>,
    record: &DurableRecord,
    member: LegacyResolvedTypeTupleMember,
) -> Result<ResolvedType, PostgresKernelError> {
    if kind == LegacyResolvedTypeTupleKind::Scalar
        && let Some(name) = scalar
        && target.is_none()
    {
        return decode_legacy_scalar(name, record, member)
            .map(LegacyResolvedTypeTuple::Scalar)
            .map(LegacyResolvedTypeTuple::into_resolved_type);
    }
    if kind == LegacyResolvedTypeTupleKind::Named
        && scalar.is_none()
        && let Some(target) = target
    {
        if member == LegacyResolvedTypeTupleMember::Field {
            return Err(record.invariant("named field types are not supported by active recovery"));
        }
        return Ok(LegacyResolvedTypeTuple::Named(target).into_resolved_type());
    }
    if kind == LegacyResolvedTypeTupleKind::Reference
        && scalar.is_none()
        && let Some(target) = target
    {
        return Ok(LegacyResolvedTypeTuple::Reference(target).into_resolved_type());
    }
    Err(record.invariant(member.tuple_rule()))
}

/// Decodes one complete version-2 stored resolved-type tuple.
///
/// The selected catalogue context provides the one verified standard snapshot.
/// This function does not query or verify a second standard snapshot.
pub(super) fn decode_resolved_type_tuple(
    tuple: ResolvedTypeTuple,
    catalogue_hash_context: &CatalogueHashContext,
    record: &DurableRecord,
    member: LegacyResolvedTypeTupleMember,
) -> Result<ResolvedType, PostgresKernelError> {
    let standard = catalogue_hash_context.standard().ok_or_else(|| {
        record.invariant("resolved value type tuple requires a version 2 catalogue context")
    })?;

    if tuple.kind.as_deref() == Some("enum") {
        let Some(enum_type) = tuple.enum_type else {
            return Err(record.invariant(member.value_tuple_rule()));
        };
        if tuple.scalar.is_some()
            || tuple.target.is_some()
            || tuple.value_type.is_some()
            || tuple.standard_library_revision.is_some()
            || tuple.record_type.is_some()
        {
            return Err(record.invariant(member.value_tuple_rule()));
        }
        return Ok(ResolvedType::named(enum_type));
    }

    if tuple.kind.as_deref() == Some("record") {
        let Some(record_type) = tuple.record_type else {
            return Err(record.invariant(member.value_tuple_rule()));
        };
        if tuple.scalar.is_some()
            || tuple.target.is_some()
            || tuple.value_type.is_some()
            || tuple.standard_library_revision.is_some()
            || tuple.enum_type.is_some()
        {
            return Err(record.invariant(member.value_tuple_rule()));
        }
        return Ok(ResolvedType::named(record_type));
    }

    if tuple.kind.as_deref() == Some("value") {
        let Some(value_type) = tuple.value_type else {
            return Err(record.invariant(member.value_tuple_rule()));
        };
        if tuple.scalar.is_some()
            || tuple.target.is_some()
            || tuple.enum_type.is_some()
            || tuple.record_type.is_some()
        {
            return Err(record.invariant(member.value_tuple_rule()));
        }
        if is_sealed_inspect_type_id(value_type) {
            if !matches!(
                member,
                LegacyResolvedTypeTupleMember::Parameter
                    | LegacyResolvedTypeTupleMember::SingleReturn
                    | LegacyResolvedTypeTupleMember::StreamReturn
            ) {
                return Err(record.invariant(member.value_tuple_rule()));
            }
            if tuple.standard_library_revision.is_some() {
                return Err(record.invariant(
                    "sealed Inspector value types must not retain a standard library revision",
                ));
            }
            return Ok(ResolvedType::value(value_type));
        }
        let Some(standard_library_revision) = tuple.standard_library_revision else {
            return Err(record.invariant(member.value_tuple_rule()));
        };
        if standard_library_revision != standard.revision() {
            return Err(record.invariant(
                "resolved value type standard library revision must equal the selected catalogue pin",
            ));
        }
        if standard.catalogue().value_type_by_id(value_type).is_none() {
            return Err(record.invariant(
                "resolved value type must identify one value type in the selected pinned standard library",
            ));
        }
        return Ok(ResolvedType::value(value_type));
    }

    if tuple.value_type.is_some()
        || tuple.standard_library_revision.is_some()
        || tuple.enum_type.is_some()
        || tuple.record_type.is_some()
    {
        return Err(record.invariant(member.value_tuple_rule()));
    }
    let kind = decode_legacy_resolved_type_tuple_kind(tuple.kind.as_deref(), record, member)?;
    decode_legacy_resolved_type_tuple(kind, tuple.scalar.as_deref(), tuple.target, record, member)
}

fn decode_legacy_scalar(
    name: &str,
    record: &DurableRecord,
    member: LegacyResolvedTypeTupleMember,
) -> Result<StandardScalar, PostgresKernelError> {
    let scalar = exact_enum(
        name,
        &[
            ("boolean", StandardScalar::Boolean),
            ("integer", StandardScalar::Integer),
            ("bigint", StandardScalar::BigInt),
            ("float", StandardScalar::Float),
            ("decimal", StandardScalar::Decimal),
            (
                "character_large_object",
                StandardScalar::CharacterLargeObject,
            ),
            ("binary_large_object", StandardScalar::BinaryLargeObject),
            ("uuid", StandardScalar::Uuid),
            ("date", StandardScalar::Date),
            ("time", StandardScalar::Time),
            ("timestamp", StandardScalar::Timestamp),
            ("duration", StandardScalar::Duration),
            ("void", StandardScalar::Void),
        ],
        record,
        member.scalar_rule(),
    )?;
    if scalar == StandardScalar::Void && !member.allows_void() {
        return Err(record.invariant(
            "void is valid only as a SINGLE function return, never as a parameter or ROWS column",
        ));
    }
    Ok(scalar)
}

fn decode_field(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<RecoveredField, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_fields";
    let row_record = DurableRecord::new(RELATION, format!("row={row_index}"));
    require_catalogue_identity(row, &row_record, expected_catalogue, "field")?;
    let owner = TypeId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "owner_type_id",
            "field owner identity must be 16 bytes",
        )?,
        &row_record,
        "field owner identity must be 16 bytes",
    )?);
    let id = FieldId::from_bytes(identity_bytes(
        row_record.column(row, "field_id", "field identity must be 16 bytes")?,
        &row_record,
        "field identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(
        RELATION,
        format!("owner={} field={}", owner.canonical(), id.canonical()),
    );
    let name: String = record.column(row, "name", "field name must be PostgreSQL text")?;
    if name.is_empty() {
        return Err(record.invariant("field name must not be empty"));
    }
    let ordinal = u32_from_i64(
        record.column(row, "ordinal", "field ordinal must fit u32")?,
        &record,
        "field ordinal must fit u32",
    )?;
    let resolved_type = if catalogue_hash_context.standard().is_some() {
        decode_version_two_field_type_columns(row, &record, catalogue_hash_context)?
    } else {
        decode_legacy_field_type_columns(row, &record)?
    };
    let nullable: bool = record.column(row, "nullable", "field nullability must be boolean")?;
    let unique: bool = record.column(row, "is_unique", "field uniqueness must be boolean")?;
    let default_expression = optional_identity_bytes(
        record.column(
            row,
            "default_expression_id",
            "field default expression identity must be null or 16 bytes",
        )?,
        &record,
        "field default expression identity must be null or 16 bytes",
    )?
    .map(ExpressionId::from_bytes);
    let delete_name: Option<String> = record.column(
        row,
        "on_delete",
        "field delete action must be null, restrict, set_null, or cascade",
    )?;
    let on_delete = decode_on_delete(delete_name.as_deref(), resolved_type, nullable, &record)?;
    let origin = decode_origin(row, &record, DefinitionIdentity::Field { owner, field: id })?;

    Ok(RecoveredField {
        owner,
        definition: FieldDefinition::new(
            id,
            name,
            ordinal,
            resolved_type,
            nullable,
            unique,
            default_expression,
            on_delete,
        ),
        origin,
    })
}

fn decode_legacy_field_type_columns(
    row: &Row,
    record: &DurableRecord,
) -> Result<ResolvedType, PostgresKernelError> {
    let kind_name: String = record.column(
        row,
        "type_kind",
        "field type kind must be scalar, named, or reference",
    )?;
    let kind = decode_legacy_resolved_type_tuple_kind(
        Some(&kind_name),
        record,
        LegacyResolvedTypeTupleMember::Field,
    )?;
    let scalar_name: Option<String> = record.column(
        row,
        "scalar_type",
        "field scalar type must be null or an exact standard scalar name",
    )?;
    let target = optional_identity_bytes(
        record.column(
            row,
            "target_type_id",
            "field target identity must be null or 16 bytes",
        )?,
        record,
        "field target identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    decode_legacy_resolved_type_tuple(
        kind,
        scalar_name.as_deref(),
        target,
        record,
        LegacyResolvedTypeTupleMember::Field,
    )
}

fn decode_version_two_field_type_columns(
    row: &Row,
    record: &DurableRecord,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<ResolvedType, PostgresKernelError> {
    let kind: Option<String> = record.column(
        row,
        "type_kind",
        "field type kind must be scalar, named, reference, value, or enum",
    )?;
    let scalar: Option<String> = record.column(
        row,
        "scalar_type",
        "field scalar type must be null or an exact standard scalar name",
    )?;
    let target = optional_identity_bytes(
        record.column(
            row,
            "target_type_id",
            "field target identity must be null or 16 bytes",
        )?,
        record,
        "field target identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let value_type = optional_identity_bytes(
        record.column(
            row,
            "value_type_id",
            "field value type identity must be null or 16 bytes",
        )?,
        record,
        "field value type identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "value_standard_library_revision_id",
            "field value type standard library revision identity must be null or 16 bytes",
        )?,
        record,
        "field value type standard library revision identity must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    let enum_type = optional_identity_bytes(
        record.column(
            row,
            "enum_type_id",
            "field enum type identity must be null or 16 bytes",
        )?,
        record,
        "field enum type identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let record_type = optional_identity_bytes(
        record.column(
            row,
            "record_type_id",
            "field record type identity must be null or 16 bytes",
        )?,
        record,
        "field record type identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    decode_resolved_type_tuple(
        ResolvedTypeTuple {
            kind,
            scalar,
            target,
            value_type,
            standard_library_revision,
            enum_type,
            record_type,
        },
        catalogue_hash_context,
        record,
        LegacyResolvedTypeTupleMember::Field,
    )
}

fn decode_on_delete(
    value: Option<&str>,
    resolved_type: ResolvedType,
    nullable: bool,
    record: &DurableRecord,
) -> Result<Option<OnDeleteAction>, PostgresKernelError> {
    if resolved_type.reference_target().is_none() {
        return value
            .is_none()
            .then_some(None)
            .ok_or_else(|| record.invariant("only reference fields may declare a delete action"));
    }
    let action = match value {
        None => None,
        Some("restrict") => Some(OnDeleteAction::Restrict),
        Some("set_null") => Some(OnDeleteAction::SetNull),
        Some("cascade") => Some(OnDeleteAction::Cascade),
        Some(_) => {
            return Err(record.invariant(
                "reference delete action must be null, restrict, set_null, or cascade",
            ));
        }
    };
    if action == Some(OnDeleteAction::SetNull) && !nullable {
        return Err(record.invariant("SET NULL reference fields must be nullable"));
    }
    Ok(action)
}

async fn load_expressions(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
) -> Result<Vec<RecoveredExpression>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT catalogue_revision_id, expression_id, format,
                    format_version::bigint AS format_version, payload, content_hash,
                    hash_algorithm, hash_contract_version,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.catalogue_expressions
             WHERE catalogue_revision_id = $1
             ORDER BY expression_id",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_expression(row, index, catalogue))
        .collect()
}

fn decode_expression(
    row: &Row,
    row_index: usize,
    expected_catalogue: CatalogueRevisionId,
) -> Result<RecoveredExpression, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.catalogue_expressions";
    let row_record = DurableRecord::new(RELATION, format!("row={row_index}"));
    require_catalogue_identity(row, &row_record, expected_catalogue, "expression")?;
    let id = ExpressionId::from_bytes(identity_bytes(
        row_record.column(row, "expression_id", "expression identity must be 16 bytes")?,
        &row_record,
        "expression identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(RELATION, id.canonical());
    require_hash_contract(
        row,
        &record,
        "hash_algorithm",
        "hash_contract_version",
        "expression hash algorithm must be sha256",
        "expression hash contract version must be 1",
    )?;
    let format: String =
        record.column(row, "format", "expression format must be PostgreSQL text")?;
    let version = u32_from_i64(
        record.column(
            row,
            "format_version",
            "expression format version must fit u32",
        )?,
        &record,
        "expression format version must fit u32",
    )?;
    let payload: Vec<u8> =
        record.column(row, "payload", "expression payload must be exact bytes")?;
    let content_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(row, "content_hash", "expression digest must be 32 bytes")?,
        &record,
        "expression digest must be 32 bytes",
    )?);
    let computed_hash =
        artifact_payload_digest(&payload).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_hash != content_hash {
        return Err(record.invariant("expression digest must match its exact artifact payload"));
    }
    let artifact = ExpressionArtifact::new(id, format, version, payload, content_hash)
        .map_err(PostgresKernelError::RevisionInvariant)?;
    let origin = decode_origin(row, &record, DefinitionIdentity::Expression(id))?;
    Ok(RecoveredExpression { artifact, origin })
}

fn require_catalogue_identity(
    row: &Row,
    record: &DurableRecord,
    expected: CatalogueRevisionId,
    member: &'static str,
) -> Result<(), PostgresKernelError> {
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "catalogue_revision_id",
            "catalogue member revision identity must be 16 bytes",
        )?,
        record,
        "catalogue member revision identity must be 16 bytes",
    )?);
    if catalogue != expected {
        return Err(record.invariant(match member {
            "object type" => "object type must belong to the selected catalogue revision",
            "field" => "field must belong to the selected catalogue revision",
            "expression" => "expression must belong to the selected catalogue revision",
            _ => "catalogue member must belong to the selected catalogue revision",
        }));
    }
    Ok(())
}

fn decode_origin(
    row: &Row,
    record: &DurableRecord,
    identity: DefinitionIdentity,
) -> Result<DefinitionOrigin, PostgresKernelError> {
    let unit: Option<Vec<u8>> = record.column(
        row,
        "source_unit_id",
        "definition origin must contain a source unit identity",
    )?;
    let start: Option<i64> = record.column(
        row,
        "source_start",
        "definition origin start must be a non-negative bigint",
    )?;
    let end: Option<i64> = record.column(
        row,
        "source_end",
        "definition origin end must be a non-negative bigint",
    )?;
    let (unit, start, end) = match (unit, start, end) {
        (Some(unit), Some(start), Some(end)) => (unit, start, end),
        _ => {
            return Err(record
                .invariant("definition origin must contain source unit, start, and end values"));
        }
    };
    let unit = SourceUnitId::from_bytes(identity_bytes(
        unit,
        record,
        "definition origin source unit identity must be 16 bytes",
    )?);
    let start = u32_from_i64(start, record, "definition origin start must fit u32")?;
    let end = u32_from_i64(end, record, "definition origin end must fit u32")?;
    let source =
        SourceOrigin::new(unit, start, end).map_err(PostgresKernelError::RevisionInvariant)?;
    Ok(DefinitionOrigin::new(identity, source))
}

async fn load_source_units(
    transaction: &Transaction<'_>,
    bundle: SourceBundleId,
) -> Result<Vec<StoredSourceUnit>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT
                id,
                bundle_id,
                ordinal,
                logical_path,
                content,
                content_hash,
                hash_algorithm,
                hash_contract_version,
                encoding
             FROM _orna_kernel.source_units
             WHERE bundle_id = $1
             ORDER BY ordinal",
            &[&bundle.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    rows.iter()
        .enumerate()
        .map(|(index, row)| decode_source_unit(row, index, bundle))
        .collect()
}

fn decode_source_unit(
    row: &Row,
    row_index: usize,
    expected_bundle: SourceBundleId,
) -> Result<StoredSourceUnit, PostgresKernelError> {
    let record = DurableRecord::new(SOURCE_UNIT_RELATION, format!("row={row_index}"));
    let id = SourceUnitId::from_bytes(identity_bytes(
        record.column(row, "id", "source unit identity must be 16 bytes")?,
        &record,
        "source unit identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(SOURCE_UNIT_RELATION, id.canonical());
    let bundle = SourceBundleId::from_bytes(identity_bytes(
        record.column(
            row,
            "bundle_id",
            "source unit bundle identity must be 16 bytes",
        )?,
        &record,
        "source unit bundle identity must be 16 bytes",
    )?);
    if bundle != expected_bundle {
        return Err(record.invariant("source unit must belong to the selected source bundle"));
    }

    let ordinal = u32_from_i64(
        record.column(row, "ordinal", "source unit ordinal must fit u32")?,
        &record,
        "source unit ordinal must fit u32",
    )?;
    let logical_path: String = record.column(
        row,
        "logical_path",
        "source unit logical path must be PostgreSQL text",
    )?;
    let content: String = record.column(
        row,
        "content",
        "source unit content must be exact PostgreSQL UTF-8 text",
    )?;
    let content_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(row, "content_hash", "source unit digest must be 32 bytes")?,
        &record,
        "source unit digest must be 32 bytes",
    )?);
    let algorithm: String = record.column(
        row,
        "hash_algorithm",
        "source unit hash algorithm must be sha256",
    )?;
    exact_enum(
        &algorithm,
        &[("sha256", HashAlgorithm::Sha256)],
        &record,
        "source unit hash algorithm must be sha256",
    )?;
    let contract_version: i16 = record.column(
        row,
        "hash_contract_version",
        "source unit hash contract version must be 1",
    )?;
    if contract_version != 1 {
        return Err(record.invariant("source unit hash contract version must be 1"));
    }
    let encoding: String = record.column(row, "encoding", "source unit encoding must be utf-8")?;
    exact_enum(
        &encoding,
        &[("utf-8", TextEncoding::Utf8)],
        &record,
        "source unit encoding must be utf-8",
    )?;

    let computed_hash =
        source_unit_content_digest(&content).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_hash != content_hash {
        return Err(record.invariant("source unit digest must match its exact UTF-8 content"));
    }

    StoredSourceUnit::new(id, ordinal, logical_path, content, content_hash)
        .map_err(PostgresKernelError::RevisionInvariant)
}

async fn load_catalogue_semantics(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    functions: Vec<functions::RecoveredFunction>,
    function_origins: Vec<DefinitionOrigin>,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<RecoveredCatalogueSemantics, PostgresKernelError> {
    assemble_catalogue_semantics(
        catalogue,
        load_schemas(transaction, catalogue).await?,
        load_object_types(transaction, catalogue).await?,
        load_enum_types(transaction, catalogue).await?,
        load_record_value_types(transaction, catalogue).await?,
        load_fields(transaction, catalogue, catalogue_hash_context).await?,
        load_record_value_fields(transaction, catalogue, catalogue_hash_context).await?,
        load_expressions(transaction, catalogue).await?,
        functions,
        function_origins,
    )
}

#[allow(clippy::too_many_arguments)]
fn assemble_catalogue_semantics(
    catalogue_id: CatalogueRevisionId,
    schemas: Vec<RecoveredSchema>,
    objects: Vec<RecoveredObjectType>,
    enum_types: Vec<RecoveredEnumType>,
    record_value_types: Vec<RecoveredRecordValueType>,
    mut fields: BTreeMap<TypeId, Vec<RecoveredField>>,
    mut record_value_fields: BTreeMap<TypeId, Vec<RecoveredRecordValueField>>,
    expressions: Vec<RecoveredExpression>,
    functions: Vec<functions::RecoveredFunction>,
    mut function_origins: Vec<DefinitionOrigin>,
) -> Result<RecoveredCatalogueSemantics, PostgresKernelError> {
    let schema_names = schemas
        .iter()
        .map(|schema| (schema.definition.id(), schema.definition.name().clone()))
        .collect::<BTreeMap<_, _>>();
    let mut origins = Vec::new();
    let schemas = schemas
        .into_iter()
        .map(|schema| {
            origins.push(schema.origin);
            schema.definition
        })
        .collect::<Vec<_>>();
    let mut object_definitions = Vec::with_capacity(objects.len());
    for object in objects {
        let record =
            DurableRecord::new("_orna_kernel.catalogue_object_types", object.id.canonical());
        let schema_name = schema_names.get(&object.schema).ok_or_else(|| {
            record.invariant("object stored schema identity must identify a recovered schema")
        })?;
        let object_parts = object.name.parts();
        let namespace = object_parts
            .get(..object_parts.len().saturating_sub(1))
            .filter(|parts| !parts.is_empty())
            .ok_or_else(|| {
                record.invariant("object qualified name must contain a schema namespace")
            })?;
        if namespace != schema_name.parts() {
            return Err(record.invariant(
                "object stored schema identity must equal the schema named by its namespace",
            ));
        }

        let recovered_fields = fields.remove(&object.id).unwrap_or_default();
        let mut definitions = Vec::with_capacity(recovered_fields.len());
        for field in recovered_fields {
            origins.push(field.origin);
            definitions.push(field.definition);
        }
        origins.push(object.origin);
        object_definitions.push(ObjectTypeDefinition::new(
            object.id,
            object.name,
            definitions,
        ));
    }
    if let Some((owner, _)) = fields.first_key_value() {
        return Err(DurableRecord::new(
            "_orna_kernel.catalogue_fields",
            format!("owner={}", owner.canonical()),
        )
        .invariant("every recovered field owner must be an active object type"));
    }

    let mut enum_definitions = Vec::with_capacity(enum_types.len());
    for enum_type in enum_types {
        let record = DurableRecord::new(
            "_orna_kernel.catalogue_enum_types",
            enum_type.definition.id().canonical(),
        );
        let schema_name = schema_names.get(&enum_type.schema).ok_or_else(|| {
            record.invariant("enum stored schema identity must identify a recovered schema")
        })?;
        let parts = enum_type.definition.name().parts();
        let namespace = parts
            .get(..parts.len().saturating_sub(1))
            .filter(|parts| !parts.is_empty())
            .ok_or_else(|| {
                record.invariant("enum qualified name must contain a schema namespace")
            })?;
        if namespace != schema_name.parts() {
            return Err(record.invariant(
                "enum stored schema identity must equal the schema named by its namespace",
            ));
        }
        origins.push(enum_type.origin);
        enum_definitions.push(enum_type.definition);
    }

    let mut record_value_definitions = Vec::with_capacity(record_value_types.len());
    for record_value_type in record_value_types {
        let record = DurableRecord::new(
            "_orna_kernel.catalogue_record_value_types",
            record_value_type.id.canonical(),
        );
        let schema_name = schema_names.get(&record_value_type.schema).ok_or_else(|| {
            record.invariant("record value stored schema identity must identify a recovered schema")
        })?;
        let parts = record_value_type.name.parts();
        let namespace = parts
            .get(..parts.len().saturating_sub(1))
            .filter(|parts| !parts.is_empty())
            .ok_or_else(|| {
                record.invariant("record value qualified name must contain a schema namespace")
            })?;
        if namespace != schema_name.parts() {
            return Err(record.invariant(
                "record value stored schema identity must equal the schema named by its namespace",
            ));
        }

        let recovered_fields = record_value_fields
            .remove(&record_value_type.id)
            .unwrap_or_default();
        let mut definitions = Vec::with_capacity(recovered_fields.len());
        for field in recovered_fields {
            origins.push(field.origin);
            definitions.push(field.definition);
        }
        origins.push(record_value_type.origin);
        record_value_definitions.push(RecordValueTypeDefinition::new(
            record_value_type.id,
            record_value_type.name,
            definitions,
        ));
    }
    if let Some((owner, _)) = record_value_fields.first_key_value() {
        return Err(DurableRecord::new(
            "_orna_kernel.catalogue_record_value_fields",
            format!("owner={}", owner.canonical()),
        )
        .invariant("every recovered record field owner must be an active record value type"));
    }

    let mut expression_artifacts = Vec::with_capacity(expressions.len());
    for expression in expressions {
        origins.push(expression.origin);
        expression_artifacts.push(expression.artifact);
    }
    let mut function_definitions = Vec::with_capacity(functions.len());
    for function in functions {
        let record = DurableRecord::new(
            "_orna_kernel.catalogue_functions",
            function.definition.id().canonical(),
        );
        let schema_name = schema_names.get(&function.schema).ok_or_else(|| {
            record.invariant("function stored schema identity must identify a recovered schema")
        })?;
        let parts = function.definition.name().parts();
        let namespace = parts
            .get(..parts.len().saturating_sub(1))
            .filter(|parts| !parts.is_empty())
            .ok_or_else(|| {
                record.invariant("function qualified name must contain a schema namespace")
            })?;
        if namespace != schema_name.parts() {
            return Err(record.invariant(
                "function stored schema identity must equal the schema named by its namespace",
            ));
        }
        function_definitions.push(function.definition);
    }
    origins.append(&mut function_origins);
    let catalogue = CatalogueSnapshot::new_with_functions_and_record_value_types(
        catalogue_id,
        schemas,
        object_definitions,
        Vec::new(),
        enum_definitions,
        record_value_definitions,
        Vec::new(),
        function_definitions,
    )
    .map_err(PostgresKernelError::CatalogueSnapshot)?;
    validate_field_links(&catalogue, &expression_artifacts)?;
    validate_function_links(&catalogue, &expression_artifacts)?;
    Ok(RecoveredCatalogueSemantics {
        catalogue,
        expressions: expression_artifacts,
        origins,
    })
}

fn assemble_revision(
    header: RecoveredRevisionHeader,
    units: Vec<StoredSourceUnit>,
    semantics: RecoveredCatalogueSemantics,
    function_state: RecoveredFunctionState,
    catalogue_hash_context: CatalogueHashContext,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    let bundle_record =
        DurableRecord::new("_orna_kernel.source_bundles", header.bundle.canonical());
    let source_record =
        DurableRecord::new("_orna_kernel.source_revisions", header.source.canonical());
    let computed_bundle_hash =
        source_bundle_digest(&units).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_bundle_hash != header.bundle_hash {
        return Err(bundle_record
            .invariant("source bundle digest must match the ordered source unit records"));
    }

    let source = StoredSourceRevision::new(
        header.bundle,
        header.source,
        header.source_parent,
        units,
        header.bundle_hash,
        header.source_hash,
    )
    .map_err(PostgresKernelError::RevisionInvariant)?;
    let computed_source_hash =
        source_revision_digest(&source).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_source_hash != header.source_hash {
        return Err(source_record
            .invariant("source revision digest must match its bundle, parent, and bundle digest"));
    }

    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(header.source, header.catalogue),
            source,
            semantics.catalogue,
            header.catalogue_hash,
            ActiveRevisionContent::new(
                semantics.expressions,
                function_state.active_revisions,
                semantics.origins,
                function_state.references,
            )
            .with_history(function_state.historical_revisions),
        ),
        catalogue_hash_context,
    )
    .map_err(PostgresKernelError::RevisionInvariant)?;
    let computed_catalogue_hash = catalogue_digest_with_context(
        active.catalogue_hash_context(),
        active.catalogue(),
        active.function_revisions(),
        active.expressions(),
        active.origins(),
        active.references(),
    )
    .map_err(PostgresKernelError::CanonicalHash)?;
    if computed_catalogue_hash != active.catalogue_hash() {
        let catalogue_record = DurableRecord::new(
            "_orna_kernel.catalogue_revisions",
            header.catalogue.canonical(),
        );
        return Err(catalogue_record
            .invariant("catalogue digest must match the exact recovered semantic catalogue"));
    }

    if let Some(introduction) = function_state.introductions.get(&header.catalogue)
        && (introduction.catalogue_hash != active.catalogue_hash()
            || introduction.source.id() != active.source().id())
    {
        return Err(DurableRecord::new(
            "_orna_kernel.catalogue_revisions",
            header.catalogue.canonical(),
        )
        .invariant(
            "active function introduction must join the exact validated catalogue and source hashes",
        ));
    }

    Ok(active)
}

fn validate_function_links(
    catalogue: &CatalogueSnapshot,
    expressions: &[ExpressionArtifact],
) -> Result<(), PostgresKernelError> {
    let expression_ids = expressions
        .iter()
        .map(ExpressionArtifact::id)
        .collect::<BTreeSet<_>>();
    for function in catalogue.functions() {
        for parameter in function.parameters() {
            let record = DurableRecord::new(
                "_orna_kernel.catalogue_function_parameters",
                format!(
                    "function={} parameter={}",
                    function.id().canonical(),
                    parameter.id().canonical()
                ),
            );
            validate_function_type(catalogue, parameter.resolved_type(), &record)?;
            if let Some(expression) = parameter.default_expression()
                && !expression_ids.contains(&expression)
            {
                return Err(record.invariant(
                    "every parameter default must identify a recovered expression artifact",
                ));
            }
        }
        match function.return_type() {
            orna_core::catalogue::FunctionReturn::Single(resolved_type)
            | orna_core::catalogue::FunctionReturn::Stream(resolved_type) => {
                validate_function_type(
                    catalogue,
                    *resolved_type,
                    &DurableRecord::new(
                        "_orna_kernel.catalogue_functions",
                        function.id().canonical(),
                    ),
                )?;
            }
            orna_core::catalogue::FunctionReturn::Rows(columns) => {
                for column in columns {
                    validate_function_type(
                        catalogue,
                        column.resolved_type(),
                        &DurableRecord::new(
                            "_orna_kernel.catalogue_function_return_columns",
                            format!(
                                "function={} ordinal={}",
                                function.id().canonical(),
                                column.ordinal()
                            ),
                        ),
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn validate_function_type(
    catalogue: &CatalogueSnapshot,
    resolved_type: ResolvedType,
    record: &DurableRecord,
) -> Result<(), PostgresKernelError> {
    if resolved_type.legacy_scalar().is_some() {
        return Ok(());
    }
    if let Some(target) = resolved_type.named_type() {
        if catalogue.object_type_by_id(target).is_none()
            && catalogue.enum_type_by_id(target).is_none()
            && catalogue.record_value_type_by_id(target).is_none()
        {
            return Err(record.invariant(
                "every named function type target must be an active object, enum, or record type",
            ));
        }
        return Ok(());
    }
    if let Some(target) = resolved_type.reference_target() {
        if target == SYS_INSPECT_INVOCATION_TYPE_ID {
            return Ok(());
        }
        if catalogue.object_type_by_id(target).is_none() {
            return Err(record
                .invariant("every reference function type target must be an active object type"));
        }
        return Ok(());
    }
    if resolved_type.value_type().is_some() {
        return Ok(());
    }
    Err(record.invariant("function resolved types are not supported by active recovery"))
}

fn validate_field_links(
    catalogue: &CatalogueSnapshot,
    expressions: &[ExpressionArtifact],
) -> Result<(), PostgresKernelError> {
    let expression_ids = expressions
        .iter()
        .map(ExpressionArtifact::id)
        .collect::<BTreeSet<_>>();
    for object in catalogue.object_types() {
        for field in object.fields() {
            let record = DurableRecord::new(
                "_orna_kernel.catalogue_fields",
                format!(
                    "owner={} field={}",
                    object.id().canonical(),
                    field.id().canonical()
                ),
            );
            if let Some(target) = field.resolved_type().reference_target()
                && catalogue.object_type_by_id(target).is_none()
            {
                return Err(
                    record.invariant("every reference field target must be an active object type")
                );
            }
            if let Some(expression) = field.default_expression()
                && !expression_ids.contains(&expression)
            {
                return Err(record.invariant(
                    "every field default must identify a recovered expression artifact",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use orna_core::{
        CatalogueRevisionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
        StandardLibraryRevisionId, TypeId,
        canonical_hash::{
            catalogue_digest, source_bundle_digest, source_revision_record_digest,
            source_unit_content_digest,
        },
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName,
            RecordValueFieldDefinition, SchemaDefinition, ValueTypeKind, ValueTypePersistence,
        },
        revision::{
            CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
            RevisionPair, SourceOrigin, StoredSourceUnit,
        },
        system::SYS_INSPECT_INVOCATION_TYPE_ID,
        types::{ResolvedType, StandardScalar, TypeDescriptor},
    };

    use crate::{PostgresKernelError, decode::DurableRecord};

    use super::{
        ACTIVE_RELATION, CATALOGUE_REVISION_RELATION, LegacyResolvedTypeTupleMember,
        RecordValueFieldTypeTuple, RecoveredCatalogueSemantics, RecoveredFunctionState,
        RecoveredRecordValueField, RecoveredRecordValueType, RecoveredRevisionHeader,
        RecoveredSchema, ResolvedTypeTuple, RevisionPairHistoryEntry, SOURCE_REVISION_RELATION,
        assemble_catalogue_semantics, assemble_revision, decode_catalogue_hash_version,
        decode_legacy_resolved_type_tuple, decode_legacy_resolved_type_tuple_kind,
        decode_record_value_field_descriptor, decode_resolved_type_tuple,
        decode_revision_pair_values, decode_standard_binding_target,
        recovered_standard_value_definition, validate_function_type,
        validate_revision_pair_listing, verify_recovered_standard_snapshot,
    };

    #[test]
    fn recovered_standard_verifier_dispatches_all_retained_revisions_and_rejects_crossed_identity()
    {
        let retained = [
            orna_standard::retained_standard_library_snapshot().expect("retained V1 standard"),
            orna_standard::retained_standard_library_v2_snapshot().expect("retained V2 standard"),
            orna_standard::retained_standard_library_v3_snapshot().expect("retained V3 standard"),
            orna_standard::retained_standard_library_v4_snapshot().expect("retained V4 standard"),
            orna_standard::retained_standard_library_v5_snapshot().expect("retained V5 standard"),
            orna_standard::retained_standard_library_v6_snapshot().expect("retained V6 standard"),
        ];
        for snapshot in retained {
            let revision = snapshot.revision();
            let verified = verify_recovered_standard_snapshot(snapshot)
                .expect("each retained standard revision must use its matching verifier");
            assert_eq!(verified.revision(), revision);
        }

        let v3 =
            orna_standard::retained_standard_library_v3_snapshot().expect("retained V3 standard");
        let crossed = orna_core::revision::StandardLibrarySnapshot::new_with_executables(
            orna_standard::STANDARD_LIBRARY_V2_REVISION_ID,
            v3.digest_version(),
            v3.source().clone(),
            v3.language_version(),
            v3.catalogue().clone(),
            v3.executables().to_vec(),
            v3.origins().to_vec(),
            v3.digest(),
        )
        .expect("crossed identity keeps snapshot shape valid");
        assert!(matches!(
            verify_recovered_standard_snapshot(crossed),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.standard_library_revisions",
                rule: "standard library retained verifier rejected the recovered snapshot",
                ..
            })
        ));

        let v1 = orna_standard::retained_standard_library_snapshot().expect("retained V1 standard");
        let unknown = orna_core::revision::StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes([0xee; 16]),
            v1.digest_version(),
            v1.source().clone(),
            v1.language_version(),
            v1.catalogue().clone(),
            v1.origins().to_vec(),
            v1.digest(),
        )
        .expect("unknown identity keeps snapshot shape valid");
        assert!(matches!(
            verify_recovered_standard_snapshot(unknown),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.standard_library_revisions",
                rule: "standard library revision identity is not an accepted retained revision",
                ..
            })
        ));
    }

    fn revision_pair_history_entry(
        source: u8,
        source_parent: Option<u8>,
        catalogue: u8,
        catalogue_parent: Option<u8>,
    ) -> RevisionPairHistoryEntry {
        revision_pair_history_entry_with_active(
            source,
            source_parent,
            catalogue,
            catalogue_parent,
            source,
            catalogue,
        )
    }

    fn revision_pair_history_entry_with_active(
        source: u8,
        source_parent: Option<u8>,
        catalogue: u8,
        catalogue_parent: Option<u8>,
        active_source: u8,
        active_catalogue: u8,
    ) -> RevisionPairHistoryEntry {
        let source_record = DurableRecord::new(SOURCE_REVISION_RELATION, "test");
        let catalogue_record = DurableRecord::new(CATALOGUE_REVISION_RELATION, "test");
        decode_revision_pair_values(
            vec![source; 16],
            source_parent.map(|id| vec![id; 16]),
            vec![catalogue; 16],
            catalogue_parent.map(|id| vec![id; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([active_source; 16]),
                CatalogueRevisionId::from_bytes([active_catalogue; 16]),
            ),
            &source_record,
            &catalogue_record,
        )
        .expect("valid revision pair test entry")
    }

    fn test_origin(identity: DefinitionIdentity, start: u32) -> DefinitionOrigin {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(SourceUnitId::from_bytes([0x91; 16]), start, start + 1)
                .expect("test source origin"),
        )
    }

    #[test]
    fn revision_pair_history_decoder_requires_exact_identity_shapes() {
        let source_record = DurableRecord::new("_orna_kernel.source_revisions", "row=0");
        let catalogue_record = DurableRecord::new("_orna_kernel.catalogue_revisions", "row=0");
        let active = RevisionPair::new(
            SourceRevisionId::from_bytes([2; 16]),
            CatalogueRevisionId::from_bytes([4; 16]),
        );

        let entry = decode_revision_pair_values(
            vec![2; 16],
            Some(vec![1; 16]),
            vec![4; 16],
            Some(vec![3; 16]),
            active,
            &source_record,
            &catalogue_record,
        )
        .expect("valid revision pair row");
        assert!(entry.is_active());
        assert_eq!(
            entry.source_parent_revision_id(),
            Some(SourceRevisionId::from_bytes([1; 16]))
        );
        assert_eq!(
            entry.catalogue_parent_revision_id(),
            Some(CatalogueRevisionId::from_bytes([3; 16]))
        );

        assert!(
            decode_revision_pair_values(
                vec![2; 15],
                None,
                vec![4; 16],
                None,
                active,
                &source_record,
                &catalogue_record,
            )
            .is_err()
        );
        assert!(
            decode_revision_pair_values(
                vec![2; 16],
                Some(vec![3; 17]),
                vec![4; 16],
                None,
                active,
                &source_record,
                &catalogue_record,
            )
            .is_err()
        );
        assert!(
            decode_revision_pair_values(
                vec![2; 16],
                None,
                vec![4; 16],
                Some(vec![3; 16]),
                active,
                &source_record,
                &catalogue_record,
            )
            .is_err()
        );
        assert!(
            decode_revision_pair_values(
                vec![2; 16],
                Some(vec![3; 16]),
                vec![4; 16],
                None,
                active,
                &source_record,
                &catalogue_record,
            )
            .is_err()
        );
    }

    #[test]
    fn revision_pair_listing_rejects_orphan_parent() {
        let entries = vec![revision_pair_history_entry(2, Some(1), 4, Some(3))];

        assert!(matches!(
            validate_revision_pair_listing(&entries),
            Err(PostgresKernelError::DurableInvariant {
                relation: CATALOGUE_REVISION_RELATION,
                rule: "each catalogue parent must exist and identify the corresponding source parent",
                ..
            })
        ));
    }

    #[test]
    fn revision_pair_listing_rejects_duplicate_source_identity() {
        let entries = vec![
            revision_pair_history_entry(1, None, 2, None),
            revision_pair_history_entry_with_active(1, None, 4, None, 1, 4),
        ];

        assert!(matches!(
            validate_revision_pair_listing(&entries),
            Err(PostgresKernelError::DurableInvariant {
                relation: SOURCE_REVISION_RELATION,
                rule: "source revision identities must be unique",
                ..
            })
        ));
    }

    #[test]
    fn revision_pair_listing_rejects_duplicate_catalogue_identity() {
        let entries = vec![
            revision_pair_history_entry(1, None, 2, None),
            revision_pair_history_entry_with_active(2, None, 2, None, 2, 2),
        ];

        assert!(matches!(
            validate_revision_pair_listing(&entries),
            Err(PostgresKernelError::DurableInvariant {
                relation: CATALOGUE_REVISION_RELATION,
                rule: "catalogue revision identities must be unique",
                ..
            })
        ));
    }

    #[test]
    fn revision_pair_listing_rejects_cycles() {
        let entries = vec![
            revision_pair_history_entry(2, Some(1), 4, Some(3)),
            revision_pair_history_entry(1, Some(2), 3, Some(4)),
        ];

        assert!(matches!(
            validate_revision_pair_listing(&entries),
            Err(PostgresKernelError::DurableInvariant {
                relation: CATALOGUE_REVISION_RELATION,
                rule: "catalogue and source revision ancestry must terminate without repeated identities",
                ..
            })
        ));
    }
    #[test]
    fn revision_pair_listing_rejects_mismatched_parent_source() {
        let entries = vec![
            revision_pair_history_entry(1, None, 3, None),
            revision_pair_history_entry_with_active(2, Some(9), 4, Some(3), 2, 4),
        ];

        assert!(matches!(
            validate_revision_pair_listing(&entries),
            Err(PostgresKernelError::DurableInvariant {
                relation: CATALOGUE_REVISION_RELATION,
                rule: "each catalogue parent must exist and identify the corresponding source parent",
                ..
            })
        ));
    }

    #[test]
    fn revision_pair_listing_requires_exactly_one_active_pair() {
        let entries = vec![
            revision_pair_history_entry_with_active(1, None, 2, None, 9, 9),
            revision_pair_history_entry_with_active(2, Some(1), 4, Some(2), 9, 9),
        ];

        assert!(matches!(
            validate_revision_pair_listing(&entries),
            Err(PostgresKernelError::DurableInvariant {
                relation: ACTIVE_RELATION,
                rule: "exactly one listed revision pair must match the active marker",
                ..
            })
        ));
    }

    #[test]
    fn revision_pair_listing_rejects_multiple_active_pairs() {
        let entries = vec![
            revision_pair_history_entry_with_active(1, None, 2, None, 1, 2),
            revision_pair_history_entry_with_active(2, Some(1), 4, Some(2), 2, 4),
        ];

        assert!(matches!(
            validate_revision_pair_listing(&entries),
            Err(PostgresKernelError::DurableInvariant {
                relation: ACTIVE_RELATION,
                rule: "exactly one listed revision pair must match the active marker",
                ..
            })
        ));
    }

    #[test]
    fn recovers_only_the_closed_opaque_standard_definition_shape() {
        let record = DurableRecord::new(
            "_orna_kernel.standard_catalogue_value_types",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        let id = TypeId::from_bytes([0xaa; 16]);
        let name =
            QualifiedSemanticName::new(["std", "example", "token"]).expect("opaque value name");
        let definition = recovered_standard_value_definition(
            &record,
            id,
            name.clone(),
            ValueTypeKind::Opaque,
            ValueTypePersistence::Transient,
            "std.example.token@1".to_owned(),
        )
        .expect("exact opaque definition");

        assert_eq!(definition.id(), id);
        assert_eq!(definition.name(), &name);
        assert_eq!(definition.kind(), ValueTypeKind::Opaque);
        assert_eq!(definition.persistence(), ValueTypePersistence::Transient);
        assert_eq!(definition.representation_contract(), "std.example.token@1");
        assert!(
            recovered_standard_value_definition(
                &record,
                id,
                name.clone(),
                ValueTypeKind::Opaque,
                ValueTypePersistence::Persistable,
                "std.example.token@1".to_owned(),
            )
            .is_err()
        );
        assert!(
            recovered_standard_value_definition(
                &record,
                id,
                name.clone(),
                ValueTypeKind::Opaque,
                ValueTypePersistence::Transient,
                String::new(),
            )
            .is_err()
        );
        assert!(
            recovered_standard_value_definition(
                &record,
                id,
                name.clone(),
                ValueTypeKind::Opaque,
                ValueTypePersistence::Transient,
                "x".repeat(129),
            )
            .is_err()
        );
        assert!(
            recovered_standard_value_definition(
                &record,
                id,
                name,
                ValueTypeKind::Opaque,
                ValueTypePersistence::Transient,
                "std.example.\ntoken@1".to_owned(),
            )
            .is_err()
        );
    }

    #[test]
    fn assembles_record_value_definitions_fields_and_origins() {
        let catalogue = CatalogueRevisionId::from_bytes([0x92; 16]);
        let schema_id = SchemaId::from_bytes([0x93; 16]);
        let record_id = TypeId::from_bytes([0x94; 16]);
        let first_field = FieldId::from_bytes([0x95; 16]);
        let second_field = FieldId::from_bytes([0x96; 16]);
        let enum_id = TypeId::from_bytes([0x97; 16]);
        let schema_identity = DefinitionIdentity::Schema(schema_id);
        let record_identity = DefinitionIdentity::ValueType(record_id);
        let first_identity = DefinitionIdentity::Field {
            owner: record_id,
            field: first_field,
        };
        let second_identity = DefinitionIdentity::Field {
            owner: record_id,
            field: second_field,
        };
        let assembled = assemble_catalogue_semantics(
            catalogue,
            vec![RecoveredSchema {
                definition: SchemaDefinition::new(
                    schema_id,
                    QualifiedSemanticName::new(["app"]).expect("schema name"),
                ),
                origin: test_origin(schema_identity, 0),
            }],
            Vec::new(),
            vec![super::RecoveredEnumType {
                schema: schema_id,
                definition: EnumTypeDefinition::new(
                    enum_id,
                    QualifiedSemanticName::new(["app", "stage"]).expect("enum name"),
                    ["open", "closed"],
                ),
                origin: test_origin(DefinitionIdentity::ValueType(enum_id), 1),
            }],
            vec![RecoveredRecordValueType {
                id: record_id,
                schema: schema_id,
                name: QualifiedSemanticName::new(["app", "status"]).expect("record name"),
                origin: test_origin(record_identity, 4),
            }],
            BTreeMap::new(),
            BTreeMap::from([(
                record_id,
                vec![
                    RecoveredRecordValueField {
                        owner: record_id,
                        definition: RecordValueFieldDefinition::try_new_descriptor(
                            first_field,
                            "enabled",
                            0,
                            TypeDescriptor::named(TypeId::from_bytes([0x98; 16])),
                        )
                        .expect("record field"),
                        origin: test_origin(first_identity, 2),
                    },
                    RecoveredRecordValueField {
                        owner: record_id,
                        definition: RecordValueFieldDefinition::try_new_descriptor(
                            second_field,
                            "stage",
                            1,
                            TypeDescriptor::named(enum_id),
                        )
                        .expect("record field"),
                        origin: test_origin(second_identity, 3),
                    },
                ],
            )]),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .expect("record catalogue semantics");

        let record = assembled
            .catalogue
            .record_value_type_by_id(record_id)
            .expect("recovered record");
        assert_eq!(record.fields().len(), 2);
        assert_eq!(record.fields()[0].id(), first_field);
        assert_eq!(record.fields()[1].id(), second_field);
        assert!(
            assembled
                .origins
                .iter()
                .any(|origin| origin.identity() == record_identity)
        );
        assert!(
            assembled
                .origins
                .iter()
                .any(|origin| origin.identity() == first_identity)
        );
        assert!(
            assembled
                .origins
                .iter()
                .any(|origin| origin.identity() == second_identity)
        );
    }

    #[test]
    fn catalogue_hash_version_decoder_accepts_only_durable_versions() {
        let record = DurableRecord::new("_orna_kernel.catalogue_revisions", "test");

        assert_eq!(
            decode_catalogue_hash_version(1, &record).expect("version 1"),
            CatalogueHashVersion::Version1
        );
        assert_eq!(
            decode_catalogue_hash_version(2, &record).expect("version 2"),
            CatalogueHashVersion::Version2
        );
        assert!(decode_catalogue_hash_version(3, &record).is_err());
    }

    #[test]
    fn function_links_accept_only_active_named_enums_and_object_references() {
        let enum_type = TypeId::from_bytes([0x81; 16]);
        let catalogue = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([0x82; 16]),
            vec![SchemaDefinition::new(
                orna_core::SchemaId::from_bytes([0x83; 16]),
                QualifiedSemanticName::new(["app"]).unwrap(),
            )],
            Vec::new(),
            Vec::new(),
            vec![EnumTypeDefinition::new(
                enum_type,
                QualifiedSemanticName::new(["app", "stage"]).unwrap(),
                ["lead"],
            )],
            Vec::new(),
        )
        .unwrap();
        let record = DurableRecord::new(
            "_orna_kernel.catalogue_function_return_columns",
            "enum-link",
        );

        assert!(
            validate_function_type(&catalogue, ResolvedType::named(enum_type), &record).is_ok()
        );
        assert!(
            validate_function_type(
                &catalogue,
                ResolvedType::named(TypeId::from_bytes([0x84; 16])),
                &record,
            )
            .is_err()
        );
        assert!(
            validate_function_type(&catalogue, ResolvedType::reference(enum_type), &record)
                .is_err()
        );
        assert!(
            validate_function_type(
                &catalogue,
                ResolvedType::reference(SYS_INSPECT_INVOCATION_TYPE_ID),
                &record,
            )
            .is_ok()
        );
    }

    #[test]
    fn legacy_resolved_type_tuple_decodes_a_scalar_field() {
        let record = DurableRecord::new("_orna_kernel.catalogue_fields", "test");
        let kind = decode_legacy_resolved_type_tuple_kind(
            Some("scalar"),
            &record,
            LegacyResolvedTypeTupleMember::Field,
        )
        .expect("scalar field kind");

        assert_eq!(
            decode_legacy_resolved_type_tuple(
                kind,
                Some("boolean"),
                None,
                &record,
                LegacyResolvedTypeTupleMember::Field,
            )
            .expect("scalar field tuple"),
            ResolvedType::scalar(StandardScalar::Boolean)
        );
    }

    #[test]
    fn resolved_value_tuple_uses_the_recovered_standard_identity() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot()
                .expect("retained standard snapshot"),
        )
        .expect("verified retained standard snapshot");
        let value_type = standard
            .catalogue()
            .value_types()
            .first()
            .expect("retained standard value type")
            .id();
        let context = CatalogueHashContext::version_two(standard.clone());
        let record = DurableRecord::new("_orna_kernel.catalogue_fields", "value-tuple");

        for member in [
            LegacyResolvedTypeTupleMember::Field,
            LegacyResolvedTypeTupleMember::Parameter,
            LegacyResolvedTypeTupleMember::ReturnColumn,
            LegacyResolvedTypeTupleMember::SingleReturn,
            LegacyResolvedTypeTupleMember::StreamReturn,
        ] {
            let resolved_type = decode_resolved_type_tuple(
                ResolvedTypeTuple {
                    kind: Some("value".to_owned()),
                    scalar: None,
                    target: None,
                    value_type: Some(value_type),
                    standard_library_revision: Some(standard.revision()),
                    enum_type: None,
                    record_type: None,
                },
                &context,
                &record,
                member,
            )
            .expect("value tuple");

            assert_eq!(resolved_type, ResolvedType::value(value_type));
        }
    }

    #[test]
    fn resolved_enum_tuple_uses_only_the_application_enum_identity() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot()
                .expect("retained standard snapshot"),
        )
        .expect("verified retained standard snapshot");
        let context = CatalogueHashContext::version_two(standard);
        let enum_type = TypeId::from_bytes([0xa3; 16]);
        let record = DurableRecord::new("_orna_kernel.catalogue_fields", "enum-tuple");

        assert_eq!(
            decode_resolved_type_tuple(
                ResolvedTypeTuple {
                    kind: Some("enum".to_owned()),
                    scalar: None,
                    target: None,
                    value_type: None,
                    standard_library_revision: None,
                    enum_type: Some(enum_type),
                    record_type: None,
                },
                &context,
                &record,
                LegacyResolvedTypeTupleMember::Field,
            )
            .expect("enum tuple"),
            ResolvedType::named(enum_type)
        );

        assert!(matches!(
            decode_resolved_type_tuple(
                ResolvedTypeTuple {
                    kind: Some("enum".to_owned()),
                    scalar: None,
                    target: Some(enum_type),
                    value_type: None,
                    standard_library_revision: None,
                    enum_type: Some(enum_type),
                    record_type: None,
                },
                &context,
                &record,
                LegacyResolvedTypeTupleMember::Field,
            ),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.catalogue_fields",
                record: failed_record,
                rule: "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple",
            }) if failed_record == "enum-tuple"
        ));
    }

    #[test]
    fn standard_binding_target_tuple_is_exactly_value_or_enum() {
        let record = DurableRecord::new(
            "_orna_kernel.standard_catalogue_type_bindings",
            "binding-target",
        );
        let value = TypeId::from_bytes([0xb1; 16]);
        let enum_type = TypeId::from_bytes([0xb2; 16]);

        assert_eq!(
            decode_standard_binding_target("value", Some(value), None, &record)
                .expect("value binding target"),
            value,
        );
        assert_eq!(
            decode_standard_binding_target("enum", None, Some(enum_type), &record)
                .expect("enum binding target"),
            enum_type,
        );
        for (kind, value_target, enum_target) in [
            ("value", None, None),
            ("value", None, Some(enum_type)),
            ("value", Some(value), Some(enum_type)),
            ("enum", None, None),
            ("enum", Some(value), None),
            ("enum", Some(value), Some(enum_type)),
            ("unknown", Some(value), None),
        ] {
            assert!(matches!(
                decode_standard_binding_target(kind, value_target, enum_target, &record),
                Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.standard_catalogue_type_bindings",
                    record: failed_record,
                    rule: "standard type binding target kind and identities must form one exact value or enum tuple",
                }) if failed_record == "binding-target"
            ));
        }
    }

    #[test]
    fn standard_enum_record_tuple_checks_shape_pin_then_membership() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot()
                .expect("retained standard snapshot"),
        )
        .expect("verified retained standard snapshot");
        let context = CatalogueHashContext::version_two(standard.clone());
        let enum_type = TypeId::from_bytes([0xb3; 16]);
        let record = DurableRecord::new(
            "_orna_kernel.catalogue_record_value_fields",
            "standard-enum-tuple",
        );

        let malformed = decode_record_value_field_descriptor(
            RecordValueFieldTypeTuple {
                kind: Some("enum".to_owned()),
                value_type: None,
                value_standard_library_revision: None,
                application_enum_type: Some(enum_type),
                enum_standard_library_revision: Some(standard.revision()),
                standard_enum_type: Some(enum_type),
                application_record_type: None,
            },
            &context,
            &record,
        );
        assert!(matches!(
            malformed,
            Err(PostgresKernelError::DurableInvariant {
                rule: "record value field type columns must form one exact pinned standard value, application enum, pinned standard enum, or application record tuple",
                ..
            })
        ));

        for (revision, type_id) in [(Some(standard.revision()), None), (None, Some(enum_type))] {
            let partial = decode_record_value_field_descriptor(
                RecordValueFieldTypeTuple {
                    kind: Some("enum".to_owned()),
                    value_type: None,
                    value_standard_library_revision: None,
                    application_enum_type: None,
                    enum_standard_library_revision: revision,
                    standard_enum_type: type_id,
                    application_record_type: None,
                },
                &context,
                &record,
            );
            assert!(matches!(
                partial,
                Err(PostgresKernelError::DurableInvariant {
                    rule: "record value field type columns must form one exact pinned standard value, application enum, pinned standard enum, or application record tuple",
                    ..
                })
            ));
        }

        let wrong_pin = decode_record_value_field_descriptor(
            RecordValueFieldTypeTuple {
                kind: Some("enum".to_owned()),
                value_type: None,
                value_standard_library_revision: None,
                application_enum_type: None,
                enum_standard_library_revision: Some(StandardLibraryRevisionId::from_bytes(
                    [0xb4; 16],
                )),
                standard_enum_type: Some(enum_type),
                application_record_type: None,
            },
            &context,
            &record,
        );
        assert!(matches!(
            wrong_pin,
            Err(PostgresKernelError::DurableInvariant {
                rule: "record value field standard enum revision must equal the selected catalogue pin",
                ..
            })
        ));

        assert!(standard.catalogue().enum_type_by_id(enum_type).is_none());
        let missing = decode_record_value_field_descriptor(
            RecordValueFieldTypeTuple {
                kind: Some("enum".to_owned()),
                value_type: None,
                value_standard_library_revision: None,
                application_enum_type: None,
                enum_standard_library_revision: Some(standard.revision()),
                standard_enum_type: Some(enum_type),
                application_record_type: None,
            },
            &context,
            &record,
        );
        assert!(matches!(
            missing,
            Err(PostgresKernelError::DurableInvariant {
                rule: "record value field standard enum must identify one enum in the selected pinned standard library",
                ..
            })
        ));
    }

    #[test]
    fn record_value_field_descriptor_rejects_partial_and_contaminated_record_tuples_exactly() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot()
                .expect("retained standard snapshot"),
        )
        .expect("verified retained standard snapshot");
        let context = CatalogueHashContext::version_two(standard);
        let record_type = TypeId::from_bytes([0xc1; 16]);
        let contamination = TypeId::from_bytes([0xc2; 16]);
        let record =
            DurableRecord::new("_orna_kernel.catalogue_record_value_fields", "record-tuple");
        let generic_rule = "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple";
        let widened_rule = "record value field type columns must form one exact pinned standard value, application enum, pinned standard enum, or application record tuple";

        assert_eq!(
            decode_record_value_field_descriptor(
                RecordValueFieldTypeTuple {
                    kind: Some("record".to_owned()),
                    value_type: None,
                    value_standard_library_revision: None,
                    application_enum_type: None,
                    enum_standard_library_revision: None,
                    standard_enum_type: None,
                    application_record_type: Some(record_type),
                },
                &context,
                &record,
            )
            .expect("exact record tuple must decode"),
            TypeDescriptor::named(record_type),
        );

        for (kind, record_target) in [
            (Some("record".to_owned()), None),
            (None, Some(record_type)),
            (Some("value".to_owned()), Some(record_type)),
            (Some("enum".to_owned()), Some(record_type)),
        ] {
            let decoded = decode_record_value_field_descriptor(
                RecordValueFieldTypeTuple {
                    kind,
                    value_type: None,
                    value_standard_library_revision: None,
                    application_enum_type: None,
                    enum_standard_library_revision: None,
                    standard_enum_type: None,
                    application_record_type: record_target,
                },
                &context,
                &record,
            );
            match decoded {
                Err(PostgresKernelError::DurableInvariant {
                    relation,
                    record,
                    rule,
                }) => assert_eq!(
                    (relation, record, rule),
                    (
                        "_orna_kernel.catalogue_record_value_fields",
                        "record-tuple".to_owned(),
                        generic_rule,
                    ),
                    "unexpected partial record tuple error",
                ),
                other => panic!("unexpected partial record tuple result: {other:?}"),
            }
        }

        for (value_type, value_standard, app_enum) in [
            (Some(contamination), None, None),
            (
                None,
                Some(StandardLibraryRevisionId::from_bytes([0xc3; 16])),
                None,
            ),
            (None, None, Some(contamination)),
        ] {
            let decoded = decode_record_value_field_descriptor(
                RecordValueFieldTypeTuple {
                    kind: Some("record".to_owned()),
                    value_type,
                    value_standard_library_revision: value_standard,
                    application_enum_type: app_enum,
                    enum_standard_library_revision: None,
                    standard_enum_type: None,
                    application_record_type: Some(record_type),
                },
                &context,
                &record,
            );
            match decoded {
                Err(PostgresKernelError::DurableInvariant {
                    relation,
                    record,
                    rule,
                }) => assert_eq!(
                    (relation, record, rule),
                    (
                        "_orna_kernel.catalogue_record_value_fields",
                        "record-tuple".to_owned(),
                        generic_rule,
                    ),
                    "unexpected contaminated record tuple error",
                ),
                other => panic!("unexpected contaminated record tuple result: {other:?}"),
            }
        }

        for (enum_standard, std_enum) in [
            (
                Some(StandardLibraryRevisionId::from_bytes([0xc4; 16])),
                None,
            ),
            (None, Some(contamination)),
            (
                Some(StandardLibraryRevisionId::from_bytes([0xc4; 16])),
                Some(contamination),
            ),
        ] {
            let decoded = decode_record_value_field_descriptor(
                RecordValueFieldTypeTuple {
                    kind: Some("record".to_owned()),
                    value_type: None,
                    value_standard_library_revision: None,
                    application_enum_type: None,
                    enum_standard_library_revision: enum_standard,
                    standard_enum_type: std_enum,
                    application_record_type: Some(record_type),
                },
                &context,
                &record,
            );
            match decoded {
                Err(PostgresKernelError::DurableInvariant {
                    relation,
                    record,
                    rule,
                }) => assert_eq!(
                    (relation, record, rule),
                    (
                        "_orna_kernel.catalogue_record_value_fields",
                        "record-tuple".to_owned(),
                        widened_rule,
                    ),
                    "unexpected standard-enum provenance record tuple error",
                ),
                other => {
                    panic!("unexpected standard-enum provenance record tuple result: {other:?}")
                }
            }
        }
    }

    #[test]
    fn resolved_record_tuple_uses_only_the_application_record_identity() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot()
                .expect("retained standard snapshot"),
        )
        .expect("verified retained standard snapshot");
        let context = CatalogueHashContext::version_two(standard);
        let record_type = TypeId::from_bytes([0xa4; 16]);
        let record = DurableRecord::new("_orna_kernel.catalogue_fields", "record-tuple");

        assert_eq!(
            decode_resolved_type_tuple(
                ResolvedTypeTuple {
                    kind: Some("record".to_owned()),
                    scalar: None,
                    target: None,
                    value_type: None,
                    standard_library_revision: None,
                    enum_type: None,
                    record_type: Some(record_type),
                },
                &context,
                &record,
                LegacyResolvedTypeTupleMember::Field,
            )
            .expect("record tuple"),
            ResolvedType::named(record_type),
        );
        assert!(
            decode_resolved_type_tuple(
                ResolvedTypeTuple {
                    kind: Some("record".to_owned()),
                    scalar: None,
                    target: Some(record_type),
                    value_type: None,
                    standard_library_revision: None,
                    enum_type: None,
                    record_type: Some(record_type),
                },
                &context,
                &record,
                LegacyResolvedTypeTupleMember::Field,
            )
            .is_err()
        );
    }

    #[test]
    fn resolved_value_tuple_checks_shape_then_pin_then_pinned_membership() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot()
                .expect("retained standard snapshot"),
        )
        .expect("verified retained standard snapshot");
        let value_type = standard
            .catalogue()
            .value_types()
            .first()
            .expect("retained standard value type")
            .id();
        let context = CatalogueHashContext::version_two(standard.clone());
        let record = DurableRecord::new("_orna_kernel.catalogue_fields", "value-tuple-order");

        let malformed = decode_resolved_type_tuple(
            ResolvedTypeTuple {
                kind: Some("value".to_owned()),
                scalar: Some("boolean".to_owned()),
                target: None,
                value_type: Some(value_type),
                standard_library_revision: None,
                enum_type: None,
                record_type: None,
            },
            &context,
            &record,
            LegacyResolvedTypeTupleMember::Field,
        );
        assert!(matches!(
            malformed,
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.catalogue_fields",
                record: failed_record,
                rule: "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple",
            }) if failed_record == "value-tuple-order"
        ));

        let wrong_pin = StandardLibraryRevisionId::from_bytes([0xa4; 16]);
        assert_ne!(wrong_pin, standard.revision());
        let mismatched_pin = decode_resolved_type_tuple(
            ResolvedTypeTuple {
                kind: Some("value".to_owned()),
                scalar: None,
                target: None,
                value_type: Some(value_type),
                standard_library_revision: Some(wrong_pin),
                enum_type: None,
                record_type: None,
            },
            &context,
            &record,
            LegacyResolvedTypeTupleMember::Field,
        );
        assert!(matches!(
            mismatched_pin,
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.catalogue_fields",
                record: failed_record,
                rule: "resolved value type standard library revision must equal the selected catalogue pin",
            }) if failed_record == "value-tuple-order"
        ));

        let missing_value_type = TypeId::from_bytes([0xa5; 16]);
        assert!(
            standard
                .catalogue()
                .value_type_by_id(missing_value_type)
                .is_none()
        );
        let missing_definition = decode_resolved_type_tuple(
            ResolvedTypeTuple {
                kind: Some("value".to_owned()),
                scalar: None,
                target: None,
                value_type: Some(missing_value_type),
                standard_library_revision: Some(standard.revision()),
                enum_type: None,
                record_type: None,
            },
            &context,
            &record,
            LegacyResolvedTypeTupleMember::Field,
        );
        assert!(matches!(
            missing_definition,
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.catalogue_fields",
                record: failed_record,
                rule: "resolved value type must identify one value type in the selected pinned standard library",
            }) if failed_record == "value-tuple-order"
        ));
    }

    #[test]
    fn version_two_legacy_resolved_type_tuples_keep_current_shapes() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot()
                .expect("retained standard snapshot"),
        )
        .expect("verified retained standard snapshot");
        let context = CatalogueHashContext::version_two(standard);
        let record = DurableRecord::new("_orna_kernel.catalogue_fields", "legacy-v2-tuple");
        let scalars = [
            ("boolean", StandardScalar::Boolean),
            ("integer", StandardScalar::Integer),
            ("bigint", StandardScalar::BigInt),
            ("float", StandardScalar::Float),
            ("decimal", StandardScalar::Decimal),
            (
                "character_large_object",
                StandardScalar::CharacterLargeObject,
            ),
            ("binary_large_object", StandardScalar::BinaryLargeObject),
            ("uuid", StandardScalar::Uuid),
            ("date", StandardScalar::Date),
            ("time", StandardScalar::Time),
            ("timestamp", StandardScalar::Timestamp),
            ("duration", StandardScalar::Duration),
            ("void", StandardScalar::Void),
        ];

        for (scalar, expected) in scalars {
            assert_eq!(
                decode_resolved_type_tuple(
                    ResolvedTypeTuple {
                        kind: Some("scalar".to_owned()),
                        scalar: Some(scalar.to_owned()),
                        target: None,
                        value_type: None,
                        standard_library_revision: None,
                        enum_type: None,
                        record_type: None,
                    },
                    &context,
                    &record,
                    LegacyResolvedTypeTupleMember::Field,
                )
                .expect("transitional scalar tuple"),
                ResolvedType::scalar(expected)
            );
        }

        let target = TypeId::from_bytes([0xa6; 16]);
        assert_eq!(
            decode_resolved_type_tuple(
                ResolvedTypeTuple {
                    kind: Some("named".to_owned()),
                    scalar: None,
                    target: Some(target),
                    value_type: None,
                    standard_library_revision: None,
                    enum_type: None,
                    record_type: None,
                },
                &context,
                &record,
                LegacyResolvedTypeTupleMember::Parameter,
            )
            .expect("transitional named tuple"),
            ResolvedType::named(target)
        );
        assert_eq!(
            decode_resolved_type_tuple(
                ResolvedTypeTuple {
                    kind: Some("reference".to_owned()),
                    scalar: None,
                    target: Some(target),
                    value_type: None,
                    standard_library_revision: None,
                    enum_type: None,
                    record_type: None,
                },
                &context,
                &record,
                LegacyResolvedTypeTupleMember::Field,
            )
            .expect("transitional reference tuple"),
            ResolvedType::reference(target)
        );
    }

    #[test]
    fn legacy_resolved_type_tuple_matrix_preserves_current_shapes_and_errors() {
        let record = DurableRecord::new("_orna_kernel.catalogue_fields", "tuple");
        let target = TypeId::from_bytes([0x91; 16]);
        let scalars = [
            ("boolean", StandardScalar::Boolean),
            ("integer", StandardScalar::Integer),
            ("bigint", StandardScalar::BigInt),
            ("float", StandardScalar::Float),
            ("decimal", StandardScalar::Decimal),
            (
                "character_large_object",
                StandardScalar::CharacterLargeObject,
            ),
            ("binary_large_object", StandardScalar::BinaryLargeObject),
            ("uuid", StandardScalar::Uuid),
            ("date", StandardScalar::Date),
            ("time", StandardScalar::Time),
            ("timestamp", StandardScalar::Timestamp),
            ("duration", StandardScalar::Duration),
            ("void", StandardScalar::Void),
        ];

        for member in [
            LegacyResolvedTypeTupleMember::Field,
            LegacyResolvedTypeTupleMember::Parameter,
            LegacyResolvedTypeTupleMember::ReturnColumn,
            LegacyResolvedTypeTupleMember::SingleReturn,
            LegacyResolvedTypeTupleMember::StreamReturn,
        ] {
            let scalar_kind =
                decode_legacy_resolved_type_tuple_kind(Some("scalar"), &record, member)
                    .expect("scalar kind");
            for (name, scalar) in scalars {
                let decoded = decode_legacy_resolved_type_tuple(
                    scalar_kind,
                    Some(name),
                    None,
                    &record,
                    member,
                );
                if scalar == StandardScalar::Void
                    && member != LegacyResolvedTypeTupleMember::Field
                    && member != LegacyResolvedTypeTupleMember::SingleReturn
                {
                    assert!(matches!(
                        decoded,
                        Err(PostgresKernelError::DurableInvariant {
                            relation: "_orna_kernel.catalogue_fields",
                            record: failed_record,
                            rule: "void is valid only as a SINGLE function return, never as a parameter or ROWS column",
                        }) if failed_record == "tuple"
                    ));
                } else {
                    assert_eq!(
                        decoded.expect("current scalar tuple"),
                        ResolvedType::scalar(scalar)
                    );
                }
            }

            let named_kind = decode_legacy_resolved_type_tuple_kind(Some("named"), &record, member)
                .expect("named kind");
            let named =
                decode_legacy_resolved_type_tuple(named_kind, None, Some(target), &record, member);
            if member == LegacyResolvedTypeTupleMember::Field {
                assert!(matches!(
                    named,
                    Err(PostgresKernelError::DurableInvariant {
                        relation: "_orna_kernel.catalogue_fields",
                        record: failed_record,
                        rule: "named field types are not supported by active recovery",
                    }) if failed_record == "tuple"
                ));
            } else {
                assert_eq!(
                    named.expect("current named tuple"),
                    ResolvedType::named(target)
                );
            }

            let reference_kind =
                decode_legacy_resolved_type_tuple_kind(Some("reference"), &record, member)
                    .expect("reference kind");
            assert_eq!(
                decode_legacy_resolved_type_tuple(
                    reference_kind,
                    None,
                    Some(target),
                    &record,
                    member,
                )
                .expect("current reference tuple"),
                ResolvedType::reference(target)
            );
        }

        let parameter_scalar = decode_legacy_resolved_type_tuple_kind(
            Some("scalar"),
            &record,
            LegacyResolvedTypeTupleMember::Parameter,
        )
        .expect("parameter scalar kind");
        for (scalar, target) in [(None, None), (Some("boolean"), Some(target))] {
            assert!(matches!(
                decode_legacy_resolved_type_tuple(
                    parameter_scalar,
                    scalar,
                    target,
                    &record,
                    LegacyResolvedTypeTupleMember::Parameter,
                ),
                Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.catalogue_fields",
                    record: failed_record,
                    rule: "parameter type columns must form one exact resolved type tuple",
                }) if failed_record == "tuple"
            ));
        }
        assert!(matches!(
            decode_legacy_resolved_type_tuple_kind(
                None,
                &record,
                LegacyResolvedTypeTupleMember::ReturnColumn,
            ),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.catalogue_fields",
                record: failed_record,
                rule: "return column type columns must form one exact resolved type tuple",
            }) if failed_record == "tuple"
        ));

        for kind_name in ["named", "reference"] {
            let kind = decode_legacy_resolved_type_tuple_kind(
                Some(kind_name),
                &record,
                LegacyResolvedTypeTupleMember::Parameter,
            )
            .expect("current parameter kind");
            for (scalar, target) in [(None, None), (Some("boolean"), Some(target))] {
                assert!(matches!(
                    decode_legacy_resolved_type_tuple(
                        kind,
                        scalar,
                        target,
                        &record,
                        LegacyResolvedTypeTupleMember::Parameter,
                    ),
                    Err(PostgresKernelError::DurableInvariant {
                        relation: "_orna_kernel.catalogue_fields",
                        record: failed_record,
                        rule: "parameter type columns must form one exact resolved type tuple",
                    }) if failed_record == "tuple"
                ));
            }
        }
        assert!(matches!(
            decode_legacy_resolved_type_tuple(
                parameter_scalar,
                Some("BOOLEAN"),
                None,
                &record,
                LegacyResolvedTypeTupleMember::Parameter,
            ),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.catalogue_fields",
                record: failed_record,
                rule: "resolved scalar type must be an exact standard scalar name",
            }) if failed_record == "tuple"
        ));
        assert!(matches!(
            decode_legacy_resolved_type_tuple_kind(
                Some("value"),
                &record,
                LegacyResolvedTypeTupleMember::Field,
            ),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.catalogue_fields",
                record: failed_record,
                rule: "field type kind must be scalar, named, or reference",
            }) if failed_record == "tuple"
        ));
    }

    #[test]
    fn assembles_the_exact_empty_semantic_revision() {
        let bundle = SourceBundleId::from_bytes([1; 16]);
        let source = SourceRevisionId::from_bytes([2; 16]);
        let catalogue = CatalogueRevisionId::from_bytes([3; 16]);
        let bundle_hash = source_bundle_digest(&[]).expect("empty source bundle hash");
        let source_hash = source_revision_record_digest(bundle, None, bundle_hash)
            .expect("empty source revision hash");
        let empty_catalogue =
            CatalogueSnapshot::new(catalogue, Vec::new(), Vec::new()).expect("empty catalogue");
        let catalogue_hash =
            catalogue_digest(&empty_catalogue, &[], &[], &[], &[]).expect("empty catalogue hash");

        let recovered = assemble_revision(
            RecoveredRevisionHeader {
                bundle,
                source,
                source_parent: None,
                catalogue,
                bundle_hash,
                source_hash,
                catalogue_hash,
                catalogue_hash_version: CatalogueHashVersion::Version1,
                standard_library_revision: None,
            },
            Vec::new(),
            RecoveredCatalogueSemantics {
                catalogue: empty_catalogue,
                expressions: Vec::new(),
                origins: Vec::new(),
            },
            RecoveredFunctionState::empty(),
            orna_core::revision::CatalogueHashContext::version_one(),
        )
        .expect("exact empty revision");

        assert_eq!(recovered.pair().source(), source);
        assert_eq!(recovered.pair().catalogue(), catalogue);
        assert!(recovered.source().units().is_empty());
        assert!(recovered.catalogue().schemas().is_empty());
        assert!(recovered.catalogue().object_types().is_empty());
        assert!(recovered.catalogue().functions().is_empty());
        assert!(recovered.function_revisions().is_empty());
        assert!(recovered.historical_function_revisions().is_empty());
    }

    #[test]
    fn rejects_an_empty_catalogue_with_a_different_digest() {
        let bundle = SourceBundleId::from_bytes([4; 16]);
        let source = SourceRevisionId::from_bytes([5; 16]);
        let catalogue = CatalogueRevisionId::from_bytes([6; 16]);
        let bundle_hash = source_bundle_digest(&[]).expect("empty source bundle hash");
        let source_hash = source_revision_record_digest(bundle, None, bundle_hash)
            .expect("empty source revision hash");
        let empty_catalogue =
            CatalogueSnapshot::new(catalogue, Vec::new(), Vec::new()).expect("empty catalogue");

        assert!(
            assemble_revision(
                RecoveredRevisionHeader {
                    bundle,
                    source,
                    source_parent: None,
                    catalogue,
                    bundle_hash,
                    source_hash,
                    catalogue_hash: bundle_hash,
                    catalogue_hash_version: CatalogueHashVersion::Version1,
                    standard_library_revision: None,
                },
                Vec::new(),
                RecoveredCatalogueSemantics {
                    catalogue: empty_catalogue,
                    expressions: Vec::new(),
                    origins: Vec::new(),
                },
                RecoveredFunctionState::empty(),
                orna_core::revision::CatalogueHashContext::version_one(),
            )
            .is_err()
        );
    }

    #[test]
    fn assembles_an_empty_semantic_revision_with_exact_source_content() {
        let bundle = SourceBundleId::from_bytes([7; 16]);
        let source = SourceRevisionId::from_bytes([8; 16]);
        let catalogue = CatalogueRevisionId::from_bytes([9; 16]);
        let content = "schema app";
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([10; 16]),
            0,
            "schema.orna",
            content,
            source_unit_content_digest(content).expect("source content hash"),
        )
        .expect("stored source unit");
        let units = vec![unit];
        let bundle_hash = source_bundle_digest(&units).expect("source bundle hash");
        let source_hash =
            source_revision_record_digest(bundle, None, bundle_hash).expect("source revision hash");
        let empty_catalogue =
            CatalogueSnapshot::new(catalogue, Vec::new(), Vec::new()).expect("empty catalogue");
        let catalogue_hash =
            catalogue_digest(&empty_catalogue, &[], &[], &[], &[]).expect("empty catalogue hash");

        let recovered = assemble_revision(
            RecoveredRevisionHeader {
                bundle,
                source,
                source_parent: None,
                catalogue,
                bundle_hash,
                source_hash,
                catalogue_hash,
                catalogue_hash_version: CatalogueHashVersion::Version1,
                standard_library_revision: None,
            },
            units,
            RecoveredCatalogueSemantics {
                catalogue: empty_catalogue,
                expressions: Vec::new(),
                origins: Vec::new(),
            },
            RecoveredFunctionState::empty(),
            orna_core::revision::CatalogueHashContext::version_one(),
        )
        .expect("empty semantic revision with source");

        assert_eq!(recovered.source().units().len(), 1);
        assert_eq!(recovered.source().units()[0].content(), content);
    }
}
