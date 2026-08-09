//! One atomic, fail-closed installation of a compiler deployable revision.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, SourceRevisionId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest, function_declaration_digest,
        source_bundle_digest, source_revision_digest, source_unit_content_digest,
    },
    catalogue::{
        FunctionDomain, FunctionReturn, FunctionSecurity, FunctionTransaction, FunctionVolatility,
        OnDeleteAction, QualifiedSemanticName,
    },
    physical::plan_physical_changes,
    revision::{
        ActiveDatabaseRevision, DefinitionIdentity, DefinitionOrigin, DefinitionReference,
        DefinitionReferenceKind, DefinitionReferenceTarget, DeployableRevision,
        FunctionRevisionRecord, RevisionPair, Sha256Digest, SourceOrigin,
    },
    types::{ResolvedType, StandardScalar},
};
use tokio_postgres::{Client, IsolationLevel, Transaction};

use crate::{
    PostgresKernel, PostgresKernelError,
    decode::{DurableRecord, identity_bytes},
    physical::{establish_trusted_search_path, install_physical_plan},
    recovery::recover_active_revision,
};

const ACTIVE_RELATION: &str = "_orna_kernel.active_revision";
const CONTRACT_VERSION: i16 = 1;

impl PostgresKernel {
    /// Installs a complete candidate revision as one atomic database change.
    pub async fn apply(
        &self,
        candidate: &DeployableRevision,
    ) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
        let mut session = self.open().await?;
        let apply_result = apply_client(&mut session.client, candidate).await;
        let shutdown_result = session.shutdown().await;
        match (apply_result, shutdown_result) {
            (Ok(active), Ok(())) => Ok(active),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
}

async fn apply_client(
    client: &mut Client,
    candidate: &DeployableRevision,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::ReadCommitted)
        .read_only(false)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;
    let result = apply_transaction(&transaction, candidate).await;
    match result {
        Ok(active) => transaction
            .commit()
            .await
            .map(|()| active)
            .map_err(PostgresKernelError::Database),
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(PostgresKernelError::Database(rollback)),
        },
    }
}

async fn apply_transaction(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    // This must remain the first statement. It prevents untrusted schemas from
    // changing the meaning of every later static query and DDL statement.
    establish_trusted_search_path(transaction).await?;
    let locked_pair = lock_active_pair(transaction).await?;
    let active = recover_active_revision(transaction).await?;
    if active.pair() != locked_pair {
        return Err(invariant(
            "locked active pair must recover as the same pair",
        ));
    }
    if candidate.expected_base() != active.pair() {
        return Err(PostgresKernelError::ExpectedBaseMismatch {
            expected: candidate.expected_base(),
            active: active.pair(),
        });
    }

    let materialized = materialize(candidate, &active)?;
    verify_candidate_hashes(candidate, &materialized)?;
    validate_postgres_encodings(candidate)?;

    let plan =
        plan_physical_changes(&active, candidate).map_err(PostgresKernelError::PhysicalPlan)?;
    install_physical_plan(transaction, &plan).await?;
    transaction
        .batch_execute("SET CONSTRAINTS ALL DEFERRED")
        .await
        .map_err(PostgresKernelError::Database)?;
    persist_candidate(transaction, candidate).await?;
    transaction
        .batch_execute("SET CONSTRAINTS ALL IMMEDIATE")
        .await
        .map_err(PostgresKernelError::Database)?;
    transition_revision_statuses(transaction, candidate, &active, &materialized).await?;
    verify_revision_statuses(transaction, &materialized).await?;
    update_active_pair(transaction, candidate, active.pair()).await?;

    let recovered = recover_active_revision(transaction).await?;
    if recovered.pair() != candidate.candidate_pair()
        || recovered.source().bundle_hash() != candidate.source().bundle_hash()
        || recovered.source().revision_hash() != candidate.source().revision_hash()
        || recovered.catalogue_hash() != candidate.catalogue_hash()
    {
        return Err(invariant(
            "post-apply recovery must exactly reproduce the candidate hashes",
        ));
    }
    Ok(recovered)
}

