use super::*;

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_nested_record_field_targets_through_the_normal_apply_pipeline() -> TestResult<()>
{
    with_test_database(|database| async move {
        let kernel_instance = kernel(&database)?;
        kernel_instance.bootstrap().await?;
        let empty = kernel_instance.recover().await?;

        let schema_bundle =
            orna_core::source::SourceBundle::new([orna_core::source::SourceUnit::new(
                "main.orna",
                STANDARD_CLIENT_SCHEMA_SOURCE,
            )])?;
        let schema_report = check(&schema_bundle, empty.catalogue());
        require(
            schema_report.diagnostics().is_empty(),
            format!(
                "schema-only compiler diagnostics: {:?}",
                schema_report.diagnostics()
            ),
        )?;
        let schema_candidate = prepare(&schema_report, empty.pair(), &empty)?;
        let version_one = kernel_instance.apply(&schema_candidate).await?;

        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)?;
        let version_two = kernel_instance.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_client_candidate(NESTED_RECORD_APPLICATION_SOURCE, &version_two, &upgrade)?;

        let records = candidate.candidate().record_value_types();
        if !(records.len() == 2
            && records[0].name().to_string() == "app.outer"
            && records[1].name().to_string() == "app.inner")
        {
            return Err(failure(format!(
                "nested candidate did not preserve source declaration order: {:?}",
                records
                    .iter()
                    .map(|record| record.name().to_string())
                    .collect::<Vec<_>>()
            )));
        }
        let outer = &records[0];
        let inner = &records[1];
        let child = outer
            .fields()
            .iter()
            .find(|field| field.name() == "child")
            .ok_or_else(|| failure("outer record has no child field"))?;
        let TypeDescriptorKind::Named(target) = child.descriptor().kind() else {
            return Err(failure(
                "child field descriptor is not a resolved Named identity",
            ));
        };
        require(
            target == inner.id(),
            "child field does not target the exact inner application record identity",
        )?;

        let applied = kernel_instance.apply(&candidate).await?;
        require_applied_revision_matches_candidate(&applied, &candidate)?;
        let post_apply = snapshot_kernel_tables(&database).await?;

        let first_restart = kernel(&database)?.recover().await?;
        require_applied_revision_matches_candidate(&first_restart, &candidate)?;
        require(
            snapshot_kernel_tables(&database).await? == post_apply,
            "the first fresh recovery wrote kernel rows",
        )?;
        let second_restart = PostgresKernel::new(database.config()?).recover().await?;
        require_applied_revision_matches_candidate(&second_restart, &candidate)?;
        require(
            snapshot_kernel_tables(&database).await? == post_apply,
            "the second fresh recovery wrote kernel rows",
        )?;
        require_applied_revisions_equal(&first_restart, &second_restart)
    })
    .await
}

const NESTED_RECORD_APPLICATION_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.mode AS ENUM ('lead', 'customer');\n\
    CREATE TYPE app.outer AS VALUE (child app.inner) IMMUTABLE PERSISTABLE;\n\
    CREATE TYPE app.inner AS VALUE (flag BOOLEAN, stage app.mode) IMMUTABLE PERSISTABLE;\n";

fn same_members<T>(left: &[T], right: &[T]) -> bool
where
    T: Eq,
{
    if left.len() != right.len() {
        return false;
    }
    let mut unmatched = right.iter().collect::<Vec<_>>();
    for member in left {
        let Some(index) = unmatched.iter().position(|candidate| *candidate == member) else {
            return false;
        };
        unmatched.swap_remove(index);
    }
    unmatched.is_empty()
}

fn require_same_standard_context(
    left: &CatalogueHashContext,
    right: &CatalogueHashContext,
) -> TestResult<()> {
    let (
        CatalogueHashContext::Version2 { standard: left },
        CatalogueHashContext::Version2 { standard: right },
    ) = (left, right)
    else {
        return Err(failure(
            "recovered and candidate standard contexts are not both version two",
        ));
    };
    require(
        left.revision() == right.revision()
            && left.digest() == right.digest()
            && left.source() == right.source()
            && left.catalogue().revision() == right.catalogue().revision()
            && same_members(left.catalogue().schemas(), right.catalogue().schemas())
            && same_members(
                left.catalogue().value_types(),
                right.catalogue().value_types(),
            )
            && same_members(
                left.catalogue().enum_types(),
                right.catalogue().enum_types(),
            )
            && same_members(
                left.catalogue().type_bindings(),
                right.catalogue().type_bindings(),
            ),
        "recovered and candidate pinned standard identity or content differ",
    )
}

