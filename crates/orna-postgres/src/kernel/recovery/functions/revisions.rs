//! Executable artifact, immutable revision, and introduction recovery.

use super::*;

pub(in super::super) struct RecoveredFunctionState {
    pub(in super::super) functions: Vec<RecoveredFunction>,
    pub(in super::super) active_revisions: Vec<FunctionRevisionRecord>,
    pub(in super::super) historical_revisions: Vec<FunctionRevisionRecord>,
    pub(in super::super) origins: Vec<DefinitionOrigin>,
    pub(in super::super) references: Vec<DefinitionReference>,
    pub(in super::super) introductions: BTreeMap<CatalogueRevisionId, RecoveredIntroduction>,
}

impl RecoveredFunctionState {
    #[cfg(test)]
    pub(in super::super) fn empty() -> Self {
        Self {
            functions: Vec::new(),
            active_revisions: Vec::new(),
            historical_revisions: Vec::new(),
            origins: Vec::new(),
            references: Vec::new(),
            introductions: BTreeMap::new(),
        }
    }
}

pub(in super::super) struct RecoveredIntroduction {
    pub(in super::super) catalogue_hash: Sha256Digest,
    pub(in super::super) source: StoredSourceRevision,
}

struct PendingRevision {
    function: FunctionId,
    id: FunctionRevisionId,
    revision_number: u64,
    declaration_origin: SourceOrigin,
    declaration_hash: Sha256Digest,
    semantic_hash: Sha256Digest,
    semantic_hash_version: FunctionSemanticHashVersion,
    language_version: String,
    artifact: ExecutableArtifact,
    status: RevisionStatus,
    introduction: IntroductionHeader,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RevisionStatus {
    Active,
    Retired,
}

#[derive(Clone)]
struct IntroductionHeader {
    catalogue: CatalogueRevisionId,
    catalogue_hash: Sha256Digest,
    source: SourceRevisionId,
    source_parent: Option<SourceRevisionId>,
    source_hash: Sha256Digest,
    bundle: SourceBundleId,
    bundle_hash: Sha256Digest,
    catalogue_hash_version: CatalogueHashVersion,
    standard_library_revision: Option<StandardLibraryRevisionId>,
}

pub(in super::super) async fn load_function_state(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    active_ancestry: &BTreeSet<(CatalogueRevisionId, SourceRevisionId)>,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<RecoveredFunctionState, PostgresKernelError> {
    let (functions, origins) =
        load_catalogue_functions(transaction, catalogue, catalogue_hash_context).await?;

    let mut artifacts = load_artifacts(transaction).await?;
    let pending = load_revisions(transaction, &mut artifacts).await?;
    if let Some((revision, _)) = artifacts.first_key_value() {
        return Err(DurableRecord::new(ARTIFACT_RELATION, revision.canonical())
            .invariant("every function artifact must belong to one recovered function revision"));
    }
    let (active_revisions, historical_revisions, introductions) =
        finish_revisions(transaction, &functions, pending, active_ancestry).await?;

    let references = load_references(
        transaction,
        catalogue,
        catalogue_hash_context
            .standard()
            .map(|standard| standard.revision()),
    )
    .await?;
    validate_reference_sources(&functions, &references)?;

    Ok(RecoveredFunctionState {
        functions,
        active_revisions,
        historical_revisions,
        origins,
        references,
        introductions,
    })
}

/// Loads the immutable revisions named by one catalogue's current function
/// rows without applying the active catalogue's global status classification.
///
/// Historical digest recovery resolves the identities stored in that catalogue
/// directly: a revision may be retired today, or may have been active when the
/// historical catalogue was committed.
pub(in super::super) async fn load_catalogue_current_revisions(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
) -> Result<BTreeMap<FunctionRevisionId, FunctionRevisionRecord>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT revision.id, revision.function_id, revision.revision_number,
                    revision.content_hash, revision.semantic_ir_hash,
                    revision.semantic_hash_version,
                    revision.hash_algorithm, revision.hash_contract_version,
                    revision.language_version, revision.status,
                    revision.introduced_catalogue_revision_id,
                    introduced_function.current_function_revision_id AS introduced_current_revision_id,
                    introduced_function.domain AS introduced_domain,
                    introduced_function.source_unit_id,
                    introduced_function.source_start,
                    introduced_function.source_end,
                    introduced_catalogue.source_revision_id,
                    introduced_catalogue.content_hash AS catalogue_hash,
                    introduced_catalogue.hash_algorithm AS catalogue_algorithm,
                    introduced_catalogue.hash_contract_version AS catalogue_contract_version,
                    introduced_catalogue.canonical_hash_version AS catalogue_canonical_hash_version,
                    introduced_catalogue.standard_library_revision_id AS catalogue_standard_library_revision_id,
                    source.parent_source_revision_id,
                    source.bundle_id,
                    source.content_hash AS source_hash,
                    source.hash_algorithm AS source_algorithm,
                    source.hash_contract_version AS source_contract_version,
                    bundle.content_hash AS bundle_hash,
                    bundle.hash_algorithm AS bundle_algorithm,
                    bundle.hash_contract_version AS bundle_contract_version
             FROM _orna_kernel.catalogue_functions AS current_function
             JOIN _orna_kernel.function_revisions AS revision
               ON revision.id = current_function.current_function_revision_id
             LEFT JOIN _orna_kernel.catalogue_revisions AS introduced_catalogue
               ON introduced_catalogue.id = revision.introduced_catalogue_revision_id
             LEFT JOIN _orna_kernel.catalogue_functions AS introduced_function
               ON introduced_function.catalogue_revision_id = revision.introduced_catalogue_revision_id
              AND introduced_function.function_id = revision.function_id
             LEFT JOIN _orna_kernel.source_revisions AS source
               ON source.id = introduced_catalogue.source_revision_id
             LEFT JOIN _orna_kernel.source_bundles AS bundle
               ON bundle.id = source.bundle_id
             WHERE current_function.catalogue_revision_id = $1
             ORDER BY current_function.function_id, revision.id",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    let mut revisions = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let row_record = DurableRecord::new(
            REVISION_RELATION,
            format!("catalogue={};row={index}", catalogue.canonical()),
        );
        let id = FunctionRevisionId::from_bytes(identity_bytes(
            row_record.column(row, "id", "function revision identity must be 16 bytes")?,
            &row_record,
            "function revision identity must be 16 bytes",
        )?);
        let artifact_rows = transaction
            .query(
                "SELECT function_revision_id, artifact_kind, format,
                        format_version::bigint AS format_version, payload, content_hash,
                        hash_algorithm, hash_contract_version
                 FROM _orna_kernel.function_artifacts
                 WHERE function_revision_id = $1
                 ORDER BY artifact_kind",
                &[&id.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        let mut artifacts = BTreeMap::<FunctionRevisionId, Vec<ExecutableArtifact>>::new();
        for (artifact_index, artifact_row) in artifact_rows.iter().enumerate() {
            let (artifact_id, artifact) = decode_artifact(artifact_row, artifact_index)?;
            artifacts.entry(artifact_id).or_default().push(artifact);
        }
        let pending = decode_revision(row, index, &mut artifacts)?;
        if let Some((orphan, _)) = artifacts.first_key_value() {
            return Err(
                DurableRecord::new(ARTIFACT_RELATION, orphan.canonical()).invariant(
                    "every catalogue current revision artifact must belong to that revision",
                ),
            );
        }
        let record = FunctionRevisionRecord::new(
            pending.function,
            pending.id,
            pending.revision_number,
            pending.declaration_origin,
            pending.declaration_hash,
            pending.semantic_hash,
            pending.language_version,
            pending.artifact,
        )
        .map_err(PostgresKernelError::RevisionInvariant)?
        .with_semantic_hash_version(pending.semantic_hash_version);
        if revisions.insert(record.id(), record).is_some() {
            return Err(
                DurableRecord::new(REVISION_RELATION, id.canonical()).invariant(
                    "each catalogue function must resolve one distinct immutable current revision",
                ),
            );
        }
    }
    Ok(revisions)
}

async fn load_artifacts(
    transaction: &Transaction<'_>,
) -> Result<BTreeMap<FunctionRevisionId, Vec<ExecutableArtifact>>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT function_revision_id, artifact_kind, format,
                    format_version::bigint AS format_version, payload, content_hash,
                    hash_algorithm, hash_contract_version
             FROM _orna_kernel.function_artifacts
             ORDER BY function_revision_id, artifact_kind",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut artifacts = BTreeMap::<FunctionRevisionId, Vec<ExecutableArtifact>>::new();
    for (index, row) in rows.iter().enumerate() {
        let (revision, artifact) = decode_artifact(row, index)?;
        artifacts.entry(revision).or_default().push(artifact);
    }
    Ok(artifacts)
}