async fn lock_active_pair(
    transaction: &Transaction<'_>,
) -> Result<RevisionPair, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT singleton, source_revision_id, catalogue_revision_id
         FROM _orna_kernel.active_revision FOR UPDATE",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    if rows.len() != 1 {
        return Err(invariant(
            "exactly one active revision singleton must exist",
        ));
    }
    let record = DurableRecord::new(ACTIVE_RELATION, "singleton=true");
    let row = &rows[0];
    let singleton: bool =
        record.column(row, "singleton", "active singleton flag must be boolean")?;
    if !singleton {
        return Err(record.invariant("active singleton flag must be true"));
    }
    let source = SourceRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_revision_id",
            "active source identity must be exactly 16 bytes",
        )?,
        &record,
        "active source identity must be exactly 16 bytes",
    )?);
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "catalogue_revision_id",
            "active catalogue identity must be exactly 16 bytes",
        )?,
        &record,
        "active catalogue identity must be exactly 16 bytes",
    )?);
    Ok(RevisionPair::new(source, catalogue))
}

struct Materialized {
    current: Vec<FunctionRevisionRecord>,
}

fn materialize(
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
    ActiveDatabaseRevision::new_with_history(
        candidate.candidate_pair(),
        candidate.source().clone(),
        candidate.candidate().clone(),
        candidate.catalogue_hash(),
        candidate.expressions().to_vec(),
        current.clone(),
        historical.clone(),
        candidate.origins().to_vec(),
        candidate.references().to_vec(),
    )
    .map_err(PostgresKernelError::RevisionInvariant)?;
    Ok(Materialized { current })
}

fn verify_candidate_hashes(
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
    let digest = catalogue_digest(
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

fn declaration_bytes(
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

fn validate_postgres_encodings(candidate: &DeployableRevision) -> Result<(), PostgresKernelError> {
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
            let _ = type_columns(field.resolved_type(), false)?;
            let _ = on_delete(field.on_delete());
        }
    }
    for function in candidate.candidate().functions() {
        schema_for_name(candidate.candidate(), function.name())?;
        let _ = function_domain(function.domain());
        let _ = function_security(function.security());
        let _ = function_transaction(function.transaction())?;
        let _ = function_volatility(function.volatility());
        for parameter in function.parameters() {
            let _ = type_columns(parameter.resolved_type(), false)?;
        }
        match function.return_type() {
            FunctionReturn::Single(resolved) => {
                let _ = type_columns(*resolved, true)?;
            }
            FunctionReturn::Rows(columns) => {
                for column in columns {
                    let _ = type_columns(column.resolved_type(), false)?;
                }
            }
        }
    }
    for origin in candidate.origins() {
        validate_origin(origin.source())?;
    }
    for reference in candidate.references() {
        validate_origin(reference.source_origin())?;
        let _ = reference_target(reference.target());
        let _ = reference_kind(reference.kind())?;
    }
    Ok(())
}

fn positive_i32(value: u32, rule: &'static str) -> Result<i32, PostgresKernelError> {
    i32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| invariant(rule))
}
fn positive_i64(value: u64, rule: &'static str) -> Result<i64, PostgresKernelError> {
    i64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| invariant(rule))
}
fn validate_origin(origin: SourceOrigin) -> Result<(), PostgresKernelError> {
    if origin.byte_start() > origin.byte_end() {
        Err(invariant("source origin must be ordered"))
    } else {
        Ok(())
    }
}