fn require_applied_revision_matches_candidate(
    active: &ActiveDatabaseRevision,
    candidate: &DeployableRevision,
) -> TestResult<()> {
    require(
        active.pair() == candidate.candidate_pair() && active.source() == candidate.source(),
        "post-apply recovery changed the candidate source pair",
    )?;
    require(
        active.catalogue().revision() == candidate.candidate().revision(),
        "post-apply recovery changed the candidate catalogue revision",
    )?;
    require(
        active.catalogue().schemas() == candidate.candidate().schemas(),
        format!(
            "post-apply recovery changed ordered schemas: {:?} vs {:?}",
            active.catalogue().schemas(),
            candidate.candidate().schemas(),
        ),
    )?;
    require(
        active.catalogue().object_types() == candidate.candidate().object_types(),
        format!(
            "post-apply recovery changed ordered object types: {:?} vs {:?}",
            active.catalogue().object_types(),
            candidate.candidate().object_types(),
        ),
    )?;
    require(
        active.catalogue().enum_types() == candidate.candidate().enum_types(),
        format!(
            "post-apply recovery changed ordered enum types: {:?} vs {:?}",
            active.catalogue().enum_types(),
            candidate.candidate().enum_types(),
        ),
    )?;
    {
        let active_records = active.catalogue().record_value_types();
        require(
            active_records
                .windows(2)
                .all(|pair| pair[0].id().to_bytes() <= pair[1].id().to_bytes()),
            "post-apply recovery did not emit record value types in canonical identity order",
        )?;
        let mut active_sorted = active_records.to_vec();
        let mut candidate_sorted = candidate.candidate().record_value_types().to_vec();
        active_sorted.sort_by_key(|record| record.id().to_bytes());
        candidate_sorted.sort_by_key(|record| record.id().to_bytes());
        require(
            active_sorted == candidate_sorted,
            format!(
                "post-apply recovery changed record value types in canonical identity order: {:?} vs {:?}",
                active_sorted, candidate_sorted,
            ),
        )?;
    }
    require(
        active.catalogue().value_types() == candidate.candidate().value_types(),
        format!(
            "post-apply recovery changed ordered value types: {:?} vs {:?}",
            active.catalogue().value_types(),
            candidate.candidate().value_types(),
        ),
    )?;
    require(
        active.catalogue().type_bindings() == candidate.candidate().type_bindings(),
        format!(
            "post-apply recovery changed ordered type bindings: {:?} vs {:?}",
            active.catalogue().type_bindings(),
            candidate.candidate().type_bindings(),
        ),
    )?;
    require(
        active.catalogue().functions() == candidate.candidate().functions(),
        format!(
            "post-apply recovery changed ordered functions: {:?} vs {:?}",
            active.catalogue().functions(),
            candidate.candidate().functions(),
        ),
    )?;
    require(
        active.catalogue_hash() == candidate.catalogue_hash()
            && active.expressions() == candidate.expressions()
            && same_members(active.origins(), candidate.origins())
            && same_members(active.references(), candidate.references())
            && active.function_revisions() == candidate.current_function_revisions().unwrap_or(&[]),
        "post-apply recovery changed candidate hashes, evidence, or function revisions",
    )?;
    require_same_standard_context(
        active.catalogue_hash_context(),
        candidate.catalogue_hash_context(),
    )
}