fn decode_artifact(
    row: &Row,
    index: usize,
) -> Result<(FunctionRevisionId, ExecutableArtifact), PostgresKernelError> {
    let row_record = DurableRecord::new(ARTIFACT_RELATION, format!("row={index}"));
    let revision = FunctionRevisionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_revision_id",
            "artifact function revision identity must be 16 bytes",
        )?,
        &row_record,
        "artifact function revision identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(ARTIFACT_RELATION, revision.canonical());
    require_hash_contract(
        row,
        &record,
        "hash_algorithm",
        "hash_contract_version",
        "function artifact hash algorithm must be sha256",
        "function artifact hash contract version must be 1",
    )?;
    let kind_name: String = record.column(row, "artifact_kind", "artifact kind must decode")?;
    let kind = exact_enum(
        &kind_name,
        &[
            ("server_plan", ExecutableArtifactKind::Server),
            ("client_bytecode", ExecutableArtifactKind::Client),
        ],
        &record,
        "artifact kind must be server_plan or client_bytecode",
    )?;
    let format: String = record.column(row, "format", "artifact format must be text")?;
    let version = u32_from_i64(
        record.column(
            row,
            "format_version",
            "artifact format version must fit u32",
        )?,
        &record,
        "artifact format version must fit u32",
    )?;
    let payload: Vec<u8> = record.column(row, "payload", "artifact payload must be exact bytes")?;
    let content_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(
            row,
            "content_hash",
            "artifact content hash must be 32 bytes",
        )?,
        &record,
        "artifact content hash must be 32 bytes",
    )?);
    let computed = artifact_payload_digest(&payload).map_err(PostgresKernelError::CanonicalHash)?;
    if computed != content_hash {
        return Err(record.invariant("artifact digest must match its exact payload"));
    }
    let artifact = ExecutableArtifact::new(kind, format, version, payload, content_hash)
        .map_err(PostgresKernelError::RevisionInvariant)?;
    Ok((revision, artifact))
}

