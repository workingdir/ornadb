use std::collections::{BTreeMap, BTreeSet, HashSet};

mod functions;

use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId,
    SourceUnitId, StandardLibraryRevisionId, TypeBindingId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest_with_context, source_bundle_digest,
        source_revision_digest, source_unit_content_digest, verify_standard_library_snapshot,
    },
    catalogue::{
        CatalogueSnapshot, FieldDefinition, ObjectTypeDefinition, OnDeleteAction, PreludeTypeName,
        QualifiedSemanticName, SchemaDefinition, TypeBinding, TypeBindingKind, ValueTypeDefinition,
        ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
        ExpressionArtifact, RevisionPair, Sha256Digest, SourceOrigin, StandardLibraryDigestVersion,
        StandardLibrarySnapshot, StoredSourceRevision, StoredSourceUnit,
        VerifiedStandardLibrarySnapshot,
    },
    types::{ResolvedType, StandardScalar},
};
use tokio_postgres::{Client, IsolationLevel, Row, Transaction};

use crate::{
    PostgresKernel, PostgresKernelError,
    bootstrap::require_current_migrations,
    decode::{
        DurableRecord, digest_bytes, exact_enum, identity_bytes, optional_identity_bytes,
        u32_from_i64,
    },
    physical::{establish_trusted_search_path, verify_physical_catalogue},
};

use self::functions::{RecoveredFunctionState, load_function_state};

const ACTIVE_RELATION: &str = "_orna_kernel.active_revision";
const SOURCE_UNIT_RELATION: &str = "_orna_kernel.source_units";

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

struct RecoveredStandardTypeBinding {
    binding: TypeBinding,
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
    /// Reconstructs and validates the complete active durable database revision.
    ///
    /// This recovery slice supports schemas, object types, fields, expression
    /// artifacts, compiler-deployable functions, immutable function history,
    /// and active definition references. It fails closed on any semantic,
    /// source, hash-chain, or physical-layout state it cannot prove complete.
    pub async fn recover(&self) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
        let mut session = self.open().await?;
        let recovery_result = recover_client(&mut session.client).await;
        let shutdown_result = session.shutdown().await;

        match (recovery_result, shutdown_result) {
            (Ok(active), Ok(())) => Ok(active),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
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
    let units = load_source_units(transaction, header.bundle).await?;
    let mut function_state = load_function_state(
        transaction,
        header.catalogue,
        &active_ancestry,
        &catalogue_hash_context,
    )
    .await?;
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

async fn load_verified_standard_library(
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
    let snapshot = StandardLibrarySnapshot::new(
        header.revision,
        header.digest_version,
        source,
        header.language_version,
        catalogue,
        origins,
        header.digest,
    )
    .map_err(PostgresKernelError::RevisionInvariant)?;
    verify_standard_library_snapshot(snapshot).map_err(PostgresKernelError::CanonicalHash)
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
    let bindings = load_standard_type_bindings(transaction, header.revision).await?;

    let schema_names = schemas
        .iter()
        .map(|schema| (schema.definition.id(), schema.definition.name().clone()))
        .collect::<BTreeMap<_, _>>();
    let mut origins = Vec::with_capacity(schemas.len() + value_types.len() + bindings.len());
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
        let schema_name = schema_names.get(&value_type.schema).ok_or_else(|| {
            record.invariant("standard value type schema identity must identify a recovered schema")
        })?;
        let name_parts = value_type.definition.name().parts();
        let namespace = name_parts
            .get(..name_parts.len().saturating_sub(1))
            .filter(|parts| !parts.is_empty())
            .ok_or_else(|| {
                record
                    .invariant("standard value type qualified name must contain a schema namespace")
            })?;
        if namespace != schema_name.parts() {
            return Err(record.invariant(
                "standard value type schema identity must equal the schema named by its namespace",
            ));
        }
        origins.push(value_type.origin);
        definitions.push(value_type.definition);
    }
    let bindings = bindings
        .into_iter()
        .map(|binding| {
            origins.push(binding.origin);
            binding.binding
        })
        .collect::<Vec<_>>();
    let catalogue = CatalogueSnapshot::new_with_types(
        header.catalogue,
        schemas,
        Vec::new(),
        definitions,
        bindings,
    )
    .map_err(PostgresKernelError::CatalogueSnapshot)?;
    Ok((catalogue, origins))
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
        "standard value type kind must be primitive",
    )?;
    exact_enum(
        &value_kind,
        &[("primitive", ())],
        &record,
        "standard value type kind must be primitive",
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
    if representation_contract.is_empty() {
        return Err(
            record.invariant("standard value type representation contract must not be empty")
        );
    }
    let origin = decode_origin(row, &record, DefinitionIdentity::ValueType(id))?;
    Ok(RecoveredStandardValueType {
        schema,
        definition: ValueTypeDefinition::primitive(
            id,
            name,
            ValueTypeMutability::Immutable,
            persistence,
            representation_contract,
        ),
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
                    target_type_id, source_unit_id, source_start, source_end
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
    let target = TypeId::from_bytes(identity_bytes(
        record.column(
            row,
            "target_type_id",
            "standard type binding target identity must be 16 bytes",
        )?,
        &record,
        "standard type binding target identity must be 16 bytes",
    )?);
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
        }
    }

    const fn value_tuple_rule(self) -> &'static str {
        match self {
            Self::Field => {
                "field type kind, scalar type, target identity, value type identity, and standard library revision must form one exact supported tuple"
            }
            Self::Parameter => {
                "parameter type columns, value type identity, and standard library revision must form one exact resolved type tuple"
            }
            Self::ReturnColumn => {
                "return column type columns, value type identity, and standard library revision must form one exact resolved type tuple"
            }
            Self::SingleReturn => {
                "function return type columns, value type identity, and standard library revision must form one exact resolved type tuple"
            }
        }
    }