fn require_applied_revisions_equal(
    left: &ActiveDatabaseRevision,
    right: &ActiveDatabaseRevision,
) -> TestResult<()> {
    require(
        left.pair() == right.pair()
            && left.source() == right.source()
            && left.catalogue().revision() == right.catalogue().revision()
            && left.catalogue().schemas() == right.catalogue().schemas()
            && left.catalogue().object_types() == right.catalogue().object_types()
            && left.catalogue().enum_types() == right.catalogue().enum_types()
            && left.catalogue().record_value_types() == right.catalogue().record_value_types()
            && left.catalogue().value_types() == right.catalogue().value_types()
            && left.catalogue().type_bindings() == right.catalogue().type_bindings()
            && left.catalogue().functions() == right.catalogue().functions()
            && left.catalogue_hash() == right.catalogue_hash()
            && left.expressions() == right.expressions()
            && same_members(left.origins(), right.origins())
            && same_members(left.references(), right.references())
            && left.function_revisions() == right.function_revisions(),
        "two fresh kernels recovered different active revisions",
    )?;
    require_same_standard_context(
        left.catalogue_hash_context(),
        right.catalogue_hash_context(),
    )
}

struct NestedRecordPipeline {
    candidate: DeployableRevision,
    standard_revision: Vec<u8>,
}

async fn install_nested_record_pipeline(
    database: &TestDatabase,
    source: &str,
) -> TestResult<NestedRecordPipeline> {
    let kernel_instance = kernel(database)?;
    kernel_instance.bootstrap().await?;
    let empty = kernel_instance.recover().await?;
    let schema_bundle =
        orna_core::source::SourceBundle::new([orna_core::source::SourceUnit::new(
            "main.orna",
            STANDARD_CLIENT_SCHEMA_SOURCE,
        )])?;
    let schema_report = check(&schema_bundle, empty.catalogue());
    require(
        schema_report.diagnostics().is_empty(),
        format!(
            "schema-only compiler diagnostics: {:?}",
            schema_report.diagnostics()
        ),
    )?;
    let schema_candidate = prepare(&schema_report, empty.pair(), &empty)?;
    let version_one = kernel_instance.apply(&schema_candidate).await?;
    let upgrade = orna_standard::prepare_standard_upgrade(&version_one)?;
    let version_two = kernel_instance.apply_standard_upgrade(&upgrade).await?;
    let candidate = standard_client_candidate(source, &version_two, &upgrade)?;
    kernel_instance.apply(&candidate).await?;
    Ok(NestedRecordPipeline {
        candidate,
        standard_revision: upgrade
            .verified_standard_snapshot()
            .revision()
            .to_bytes()
            .to_vec(),
    })
}

fn field_where(owner_type_id: impl AsRef<[u8]>, field_id: impl AsRef<[u8]>) -> String {
    let owner_type_id = owner_type_id.as_ref();
    let field_id = field_id.as_ref();
    format!(
        "owner_type_id = {} AND field_id = {}",
        bytea_literal(owner_type_id),
        bytea_literal(field_id)
    )
}

fn find_record_field<'a>(
    candidate: &'a DeployableRevision,
    record_name: &str,
    field_name: &str,
) -> TestResult<(
    &'a RecordValueTypeDefinition,
    &'a RecordValueFieldDefinition,
)> {
    let record = candidate
        .candidate()
        .record_value_types()
        .iter()
        .find(|record| record.name().to_string() == record_name)
        .ok_or_else(|| failure(format!("candidate has no record {record_name}")))?;
    let field = record
        .fields()
        .iter()
        .find(|field| field.name() == field_name)
        .ok_or_else(|| failure(format!("record {record_name} has no field {field_name}")))?;
    Ok((record, field))
}

async fn drop_record_field_constraints(
    database: &TestDatabase,
    constraints: &[&str],
) -> TestResult<()> {
    let session = database.open().await?;
    for constraint in constraints {
        session
            .client()
            .batch_execute(&format!(
                "ALTER TABLE _orna_kernel.catalogue_record_value_fields
                 DROP CONSTRAINT {constraint}"
            ))
            .await?;
    }
    session.shutdown().await
}

async fn field_row_sans_columns(
    database: &TestDatabase,
    row_where: &str,
    excluded_columns: &[&str],
) -> TestResult<String> {
    let minus = excluded_columns
        .iter()
        .map(|column| format!(" - '{column}'"))
        .collect::<String>();
    let session = database.open().await?;
    let row = session
        .client()
        .query_one(
            &format!(
                "SELECT (to_jsonb(source){minus})::text
                 FROM _orna_kernel.catalogue_record_value_fields AS source
                 WHERE {row_where}"
            ),
            &[],
        )
        .await?;
    let value: String = row.try_get(0)?;
    session.shutdown().await?;
    Ok(value)
}

