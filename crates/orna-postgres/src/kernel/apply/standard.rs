//! Persistence of verified standard-library state during apply.

use super::*;

/// Persists the retained version-one standard as historical state when the
/// installed standard's source revision descends from it (ADR 0055).
///
/// The V1-to-V2 upgrade is prepared against the empty expected base, so a
/// fresh database has no V1 rows. The V2 source revision is the append-only
/// child of the exact V1 standard source revision, so the single install
/// transaction must persist V1 as retained historical standard state before
/// V2 can claim its parent. The operation is idempotent: a database that
/// already retains V1 is left untouched, and only the exact V2 parent edge is
/// repaired.
pub(super) async fn persist_retained_v1_standard_parent(
    transaction: &Transaction<'_>,
    standard: &VerifiedStandardLibrarySnapshot,
) -> Result<(), PostgresKernelError> {
    if standard.digest_version() != StandardLibraryDigestVersion::Version2 {
        return Ok(());
    }
    if standard.source().parent() != Some(STANDARD_SOURCE_REVISION_ID) {
        return Ok(());
    }
    let parent = standard
        .source()
        .parent()
        .expect("the exact V2 parent edge was checked above");
    let row = transaction
        .query_opt(
            "SELECT 1 FROM _orna_kernel.source_revisions WHERE id = $1",
            &[&bytes(parent)],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    if row.is_some() {
        return Ok(());
    }
    let retained = retained_standard_library_snapshot()
        .and_then(verify_standard_library_snapshot)
        .map_err(|_| invariant("the retained version-one standard must remain verifiable"))?;
    persist_standard_library(transaction, &retained).await
}

pub(super) async fn persist_standard_library(
    transaction: &Transaction<'_>,
    standard: &VerifiedStandardLibrarySnapshot,
) -> Result<(), PostgresKernelError> {
    persist_source(transaction, standard.source(), true).await?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.standard_library_revisions
                (id, source_revision_id, catalogue_revision_id, digest_version,
                 language_version, content_hash, hash_algorithm)
             VALUES ($1, $2, $3, $4, $5, $6, 'sha256')",
            &[
                &bytes(standard.revision()),
                &bytes(standard.source().id()),
                &bytes(standard.catalogue().revision()),
                &standard_digest_version(standard.digest_version())?,
                &standard.language_version(),
                &digest(standard.digest()),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    for schema in standard.catalogue().schemas() {
        let source = origin(standard.origins(), DefinitionIdentity::Schema(schema.id()))?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_schemas
                    (standard_library_revision_id, schema_id, name_parts, source_unit_id,
                     source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[
                    &bytes(standard.revision()),
                    &bytes(schema.id()),
                    &schema.name().parts(),
                    &bytes(source.source_unit()),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }

    for value_type in standard.catalogue().value_types() {
        let schema = schema_for_name(standard.catalogue(), value_type.name())?;
        let source = origin(
            standard.origins(),
            DefinitionIdentity::ValueType(value_type.id()),
        )?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_value_types
                    (standard_library_revision_id, type_id, schema_id, name_parts, value_kind,
                     mutability, persistence, representation_contract, source_unit_id,
                     source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                &[
                    &bytes(standard.revision()),
                    &bytes(value_type.id()),
                    &bytes(schema),
                    &value_type.name().parts(),
                    &standard_value_kind(value_type.kind())?,
                    &standard_value_mutability(value_type.mutability())?,
                    &standard_value_persistence(value_type.persistence())?,
                    &value_type.representation_contract(),
                    &bytes(source.source_unit()),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }

    for enum_type in standard.catalogue().enum_types() {
        let schema = schema_for_name(standard.catalogue(), enum_type.name())?;
        let source = origin(
            standard.origins(),
            DefinitionIdentity::ValueType(enum_type.id()),
        )?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_enum_types
                    (standard_library_revision_id, type_id, schema_id, name_parts, labels,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                &[
                    &bytes(standard.revision()),
                    &bytes(enum_type.id()),
                    &bytes(schema),
                    &enum_type.name().parts(),
                    &enum_type.labels(),
                    &bytes(source.source_unit()),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }

    for binding in standard.catalogue().type_bindings() {
        let source = origin(
            standard.origins(),
            DefinitionIdentity::TypeBinding(binding.id()),
        )?;
        let (kind, name_parts) = standard_type_binding_name(binding.kind(), binding.name())?;
        let (target_kind, value_target, enum_target) = if standard
            .catalogue()
            .value_type_by_id(binding.target())
            .is_some()
        {
            ("value", Some(bytes(binding.target())), None)
        } else if standard
            .catalogue()
            .enum_type_by_id(binding.target())
            .is_some()
        {
            ("enum", None, Some(bytes(binding.target())))
        } else {
            return Err(invariant(
                "standard type binding target must identify one standard value or enum type",
            ));
        };
        transaction
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_type_bindings
                    (standard_library_revision_id, type_binding_id, kind, name_parts,
                     target_type_kind, target_type_id, target_enum_type_id,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                &[
                    &bytes(standard.revision()),
                    &bytes(binding.id()),
                    &kind,
                    &name_parts,
                    &target_kind,
                    &value_target,
                    &enum_target,
                    &bytes(source.source_unit()),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    validate_standard_executable_facts(
        standard.digest_version(),
        standard.catalogue(),
        standard.executables(),
    )?;
    if standard.digest_version() == StandardLibraryDigestVersion::Version2 {
        persist_standard_executable_facts(transaction, standard).await?;
    }
    Ok(())
}

/// Fail-closed checks for the executable facts of one verified standard
/// snapshot. A version-1 snapshot must carry no executable; a version-2
/// snapshot must carry one executable per catalogue function. Each
/// executable's catalogue function, current revision, artifact domain, and
/// reference evidence must agree.
pub(super) fn validate_standard_executable_facts(
    digest_version: StandardLibraryDigestVersion,
    catalogue: &CatalogueSnapshot,
    executables: &[StandardExecutable],
) -> Result<(), PostgresKernelError> {
    match digest_version {
        StandardLibraryDigestVersion::Version1 => {
            if !executables.is_empty() {
                return Err(invariant(
                    "version-one standard library snapshot must not carry executable records",
                ));
            }
        }
        StandardLibraryDigestVersion::Version2 => {
            if executables.is_empty() || executables.len() != catalogue.functions().len() {
                return Err(invariant(
                    "version-two standard library snapshot must carry one executable per catalogue function",
                ));
            }
            let mut function_ids = HashSet::with_capacity(executables.len());
            for executable in executables {
                if !function_ids.insert(executable.function()) {
                    return Err(invariant(
                        "version-two standard library snapshot must carry each catalogue function exactly once",
                    ));
                }
                let function =
                    catalogue
                        .function_by_id(executable.function())
                        .ok_or_else(|| {
                            invariant(
                                "standard executable function must exist in the standard catalogue",
                            )
                        })?;
                if function.current_revision() != executable.revision().id() {
                    return Err(invariant(
                        "standard catalogue function and executable current revision must agree",
                    ));
                }
                if executable.revision().function() != executable.function() {
                    return Err(invariant(
                        "standard executable revision function must agree with its executable",
                    ));
                }
                let domain_matches = match function.domain() {
                    FunctionDomain::Server => matches!(
                        executable.revision().artifact().kind(),
                        ExecutableArtifactKind::Server
                    ),
                    FunctionDomain::Client => matches!(
                        executable.revision().artifact().kind(),
                        ExecutableArtifactKind::Client
                    ),
                };
                if !domain_matches {
                    return Err(invariant(
                        "standard executable artifact kind must match its function domain",
                    ));
                }
                for (index, reference) in executable.references().iter().enumerate() {
                    let ordinal = u32::try_from(index).map_err(|_| {
                        invariant("standard executable reference ordinal must fit the u32 range")
                    })?;
                    if reference.ordinal() != ordinal
                        || reference.source_function() != executable.function()
                        || reference.source_revision() != executable.revision().id()
                    {
                        return Err(invariant(
                            "standard executable references must name the exact function revision with contiguous zero-based ordinals",
                        ));
                    }
                }
            }
        }
        _ => {
            return Err(invariant(
                "standard library digest version is not supported by PostgreSQL persistence",
            ));
        }
    }
    Ok(())
}

/// Persists the complete V2 standard executable facts: the immutable function
/// revisions, their server or client artifacts, the catalogue functions with
/// their resolved signatures, the ordered parameter records, and the ordered
/// reference sequences. Every row is written under the selected standard
/// revision.
async fn persist_standard_executable_facts(
    transaction: &Transaction<'_>,
    standard: &VerifiedStandardLibrarySnapshot,
) -> Result<(), PostgresKernelError> {
    validate_standard_executable_facts(
        standard.digest_version(),
        standard.catalogue(),
        standard.executables(),
    )?;
    for executable in standard.executables() {
        persist_standard_executable_fact(transaction, standard, executable).await?;
    }
    Ok(())
}

/// Persists one standard executable and all facts owned by its function
/// revision.
async fn persist_standard_executable_fact(
    transaction: &Transaction<'_>,
    standard: &VerifiedStandardLibrarySnapshot,
    executable: &StandardExecutable,
) -> Result<(), PostgresKernelError> {
    let catalogue = standard.catalogue();
    let function = catalogue
        .function_by_id(executable.function())
        .ok_or_else(|| {
            invariant("standard executable function must exist in the standard catalogue")
        })?;
    let revision = executable.revision();
    let artifact = revision.artifact();
    let standard_revision = standard.revision();
    let function_origin = origin(
        standard.origins(),
        DefinitionIdentity::Function(function.id()),
    )?;
    let schema = schema_for_name(catalogue, function.name())?;
    let revision_number = positive_i64(
        revision.revision_number(),
        "standard function revision number",
    )?;
    let artifact_version = positive_i32(
        artifact.version(),
        "standard function artifact format version",
    )?;
    let semantic_hash_version = semantic_hash_version(revision.semantic_hash_version())?;

    transaction
        .execute(
            "INSERT INTO _orna_kernel.standard_function_revisions
                (standard_library_revision_id, function_revision_id, function_id,
                 revision_number, declaration_source_unit_id, declaration_source_start,
                 declaration_source_end, declaration_content_hash, semantic_hash,
                 semantic_hash_version, language_version, hash_contract_version)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
            &[
                &bytes(standard_revision),
                &bytes(revision.id()),
                &bytes(revision.function()),
                &revision_number,
                &bytes(revision.declaration_origin().source_unit()),
                &i64::from(revision.declaration_origin().byte_start()),
                &i64::from(revision.declaration_origin().byte_end()),
                &digest(revision.declaration_content_hash()),
                &digest(revision.semantic_hash()),
                &semantic_hash_version,
                &revision.language_version(),
                &CONTRACT_VERSION,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    transaction
        .execute(
            "INSERT INTO _orna_kernel.standard_function_artifacts
                (standard_library_revision_id, function_revision_id, artifact_kind,
                 format, format_version, payload, content_hash, hash_algorithm,
                 hash_contract_version)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'sha256', $8)",
            &[
                &bytes(standard_revision),
                &bytes(revision.id()),
                &artifact_kind(artifact.kind()),
                &artifact.format(),
                &artifact_version,
                &artifact.payload(),
                &digest(artifact.content_hash()),
                &CONTRACT_VERSION,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    let (return_shape, return_kind, return_scalar, return_value_type) = match function.return_type()
    {
        FunctionReturn::Single(value) => {
            let columns = standard_resolved_type_columns(*value, true)?;
            (
                "single",
                Some(columns.kind),
                columns.scalar,
                columns.value_type.map(bytes),
            )
        }
        FunctionReturn::Rows(_) => {
            return Err(invariant(
                "standard catalogue functions with ROWS results are not supported by standard persistence",
            ));
        }
        FunctionReturn::Stream(_) => {
            return Err(invariant(
                "standard catalogue functions with STREAM results are not supported by standard persistence",
            ));
        }
    };
    transaction
        .execute(
            "INSERT INTO _orna_kernel.standard_catalogue_functions
                (standard_library_revision_id, function_id, schema_id, name_parts,
                 domain, security_mode, transaction_mode, volatility, return_shape,
                 return_type_kind, return_scalar_type, return_value_type_id,
                 current_function_revision_id, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
            &[
                &bytes(standard_revision),
                &bytes(function.id()),
                &bytes(schema),
                &function.name().parts(),
                &function_domain(function.domain()),
                &function_security(function.security()),
                &function_transaction(function.transaction())?,
                &function_volatility(function.volatility()),
                &return_shape,
                &return_kind,
                &return_scalar,
                &return_value_type,
                &bytes(function.current_revision()),
                &bytes(function_origin.source_unit()),
                &i64::from(function_origin.byte_start()),
                &i64::from(function_origin.byte_end()),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    for parameter in function.parameters() {
        let columns = standard_resolved_type_columns(parameter.resolved_type(), false)?;
        let parameter_origin = origin(
            standard.origins(),
            DefinitionIdentity::Parameter {
                owner: function.id(),
                parameter: parameter.id(),
            },
        )?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.standard_catalogue_function_parameters
                    (standard_library_revision_id, function_id, parameter_id, name,
                     ordinal, type_kind, scalar_type, value_type_id,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                &[
                    &bytes(standard_revision),
                    &bytes(function.id()),
                    &bytes(parameter.id()),
                    &parameter.name(),
                    &i64::from(parameter.ordinal()),
                    &columns.kind,
                    &columns.scalar,
                    &columns.value_type.map(bytes),
                    &bytes(parameter_origin.source_unit()),
                    &i64::from(parameter_origin.byte_start()),
                    &i64::from(parameter_origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }

    for reference in executable.references() {
        let (target_kind, target_definition_id, owner_type, owner_function, standard_pin) =
            standard_reference_target_columns(reference.target(), standard_revision)?;
        let reference_kind = reference_kind(reference.kind())?;
        let source = reference.source_origin();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.standard_definition_references
                    (standard_library_revision_id, function_revision_id, ordinal,
                     target_definition_id, target_kind, target_owner_type_id,
                     target_owner_function_id, target_standard_library_revision_id,
                     reference_kind, source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                &[
                    &bytes(standard_revision),
                    &bytes(reference.source_revision()),
                    &i64::from(reference.ordinal()),
                    &target_definition_id,
                    &target_kind,
                    &owner_type,
                    &owner_function,
                    &standard_pin,
                    &reference_kind,
                    &bytes(source.source_unit()),
                    &i64::from(source.byte_start()),
                    &i64::from(source.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    Ok(())
}

/// The closed resolved-type projection persisted for standard catalogue
/// functions and parameters. Standard signatures resolve to legacy scalars or
/// pinned standard value types; every other shape is rejected.
pub(super) struct StandardResolvedTypeColumns {
    pub(super) kind: &'static str,
    pub(super) scalar: Option<&'static str>,
    pub(super) value_type: Option<TypeId>,
}

pub(super) fn standard_resolved_type_columns(
    value: ResolvedType,
    allow_void: bool,
) -> Result<StandardResolvedTypeColumns, PostgresKernelError> {
    if let Some(value) = value.legacy_scalar() {
        return Ok(StandardResolvedTypeColumns {
            kind: "scalar",
            scalar: Some(scalar(value, allow_void)?),
            value_type: None,
        });
    }
    if let Some(value_type) = value.value_type() {
        return Ok(StandardResolvedTypeColumns {
            kind: "value",
            scalar: None,
            value_type: Some(value_type),
        });
    }
    Err(invariant(
        "standard catalogue resolved types must be scalar or standard value types",
    ))
}

/// The closed reference target projection persisted for standard definition
/// references. The standard pin is the row's own standard revision.
pub(super) type StandardReferenceTargetColumns = (
    &'static str,
    Vec<u8>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);

pub(super) fn standard_reference_target_columns(
    value: DefinitionReferenceTarget,
    standard_revision: StandardLibraryRevisionId,
) -> Result<StandardReferenceTargetColumns, PostgresKernelError> {
    Ok(match value {
        DefinitionReferenceTarget::ObjectType(id) => ("object_type", bytes(id), None, None, None),
        DefinitionReferenceTarget::Field { owner, field } => {
            ("field", bytes(field), Some(bytes(owner)), None, None)
        }
        DefinitionReferenceTarget::Function(id) => ("function", bytes(id), None, None, None),
        DefinitionReferenceTarget::Parameter { owner, parameter } => (
            "parameter",
            bytes(parameter),
            None,
            Some(bytes(owner)),
            None,
        ),
        DefinitionReferenceTarget::ValueType(id) => (
            "value_type",
            bytes(id),
            None,
            None,
            Some(bytes(standard_revision)),
        ),
        DefinitionReferenceTarget::Expression(id) => ("expression", bytes(id), None, None, None),
        _ => {
            return Err(invariant(
                "definition reference target is not supported by standard persistence",
            ));
        }
    })
}

/// Persists one target-authority row for every applied application catalogue
/// function and, for a version-two standard install, one standard authority
/// row under the new application catalogue revision. Apply is the only writer
/// of this relation; the migration backfilled the historical application
/// rows. A missing or duplicate authority row fails the apply closed.
pub(super) async fn persist_target_authorities(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    standard: Option<&VerifiedStandardLibrarySnapshot>,
) -> Result<(), PostgresKernelError> {
    let catalogue_revision = candidate.candidate().revision();
    let system_identity_functions = [
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID,
    ];
    let mut expected_authority_count =
        candidate.candidate().functions().len() as i64 + system_identity_functions.len() as i64;
    for function in candidate.candidate().functions() {
        transaction
            .execute(
                "INSERT INTO _orna_kernel.invocation_target_authorities
                    (catalogue_revision_id, function_id, target_class,
                     function_revision_id, standard_library_revision_id)
                 VALUES ($1, $2, 'application', $3, NULL)",
                &[
                    &bytes(catalogue_revision),
                    &bytes(function.id()),
                    &bytes(function.current_revision()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for function in system_identity_functions {
        if !system_function_by_id(function).is_some_and(is_admitted_security_identity) {
            return Err(invariant(
                "persisted system invocation target must be an admitted sealed security identity",
            ));
        }
        transaction
            .execute(
                "INSERT INTO _orna_kernel.invocation_target_authorities
                    (catalogue_revision_id, function_id, target_class,
                     function_revision_id, standard_library_revision_id)
                 VALUES ($1, $2, 'system', $2, NULL)",
                &[&bytes(catalogue_revision), &bytes(function)],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    if let Some(standard) = standard
        && standard.digest_version() == StandardLibraryDigestVersion::Version2
    {
        validate_standard_executable_facts(
            standard.digest_version(),
            standard.catalogue(),
            standard.executables(),
        )?;
        for executable in standard.executables() {
            transaction
                .execute(
                    "INSERT INTO _orna_kernel.invocation_target_authorities
                        (catalogue_revision_id, function_id, target_class,
                         function_revision_id, standard_library_revision_id)
                     VALUES ($1, $2, 'standard', $3, $4)",
                    &[
                        &bytes(catalogue_revision),
                        &bytes(executable.function()),
                        &bytes(executable.revision().id()),
                        &bytes(standard.revision()),
                    ],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            expected_authority_count += 1;
        }
    }
    let rows = transaction
        .query(
            "SELECT count(*) FROM _orna_kernel.invocation_target_authorities
             WHERE catalogue_revision_id = $1",
            &[&bytes(catalogue_revision)],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let written: i64 = rows[0].try_get(0).map_err(PostgresKernelError::Database)?;
    if written != expected_authority_count {
        return Err(invariant(
            "invocation target authority rows must match the applied catalogue functions and sealed system audit anchors exactly",
        ));
    }
    Ok(())
}

fn standard_digest_version(
    version: orna_core::revision::StandardLibraryDigestVersion,
) -> Result<i16, PostgresKernelError> {
    i16::try_from(version.to_u32())
        .map_err(|_| invariant("standard library digest version must fit PostgreSQL smallint"))
}

pub(super) fn standard_value_kind(
    value: ValueTypeKind,
) -> Result<&'static str, PostgresKernelError> {
    match value {
        ValueTypeKind::Primitive => Ok("primitive"),
        ValueTypeKind::Opaque => Ok("opaque"),
        _ => Err(invariant(
            "standard value type kind is not supported by PostgreSQL persistence",
        )),
    }
}

fn standard_value_mutability(
    value: ValueTypeMutability,
) -> Result<&'static str, PostgresKernelError> {
    if matches!(value, ValueTypeMutability::Immutable) {
        Ok("immutable")
    } else {
        Err(invariant(
            "standard value type mutability is not supported by PostgreSQL persistence",
        ))
    }
}

fn standard_value_persistence(
    value: ValueTypePersistence,
) -> Result<&'static str, PostgresKernelError> {
    if matches!(value, ValueTypePersistence::Persistable) {
        Ok("persistable")
    } else if matches!(value, ValueTypePersistence::Transient) {
        Ok("transient")
    } else {
        Err(invariant(
            "standard value type persistence is not supported by PostgreSQL persistence",
        ))
    }
}

fn standard_type_binding_name(
    kind: TypeBindingKind,
    name: &TypeLookupName,
) -> Result<(&'static str, Vec<String>), PostgresKernelError> {
    if matches!(kind, TypeBindingKind::Qualified) {
        if let TypeLookupName::Qualified(name) = name {
            return Ok(("qualified", name.parts().to_vec()));
        }
        return Err(invariant(
            "qualified standard type binding must retain a qualified name",
        ));
    }
    if matches!(kind, TypeBindingKind::Prelude) {
        if let TypeLookupName::Prelude(name) = name {
            return Ok(("prelude", name.words().to_vec()));
        }
        return Err(invariant(
            "prelude standard type binding must retain a prelude name",
        ));
    }
    Err(invariant(
        "standard type binding kind is not supported by PostgreSQL persistence",
    ))
}
