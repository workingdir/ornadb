//! Candidate revision materialization, canonical hash checks, and encoding preflight.

use super::*;

pub(super) struct Materialized {
    pub(super) current: Vec<FunctionRevisionRecord>,
    pub(super) catalogue_hash_context: CatalogueHashContext,
}

pub(super) fn materialize(
    candidate: &DeployableRevision,
    locked: &ActiveDatabaseRevision,
) -> Result<Materialized, PostgresKernelError> {
    let locked_records = locked
        .function_revisions()
        .iter()
        .chain(locked.historical_function_revisions())
        .map(|revision| (revision.id(), revision.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut locked_numbers = BTreeSet::new();
    let mut locked_hashes = HashSet::new();
    for revision in locked_records.values() {
        locked_numbers.insert((revision.function(), revision.revision_number()));
        locked_hashes.insert((
            revision.function(),
            revision.declaration_content_hash(),
            revision.semantic_hash(),
        ));
    }
    for revision in candidate.new_function_revisions() {
        if locked_records.contains_key(&revision.id())
            || locked_numbers.contains(&(revision.function(), revision.revision_number()))
            || locked_hashes.contains(&(
                revision.function(),
                revision.declaration_content_hash(),
                revision.semantic_hash(),
            ))
        {
            return Err(invariant(
                "a new function revision collides with a locked current or historical revision",
            ));
        }
    }
    let new_by_id = candidate
        .new_function_revisions()
        .iter()
        .map(|revision| (revision.id(), revision))
        .collect::<BTreeMap<_, _>>();
    let mut current = Vec::with_capacity(candidate.candidate().functions().len());
    for function in candidate.candidate().functions() {
        let revision = if let Some(revision) = new_by_id.get(&function.current_revision()) {
            (*revision).clone()
        } else {
            locked_records
                .get(&function.current_revision())
                .cloned()
                .ok_or_else(|| {
                    invariant(
                        "candidate current function revision is absent from locked revision history",
                    )
                })?
        };
        if revision.function() != function.id() {
            return Err(invariant(
                "candidate current function revision must belong to its function",
            ));
        }
        current.push(revision);
    }
    let current_ids = current
        .iter()
        .map(FunctionRevisionRecord::id)
        .collect::<BTreeSet<_>>();
    let historical: Vec<_> = locked_records
        .into_values()
        .filter(|revision| !current_ids.contains(&revision.id()))
        .collect();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            candidate.candidate_pair(),
            candidate.source().clone(),
            candidate.candidate().clone(),
            candidate.catalogue_hash(),
            ActiveRevisionContent::new(
                candidate.expressions().to_vec(),
                current.clone(),
                candidate.origins().to_vec(),
                candidate.references().to_vec(),
            )
            .with_history(historical.clone()),
        ),
        candidate.catalogue_hash_context().clone(),
    )
    .map_err(PostgresKernelError::RevisionInvariant)?;
    Ok(Materialized {
        current,
        catalogue_hash_context: candidate.catalogue_hash_context().clone(),
    })
}

pub(super) fn verify_candidate_hashes(
    candidate: &DeployableRevision,
    materialized: &Materialized,
) -> Result<(), PostgresKernelError> {
    for unit in candidate.source().units() {
        if source_unit_content_digest(unit.content()).map_err(PostgresKernelError::CanonicalHash)?
            != unit.content_hash()
        {
            return Err(invariant(
                "source unit digest must match exact UTF-8 content",
            ));
        }
    }
    if source_bundle_digest(candidate.source().units())
        .map_err(PostgresKernelError::CanonicalHash)?
        != candidate.source().bundle_hash()
    {
        return Err(invariant(
            "source bundle digest must match candidate source units",
        ));
    }
    if source_revision_digest(candidate.source()).map_err(PostgresKernelError::CanonicalHash)?
        != candidate.source().revision_hash()
    {
        return Err(invariant(
            "source revision digest must match candidate source record",
        ));
    }
    for expression in candidate.expressions() {
        if artifact_payload_digest(expression.payload())
            .map_err(PostgresKernelError::CanonicalHash)?
            != expression.content_hash()
        {
            return Err(invariant(
                "expression artifact digest must match exact payload",
            ));
        }
    }
    for revision in candidate.new_function_revisions() {
        let declaration = declaration_bytes(candidate, revision.declaration_origin())?;
        if function_declaration_digest(declaration).map_err(PostgresKernelError::CanonicalHash)?
            != revision.declaration_content_hash()
        {
            return Err(invariant(
                "function declaration digest must match exact candidate source bytes",
            ));
        }
        if artifact_payload_digest(revision.artifact().payload())
            .map_err(PostgresKernelError::CanonicalHash)?
            != revision.artifact().content_hash()
        {
            return Err(invariant(
                "function artifact digest must match exact payload",
            ));
        }
    }
    let digest = catalogue_digest_with_context(
        &materialized.catalogue_hash_context,
        candidate.candidate(),
        &materialized.current,
        candidate.expressions(),
        candidate.origins(),
        candidate.references(),
    )
    .map_err(PostgresKernelError::CanonicalHash)?;
    if digest != candidate.catalogue_hash() {
        return Err(invariant(
            "candidate catalogue digest must match all current semantic records",
        ));
    }
    Ok(())
}