async fn load_revisions(
    transaction: &Transaction<'_>,
    artifacts: &mut BTreeMap<FunctionRevisionId, Vec<ExecutableArtifact>>,
) -> Result<Vec<PendingRevision>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT revision.id, revision.function_id, revision.revision_number,
                    revision.content_hash, revision.semantic_ir_hash,
                    revision.semantic_hash_version,
                    revision.hash_algorithm, revision.hash_contract_version,
                    revision.language_version, revision.status,
                    revision.introduced_catalogue_revision_id,
                    introduced_function.current_function_revision_id AS introduced_current_revision_id,
                    introduced_function.domain AS introduced_domain,
                    introduced_function.source_unit_id,
                    introduced_function.source_start,
                    introduced_function.source_end,
                    catalogue.source_revision_id,
                    catalogue.content_hash AS catalogue_hash,
                    catalogue.hash_algorithm AS catalogue_algorithm,
                    catalogue.hash_contract_version AS catalogue_contract_version,
                    catalogue.canonical_hash_version AS catalogue_canonical_hash_version,
                    catalogue.standard_library_revision_id AS catalogue_standard_library_revision_id,
                    source.parent_source_revision_id,
                    source.bundle_id,
                    source.content_hash AS source_hash,
                    source.hash_algorithm AS source_algorithm,
                    source.hash_contract_version AS source_contract_version,
                    bundle.content_hash AS bundle_hash,
                    bundle.hash_algorithm AS bundle_algorithm,
                    bundle.hash_contract_version AS bundle_contract_version
             FROM _orna_kernel.function_revisions AS revision
             LEFT JOIN _orna_kernel.catalogue_revisions AS catalogue
               ON catalogue.id = revision.introduced_catalogue_revision_id
             LEFT JOIN _orna_kernel.catalogue_functions AS introduced_function
               ON introduced_function.catalogue_revision_id = revision.introduced_catalogue_revision_id
              AND introduced_function.function_id = revision.function_id
             LEFT JOIN _orna_kernel.source_revisions AS source
               ON source.id = catalogue.source_revision_id
             LEFT JOIN _orna_kernel.source_bundles AS bundle ON bundle.id = source.bundle_id
             ORDER BY revision.function_id, revision.revision_number, revision.id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut revisions = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        revisions.push(decode_revision(row, index, artifacts)?);
    }
    Ok(revisions)
}