fn require_durable_rule(error: &PostgresKernelError, record: &str, rule: &str) -> TestResult<()> {
    match error {
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.catalogue_record_value_fields",
            record: actual_record,
            rule: actual_rule,
        } => require(
            actual_record.as_str() == record && *actual_rule == rule,
            format!(
                "unexpected durable record {actual_record:?}/rule {actual_rule:?}; expected {record:?}/{rule:?}"
            ),
        ),
        other => Err(failure(format!(
            "expected catalogue_record_value_fields durable invariant, got {other}"
        ))),
    }
}

fn require_revision_record_field_error(
    error: &PostgresKernelError,
    expected: &RevisionInvariantError,
) -> TestResult<()> {
    let PostgresKernelError::RevisionInvariant(actual) = error else {
        return Err(failure(format!(
            "expected revision invariant record field error, got {error}"
        )));
    };
    require(
        actual == expected,
        format!("revision invariant record field error differs: {actual:?}; expected {expected:?}"),
    )
}

async fn reject_nested_record_tamper(
    database: &TestDatabase,
    pipeline: &NestedRecordPipeline,
    row_where: &str,
    excluded_columns: &[&str],
    drops: &[&str],
    tamper: &str,
    expected_error: impl Fn(&PostgresKernelError) -> TestResult<()>,
) -> TestResult<()> {
    require_applied_revision_matches_candidate(
        &kernel(database)?.recover().await?,
        &pipeline.candidate,
    )?;
    let before = field_row_sans_columns(database, row_where, excluded_columns).await?;
    drop_record_field_constraints(database, drops).await?;
    run_single_row_statement(database, tamper).await?;
    let after = field_row_sans_columns(database, row_where, excluded_columns).await?;
    require(
        before == after,
        "tamper changed record field columns beyond the intended ones",
    )?;
    let post_tamper = snapshot_kernel_tables(database).await?;
    expected_error(&recovery_error(database).await?)?;
    expected_error(&recovery_error(database).await?)?;
    require(
        snapshot_kernel_tables(database).await? == post_tamper,
        "rejected record field tamper repaired durable kernel state",
    )?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_nested_record_target_null_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline = install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let where_clause = field_where(
            outer.id().to_bytes(),
            child.id().to_bytes(),
        );
        let record = format!(
            "owner={} field={}",
            outer.id().canonical(),
            child.id().canonical()
        );
        let rule = "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple";
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &["record_type_id"],
            &["cat_record_value_fields_type_check"],
            &format!("UPDATE _orna_kernel.catalogue_record_value_fields SET record_type_id = NULL WHERE {where_clause}"),
            |error| {
                require_durable_rule(error, &record, rule)?;
                require(
                    error.to_string()
                        == format!(
                            "durable invariant failed for _orna_kernel.catalogue_record_value_fields record {record}: {rule}"
                        )
                        && std::error::Error::source(error).is_none(),
                    format!(
                        "null record target error did not preserve its exact display: {error}"
                    ),
                )
            },
        )
        .await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_nested_record_mixed_with_application_enum_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline = install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let mode = pipeline
            .candidate
            .candidate()
            .enum_types()
            .iter()
            .find(|enum_type| enum_type.name().to_string() == "app.mode")
            .ok_or_else(|| failure("candidate has no app.mode enum"))?;
        let where_clause = field_where(
            outer.id().to_bytes(),
            child.id().to_bytes(),
        );
        let record = format!(
            "owner={} field={}",
            outer.id().canonical(),
            child.id().canonical()
        );
        let rule = "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple";
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &["enum_type_id"],
            &["cat_record_value_fields_type_check"],
            &format!(
                "UPDATE _orna_kernel.catalogue_record_value_fields
                 SET enum_type_id = {}
                 WHERE {where_clause}",
                bytea_literal(mode.id().to_bytes())
            ),
            |error| {
                require_durable_rule(error, &record, rule)?;
                require(
                    error.to_string()
                        == format!(
                            "durable invariant failed for _orna_kernel.catalogue_record_value_fields record {record}: {rule}"
                        )
                        && std::error::Error::source(error).is_none(),
                    format!(
                        "application-enum record target error did not preserve its exact display: {error}"
                    ),
                )
            },
        )
        .await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_nested_record_mixed_with_standard_enum_pin_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline = install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let where_clause = field_where(
            outer.id().to_bytes(),
            child.id().to_bytes(),
        );
        let record = format!(
            "owner={} field={}",
            outer.id().canonical(),
            child.id().canonical()
        );
        let rule = "record value field type columns must form one exact pinned standard value, application enum, pinned standard enum, or application record tuple";
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &["enum_standard_library_revision_id"],
            &["cat_record_value_fields_type_check"],
            &format!(
                "UPDATE _orna_kernel.catalogue_record_value_fields
                 SET enum_standard_library_revision_id = {}
                 WHERE {where_clause}",
                bytea_literal(&pipeline.standard_revision)
            ),
            |error| {
                require_durable_rule(error, &record, rule)?;
                require(
                    error.to_string()
                        == format!(
                            "durable invariant failed for _orna_kernel.catalogue_record_value_fields record {record}: {rule}"
                        )
                        && std::error::Error::source(error).is_none(),
                    format!(
                        "standard-enum pin record target error did not preserve its exact display: {error}"
                    ),
                )
            },
        )
        .await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_fifteen_byte_nested_record_target_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline =
            install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let where_clause = field_where(outer.id().to_bytes(), child.id().to_bytes());
        let record = format!(
            "owner={} field={}",
            outer.id().canonical(),
            child.id().canonical()
        );
        let rule = "record value field record identity must be null or 16 bytes";
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &["record_type_id"],
            &[
                "cat_record_value_fields_type_check",
                "cat_record_value_fields_record_type_id_length",
                "cat_record_value_fields_record_type_fk",
            ],
            &format!(
                "UPDATE _orna_kernel.catalogue_record_value_fields
                 SET record_type_id = '\\x{}'::bytea
                 WHERE {where_clause}",
                "ab".repeat(15)
            ),
            |error| {
                require_durable_rule(error, &record, rule)?;
                require(
                    error.to_string()
                        == format!(
                            "durable invariant failed for _orna_kernel.catalogue_record_value_fields record {record}: {rule}"
                        )
                        && std::error::Error::source(error).is_none(),
                    format!(
                        "fifteen-byte record target error did not preserve its exact display: {error}"
                    ),
                )
            },
        )
        .await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_unknown_nested_record_target_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline = install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let unknown = TypeId::from_bytes([0x7d; 16]).to_bytes().to_vec();
        let where_clause = field_where(
            outer.id().to_bytes(),
            child.id().to_bytes(),
        );
        let expected = RevisionInvariantError::UnsupportedRecordValueFieldType {
            record_value_type: outer.id(),
            field: child.id(),
            descriptor: TypeDescriptor::named(TypeId::from_bytes([0x7d; 16])),
        };
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &["record_type_id"],
            &[
                "cat_record_value_fields_type_check",
                "cat_record_value_fields_record_type_fk",
            ],
            &format!(
                "UPDATE _orna_kernel.catalogue_record_value_fields
                 SET record_type_id = {}
                 WHERE {where_clause}",
                bytea_literal(&unknown)
            ),
            |error| {
                match error {
                    PostgresKernelError::RevisionInvariant(inner) => {
                        require_revision_record_field_error(error, &expected)?;
                        require(
                            inner.to_string()
                                == "record value field has an unsupported resolved type"
                                && std::error::Error::source(inner).is_none(),
                            format!(
                                "unsupported record target error did not preserve its exact inner display: {error}"
                            ),
                        )
                    }
                    other => Err(failure(format!(
                        "unsupported record target error is not a revision invariant: {other}"
                    ))),
                }
            },
        )
        .await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_record_cycle_closing_edge_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline =
            install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        let (inner, flag) = find_record_field(&pipeline.candidate, "app.inner", "flag")?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let where_clause = field_where(inner.id().to_bytes(), flag.id().to_bytes());
        // The two-node cycle closes at the back edge of whichever record the
        // identity-sorted walk visits second. The compiler allocates the
        // fixture identities nondeterministically, so name the closing edge
        // from the actual pair without reproducing the production walk.
        let (closing_owner, closing_field, closing_target) =
            if inner.id().to_bytes() < outer.id().to_bytes() {
                (outer.id(), child.id(), inner.id())
            } else {
                (inner.id(), flag.id(), outer.id())
            };
        let expected = RevisionInvariantError::RecursiveRecordValueField {
            record_value_type: closing_owner,
            field: closing_field,
            nested_record_value_type: closing_target,
        };
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &[
                "type_kind",
                "record_type_id",
                "value_type_id",
                "value_standard_library_revision_id",
            ],
            &[],
            &format!(
                "UPDATE _orna_kernel.catalogue_record_value_fields
                 SET type_kind = 'record',
                     record_type_id = {},
                     value_type_id = NULL,
                     value_standard_library_revision_id = NULL
                 WHERE {where_clause}",
                bytea_literal(outer.id().to_bytes())
            ),
            |error| {
                require_revision_record_field_error(error, &expected)?;
                match error {
                    PostgresKernelError::RevisionInvariant(inner) => require(
                        inner.to_string() == "record value fields must not form a recursive cycle"
                            && std::error::Error::source(inner).is_none(),
                        format!(
                            "record cycle error did not preserve its exact inner display: {error}"
                        ),
                    ),
                    other => Err(failure(format!(
                        "record cycle error is not a revision invariant: {other}"
                    ))),
                }
            },
        )
        .await
    })
    .await
}

