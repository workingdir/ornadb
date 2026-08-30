//! Standard-library executable artifact, revision, and reference recovery.

use super::*;

/// A version-one standard revision must have no row in any new executable
/// relation. The version-one digest contract covers no executable fact, so
/// stray executable rows would otherwise survive recovery unverified.
pub(super) async fn require_no_standard_executable_rows(
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

/// Reconstructs the complete version-2 standard executable sequence: the
/// immutable function revisions with their artifacts and the ordered
/// definition references, aligned one-to-one with the catalogue functions.
pub(super) async fn load_standard_executable_facts(
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