fn decode_revision(
    row: &Row,
    index: usize,
    artifacts: &mut BTreeMap<FunctionRevisionId, Vec<ExecutableArtifact>>,
) -> Result<PendingRevision, PostgresKernelError> {
    let row_record = DurableRecord::new(REVISION_RELATION, format!("row={index}"));
    let id = FunctionRevisionId::from_bytes(identity_bytes(
        row_record.column(row, "id", "function revision identity must be 16 bytes")?,
        &row_record,
        "function revision identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(REVISION_RELATION, id.canonical());
    let function = FunctionId::from_bytes(identity_bytes(
        record.column(
            row,
            "function_id",
            "function revision owner identity must be 16 bytes",
        )?,
        &record,
        "function revision owner identity must be 16 bytes",
    )?);
    let revision_number = u64_from_i64(
        record.column(
            row,
            "revision_number",
            "function revision number must be a positive bigint",
        )?,
        &record,
        "function revision number must be a positive u64",
    )?;
    if revision_number == 0 {
        return Err(record.invariant("function revision number must be positive"));
    }
    let status_name: String =
        record.column(row, "status", "function revision status must decode")?;
    let status = exact_enum(
        &status_name,
        &[
            ("active", RevisionStatus::Active),
            ("retired", RevisionStatus::Retired),
        ],
        &record,
        "recoverable function revision status must be active or retired, never candidate or invalid",
    )?;
    require_hash_contract(
        row,
        &record,
        "hash_algorithm",
        "hash_contract_version",
        "function revision hash algorithm must be sha256",
        "function revision hash contract version must be 1",
    )?;
    let declaration_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(
            row,
            "content_hash",
            "function declaration hash must be 32 bytes",
        )?,
        &record,
        "function declaration hash must be 32 bytes",
    )?);
    let semantic_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(
            row,
            "semantic_ir_hash",
            "function semantic hash must be 32 bytes",
        )?,
        &record,
        "function semantic hash must be 32 bytes",
    )?);
    let semantic_hash_version = decode_function_semantic_hash_version(
        record.column(
            row,
            "semantic_hash_version",
            "function semantic hash version must be a supported smallint",
        )?,
        &record,
    )?;
    let language_version: String = record.column(
        row,
        "language_version",
        "function language version must be text",
    )?;
    if language_version.is_empty() {
        return Err(record.invariant("function language version must not be empty"));
    }
    let mut revision_artifacts = artifacts.remove(&id).unwrap_or_default();
    if revision_artifacts.len() != 1 {
        return Err(record.invariant(
            "each function revision must have exactly one versioned executable artifact",
        ));
    }
    let artifact = revision_artifacts
        .pop()
        .ok_or_else(|| record.invariant("function revision artifact must exist"))?;
    let introduced_domain_name: String = record.column(
        row,
        "introduced_domain",
        "introducing function domain must decode",
    )?;
    let introduced_domain = exact_enum(
        &introduced_domain_name,
        &[
            ("server", FunctionDomain::Server),
            ("client", FunctionDomain::Client),
        ],
        &record,
        "introducing function domain must be server or client",
    )?;
    let expected_kind = match introduced_domain {
        FunctionDomain::Server => ExecutableArtifactKind::Server,
        FunctionDomain::Client => ExecutableArtifactKind::Client,
    };
    if artifact.kind() != expected_kind {
        return Err(record.invariant(
            "function artifact kind must exactly match the introducing function domain",
        ));
    }
    let introduced_current = FunctionRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "introduced_current_revision_id",
            "introducing function current revision identity must be 16 bytes",
        )?,
        &record,
        "introducing function current revision identity must be 16 bytes",
    )?);
    if introduced_current != id {
        return Err(record.invariant(
            "the introducing catalogue function must identify the immutable revision it introduced",
        ));
    }
    let declaration_origin = decode_required_source_origin(row, &record)?;
    let introduction = decode_introduction_header(row, &record)?;
    Ok(PendingRevision {
        function,
        id,
        revision_number,
        declaration_origin,
        declaration_hash,
        semantic_hash,
        semantic_hash_version,
        language_version,
        artifact,
        status,
        introduction,
    })
}