    const fn scalar_rule(self) -> &'static str {
        match self {
            Self::Field => "field scalar type must be an exact standard scalar name",
            Self::Parameter | Self::ReturnColumn | Self::SingleReturn => {
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

/// The five stored columns that describe one version-2 resolved type.
///
/// This is the only recovery projection that combines legacy type columns with
/// a standard value identity and its standard-library revision pin.
pub(super) struct ResolvedTypeTuple {
    pub(super) kind: Option<String>,
    pub(super) scalar: Option<String>,
    pub(super) target: Option<TypeId>,
    pub(super) value_type: Option<TypeId>,
    pub(super) standard_library_revision: Option<StandardLibraryRevisionId>,
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

    if tuple.kind.as_deref() == Some("value") {
        let (Some(value_type), Some(standard_library_revision)) =
            (tuple.value_type, tuple.standard_library_revision)
        else {
            return Err(record.invariant(member.value_tuple_rule()));
        };
        if tuple.scalar.is_some() || tuple.target.is_some() {
            return Err(record.invariant(member.value_tuple_rule()));
        }
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

    if tuple.value_type.is_some() || tuple.standard_library_revision.is_some() {
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
        "field type kind must be scalar, named, reference, or value",
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
    decode_resolved_type_tuple(
        ResolvedTypeTuple {
            kind,
            scalar,
            target,
            value_type,
            standard_library_revision,
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
        load_fields(transaction, catalogue, catalogue_hash_context).await?,
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
    mut fields: BTreeMap<TypeId, Vec<RecoveredField>>,
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
    let catalogue = CatalogueSnapshot::new_with_functions(
        catalogue_id,
        schemas,
        object_definitions,
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
            orna_core::catalogue::FunctionReturn::Single(resolved_type) => {
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
    if let Some(target) = resolved_type
        .named_type()
        .or(resolved_type.reference_target())
    {
        if catalogue.object_type_by_id(target).is_none() {
            return Err(record.invariant(
                "every named or reference function type target must be an active object type",
            ));
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
    use orna_core::{
        CatalogueRevisionId, SourceBundleId, SourceRevisionId, SourceUnitId,
        StandardLibraryRevisionId, TypeId,
        canonical_hash::{
            catalogue_digest, source_bundle_digest, source_revision_record_digest,
            source_unit_content_digest,
        },
        catalogue::CatalogueSnapshot,
        revision::StoredSourceUnit,
        revision::{CatalogueHashContext, CatalogueHashVersion},
        types::{ResolvedType, StandardScalar},
    };

    use crate::{PostgresKernelError, decode::DurableRecord};

    use super::{
        LegacyResolvedTypeTupleMember, RecoveredCatalogueSemantics, RecoveredFunctionState,
        RecoveredRevisionHeader, ResolvedTypeTuple, assemble_revision,
        decode_catalogue_hash_version, decode_legacy_resolved_type_tuple,
        decode_legacy_resolved_type_tuple_kind, decode_resolved_type_tuple,
    };

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
        ] {
            let resolved_type = decode_resolved_type_tuple(
                ResolvedTypeTuple {
                    kind: Some("value".to_owned()),
                    scalar: None,
                    target: None,
                    value_type: Some(value_type),
                    standard_library_revision: Some(standard.revision()),
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
                rule: "field type kind, scalar type, target identity, value type identity, and standard library revision must form one exact supported tuple",
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