async fn persist_candidate(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
) -> Result<(), PostgresKernelError> {
    let source = candidate.source();
    transaction
        .execute(
            "INSERT INTO _orna_kernel.source_bundles
                (id, content_hash, hash_algorithm, hash_contract_version)
             VALUES ($1, $2, 'sha256', $3)",
            &[
                &bytes(source.bundle()),
                &digest(source.bundle_hash()),
                &CONTRACT_VERSION,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    for unit in source.units() {
        transaction
            .execute(
                "INSERT INTO _orna_kernel.source_units
                    (id, bundle_id, ordinal, logical_path, content, content_hash,
                     hash_algorithm, encoding, hash_contract_version)
                 VALUES ($1, $2, $3, $4, $5, $6, 'sha256', 'utf-8', $7)",
                &[
                    &bytes(unit.id()),
                    &bytes(source.bundle()),
                    &i64::from(unit.ordinal()),
                    &unit.logical_path(),
                    &unit.content(),
                    &digest(unit.content_hash()),
                    &CONTRACT_VERSION,
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    transaction
        .execute(
            "INSERT INTO _orna_kernel.source_revisions
                (id, parent_source_revision_id, bundle_id, content_hash,
                 hash_algorithm, hash_contract_version)
             VALUES ($1, $2, $3, $4, 'sha256', $5)",
            &[
                &bytes(source.id()),
                &source.parent().map(bytes),
                &bytes(source.bundle()),
                &digest(source.revision_hash()),
                &CONTRACT_VERSION,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.catalogue_revisions
                (id, source_revision_id, parent_catalogue_revision_id, content_hash,
                 hash_algorithm, hash_contract_version)
             VALUES ($1, $2, $3, $4, 'sha256', $5)",
            &[
                &bytes(candidate.candidate().revision()),
                &bytes(source.id()),
                &bytes(candidate.parent_catalogue()),
                &digest(candidate.catalogue_hash()),
                &CONTRACT_VERSION,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    persist_semantics(transaction, candidate).await?;
    persist_revisions_and_references(transaction, candidate).await
}

async fn persist_semantics(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
) -> Result<(), PostgresKernelError> {
    let catalogue = candidate.candidate().revision();
    for schema in candidate.candidate().schemas() {
        let origin = origin(candidate.origins(), DefinitionIdentity::Schema(schema.id()))?;
        transaction.execute(
            "INSERT INTO _orna_kernel.catalogue_schemas
                (catalogue_revision_id, schema_id, name_parts, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &bytes(catalogue), &bytes(schema.id()), &schema.name().parts(),
                &bytes(origin.source_unit()), &i64::from(origin.byte_start()),
                &i64::from(origin.byte_end()),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    }
    for expression in candidate.expressions() {
        let expression_origin = origin(
            candidate.origins(),
            DefinitionIdentity::Expression(expression.id()),
        )?;
        let version = positive_i32(expression.version(), "expression format version")?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.catalogue_expressions
                (catalogue_revision_id, expression_id, format, format_version, payload,
                 content_hash, hash_algorithm, hash_contract_version, source_unit_id,
                 source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, 'sha256', $7, $8, $9, $10)",
                &[
                    &bytes(catalogue),
                    &bytes(expression.id()),
                    &expression.format(),
                    &version,
                    &expression.payload(),
                    &digest(expression.content_hash()),
                    &CONTRACT_VERSION,
                    &bytes(expression_origin.source_unit()),
                    &i64::from(expression_origin.byte_start()),
                    &i64::from(expression_origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for object in candidate.candidate().object_types() {
        let schema = schema_for_name(candidate.candidate(), object.name())?;
        let origin = origin(
            candidate.origins(),
            DefinitionIdentity::ObjectType(object.id()),
        )?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.catalogue_object_types
                (catalogue_revision_id, type_id, schema_id, name_parts,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[
                    &bytes(catalogue),
                    &bytes(object.id()),
                    &bytes(schema),
                    &object.name().parts(),
                    &bytes(origin.source_unit()),
                    &i64::from(origin.byte_start()),
                    &i64::from(origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for object in candidate.candidate().object_types() {
        for field in object.fields() {
            let (kind, scalar, target) = type_columns(field.resolved_type(), false)?;
            let delete = on_delete(field.on_delete());
            let origin = origin(
                candidate.origins(),
                DefinitionIdentity::Field {
                    owner: object.id(),
                    field: field.id(),
                },
            )?;
            transaction
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_fields
                    (catalogue_revision_id, owner_type_id, field_id, name, ordinal,
                     type_kind, scalar_type, target_type_id, nullable, is_unique,
                     default_expression_id, on_delete, source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
                    &[
                        &bytes(catalogue),
                        &bytes(object.id()),
                        &bytes(field.id()),
                        &field.name(),
                        &i64::from(field.ordinal()),
                        &kind,
                        &scalar,
                        &target.map(bytes),
                        &field.nullable(),
                        &field.unique(),
                        &field.default_expression().map(bytes),
                        &delete,
                        &bytes(origin.source_unit()),
                        &i64::from(origin.byte_start()),
                        &i64::from(origin.byte_end()),
                    ],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
        }
    }
    persist_functions(transaction, candidate).await
}

async fn persist_functions(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
) -> Result<(), PostgresKernelError> {
    let catalogue = candidate.candidate().revision();
    for function in candidate.candidate().functions() {
        let schema = schema_for_name(candidate.candidate(), function.name())?;
        let function_origin = origin(
            candidate.origins(),
            DefinitionIdentity::Function(function.id()),
        )?;
        let (shape, kind, scalar, target) = match function.return_type() {
            FunctionReturn::Single(value) => {
                let (k, s, t) = type_columns(*value, true)?;
                ("single", Some(k), s, t)
            }
            FunctionReturn::Rows(_) => ("rows", None, None, None),
        };
        transaction
            .execute(
                "INSERT INTO _orna_kernel.catalogue_functions
                (catalogue_revision_id, function_id, schema_id, name_parts, domain,
                 security_mode, transaction_mode, volatility, return_shape,
                 return_type_kind, return_scalar_type, return_target_type_id,
                 current_function_revision_id, source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
                &[
                    &bytes(catalogue),
                    &bytes(function.id()),
                    &bytes(schema),
                    &function.name().parts(),
                    &function_domain(function.domain()),
                    &function_security(function.security()),
                    &function_transaction(function.transaction())?,
                    &function_volatility(function.volatility()),
                    &shape,
                    &kind,
                    &scalar,
                    &target.map(bytes),
                    &bytes(function.current_revision()),
                    &bytes(function_origin.source_unit()),
                    &i64::from(function_origin.byte_start()),
                    &i64::from(function_origin.byte_end()),
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        for parameter in function.parameters() {
            let (kind, scalar, target) = type_columns(parameter.resolved_type(), false)?;
            let origin = origin(
                candidate.origins(),
                DefinitionIdentity::Parameter {
                    owner: function.id(),
                    parameter: parameter.id(),
                },
            )?;
            transaction
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_function_parameters
                    (catalogue_revision_id, function_id, parameter_id, name, ordinal,
                     type_kind, scalar_type, target_type_id, default_expression_id,
                     source_unit_id, source_start, source_end)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                    &[
                        &bytes(catalogue),
                        &bytes(function.id()),
                        &bytes(parameter.id()),
                        &parameter.name(),
                        &i64::from(parameter.ordinal()),
                        &kind,
                        &scalar,
                        &target.map(bytes),
                        &parameter.default_expression().map(bytes),
                        &bytes(origin.source_unit()),
                        &i64::from(origin.byte_start()),
                        &i64::from(origin.byte_end()),
                    ],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
        }
        if let FunctionReturn::Rows(columns) = function.return_type() {
            for column in columns {
                let (kind, scalar, target) = type_columns(column.resolved_type(), false)?;
                let origin = origin(
                    candidate.origins(),
                    DefinitionIdentity::FunctionReturnColumn {
                        owner: function.id(),
                        ordinal: column.ordinal(),
                    },
                )?;
                transaction
                    .execute(
                        "INSERT INTO _orna_kernel.catalogue_function_return_columns
                        (catalogue_revision_id, function_id, name, ordinal, type_kind,
                         scalar_type, target_type_id, source_unit_id, source_start, source_end)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                        &[
                            &bytes(catalogue),
                            &bytes(function.id()),
                            &column.name(),
                            &i64::from(column.ordinal()),
                            &kind,
                            &scalar,
                            &target.map(bytes),
                            &bytes(origin.source_unit()),
                            &i64::from(origin.byte_start()),
                            &i64::from(origin.byte_end()),
                        ],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?;
            }
        }
    }
    Ok(())
}

async fn persist_revisions_and_references(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
) -> Result<(), PostgresKernelError> {
    let catalogue = candidate.candidate().revision();
    for revision in candidate.new_function_revisions() {
        let version = positive_i64(revision.revision_number(), "function revision number")?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.function_revisions
                (id, introduced_catalogue_revision_id, function_id, revision_number,
                 content_hash, semantic_ir_hash, hash_algorithm, language_version,
                 status, hash_contract_version)
             VALUES ($1, $2, $3, $4, $5, $6, 'sha256', $7, 'candidate', $8)",
                &[
                    &bytes(revision.id()),
                    &bytes(catalogue),
                    &bytes(revision.function()),
                    &version,
                    &digest(revision.declaration_content_hash()),
                    &digest(revision.semantic_hash()),
                    &revision.language_version(),
                    &CONTRACT_VERSION,
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        let artifact = revision.artifact();
        let version = positive_i32(artifact.version(), "function artifact format version")?;
        transaction
            .execute(
                "INSERT INTO _orna_kernel.function_artifacts
                (function_revision_id, artifact_kind, format, format_version, payload,
                 content_hash, hash_algorithm, hash_contract_version)
             VALUES ($1, $2, $3, $4, $5, $6, 'sha256', $7)",
                &[
                    &bytes(revision.id()),
                    &artifact_kind(artifact.kind()),
                    &artifact.format(),
                    &version,
                    &artifact.payload(),
                    &digest(artifact.content_hash()),
                    &CONTRACT_VERSION,
                ],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for reference in candidate.references() {
        let (target, kind, owner_type, owner_function) = reference_columns(reference)?;
        let reference_kind = reference_kind(reference.kind())?;
        let source = reference.source_origin();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.definition_references
                (catalogue_revision_id, source_function_id, source_function_revision_id,
                 ordinal, target_definition_id, target_kind, target_owner_type_id,
                 target_owner_function_id, reference_kind, source_subobject_id,
                 source_unit_id, source_start, source_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NULL, $10, $11, $12)",
                &[
                    &bytes(catalogue),
                    &bytes(reference.source_function()),
                    &bytes(reference.source_revision()),
                    &i64::from(reference.ordinal()),
                    &target,
                    &kind,
                    &owner_type,
                    &owner_function,
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

async fn transition_revision_statuses(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    locked: &ActiveDatabaseRevision,
    materialized: &Materialized,
) -> Result<(), PostgresKernelError> {
    let new_current = materialized
        .current
        .iter()
        .map(FunctionRevisionRecord::id)
        .collect::<BTreeSet<_>>();
    for revision in locked.function_revisions() {
        if !new_current.contains(&revision.id()) {
            let updated = transaction
                .execute(
                    "UPDATE _orna_kernel.function_revisions
                     SET status = 'retired'
                     WHERE id = $1 AND status = 'active'",
                    &[&bytes(revision.id())],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            require_one(updated, "active function revision retirement")?;
        }
    }
    let new_ids = candidate
        .new_function_revisions()
        .iter()
        .map(FunctionRevisionRecord::id)
        .collect::<BTreeSet<_>>();
    for revision in &materialized.current {
        if new_ids.contains(&revision.id()) {
            let updated = transaction
                .execute(
                    "UPDATE _orna_kernel.function_revisions
                     SET status = 'active'
                     WHERE id = $1 AND status = 'candidate'",
                    &[&bytes(revision.id())],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            require_one(updated, "candidate function revision activation")?;
        } else if locked
            .historical_function_revisions()
            .iter()
            .any(|old| old.id() == revision.id())
        {
            let updated = transaction
                .execute(
                    "UPDATE _orna_kernel.function_revisions
                     SET status = 'active'
                     WHERE id = $1 AND status = 'retired'",
                    &[&bytes(revision.id())],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            require_one(updated, "historical function revision activation")?;
        }
    }
    Ok(())
}

async fn verify_revision_statuses(
    transaction: &Transaction<'_>,
    materialized: &Materialized,
) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT id, status FROM _orna_kernel.function_revisions ORDER BY id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let expected = materialized
        .current
        .iter()
        .map(FunctionRevisionRecord::id)
        .collect::<BTreeSet<_>>();
    let record = DurableRecord::new("_orna_kernel.function_revisions", "status sweep");
    let mut actual = BTreeSet::new();
    for row in rows {
        let id = FunctionRevisionId::from_bytes(identity_bytes(
            record.column(
                &row,
                "id",
                "function revision identity must be exactly 16 bytes",
            )?,
            &record,
            "function revision identity must be exactly 16 bytes",
        )?);
        let status: String = record.column(
            &row,
            "status",
            "function revision status must be active or retired after apply",
        )?;
        match status.as_str() {
            "active" => {
                actual.insert(id);
            }
            "retired" => {}
            _ => {
                return Err(record
                    .invariant("function revision status must be active or retired after apply"));
            }
        }
    }
    if actual != expected {
        return Err(record.invariant(
            "active function revision identities must exactly equal candidate current identities",
        ));
    }
    Ok(())
}

async fn update_active_pair(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
    expected: RevisionPair,
) -> Result<(), PostgresKernelError> {
    let updated = transaction
        .execute(
            "UPDATE _orna_kernel.active_revision
             SET source_revision_id = $1,
                 catalogue_revision_id = $2,
                 updated_at = transaction_timestamp()
             WHERE singleton = true
               AND source_revision_id = $3
               AND catalogue_revision_id = $4",
            &[
                &bytes(candidate.source().id()),
                &bytes(candidate.candidate().revision()),
                &bytes(expected.source()),
                &bytes(expected.catalogue()),
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    require_one(updated, "active revision pointer update")
}

fn require_one(value: u64, rule: &'static str) -> Result<(), PostgresKernelError> {
    if value == 1 {
        Ok(())
    } else {
        Err(invariant(rule))
    }
}
fn invariant(rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.apply",
        record: "candidate".into(),
        rule,
    }
}
fn bytes<I>(id: I) -> Vec<u8>
where
    I: IntoBytes,
{
    id.into_bytes().to_vec()
}
trait IntoBytes {
    fn into_bytes(self) -> [u8; 16];
}
macro_rules! id_bytes {
    ($($ty:ty),* $(,)?) => {
        $(
            impl IntoBytes for $ty {
                fn into_bytes(self) -> [u8; 16] {
                    self.to_bytes()
                }
            }
        )*
    };
}
id_bytes!(
    CatalogueRevisionId,
    ExpressionId,
    FieldId,
    FunctionId,
    FunctionRevisionId,
    ParameterId,
    SchemaId,
    SourceRevisionId,
    TypeId,
    orna_core::SourceBundleId,
    orna_core::SourceUnitId
);
fn digest(value: Sha256Digest) -> Vec<u8> {
    value.to_bytes().to_vec()
}
fn origin(
    origins: &[DefinitionOrigin],
    identity: DefinitionIdentity,
) -> Result<SourceOrigin, PostgresKernelError> {
    origins
        .iter()
        .find(|origin| origin.identity() == identity)
        .map(DefinitionOrigin::source)
        .ok_or_else(|| {
            invariant("every persisted semantic definition must have one candidate source origin")
        })
}
fn schema_for_name(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    name: &QualifiedSemanticName,
) -> Result<SchemaId, PostgresKernelError> {
    let namespace = name
        .parts()
        .get(..name.parts().len().saturating_sub(1))
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| invariant("qualified definition name must contain its schema namespace"))?;
    catalogue
        .schemas()
        .iter()
        .find(|schema| schema.name().parts() == namespace)
        .map(|schema| schema.id())
        .ok_or_else(|| invariant("definition schema namespace must resolve exactly"))
}
fn scalar(scalar: StandardScalar, allow_void: bool) -> Result<&'static str, PostgresKernelError> {
    match scalar {
        StandardScalar::Boolean => Ok("boolean"),
        StandardScalar::Integer => Ok("integer"),
        StandardScalar::BigInt => Ok("bigint"),
        StandardScalar::Float => Ok("float"),
        StandardScalar::Decimal => Ok("decimal"),
        StandardScalar::CharacterLargeObject => Ok("character_large_object"),
        StandardScalar::BinaryLargeObject => Ok("binary_large_object"),
        StandardScalar::Uuid => Ok("uuid"),
        StandardScalar::Date => Ok("date"),
        StandardScalar::Time => Ok("time"),
        StandardScalar::Timestamp => Ok("timestamp"),
        StandardScalar::Duration => Ok("duration"),
        StandardScalar::Void if allow_void => Ok("void"),
        StandardScalar::Void => Err(invariant("VOID is valid only as a SINGLE function return")),
    }
}
fn type_columns(
    value: ResolvedType,
    allow_void: bool,
) -> Result<(&'static str, Option<&'static str>, Option<TypeId>), PostgresKernelError> {
    match value {
        ResolvedType::Scalar(value) => Ok(("scalar", Some(scalar(value, allow_void)?), None)),
        ResolvedType::Named(value) => Ok(("named", None, Some(value))),
        ResolvedType::Reference { target } => Ok(("reference", None, Some(target))),
    }
}
fn on_delete(value: Option<OnDeleteAction>) -> Option<&'static str> {
    value.map(|value| match value {
        OnDeleteAction::Restrict => "restrict",
        OnDeleteAction::SetNull => "set_null",
        OnDeleteAction::Cascade => "cascade",
    })
}
fn function_domain(value: FunctionDomain) -> &'static str {
    match value {
        FunctionDomain::Server => "server",
        FunctionDomain::Client => "client",
    }
}
fn function_security(value: FunctionSecurity) -> &'static str {
    match value {
        FunctionSecurity::Invoker => "invoker",
        FunctionSecurity::Definer => "definer",
    }
}
fn function_transaction(
    value: Option<FunctionTransaction>,
) -> Result<Option<&'static str>, PostgresKernelError> {
    match value {
        None => Ok(None),
        Some(FunctionTransaction::Atomic) => Ok(Some("atomic")),
        Some(FunctionTransaction::ReadOnly) => Ok(Some("read_only")),
        Some(FunctionTransaction::Manual) => Err(invariant(
            "manual function transactions are not supported by PostgreSQL",
        )),
    }
}
fn function_volatility(value: FunctionVolatility) -> &'static str {
    match value {
        FunctionVolatility::Immutable => "immutable",
        FunctionVolatility::Stable => "stable",
        FunctionVolatility::Volatile => "volatile",
    }
}
fn artifact_kind(value: orna_core::revision::ExecutableArtifactKind) -> &'static str {
    match value {
        orna_core::revision::ExecutableArtifactKind::Server => "server_plan",
        orna_core::revision::ExecutableArtifactKind::Client => "client_bytecode",
    }
}
fn reference_kind(value: DefinitionReferenceKind) -> Result<&'static str, PostgresKernelError> {
    POSTGRES_REFERENCE_KINDS
        .iter()
        .find(|(kind, _)| *kind == value)
        .map(|(_, name)| *name)
        .ok_or_else(|| {
            invariant("definition reference kind is not supported by PostgreSQL persistence")
        })
}
const POSTGRES_REFERENCE_KINDS: &[(DefinitionReferenceKind, &str)] = &[
    (DefinitionReferenceKind::FunctionCall, "function_call"),
    (DefinitionReferenceKind::NamedType, "named_type"),
    (DefinitionReferenceKind::ObjectReference, "object_reference"),
    (DefinitionReferenceKind::ParameterRead, "parameter_read"),
    (DefinitionReferenceKind::QueryObject, "query_object"),
    (DefinitionReferenceKind::QueryField, "query_field"),
    (DefinitionReferenceKind::Expression, "expression"),
    (DefinitionReferenceKind::WriteObject, "write_object"),
    (DefinitionReferenceKind::WriteField, "write_field"),
];
type ReferenceTargetColumns = (&'static str, Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>);

fn reference_target(
    value: DefinitionReferenceTarget,
) -> Result<ReferenceTargetColumns, PostgresKernelError> {
    Ok(match value {
        DefinitionReferenceTarget::ObjectType(id) => ("object_type", bytes(id), None, None),
        DefinitionReferenceTarget::Field { owner, field } => {
            ("field", bytes(field), Some(bytes(owner)), None)
        }
        DefinitionReferenceTarget::Function(id) => ("function", bytes(id), None, None),
        DefinitionReferenceTarget::Parameter { owner, parameter } => {
            ("parameter", bytes(parameter), None, Some(bytes(owner)))
        }
        other => {
            let DefinitionReferenceTarget::Expression(id) = other else {
                return Err(invariant(
                    "definition reference target is not supported by PostgreSQL persistence",
                ));
            };
            ("expression", bytes(id), None, None)
        }
    })
}
type ReferenceInsertColumns = (Vec<u8>, &'static str, Option<Vec<u8>>, Option<Vec<u8>>);

fn reference_columns(
    reference: &DefinitionReference,
) -> Result<ReferenceInsertColumns, PostgresKernelError> {
    let (kind, target, owner_type, owner_function) = reference_target(reference.target())?;
    Ok((target, kind, owner_type, owner_function))
}

#[cfg(test)]
mod tests {
    use orna_core::{
        ExpressionId, FieldId, FunctionId, ParameterId, TypeId,
        catalogue::FunctionTransaction,
        revision::{DefinitionReferenceKind, DefinitionReferenceTarget, ExecutableArtifactKind},
        types::{ResolvedType, StandardScalar},
    };

    use super::{
        POSTGRES_REFERENCE_KINDS, artifact_kind, function_transaction, positive_i32, positive_i64,
        reference_kind, reference_target, scalar, type_columns,
    };

    #[test]
    fn scalar_encoder_uses_the_complete_stable_postgres_vocabulary() {
        let expected = [
            (StandardScalar::Boolean, "boolean"),
            (StandardScalar::Integer, "integer"),
            (StandardScalar::BigInt, "bigint"),
            (StandardScalar::Float, "float"),
            (StandardScalar::Decimal, "decimal"),
            (
                StandardScalar::CharacterLargeObject,
                "character_large_object",
            ),
            (StandardScalar::BinaryLargeObject, "binary_large_object"),
            (StandardScalar::Uuid, "uuid"),
            (StandardScalar::Date, "date"),
            (StandardScalar::Time, "time"),
            (StandardScalar::Timestamp, "timestamp"),
            (StandardScalar::Duration, "duration"),
        ];
        for (value, spelling) in expected {
            assert_eq!(scalar(value, false).expect("storable scalar"), spelling);
        }
        assert!(scalar(StandardScalar::Void, false).is_err());
        assert_eq!(
            scalar(StandardScalar::Void, true).expect("single VOID"),
            "void"
        );
    }

    #[test]
    fn type_encoder_preserves_closed_type_tuple_shapes() {
        let target = TypeId::from_bytes([3; 16]);
        assert_eq!(
            type_columns(ResolvedType::scalar(StandardScalar::Integer), false).unwrap(),
            ("scalar", Some("integer"), None)
        );
        assert_eq!(
            type_columns(ResolvedType::named(target), false).unwrap(),
            ("named", None, Some(target))
        );
        assert_eq!(
            type_columns(ResolvedType::reference(target), false).unwrap(),
            ("reference", None, Some(target))
        );
        assert!(type_columns(ResolvedType::scalar(StandardScalar::Void), false).is_err());
    }

    #[test]
    fn transaction_and_artifact_encoders_are_closed() {
        assert_eq!(function_transaction(None).unwrap(), None);
        assert_eq!(
            function_transaction(Some(FunctionTransaction::Atomic)).unwrap(),
            Some("atomic")
        );
        assert_eq!(
            function_transaction(Some(FunctionTransaction::ReadOnly)).unwrap(),
            Some("read_only")
        );
        assert!(function_transaction(Some(FunctionTransaction::Manual)).is_err());
        assert_eq!(artifact_kind(ExecutableArtifactKind::Server), "server_plan");
        assert_eq!(
            artifact_kind(ExecutableArtifactKind::Client),
            "client_bytecode"
        );
    }

    #[test]
    fn reference_encoder_keeps_owner_qualified_targets() {
        let object = TypeId::from_bytes([1; 16]);
        let field = FieldId::from_bytes([2; 16]);
        let function = FunctionId::from_bytes([3; 16]);
        let parameter = ParameterId::from_bytes([4; 16]);
        let expression = ExpressionId::from_bytes([5; 16]);
        assert_eq!(
            reference_target(DefinitionReferenceTarget::ObjectType(object))
                .unwrap()
                .0,
            "object_type"
        );
        let field_target = reference_target(DefinitionReferenceTarget::Field {
            owner: object,
            field,
        })
        .unwrap();
        assert_eq!(field_target.0, "field");
        assert_eq!(field_target.2, Some(object.to_bytes().to_vec()));
        assert_eq!(
            reference_target(DefinitionReferenceTarget::Function(function))
                .unwrap()
                .0,
            "function"
        );
        let parameter_target = reference_target(DefinitionReferenceTarget::Parameter {
            owner: function,
            parameter,
        })
        .unwrap();
        assert_eq!(parameter_target.0, "parameter");
        assert_eq!(parameter_target.3, Some(function.to_bytes().to_vec()));
        assert_eq!(
            reference_target(DefinitionReferenceTarget::Expression(expression))
                .unwrap()
                .0,
            "expression"
        );
        let expected_kinds = [
            (DefinitionReferenceKind::FunctionCall, "function_call"),
            (DefinitionReferenceKind::NamedType, "named_type"),
            (DefinitionReferenceKind::ObjectReference, "object_reference"),
            (DefinitionReferenceKind::ParameterRead, "parameter_read"),
            (DefinitionReferenceKind::QueryObject, "query_object"),
            (DefinitionReferenceKind::QueryField, "query_field"),
            (DefinitionReferenceKind::Expression, "expression"),
            (DefinitionReferenceKind::WriteObject, "write_object"),
            (DefinitionReferenceKind::WriteField, "write_field"),
        ];
        assert_eq!(POSTGRES_REFERENCE_KINDS, expected_kinds.as_slice());
        assert_eq!(
            reference_kind(DefinitionReferenceKind::FunctionCall).unwrap(),
            "function_call"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::NamedType).unwrap(),
            "named_type"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::ObjectReference).unwrap(),
            "object_reference"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::ParameterRead).unwrap(),
            "parameter_read"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::QueryObject).unwrap(),
            "query_object"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::QueryField).unwrap(),
            "query_field"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::Expression).unwrap(),
            "expression"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::WriteObject).unwrap(),
            "write_object"
        );
        assert_eq!(
            reference_kind(DefinitionReferenceKind::WriteField).unwrap(),
            "write_field"
        );
    }

    #[test]
    fn postgres_positive_integer_bounds_fail_closed() {
        assert_eq!(positive_i32(1, "test").unwrap(), 1);
        assert_eq!(positive_i32(i32::MAX as u32, "test").unwrap(), i32::MAX);
        assert!(positive_i32(0, "test").is_err());
        assert!(positive_i32(i32::MAX as u32 + 1, "test").is_err());
        assert_eq!(positive_i64(1, "test").unwrap(), 1);
        assert_eq!(positive_i64(i64::MAX as u64, "test").unwrap(), i64::MAX);
        assert!(positive_i64(0, "test").is_err());
        assert!(positive_i64(i64::MAX as u64 + 1, "test").is_err());
    }
}