fn deep_record_source() -> String {
    let mut source = String::from("CREATE SCHEMA app;\n");
    for index in 0..=31 {
        source.push_str(&format!(
            "CREATE TYPE app.d{index} AS VALUE (next app.d{}) IMMUTABLE PERSISTABLE;\n",
            index + 1
        ));
    }
    source.push_str("CREATE TYPE app.d32 AS VALUE (leaf BOOLEAN) IMMUTABLE PERSISTABLE;\n");
    source.push_str("CREATE TYPE app.d33 AS VALUE (leaf BOOLEAN) IMMUTABLE PERSISTABLE;\n");
    source
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_record_nesting_depth_33_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        let pipeline = install_nested_record_pipeline(&database, &deep_record_source()).await?;
        let (d32, leaf) = find_record_field(&pipeline.candidate, "app.d32", "leaf")?;
        let d33 = pipeline
            .candidate
            .candidate()
            .record_value_types()
            .iter()
            .find(|record| record.name().to_string() == "app.d33")
            .ok_or_else(|| failure("candidate has no app.d33 record"))?;
        let where_clause = field_where(d32.id().to_bytes(), leaf.id().to_bytes());
        let expected = RevisionInvariantError::RecordValueNestingTooDeep {
            record_value_type: d32.id(),
            field: leaf.id(),
            nested_record_value_type: d33.id(),
            maximum: 32,
            actual: 33,
        };
        reject_nested_record_tamper(
            &database,
            &pipeline,
            &where_clause,
            &[
                "type_kind",
                "record_type_id",
                "value_type_id",
                "value_standard_library_revision_id",
            ],
            &[],
            &format!(
                "UPDATE _orna_kernel.catalogue_record_value_fields
                 SET type_kind = 'record',
                     record_type_id = {},
                     value_type_id = NULL,
                     value_standard_library_revision_id = NULL
                 WHERE {where_clause}",
                bytea_literal(d33.id().to_bytes())
            ),
            |error| {
                require_revision_record_field_error(error, &expected)?;
                match error {
                    PostgresKernelError::RevisionInvariant(inner) => require(
                        inner.to_string() == "record value nesting exceeds the maximum depth"
                            && std::error::Error::source(inner).is_none(),
                        format!(
                            "record nesting error did not preserve its exact inner display: {error}"
                        ),
                    ),
                    other => Err(failure(format!(
                        "record nesting error is not a revision invariant: {other}"
                    ))),
                }
            },
        )
        .await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_ambiguous_nested_record_identity_before_the_cycle_without_repair() -> TestResult<()>
{
    with_test_database(|database| async move {
        let pipeline =
            install_nested_record_pipeline(&database, NESTED_RECORD_APPLICATION_SOURCE).await?;
        require_applied_revision_matches_candidate(
            &kernel(&database)?.recover().await?,
            &pipeline.candidate,
        )?;
        let (inner, flag) = find_record_field(&pipeline.candidate, "app.inner", "flag")?;
        let (outer, child) = find_record_field(&pipeline.candidate, "app.outer", "child")?;
        let boolean = orna_standard::BOOLEAN_TYPE_ID;
        let revision = pipeline.candidate.candidate().revision().to_bytes().to_vec();
        let inner_id = inner.id().to_bytes().to_vec();
        let outer_id = outer.id().to_bytes().to_vec();
        let flag_id = flag.id().to_bytes().to_vec();
        let child_id = child.id().to_bytes().to_vec();
        let boolean_id = boolean.to_bytes().to_vec();

        let post_apply = snapshot_kernel_tables(&database).await?;
        let session = database.open().await?;
        let operation_result: TestResult<()> = async {
            session
                .client()
                .batch_execute("BEGIN; SET CONSTRAINTS ALL DEFERRED")
                .await?;
            let rename_count = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.catalogue_record_value_types
                     SET type_id = $1
                     WHERE catalogue_revision_id = $2 AND type_id = $3",
                    &[&boolean_id, &revision, &inner_id],
                )
                .await?;
            require(
                rename_count == 1,
                "inner record type rename must affect exactly one row",
            )?;
            let flag_count = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.catalogue_record_value_fields
                     SET type_kind = 'record',
                         record_type_id = $1,
                         value_type_id = NULL,
                         value_standard_library_revision_id = NULL
                     WHERE catalogue_revision_id = $2
                       AND owner_type_id = $3 AND field_id = $4",
                    &[&outer_id, &revision, &inner_id, &flag_id],
                )
                .await?;
            require(
                flag_count == 1,
                "inner flag tuple conversion must affect exactly one row",
            )?;
            let owner_count = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.catalogue_record_value_fields
                     SET owner_type_id = $1
                     WHERE catalogue_revision_id = $2 AND owner_type_id = $3",
                    &[&boolean_id, &revision, &inner_id],
                )
                .await?;
            require(
                owner_count == 2,
                "inner field owner rename must affect exactly two rows",
            )?;
            let child_count = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.catalogue_record_value_fields
                     SET record_type_id = $1
                     WHERE catalogue_revision_id = $2
                       AND owner_type_id = $3 AND field_id = $4",
                    &[&boolean_id, &revision, &outer_id, &child_id],
                )
                .await?;
            require(
                child_count == 1,
                "outer child target rename must affect exactly one row",
            )?;
            session.client().batch_execute("COMMIT").await?;
            Ok(())
        }
        .await;
        finish_session(
            operation_result,
            session.shutdown().await,
            "ambiguous nested record tamper",
        )?;
        let post_tamper = snapshot_kernel_tables(&database).await?;
        require(
            post_tamper != post_apply,
            "ambiguous nested record tamper did not change the durable kernel state",
        )?;
        let expected = RevisionInvariantError::AmbiguousRecordValueFieldType {
            record_value_type: outer.id(),
            field: child.id(),
            type_id: boolean,
        };
        let error = recovery_error(&database).await?;
        require_revision_record_field_error(&error, &expected)?;
        match &error {
            PostgresKernelError::RevisionInvariant(inner) => require(
                inner.to_string()
                    == "record field type is present in both application and standard catalogues"
                    && std::error::Error::source(inner).is_none(),
                format!(
                    "ambiguous record identity error did not preserve its exact inner display: {error}"
                ),
            ),
            other => Err(failure(format!(
                "ambiguous record identity error is not a revision invariant: {other}"
            ))),
        }?;
        require_revision_record_field_error(&recovery_error(&database).await?, &expected)?;
        require(
            snapshot_kernel_tables(&database).await? == post_tamper,
            "rejected ambiguous nested record fixture repaired durable kernel state",
        )?;
        Ok(())
    })
    .await
}