fn decode_function_semantic_hash_version(
    value: i16,
    record: &DurableRecord,
) -> Result<FunctionSemanticHashVersion, PostgresKernelError> {
    let value = decode_durable_version(
        value,
        record,
        "function semantic hash version must be a supported smallint",
    )?;
    FunctionSemanticHashVersion::try_from(value)
        .map_err(|_| record.invariant("function semantic hash version must be 1 or 2"))
}

fn decode_required_source_origin(
    row: &Row,
    record: &DurableRecord,
) -> Result<SourceOrigin, PostgresKernelError> {
    let unit = SourceUnitId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_unit_id",
            "historical declaration source unit identity must be 16 bytes",
        )?,
        record,
        "historical declaration source unit identity must be 16 bytes",
    )?);
    let start = u32_from_i64(
        record.column(
            row,
            "source_start",
            "historical declaration origin start must fit u32",
        )?,
        record,
        "historical declaration origin start must fit u32",
    )?;
    let end = u32_from_i64(
        record.column(
            row,
            "source_end",
            "historical declaration origin end must fit u32",
        )?,
        record,
        "historical declaration origin end must fit u32",
    )?;
    SourceOrigin::new(unit, start, end).map_err(PostgresKernelError::RevisionInvariant)
}

fn decode_introduction_header(
    row: &Row,
    record: &DurableRecord,
) -> Result<IntroductionHeader, PostgresKernelError> {
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "introduced_catalogue_revision_id",
            "introducing catalogue identity must be 16 bytes",
        )?,
        record,
        "introducing catalogue identity must be 16 bytes",
    )?);
    let source = SourceRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_revision_id",
            "introducing source revision identity must be 16 bytes",
        )?,
        record,
        "introducing source revision identity must be 16 bytes",
    )?);
    let source_parent = optional_identity_bytes(
        record.column(
            row,
            "parent_source_revision_id",
            "introducing source parent identity must be null or 16 bytes",
        )?,
        record,
        "introducing source parent identity must be null or 16 bytes",
    )?
    .map(SourceRevisionId::from_bytes);
    let bundle = SourceBundleId::from_bytes(identity_bytes(
        record.column(
            row,
            "bundle_id",
            "introducing source bundle identity must be 16 bytes",
        )?,
        record,
        "introducing source bundle identity must be 16 bytes",
    )?);
    let catalogue_hash_version = decode_catalogue_hash_version(
        record.column(
            row,
            "catalogue_canonical_hash_version",
            "introducing catalogue hash version must be a supported smallint",
        )?,
        record,
    )?;
    let standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "catalogue_standard_library_revision_id",
            "introducing catalogue standard library revision identity must be null or 16 bytes",
        )?,
        record,
        "introducing catalogue standard library revision identity must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    match (catalogue_hash_version, standard_library_revision) {
        (CatalogueHashVersion::Version1, None) | (CatalogueHashVersion::Version2, Some(_)) => {}
        _ => {
            return Err(record.invariant(
                "introducing catalogue hash version and standard library revision must form one exact context",
            ));
        }
    }
    for (algorithm, version, algorithm_rule, version_rule) in [
        (
            "catalogue_algorithm",
            "catalogue_contract_version",
            "introducing catalogue hash algorithm must be sha256",
            "introducing catalogue hash contract version must be 1",
        ),
        (
            "source_algorithm",
            "source_contract_version",
            "introducing source hash algorithm must be sha256",
            "introducing source hash contract version must be 1",
        ),
        (
            "bundle_algorithm",
            "bundle_contract_version",
            "introducing bundle hash algorithm must be sha256",
            "introducing bundle hash contract version must be 1",
        ),
    ] {
        require_hash_contract(
            row,
            record,
            algorithm,
            version,
            algorithm_rule,
            version_rule,
        )?;
    }
    Ok(IntroductionHeader {
        catalogue,
        catalogue_hash: Sha256Digest::from_bytes(digest_bytes(
            record.column(
                row,
                "catalogue_hash",
                "introducing catalogue hash must be 32 bytes",
            )?,
            record,
            "introducing catalogue hash must be 32 bytes",
        )?),
        source,
        source_parent,
        source_hash: Sha256Digest::from_bytes(digest_bytes(
            record.column(
                row,
                "source_hash",
                "introducing source hash must be 32 bytes",
            )?,
            record,
            "introducing source hash must be 32 bytes",
        )?),
        bundle,
        bundle_hash: Sha256Digest::from_bytes(digest_bytes(
            record.column(
                row,
                "bundle_hash",
                "introducing bundle hash must be 32 bytes",
            )?,
            record,
            "introducing bundle hash must be 32 bytes",
        )?),
        catalogue_hash_version,
        standard_library_revision,
    })
}