pub(super) fn declaration_bytes(
    candidate: &DeployableRevision,
    origin: SourceOrigin,
) -> Result<&[u8], PostgresKernelError> {
    let unit = candidate
        .source()
        .units()
        .iter()
        .find(|unit| unit.id() == origin.source_unit())
        .ok_or_else(|| {
            invariant("function declaration origin must identify a candidate source unit")
        })?;
    let content = unit.content().as_bytes();
    let start = usize::try_from(origin.byte_start())
        .map_err(|_| invariant("function declaration origin start must fit usize"))?;
    let end = usize::try_from(origin.byte_end())
        .map_err(|_| invariant("function declaration origin end must fit usize"))?;
    content.get(start..end).ok_or_else(|| {
        invariant("function declaration origin must select exact candidate source bytes")
    })
}

pub(super) fn validate_postgres_encodings(
    candidate: &DeployableRevision,
    encoder: &CandidateEncoder<'_>,
) -> Result<(), PostgresKernelError> {
    for expression in candidate.expressions() {
        let _ = positive_i32(expression.version(), "expression format version")?;
    }
    for revision in candidate.new_function_revisions() {
        let _ = positive_i64(revision.revision_number(), "function revision number")?;
        let _ = positive_i32(
            revision.artifact().version(),
            "function artifact format version",
        )?;
    }
    for object in candidate.candidate().object_types() {
        schema_for_name(candidate.candidate(), object.name())?;
        for field in object.fields() {
            let _ = encoder.type_columns(field.resolved_type(), false)?;
            let _ = on_delete(field.on_delete());
        }
    }
    for record_type in candidate.candidate().record_value_types() {
        for field in record_type.fields() {
            let _ = encoder.record_value_field_columns(candidate, field.descriptor())?;
        }
    }
    for function in candidate.candidate().functions() {
        schema_for_name(candidate.candidate(), function.name())?;
        let _ = function_domain(function.domain());
        let _ = function_security(function.security());
        let _ = function_transaction(function.transaction())?;
        let _ = function_volatility(function.volatility());
        for parameter in function.parameters() {
            let _ = encoder.function_type_columns(
                function.domain(),
                parameter.resolved_type(),
                false,
            )?;
        }
        match function.return_type() {
            FunctionReturn::Single(resolved) => {
                let _ = encoder.function_type_columns(function.domain(), *resolved, true)?;
            }
            FunctionReturn::Rows(columns) => {
                for column in columns {
                    let _ = encoder.type_columns(column.resolved_type(), false)?;
                }
            }
            FunctionReturn::Stream(resolved) => {
                let _ = encoder.function_type_columns(function.domain(), *resolved, false)?;
            }
        }
    }
    for origin in candidate.origins() {
        validate_origin(origin.source())?;
    }
    for reference in candidate.references() {
        validate_origin(reference.source_origin())?;
        let _ = encoder.reference_target(reference.target())?;
        let _ = reference_kind(reference.kind())?;
    }
    Ok(())
}

pub(super) fn positive_i32(value: u32, rule: &'static str) -> Result<i32, PostgresKernelError> {
    i32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| invariant(rule))
}
pub(super) fn positive_i64(value: u64, rule: &'static str) -> Result<i64, PostgresKernelError> {
    i64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| invariant(rule))
}

pub(super) fn semantic_hash_version(
    version: orna_core::revision::FunctionSemanticHashVersion,
) -> Result<i16, PostgresKernelError> {
    i16::try_from(version.to_u32())
        .map_err(|_| invariant("function semantic hash version must fit PostgreSQL smallint"))
}

pub(super) fn validate_origin(origin: SourceOrigin) -> Result<(), PostgresKernelError> {
    if origin.byte_start() > origin.byte_end() {
        Err(invariant("source origin must be ordered"))
    } else {
        Ok(())
    }
}