async fn finish_revisions(
    transaction: &Transaction<'_>,
    functions: &[RecoveredFunction],
    pending: Vec<PendingRevision>,
    active_ancestry: &BTreeSet<(CatalogueRevisionId, SourceRevisionId)>,
) -> Result<
    (
        Vec<FunctionRevisionRecord>,
        Vec<FunctionRevisionRecord>,
        BTreeMap<CatalogueRevisionId, RecoveredIntroduction>,
    ),
    PostgresKernelError,
> {
    let mut headers = BTreeMap::<CatalogueRevisionId, IntroductionHeader>::new();
    for revision in &pending {
        match headers.get(&revision.introduction.catalogue) {
            Some(existing) if !same_introduction(existing, &revision.introduction) => {
                return Err(DurableRecord::new(
                    REVISION_RELATION,
                    revision.introduction.catalogue.canonical(),
                )
                .invariant(
                    "all revisions introduced by one catalogue must join one exact source and hash chain",
                ));
            }
            Some(_) => {}
            None => {
                headers.insert(
                    revision.introduction.catalogue,
                    revision.introduction.clone(),
                );
            }
        }
    }
    let mut introductions = BTreeMap::new();
    for (catalogue, header) in headers {
        if !active_ancestry.contains(&(catalogue, header.source)) {
            return Err(DurableRecord::new(
                "_orna_kernel.catalogue_revisions",
                catalogue.canonical(),
            )
            .invariant(
                "every function revision introduction catalogue/source pair must lie on the active paired ancestry",
            ));
        }
        let units = load_source_units(transaction, header.bundle).await?;
        let bundle_record =
            DurableRecord::new("_orna_kernel.source_bundles", header.bundle.canonical());
        let computed_bundle =
            source_bundle_digest(&units).map_err(PostgresKernelError::CanonicalHash)?;
        if computed_bundle != header.bundle_hash {
            return Err(bundle_record.invariant(
                "introducing source bundle digest must match its complete ordered source units",
            ));
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
        let computed_source =
            source_revision_digest(&source).map_err(PostgresKernelError::CanonicalHash)?;
        if computed_source != header.source_hash {
            return Err(DurableRecord::new(
                "_orna_kernel.source_revisions",
                header.source.canonical(),
            )
            .invariant(
                "introducing source revision digest must match its bundle, parent, and bundle digest",
            ));
        }
        introductions.insert(
            catalogue,
            RecoveredIntroduction {
                catalogue_hash: header.catalogue_hash,
                source,
            },
        );
    }

    let current_ids = functions
        .iter()
        .map(|function| {
            (
                function.definition.current_revision(),
                function.definition.id(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut seen_current = BTreeSet::new();
    let mut active = Vec::new();
    let mut historical = Vec::new();
    for revision in pending {
        let introduction = introductions
            .get(&revision.introduction.catalogue)
            .ok_or_else(|| {
                DurableRecord::new(REVISION_RELATION, revision.id.canonical())
                    .invariant("function revision introduction must be recovered")
            })?;
        validate_declaration(&revision, &introduction.source)?;
        let record = FunctionRevisionRecord::new(
            revision.function,
            revision.id,
            revision.revision_number,
            revision.declaration_origin,
            revision.declaration_hash,
            revision.semantic_hash,
            revision.language_version,
            revision.artifact,
        )
        .map_err(PostgresKernelError::RevisionInvariant)?
        .with_semantic_hash_version(revision.semantic_hash_version);
        if let Some(expected_function) = current_ids.get(&record.id()) {
            if revision.status != RevisionStatus::Active {
                return Err(
                    DurableRecord::new(REVISION_RELATION, record.id().canonical())
                        .invariant("every current function revision must have active status"),
                );
            }
            if *expected_function != record.function() {
                return Err(
                    DurableRecord::new(REVISION_RELATION, record.id().canonical())
                        .invariant("current function revision must belong to its active function"),
                );
            }
            seen_current.insert(record.id());
            active.push(record);
        } else {
            if revision.status != RevisionStatus::Retired {
                return Err(
                    DurableRecord::new(REVISION_RELATION, record.id().canonical()).invariant(
                        "every non-current immutable function revision must have retired status",
                    ),
                );
            }
            historical.push(record);
        }
    }
    if let Some((missing, _)) = current_ids
        .iter()
        .find(|(revision, _)| !seen_current.contains(revision))
    {
        return Err(DurableRecord::new(REVISION_RELATION, missing.canonical())
            .invariant("every active function must identify one recovered current revision"));
    }
    Ok((active, historical, introductions))
}

fn same_introduction(left: &IntroductionHeader, right: &IntroductionHeader) -> bool {
    left.catalogue == right.catalogue
        && left.catalogue_hash == right.catalogue_hash
        && left.source == right.source
        && left.source_parent == right.source_parent
        && left.source_hash == right.source_hash
        && left.bundle == right.bundle
        && left.bundle_hash == right.bundle_hash
        && left.catalogue_hash_version == right.catalogue_hash_version
        && left.standard_library_revision == right.standard_library_revision
}

fn validate_declaration(
    revision: &PendingRevision,
    source: &StoredSourceRevision,
) -> Result<(), PostgresKernelError> {
    let record = DurableRecord::new(REVISION_RELATION, revision.id.canonical());
    let origin = revision.declaration_origin;
    let unit = source
        .units()
        .iter()
        .find(|unit| unit.id() == origin.source_unit())
        .ok_or_else(|| {
            record.invariant(
                "historical declaration origin source unit must belong to its introducing source revision",
            )
        })?;
    let start = usize::try_from(origin.byte_start()).map_err(|_| {
        record.invariant("historical declaration origin start must fit the platform index")
    })?;
    let end = usize::try_from(origin.byte_end()).map_err(|_| {
        record.invariant("historical declaration origin end must fit the platform index")
    })?;
    if end > unit.content().len()
        || !unit.content().is_char_boundary(start)
        || !unit.content().is_char_boundary(end)
    {
        return Err(record.invariant(
            "historical declaration origin must be in bounds on exact UTF-8 character boundaries",
        ));
    }
    let declaration = unit
        .content()
        .as_bytes()
        .get(start..end)
        .ok_or_else(|| record.invariant("historical declaration byte range must exist"))?;
    let computed =
        function_declaration_digest(declaration).map_err(PostgresKernelError::CanonicalHash)?;
    if computed != revision.declaration_hash {
        return Err(record
            .invariant("function declaration hash must match the exact introducing source bytes"));
    }
    Ok(())
}
