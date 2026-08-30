use super::*;

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn decodes_an_exact_opaque_standard_row_before_detecting_digest_tamper() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let fixture = install_raw_v2_standard_revision(&database).await?;
        let standard_revision = fixture.standard.revision().to_bytes().to_vec();
        let void_type = orna_standard::VOID_TYPE_ID.to_bytes().to_vec();
        let session = database.open().await?;
        let operation: TestResult<()> = async {
            let invalid_persistence = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.standard_catalogue_value_types
                     SET value_kind = 'opaque', persistence = 'persistable'
                     WHERE standard_library_revision_id = $1 AND type_id = $2",
                    &[&standard_revision, &void_type],
                )
                .await
                .expect_err("persistable opaque standard row must be rejected");
            require(
                invalid_persistence.as_db_error().is_some_and(|error| {
                    error.code().code() == "23514"
                        && error.constraint() == Some("std_cat_value_types_opaque_contract_check")
                }),
                "persistable opaque standard row did not fail its exact database constraint",
            )?;
            let invalid_contract = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.standard_catalogue_value_types
                     SET value_kind = 'opaque', representation_contract = E'opaque\\ncontract'
                     WHERE standard_library_revision_id = $1 AND type_id = $2",
                    &[&standard_revision, &void_type],
                )
                .await
                .expect_err("non-printable opaque standard contract must be rejected");
            require(
                invalid_contract.as_db_error().is_some_and(|error| {
                    error.code().code() == "23514"
                        && error.constraint() == Some("std_cat_value_types_opaque_contract_check")
                }),
                "non-printable opaque standard contract did not fail its exact database constraint",
            )?;
            let updated = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.standard_catalogue_value_types
                     SET value_kind = 'opaque'
                     WHERE standard_library_revision_id = $1 AND type_id = $2
                       AND persistence = 'transient'",
                    &[&standard_revision, &void_type],
                )
                .await?;
            require(
                updated == 1,
                format!("opaque kind tamper changed {updated} rows"),
            )
        }
        .await;
        finish_session(
            operation,
            session.shutdown().await,
            "opaque standard row tamper",
        )?;

        let error = recovery_error(&database).await?;
        require_standard_library_digest_mismatch(&error, fixture.standard.revision().to_bytes())?;
        let session = database.open().await?;
        let operation: TestResult<()> = async {
            let row = session
                .client()
                .query_one(
                    "SELECT value_kind, persistence, representation_contract
                     FROM _orna_kernel.standard_catalogue_value_types
                     WHERE standard_library_revision_id = $1 AND type_id = $2",
                    &[&standard_revision, &void_type],
                )
                .await?;
            require(
                row.try_get::<_, String>(0)? == "opaque"
                    && row.try_get::<_, String>(1)? == "transient"
                    && row.try_get::<_, String>(2)? == "orna.kernel.value.void@1",
                "failed opaque recovery repaired or changed the durable row",
            )
        }
        .await;
        finish_session(
            operation,
            session.shutdown().await,
            "opaque standard row postcondition",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_the_offline_application_catalogue_identity_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        replace_active_catalogue_identity_with_offline_sentinel(&database).await?;

        let before = snapshot_kernel_tables(&database).await?;
        require(
            active_catalogue_identity(&database).await? == EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            "fixture did not retain the offline application catalogue identity",
        )?;

        let first = recovery_error(&database).await?;
        require_offline_application_catalogue_error(&first)?;
        require(
            active_catalogue_identity(&database).await? == EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            "the first rejected recovery changed the offline application catalogue identity",
        )?;
        require(
            snapshot_kernel_tables(&database).await? == before,
            "recovery repaired or wrote a table after the first sentinel rejection",
        )?;

        let second = recovery_error(&database).await?;
        require_offline_application_catalogue_error(&second)?;
        require(
            active_catalogue_identity(&database).await? == EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            "the repeated rejected recovery changed the offline application catalogue identity",
        )?;
        require(
            snapshot_kernel_tables(&database).await? == before,
            "recovery repaired or wrote a table after the repeated sentinel rejection",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_a_complete_raw_v2_standard_revision() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let expected = install_raw_v2_standard_revision(&database).await?;

        let recovered = kernel(&database)?.recover().await?;
        let standard = recovered
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("version-2 recovery returned no standard context"))?;
        let expected_boolean = expected
            .standard
            .catalogue()
            .value_types()
            .iter()
            .find(|value_type| {
                value_type.representation_contract() == "orna.kernel.value.boolean@1"
            })
            .ok_or_else(|| failure("retained standard fixture has no Boolean value type"))?;
        let application_origin = SourceOrigin::new(
            expected.application.unit.id(),
            0,
            u32::try_from(expected.application.unit.content().len())?,
        )?;

        require_standard_snapshot(standard, &expected.standard)?;
        require(
            standard
                .catalogue()
                .value_type_by_id(expected_boolean.id())
                .is_some_and(|value_type| {
                    value_type.representation_contract() == "orna.kernel.value.boolean@1"
                }),
            "the pinned recovered standard does not contain the Boolean value type",
        )?;
        let recovered_standard_reference = recovered.references().iter().find(|reference| {
            matches!(
                reference.target(),
                DefinitionReferenceTarget::ValueType(id) if id == expected_boolean.id()
            )
        });
        require(
            recovered_standard_reference.is_some_and(|reference| {
                reference.source_function() == expected.application.revisions[0].function()
                    && reference.source_revision() == expected.application.revisions[0].id()
                    && reference.ordinal() == 5
                    && reference.kind() == DefinitionReferenceKind::NamedType
                    && reference.source_origin() == application_origin
            }),
            "version-2 recovery did not return the exact standard ValueType reference",
        )?;
        require(
            recovered.pair().catalogue() == expected.application.catalogue.revision()
                && recovered.source().units() == [expected.application.unit.clone()],
            "version-2 recovery changed the active application pair or source",
        )?;
        require(
            recovered.catalogue().revision() == expected.application.catalogue.revision()
                && recovered.catalogue().schemas() == expected.application.catalogue.schemas()
                && recovered.catalogue().object_types()
                    == expected.application.catalogue.object_types()
                && recovered.catalogue().value_types()
                    == expected.application.catalogue.value_types()
                && recovered.catalogue().type_bindings()
                    == expected.application.catalogue.type_bindings()
                && recovered.catalogue().functions() == expected.application.catalogue.functions()
                && recovered.expressions() == [expected.application.expression.clone()]
                && recovered.function_revisions() == expected.revisions
                && recovered.references() == expected.application.references
                && recovered.origins() == expected.application.origins,
            "version-2 recovery changed application semantic facts",
        )?;
        require_raw_v2_value_slots(&recovered, &expected.standard)?;
        require_raw_v2_value_inventory(&recovered, &expected.standard)?;
        require(
            recovered
                .catalogue()
                .object_types()
                .iter()
                .flat_map(ObjectTypeDefinition::fields)
                .filter(|field| field.name().starts_with("scalar_"))
                .count()
                == 12,
            "version-2 raw fixture did not retain all twelve value fields",
        )
    })
    .await
}

#[test]
fn nonempty_standard_enum_fixture_matches_its_frozen_digest() -> TestResult<()> {
    let standard = verified_standard_enum_fixture()?;

    require(
        standard.digest().to_bytes() == FROZEN_STANDARD_ENUM_DIGEST,
        "verified standard enum fixture changed its frozen digest",
    )?;
    require(
        standard.catalogue().enum_types().len() == 1
            && standard.catalogue().type_bindings().len() == 1,
        "verified standard enum fixture changed its exact definition inventory",
    )
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_a_nonempty_standard_enum_and_binding_twice() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let initial = kernel.recover().await?;
        let expected = verified_standard_enum_fixture()?;
        insert_standard_snapshot(&database, &expected).await?;

        let context = CatalogueHashContext::version_two(expected.clone());
        let content_hash = catalogue_digest_with_context(
            &context,
            initial.catalogue(),
            initial.function_revisions(),
            initial.expressions(),
            initial.origins(),
            initial.references(),
        )?;
        let session = database.open().await?;
        let operation_result: TestResult<()> = async {
            session.client().batch_execute("BEGIN").await?;
            let updated = session
                .client()
                .execute(
                    "UPDATE _orna_kernel.catalogue_revisions
                     SET canonical_hash_version = 2,
                         standard_library_revision_id = $2,
                         content_hash = $3
                     WHERE id = $1",
                    &[
                        &initial.pair().catalogue().to_bytes().to_vec(),
                        &expected.revision().to_bytes().to_vec(),
                        &content_hash.to_bytes().to_vec(),
                    ],
                )
                .await?;
            require(
                updated == 1,
                "standard enum fixture did not update one catalogue",
            )?;
            session.client().batch_execute("COMMIT").await?;
            Ok(())
        }
        .await;
        finish_session(
            operation_result,
            session.shutdown().await,
            "standard enum catalogue pin",
        )?;

        let recovered = kernel.recover().await?;
        let actual = recovered
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("standard enum recovery returned no pinned standard"))?;
        require_standard_snapshot(actual, &expected)?;
        let expected_enum = expected
            .catalogue()
            .enum_types()
            .first()
            .ok_or_else(|| failure("standard enum fixture has no enum"))?;
        let expected_binding = expected
            .catalogue()
            .type_bindings()
            .first()
            .ok_or_else(|| failure("standard enum fixture has no binding"))?;
        let expected_origin =
            standard_origin(&expected, DefinitionIdentity::ValueType(expected_enum.id()))?;
        let session = database.open().await?;
        let operation_result: TestResult<()> = async {
            let enum_row = session
                .client()
                .query_one(
                    "SELECT type_id, schema_id, name_parts, labels,
                            source_unit_id, source_start, source_end
                     FROM _orna_kernel.standard_catalogue_enum_types
                     WHERE standard_library_revision_id = $1",
                    &[&expected.revision().to_bytes().to_vec()],
                )
                .await?;
            let binding_row = session
                .client()
                .query_one(
                    "SELECT type_binding_id, kind, name_parts, target_type_kind,
                            target_type_id, target_enum_type_id
                     FROM _orna_kernel.standard_catalogue_type_bindings
                     WHERE standard_library_revision_id = $1",
                    &[&expected.revision().to_bytes().to_vec()],
                )
                .await?;
            require(
                enum_row.try_get::<_, Vec<u8>>(0)? == expected_enum.id().to_bytes().to_vec()
                    && enum_row.try_get::<_, Vec<u8>>(1)?
                        == SchemaId::from_bytes([0xc4; 16]).to_bytes().to_vec()
                    && enum_row.try_get::<_, Vec<String>>(2)? == expected_enum.name().parts()
                    && enum_row.try_get::<_, Vec<String>>(3)? == expected_enum.labels()
                    && enum_row.try_get::<_, Vec<u8>>(4)?
                        == expected_origin.source_unit().to_bytes().to_vec()
                    && enum_row.try_get::<_, i64>(5)? == i64::from(expected_origin.byte_start())
                    && enum_row.try_get::<_, i64>(6)? == i64::from(expected_origin.byte_end()),
                "standard enum recovery fixture did not retain its exact durable row",
            )?;
            require(
                binding_row.try_get::<_, Vec<u8>>(0)? == expected_binding.id().to_bytes().to_vec()
                    && binding_row.try_get::<_, String>(1)? == "qualified"
                    && binding_row.try_get::<_, Vec<String>>(2)?
                        == ["std".to_owned(), "mode_alias".to_owned()]
                    && binding_row.try_get::<_, String>(3)? == "enum"
                    && binding_row.try_get::<_, Option<Vec<u8>>>(4)?.is_none()
                    && binding_row.try_get::<_, Option<Vec<u8>>>(5)?
                        == Some(expected_enum.id().to_bytes().to_vec()),
                "standard enum recovery fixture did not retain its exact enum binding tuple",
            )
        }
        .await;
        finish_session(
            operation_result,
            session.shutdown().await,
            "standard enum durable rows",
        )?;

        let repeated = kernel.recover().await?;
        let repeated_standard = repeated
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("repeated standard enum recovery returned no pin"))?;
        require_standard_snapshot(repeated_standard, &expected)
    })
    .await
}

fn require_raw_v2_value_inventory(
    revision: &orna_core::revision::ActiveDatabaseRevision,
    standard: &VerifiedStandardLibrarySnapshot,
) -> TestResult<()> {
    let mut value_ids = Vec::new();
    let mut legacy_scalar_slots = 0;
    let mut field_value_slots = 0;
    let mut parameter_value_slots = 0;
    let mut return_value_slots = 0;

    for object in revision.catalogue().object_types() {
        for field in object.fields() {
            let resolved = field.resolved_type();
            if let Some(value_type) = resolved.value_type() {
                value_ids.push(value_type);
                field_value_slots += 1;
            } else if resolved.legacy_scalar().is_some() {
                legacy_scalar_slots += 1;
            }
        }
    }
    for function in revision.catalogue().functions() {
        for parameter in function.parameters() {
            let resolved = parameter.resolved_type();
            if let Some(value_type) = resolved.value_type() {
                value_ids.push(value_type);
                parameter_value_slots += 1;
            } else if resolved.legacy_scalar().is_some() {
                legacy_scalar_slots += 1;
            }
        }
        match function.return_type() {
            FunctionReturn::Single(resolved) => {
                if let Some(value_type) = resolved.value_type() {
                    value_ids.push(value_type);
                    return_value_slots += 1;
                } else if resolved.legacy_scalar().is_some() {
                    legacy_scalar_slots += 1;
                }
            }
            FunctionReturn::Rows(columns) => {
                for column in columns {
                    let resolved = column.resolved_type();
                    if let Some(value_type) = resolved.value_type() {
                        value_ids.push(value_type);
                        return_value_slots += 1;
                    } else if resolved.legacy_scalar().is_some() {
                        legacy_scalar_slots += 1;
                    }
                }
            }
            FunctionReturn::Stream(_) => {}
        }
    }

    require(
        legacy_scalar_slots == 0,
        format!("raw V2 recovery retained {legacy_scalar_slots} legacy scalar slots"),
    )?;
    require(
        field_value_slots == 12,
        format!("raw V2 recovery returned {field_value_slots} value fields, expected 12"),
    )?;
    require(
        parameter_value_slots == 13,
        format!("raw V2 recovery returned {parameter_value_slots} value parameters, expected 13"),
    )?;
    require(
        return_value_slots == 2,
        format!("raw V2 recovery returned {return_value_slots} value return slots, expected 2"),
    )?;
    require(
        value_ids.len() == 27,
        format!(
            "raw V2 recovery returned {} value slots, expected 27",
            value_ids.len()
        ),
    )?;

    for (local_name, expected_count) in [
        ("boolean", 3),
        ("integer", 3),
        ("bigint", 2),
        ("float", 2),
        ("decimal", 2),
        ("character_large_object", 2),
        ("binary_large_object", 2),
        ("uuid", 2),
        ("date", 2),
        ("time", 2),
        ("timestamp", 2),
        ("duration", 2),
        ("void", 1),
    ] {
        let name = QualifiedSemanticName::new(["std", "types", local_name])?;
        let value_type = standard
            .catalogue()
            .value_type_by_name(&name)
            .ok_or_else(|| failure(format!("retained standard fixture has no {name} value")))?;
        let actual_count = value_ids
            .iter()
            .filter(|value_type_id| **value_type_id == value_type.id())
            .count();
        require(
            actual_count == expected_count,
            format!(
                "raw V2 recovery returned {actual_count} {name} value slots, expected {expected_count}"
            ),
        )?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_raw_v2_value_tuple_pin_and_definition_tampering_without_repair() -> TestResult<()>
{
    let field_record = format!(
        "owner={} field={}",
        TypeId::from_bytes([0x81; 16]).canonical(),
        FieldId::from_bytes([0x90; 16]).canonical(),
    );
    let cases = [
        (
            "ALTER TABLE _orna_kernel.catalogue_fields DISABLE TRIGGER ALL;
             UPDATE _orna_kernel.catalogue_fields
             SET value_type_id = decode(repeat('ee', 16), 'hex')
             WHERE owner_type_id = decode(repeat('81', 16), 'hex')
               AND field_id = decode(repeat('90', 16), 'hex')",
            "resolved value type must identify one value type in the selected pinned standard library",
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields DISABLE TRIGGER ALL;
             UPDATE _orna_kernel.catalogue_fields
             SET value_standard_library_revision_id = decode(repeat('ee', 16), 'hex')
             WHERE owner_type_id = decode(repeat('81', 16), 'hex')
               AND field_id = decode(repeat('90', 16), 'hex')",
            "resolved value type standard library revision must equal the selected catalogue pin",
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_check;
             ALTER TABLE _orna_kernel.catalogue_fields DISABLE TRIGGER ALL;
             UPDATE _orna_kernel.catalogue_fields
             SET scalar_type = 'boolean'
             WHERE owner_type_id = decode(repeat('81', 16), 'hex')
               AND field_id = decode(repeat('90', 16), 'hex')",
            "field type kind and identity columns must form one exact supported scalar, object, value, enum, or record tuple",
        ),
    ];
    for (statement, rule) in cases {
        let expected_record = field_record.clone();
        with_test_database(|database| async move {
            kernel(&database)?.bootstrap().await?;
            install_raw_v2_standard_revision(&database).await?;
            run_batch(&database, statement).await?;

            let before = snapshot_kernel_tables(&database).await?;
            let first = recovery_error(&database).await?;
            require_exact_raw_v2_error(&first, &expected_record, rule)?;
            require(
                snapshot_kernel_tables(&database).await? == before,
                "first raw V2 value recovery rejection changed a durable table",
            )?;

            let second = recovery_error(&database).await?;
            require_exact_raw_v2_error(&second, &expected_record, rule)?;
            require(
                snapshot_kernel_tables(&database).await? == before,
                "repeated raw V2 value recovery rejection changed a durable table",
            )
        })
        .await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_the_raw_standard_catalogue_offline_sentinel_without_repair() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_raw_v2_standard_revision(&database).await?;
        run_batch(
            &database,
            "UPDATE _orna_kernel.standard_library_revisions
             SET catalogue_revision_id = decode(repeat('00', 16), 'hex')",
        )
        .await?;

        let before = snapshot_kernel_tables(&database).await?;
        let first = recovery_error(&database).await?;
        require_offline_standard_catalogue_error(&first)?;
        require(
            snapshot_kernel_tables(&database).await? == before,
            "standard sentinel recovery changed a durable table after the first rejection",
        )?;

        let second = recovery_error(&database).await?;
        require_offline_standard_catalogue_error(&second)?;
        require(
            snapshot_kernel_tables(&database).await? == before,
            "standard sentinel recovery changed a durable table after the repeated rejection",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_exact_nonempty_source_for_an_empty_catalogue() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let expected = install_source_only_revision(&database, "schema source_only;\n").await?;

        let recovered = kernel(&database)?.recover().await?;
        let units = recovered.source().units();

        require(
            units == [expected],
            "recovery changed exact retained source",
        )?;
        require(
            recovered.catalogue().schemas().is_empty(),
            "source-only fixture recovered semantic definitions",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_tampered_source_only_ancestor_hash_without_changing_active_state() -> TestResult<()>
{
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_source_only_revision(&database, "schema parent;\n").await?;

        let session = database.open().await?;
        let operation_result: TestResult<RevisionPair> = async {
            let row = session
                .client()
                .query_one(
                    "SELECT
                        active.source_revision_id AS parent_source_id,
                        active.catalogue_revision_id AS parent_catalogue_id,
                        catalogue.content_hash AS catalogue_hash
                     FROM _orna_kernel.active_revision AS active
                     JOIN _orna_kernel.catalogue_revisions AS catalogue
                       ON catalogue.id = active.catalogue_revision_id
                     WHERE active.singleton = true",
                    &[],
                )
                .await?;
            let parent_source = SourceRevisionId::from_bytes(exact_identity(
                row.try_get("parent_source_id")?,
                "source-only parent source revision identity",
            )?);
            let parent_catalogue = CatalogueRevisionId::from_bytes(exact_identity(
                row.try_get("parent_catalogue_id")?,
                "source-only parent catalogue revision identity",
            )?);
            let catalogue_hash: Vec<u8> = row.try_get("catalogue_hash")?;
            let child_bundle = SourceBundleId::from_bytes([0x55; 16]);
            let child_source = SourceRevisionId::from_bytes([0x56; 16]);
            let child_catalogue = CatalogueRevisionId::from_bytes([0x57; 16]);
            let child_unit = StoredSourceUnit::new(
                SourceUnitId::from_bytes([0x54; 16]),
                0,
                "child-source.orna",
                "schema child;\n",
                source_unit_content_digest("schema child;\n")?,
            )?;
            let bundle_hash = source_bundle_digest(std::slice::from_ref(&child_unit))?;
            let source_hash =
                source_revision_record_digest(child_bundle, Some(parent_source), bundle_hash)?;

            session.client().batch_execute("BEGIN").await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.source_bundles (id, content_hash)
                     VALUES ($1, $2)",
                    &[
                        &child_bundle.to_bytes().to_vec(),
                        &bundle_hash.to_bytes().to_vec(),
                    ],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.source_units
                        (id, bundle_id, ordinal, logical_path, content, content_hash)
                     VALUES ($1, $2, $3, $4, $5, $6)",
                    &[
                        &child_unit.id().to_bytes().to_vec(),
                        &child_bundle.to_bytes().to_vec(),
                        &i64::from(child_unit.ordinal()),
                        &child_unit.logical_path(),
                        &child_unit.content(),
                        &child_unit.content_hash().to_bytes().to_vec(),
                    ],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.source_bundle_units
                        (bundle_id, source_unit_id, ordinal)
                     VALUES ($1, $2, $3)",
                    &[
                        &child_bundle.to_bytes().to_vec(),
                        &child_unit.id().to_bytes().to_vec(),
                        &i64::from(child_unit.ordinal()),
                    ],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.source_revisions
                        (id, parent_source_revision_id, bundle_id, content_hash)
                     VALUES ($1, $2, $3, $4)",
                    &[
                        &child_source.to_bytes().to_vec(),
                        &parent_source.to_bytes().to_vec(),
                        &child_bundle.to_bytes().to_vec(),
                        &source_hash.to_bytes().to_vec(),
                    ],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_revisions
                        (id, source_revision_id, parent_catalogue_revision_id, content_hash)
                     VALUES ($1, $2, $3, $4)",
                    &[
                        &child_catalogue.to_bytes().to_vec(),
                        &child_source.to_bytes().to_vec(),
                        &parent_catalogue.to_bytes().to_vec(),
                        &catalogue_hash,
                    ],
                )
                .await?;
            session
                .client()
                .execute(
                    "UPDATE _orna_kernel.active_revision
                     SET source_revision_id = $1, catalogue_revision_id = $2
                     WHERE singleton = true",
                    &[
                        &child_source.to_bytes().to_vec(),
                        &child_catalogue.to_bytes().to_vec(),
                    ],
                )
                .await?;
            session.client().batch_execute("COMMIT").await?;
            Ok(RevisionPair::new(child_source, child_catalogue))
        }
        .await;
        let active_before_tamper = finish_session(
            operation_result,
            session.shutdown().await,
            "source-only ancestry child fixture",
        )?;

        run_batch(
            &database,
            "UPDATE _orna_kernel.source_revisions
             SET content_hash = decode(repeat('ee', 32), 'hex')
             WHERE id = (
                 SELECT parent_source_revision_id
                 FROM _orna_kernel.source_revisions
                 WHERE id = (SELECT source_revision_id FROM _orna_kernel.active_revision)
             )",
        )
        .await?;
        let tampered_state = snapshot_kernel_tables(&database).await?;

        require_expected_error(
            recovery_error(&database).await?,
            ExpectedRecoveryError::Durable("_orna_kernel.source_revisions"),
        )?;
        require(
            active_revision_pair(&database).await? == active_before_tamper,
            "ancestor recovery rejection changed the active revision pair",
        )?;
        require(
            snapshot_kernel_tables(&database).await? == tampered_state,
            "ancestor recovery rejection changed durable state",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_tampered_catalogue_only_ancestor_hash_without_changing_active_state()
-> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_source_only_revision(&database, "schema parent;\n").await?;

        let session = database.open().await?;
        let operation_result: TestResult<RevisionPair> = async {
            let row = session
                .client()
                .query_one(
                    "SELECT
                        active.source_revision_id AS parent_source_id,
                        active.catalogue_revision_id AS parent_catalogue_id,
                        catalogue.content_hash AS catalogue_hash
                     FROM _orna_kernel.active_revision AS active
                     JOIN _orna_kernel.catalogue_revisions AS catalogue
                       ON catalogue.id = active.catalogue_revision_id
                     WHERE active.singleton = true",
                    &[],
                )
                .await?;
            let parent_source = SourceRevisionId::from_bytes(exact_identity(
                row.try_get("parent_source_id")?,
                "catalogue-only parent source revision identity",
            )?);
            let parent_catalogue = CatalogueRevisionId::from_bytes(exact_identity(
                row.try_get("parent_catalogue_id")?,
                "catalogue-only parent catalogue revision identity",
            )?);
            let catalogue_hash: Vec<u8> = row.try_get("catalogue_hash")?;
            let child_bundle = SourceBundleId::from_bytes([0x64; 16]);
            let child_source = SourceRevisionId::from_bytes([0x65; 16]);
            let child_catalogue = CatalogueRevisionId::from_bytes([0x66; 16]);
            let child_unit = StoredSourceUnit::new(
                SourceUnitId::from_bytes([0x63; 16]),
                0,
                "child-catalogue.orna",
                "schema child;\n",
                source_unit_content_digest("schema child;\n")?,
            )?;
            let bundle_hash = source_bundle_digest(std::slice::from_ref(&child_unit))?;
            let source_hash =
                source_revision_record_digest(child_bundle, Some(parent_source), bundle_hash)?;

            session.client().batch_execute("BEGIN").await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.source_bundles (id, content_hash)
                     VALUES ($1, $2)",
                    &[
                        &child_bundle.to_bytes().to_vec(),
                        &bundle_hash.to_bytes().to_vec(),
                    ],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.source_units
                        (id, bundle_id, ordinal, logical_path, content, content_hash)
                     VALUES ($1, $2, $3, $4, $5, $6)",
                    &[
                        &child_unit.id().to_bytes().to_vec(),
                        &child_bundle.to_bytes().to_vec(),
                        &i64::from(child_unit.ordinal()),
                        &child_unit.logical_path(),
                        &child_unit.content(),
                        &child_unit.content_hash().to_bytes().to_vec(),
                    ],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.source_bundle_units
                        (bundle_id, source_unit_id, ordinal)
                     VALUES ($1, $2, $3)",
                    &[
                        &child_bundle.to_bytes().to_vec(),
                        &child_unit.id().to_bytes().to_vec(),
                        &i64::from(child_unit.ordinal()),
                    ],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.source_revisions
                        (id, parent_source_revision_id, bundle_id, content_hash)
                     VALUES ($1, $2, $3, $4)",
                    &[
                        &child_source.to_bytes().to_vec(),
                        &parent_source.to_bytes().to_vec(),
                        &child_bundle.to_bytes().to_vec(),
                        &source_hash.to_bytes().to_vec(),
                    ],
                )
                .await?;
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.catalogue_revisions
                        (id, source_revision_id, parent_catalogue_revision_id, content_hash)
                     VALUES ($1, $2, $3, $4)",
                    &[
                        &child_catalogue.to_bytes().to_vec(),
                        &child_source.to_bytes().to_vec(),
                        &parent_catalogue.to_bytes().to_vec(),
                        &catalogue_hash,
                    ],
                )
                .await?;
            session
                .client()
                .execute(
                    "UPDATE _orna_kernel.active_revision
                     SET source_revision_id = $1, catalogue_revision_id = $2
                     WHERE singleton = true",
                    &[
                        &child_source.to_bytes().to_vec(),
                        &child_catalogue.to_bytes().to_vec(),
                    ],
                )
                .await?;
            session.client().batch_execute("COMMIT").await?;
            Ok(RevisionPair::new(child_source, child_catalogue))
        }
        .await;
        let active_before_tamper = finish_session(
            operation_result,
            session.shutdown().await,
            "catalogue-only ancestry child fixture",
        )?;

        run_batch(
            &database,
            "UPDATE _orna_kernel.catalogue_revisions
             SET content_hash = decode(repeat('ee', 32), 'hex')
             WHERE id = (
                 SELECT parent_catalogue_revision_id
                 FROM _orna_kernel.catalogue_revisions
                 WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
             )",
        )
        .await?;
        let tampered_state = snapshot_kernel_tables(&database).await?;

        require_expected_error(
            recovery_error(&database).await?,
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
        )?;
        require(
            active_revision_pair(&database).await? == active_before_tamper,
            "catalogue ancestor recovery rejection changed the active revision pair",
        )?;
        require(
            snapshot_kernel_tables(&database).await? == tampered_state,
            "catalogue ancestor recovery rejection changed durable state",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_an_exact_schema_and_its_unicode_source_origin() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let expected = install_schema_revision(&database).await?;

        let recovered = kernel(&database)?.recover().await?;

        require(
            recovered.source().units() == [expected.unit],
            "schema recovery changed exact retained source",
        )?;
        require(
            recovered.catalogue().schemas() == [expected.schema],
            "schema recovery changed the exact schema definition",
        )?;
        require(
            recovered.origins() == [expected.origin],
            "schema recovery changed the exact source origin",
        )?;
        require(
            recovered.catalogue().object_types().is_empty()
                && recovered.catalogue().functions().is_empty(),
            "schema recovery invented later catalogue members",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_complete_objects_fields_references_and_expression_origins() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let expected = install_object_revision(&database, false).await?;

        let recovered = kernel(&database)?.recover().await?;

        require(
            recovered.source().units() == [expected.unit],
            "object recovery changed exact retained Unicode source",
        )?;
        require(
            recovered.catalogue().revision() == expected.catalogue.revision()
                && recovered.catalogue().schemas() == expected.catalogue.schemas()
                && recovered.catalogue().object_types() == expected.catalogue.object_types()
                && recovered.catalogue().functions() == expected.catalogue.functions(),
            "object recovery changed object, owner-qualified field, or reference semantics",
        )?;
        require(
            recovered.expressions() == [expected.expression],
            "object recovery changed the expression artifact",
        )?;
        require(
            recovered.origins() == expected.origins,
            "object recovery changed exact Unicode definition origins",
        )?;
        let objects = recovered.catalogue().object_types();
        require(
            objects.len() == 3
                && objects[0].fields().len() == 17
                && objects[1].fields().len() == 1
                && objects[2].fields().is_empty(),
            "object recovery changed owner-qualified field grouping",
        )?;
        require(
            objects[0].fields()[12].id() == objects[1].fields()[0].id(),
            "duplicate field identities across owners did not remain owner-qualified",
        )?;
        require(
            objects[0].fields()[12].is_required_unique_reference()
                && objects[0].fields()[12].resolved_type()
                    == ResolvedType::reference(objects[1].id()),
            "object recovery changed the required unique reference shape or target",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn reconstructs_shared_expression_defaults_before_physical_rejection() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_object_revision(&database, true).await?;

        require_expected_error(
            recovery_error(&database).await?,
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_compiler_deployable_server_and_client_function_state() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let expected = install_function_revision(&database).await?;

        let recovered = kernel(&database)?.recover().await?;

        require(
            recovered.catalogue().functions() == expected.catalogue.functions(),
            "function recovery changed signatures, modifiers, parameters, or returns",
        )?;
        require(
            recovered.function_revisions() == expected.revisions,
            "function recovery changed current immutable revision records",
        )?;
        require(
            recovered.historical_function_revisions().is_empty(),
            "function recovery invented historical revisions",
        )?;
        require(
            recovered.references() == expected.references,
            "function recovery changed ordered owner-qualified references",
        )?;
        require(
            recovered.origins() == expected.origins,
            "function recovery changed exact current definition origins",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_raw_call_denies_security_definer_before_execution() -> TestResult<()> {
    const SERVICE_UID: u32 = 61_031;
    const SERVICE: PrincipalId = PrincipalId::from_bytes([0xd0; 16]);

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let fixture = install_function_revision(&database).await?;
        let active = kernel.recover().await?;
        let target = fixture
            .catalogue
            .functions()
            .iter()
            .find(|definition| {
                definition.domain() == FunctionDomain::Server
                    && definition.security() == FunctionSecurity::Definer
            })
            .ok_or_else(|| failure("raw SECURITY DEFINER fixture target is missing"))?;
        require(
            active.catalogue().function_by_id(target.id()) == Some(target),
            "raw SECURITY DEFINER target was not recovered from the active catalogue",
        )?;

        let security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            active
                .catalogue()
                .functions()
                .iter()
                .map(|definition| definition.id())
                .collect(),
            vec![Principal::new(
                SERVICE,
                PrincipalKind::Service,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(SERVICE, target.id())],
            vec![LocalPeerCredential::new(SERVICE_UID, SERVICE)],
        )?;
        kernel.replace_security_snapshot(&security).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        let data_relation = format!("_orna_data.t_{}", raw_id_hex([0x83; 16]));
        run_single_row_statement(
            &database,
            &format!(
                "INSERT INTO {data_relation} (_orna_object_id) VALUES (decode(repeat('ee', 16), 'hex'))"
            ),
        )
        .await?;
        let before = data_row_count(&database, &data_relation).await?;
        let denied = kernel
            .dispatch_authenticated_raw_call(&session, target.id())
            .await
            .expect_err("an authenticated raw SECURITY DEFINER call must deny");
        require(
            matches!(
                denied,
                PostgresKernelError::RawExecuteDenied {
                    pair,
                    function,
                    reason: ExecuteDenial::UnsupportedSecurityDefiner,
                } if pair == active.pair() && function == target.id()
            ),
            "raw SECURITY DEFINER dispatch returned the wrong denial",
        )?;
        let after = data_row_count(&database, &data_relation).await?;
        require(
            before == after,
            "raw SECURITY DEFINER denial changed application data",
        )?;

        let execute = kernel
            .recover_security_audit_events()
            .await?
            .into_iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
            .collect::<Vec<_>>();
        require(
            execute.len() == 1,
            "raw SECURITY DEFINER denial must append exactly one EXECUTE audit",
        )?;
        require_execute_audit(
            &execute[0],
            SecurityAuditOutcome::Denied,
            SERVICE,
            None,
            None,
            InvocationTarget::new(target.id(), active.pair()),
            Some(ExecuteDenial::UnsupportedSecurityDefiner),
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticated_resource_denies_recovered_security_definer_before_execution()
-> TestResult<()> {
    const SERVICE_UID: u32 = 61_032;
    const SERVICE: PrincipalId = PrincipalId::from_bytes([0xd4; 16]);
    const RAW_MARKER: &str = "resource-security-definer-secret";

    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let fixture = install_function_revision(&database).await?;
        let active = kernel.recover().await?;
        let target = fixture
            .catalogue
            .functions()
            .iter()
            .find(|definition| {
                definition.domain() == FunctionDomain::Server
                    && definition.security() == FunctionSecurity::Definer
            })
            .ok_or_else(|| failure("resource SECURITY DEFINER fixture target is missing"))?;
        require(
            active.catalogue().function_by_id(target.id()) == Some(target),
            "resource SECURITY DEFINER target was not recovered from the active catalogue",
        )?;
        let parameter = target
            .parameters()
            .first()
            .ok_or_else(|| failure("resource SECURITY DEFINER target has no fixture parameter"))?
            .id();

        let security = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            active
                .catalogue()
                .functions()
                .iter()
                .map(|definition| definition.id())
                .collect(),
            vec![Principal::new(
                SERVICE,
                PrincipalKind::Service,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(SERVICE, target.id())],
            vec![LocalPeerCredential::new(SERVICE_UID, SERVICE)],
        )?;
        kernel.replace_security_snapshot(&security).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;
        run_single_row_statement(
            &database,
            &format!(
                "INSERT INTO _orna_kernel.invocation_audit_events\n                    (event_id, invocation_id, outcome, session_principal_id)\n                 VALUES ({event_id}, {parent_invocation}, 'denied', {principal})",
                event_id = bytea_literal([0xe7; 16]),
                parent_invocation = bytea_literal([0xe5; 16]),
                principal = bytea_literal(SERVICE.to_bytes()),
            ),
        )
        .await?;

        let data_relation = format!("_orna_data.t_{}", raw_id_hex([0x83; 16]));
        run_single_row_statement(
            &database,
            &format!(
                "INSERT INTO {data_relation} (_orna_object_id) VALUES (decode(repeat('ee', 16), 'hex'))"
            ),
        )
        .await?;
        let before = data_row_count(&database, &data_relation).await?;
        let request = ResourceRequest {
            stream_id: 92,
            request_id: InvocationId::from_bytes([0xe4; 16]),
            parent_invocation_id: InvocationId::from_bytes([0xe5; 16]),
            call_site_id: CallSiteId::from_bytes([0xe6; 16]),
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: target.id(),
            target_revision: active.pair(),
            generation: 1,
            resource_kind: ResourceKind::Single,
            arguments: vec![ResourceArgument {
                parameter,
                value: RuntimeValue::Text(RAW_MARKER.into()),
            }],
            item_window: 1,
            byte_window: 1024,
        };
        let denied = kernel
            .dispatch_authenticated_server_resource(&session, &request)
            .await?;
        require(
            denied
                == orna_postgres::AuthenticatedServerResourceResult::Failed {
                    stream_id: request.stream_id,
                    request_id: request.request_id,
                    failure: CallFailure::ExecuteDenied,
                },
            "authenticated resource SECURITY DEFINER dispatch was not denied before execution",
        )?;
        let after = data_row_count(&database, &data_relation).await?;
        require(
            before == after,
            "authenticated resource SECURITY DEFINER denial changed application data",
        )?;

        let execute = kernel
            .recover_security_audit_events()
            .await?
            .into_iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
            .collect::<Vec<_>>();
        require(
            execute.len() == 1,
            "authenticated resource SECURITY DEFINER denial must append exactly one EXECUTE audit",
        )?;
        require_execute_audit(
            &execute[0],
            SecurityAuditOutcome::Denied,
            SERVICE,
            None,
            None,
            InvocationTarget::new(target.id(), active.pair()),
            Some(ExecuteDenial::UnsupportedSecurityDefiner),
        )?;

        let audit_session = database.open().await?;
        let audit_operation: TestResult<()> = async {
            let row = audit_session
                .client()
                .query_one(
                    "SELECT resource.nested_invocation_id,
                            resource.target_function_id,
                            resource.source_revision_id,
                            resource.catalogue_revision_id,
                            resource.decision_outcome,
                            resource.terminal_outcome,
                            invocation.outcome AS invocation_outcome,
                            row_to_json(resource)::text AS resource_json,
                            row_to_json(invocation)::text AS invocation_json,
                            (SELECT count(*)
                               FROM _orna_kernel.resource_audit_events
                              WHERE request_id = $1) AS resource_count,
                            (SELECT count(*)
                               FROM _orna_kernel.invocation_audit_events
                              WHERE invocation_id = resource.nested_invocation_id) AS invocation_count,
                            (SELECT count(*)
                               FROM _orna_kernel.resource_request_history
                              WHERE request_id = $1) AS history_count
                     FROM _orna_kernel.resource_audit_events AS resource
                     LEFT JOIN _orna_kernel.invocation_audit_events AS invocation
                       ON invocation.invocation_id = resource.nested_invocation_id
                    WHERE resource.request_id = $1",
                    &[&request.request_id.to_bytes().to_vec()],
                )
                .await?;
            let nested_invocation_id: Option<Vec<u8>> = row.try_get("nested_invocation_id")?;
            let target_function_id: Option<Vec<u8>> = row.try_get("target_function_id")?;
            let source_revision_id: Option<Vec<u8>> = row.try_get("source_revision_id")?;
            let catalogue_revision_id: Option<Vec<u8>> =
                row.try_get("catalogue_revision_id")?;
            let decision_outcome: String = row.try_get("decision_outcome")?;
            let terminal_outcome: String = row.try_get("terminal_outcome")?;
            let invocation_outcome: Option<String> = row.try_get("invocation_outcome")?;
            let resource_json: String = row.try_get("resource_json")?;
            let invocation_json: Option<String> = row.try_get("invocation_json")?;
            let resource_count: i64 = row.try_get("resource_count")?;
            let invocation_count: i64 = row.try_get("invocation_count")?;
            let history_count: i64 = row.try_get("history_count")?;
            require(
                nested_invocation_id.is_none()
                    && target_function_id == Some(target.id().to_bytes().to_vec())
                    && source_revision_id == Some(active.pair().source().to_bytes().to_vec())
                    && catalogue_revision_id
                        == Some(active.pair().catalogue().to_bytes().to_vec())
                    && decision_outcome == "denied"
                    && terminal_outcome == "failed"
                    && invocation_outcome.is_none()
                    && resource_count == 1
                    && invocation_count == 0
                    && history_count == 1
                    && !resource_json.contains(RAW_MARKER)
                    && invocation_json.is_none(),
                "resource denial did not retain nullable nested identity and redacted target audit",
            )
        }
        .await;
        finish_session(
            audit_operation,
            audit_session.shutdown().await,
            "resource denial audit inspection",
        )?;
        require_no_session_leaks(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn recovers_reused_current_revisions_and_retired_function_history() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let introduction = install_function_revision(&database).await?;
        let expected = install_reused_function_catalogue(&database, &introduction).await?;

        let recovered = kernel(&database)?.recover().await?;

        require(
            recovered.source().units() == [expected.unit],
            "reused revision recovery changed the active retained source",
        )?;
        require(
            recovered.catalogue().revision() == expected.catalogue.revision()
                && recovered.catalogue().schemas() == expected.catalogue.schemas()
                && recovered.catalogue().object_types() == expected.catalogue.object_types()
                && recovered.catalogue().functions() == expected.catalogue.functions(),
            "reused revision recovery changed the active semantic catalogue",
        )?;
        require(
            recovered.function_revisions() == expected.current_revisions,
            "reused revision recovery changed current immutable revision records",
        )?;
        require(
            recovered.historical_function_revisions() == [expected.retired_revision],
            "retired function revision was not recovered as immutable history",
        )?;
        require(
            recovered.references() == expected.references,
            "reused revision recovery changed active definition references",
        )?;
        require(
            recovered.origins() == expected.origins,
            "reused revision recovery changed current definition origins",
        )?;
        let current_function_origin = recovered
            .origins()
            .iter()
            .find(|origin| {
                origin.identity()
                    == DefinitionIdentity::Function(recovered.catalogue().functions()[0].id())
            })
            .ok_or_else(|| failure("recovered current function origin is missing"))?;
        require(
            current_function_origin.source()
                != recovered.function_revisions()[0].declaration_origin(),
            "reused revision collapsed current definition origin into historical declaration origin",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_function_signature_revision_artifact_and_reference_tampering() -> TestResult<()> {
    let cases = [
        "ALTER TABLE _orna_kernel.catalogue_function_parameters
             DROP CONSTRAINT catalogue_function_parameters_check;
         UPDATE _orna_kernel.catalogue_function_parameters
         SET scalar_type = NULL
         WHERE function_id = decode(repeat('d1', 16), 'hex')
           AND parameter_id = decode(repeat('b1', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_function_parameters
         SET ordinal = 99
         WHERE function_id = decode(repeat('d1', 16), 'hex')
           AND parameter_id = decode(repeat('b1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.catalogue_function_parameters DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.catalogue_function_parameters
         SET function_id = decode(repeat('ff', 16), 'hex')
         WHERE function_id = decode(repeat('d1', 16), 'hex')
           AND parameter_id = decode(repeat('b1', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_functions
         SET name_parts = ARRAY['wrong', 'server_rows']
         WHERE function_id = decode(repeat('d1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.catalogue_functions DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.catalogue_functions
         SET current_function_revision_id = decode(repeat('e2', 16), 'hex')
         WHERE function_id = decode(repeat('d1', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_functions
         SET domain = 'client', transaction_mode = NULL
         WHERE function_id = decode(repeat('d1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.catalogue_functions
             DROP CONSTRAINT catalogue_functions_transaction_mode_check;
         UPDATE _orna_kernel.catalogue_functions
         SET transaction_mode = 'manual'
         WHERE function_id = decode(repeat('d1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.catalogue_functions
             DROP CONSTRAINT catalogue_functions_check1,
             DROP CONSTRAINT catalogue_functions_return_kind_presence_check;
         UPDATE _orna_kernel.catalogue_functions
         SET return_type_kind = 'scalar', return_scalar_type = 'boolean'
         WHERE function_id = decode(repeat('d1', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_function_return_columns
         SET ordinal = 99
         WHERE function_id = decode(repeat('d1', 16), 'hex') AND ordinal = 1",
        "ALTER TABLE _orna_kernel.catalogue_function_parameters DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.catalogue_function_parameters
         SET default_expression_id = decode(repeat('ff', 16), 'hex')
         WHERE function_id = decode(repeat('d1', 16), 'hex')
           AND parameter_id = decode(repeat('b1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.definition_references DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.definition_references
         SET target_owner_type_id = decode(repeat('82', 16), 'hex')
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
           AND ordinal = 1",
        "ALTER TABLE _orna_kernel.definition_references DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.definition_references
         SET target_owner_function_id = decode(repeat('d2', 16), 'hex')
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
           AND ordinal = 2",
        "ALTER TABLE _orna_kernel.definition_references DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.definition_references
         SET source_function_revision_id = decode(repeat('e2', 16), 'hex')
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.definition_references
         SET ordinal = ordinal + 10
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.definition_references
         SET source_end = 999
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.definition_references
             DROP CONSTRAINT definition_references_reference_target_compatibility_check;
         UPDATE _orna_kernel.definition_references
         SET reference_kind = 'function_call'
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
           AND ordinal = 0",
        "ALTER TABLE _orna_kernel.definition_references DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.definition_references
         SET target_definition_id = decode(repeat('ff', 16), 'hex')
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
           AND ordinal = 0",
        "UPDATE _orna_kernel.function_revisions
         SET status = 'retired'
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.function_revisions
         SET status = 'candidate'
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.function_revisions
         SET status = 'invalid'
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_revisions
             DROP CONSTRAINT function_revisions_revision_number_check;
         UPDATE _orna_kernel.function_revisions
         SET revision_number = 0
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.function_revisions
         SET content_hash = decode(repeat('ff', 32), 'hex')
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.function_revisions
         SET semantic_ir_hash = decode(repeat('ff', 32), 'hex')
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_revisions
             DROP CONSTRAINT function_revisions_hash_contract_version_check;
         UPDATE _orna_kernel.function_revisions
         SET hash_contract_version = 2
         WHERE id = decode(repeat('e1', 16), 'hex')",
        "DELETE FROM _orna_kernel.function_artifacts
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "INSERT INTO _orna_kernel.function_artifacts
            (function_revision_id, artifact_kind, format, format_version,
             payload, content_hash)
         SELECT function_revision_id, 'client_bytecode', format, format_version,
                payload, content_hash
         FROM _orna_kernel.function_artifacts
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.function_artifacts
         SET artifact_kind = 'client_bytecode'
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_artifacts
             DROP CONSTRAINT function_artifacts_format_check;
         UPDATE _orna_kernel.function_artifacts
         SET format = ''
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_artifacts
             DROP CONSTRAINT function_artifacts_format_version_check;
         UPDATE _orna_kernel.function_artifacts
         SET format_version = 0
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "UPDATE _orna_kernel.function_artifacts
         SET payload = payload || decode('00', 'hex')
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_artifacts
             DROP CONSTRAINT function_artifacts_content_hash_check;
         UPDATE _orna_kernel.function_artifacts
         SET content_hash = decode(repeat('ff', 31), 'hex')
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_artifacts
             DROP CONSTRAINT function_artifacts_hash_contract_version_check;
         UPDATE _orna_kernel.function_artifacts
         SET hash_contract_version = 2
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_artifacts DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.function_artifacts
         SET function_revision_id = decode(repeat('ff', 16), 'hex')
         WHERE function_revision_id = decode(repeat('e1', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_revisions DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.function_revisions
         SET introduced_catalogue_revision_id = decode(repeat('ff', 16), 'hex')
         WHERE id = decode(repeat('e3', 16), 'hex')",
        "ALTER TABLE _orna_kernel.function_revisions DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.function_revisions
         SET introduced_catalogue_revision_id = decode(repeat('ff', 16), 'hex')
         WHERE id = decode(repeat('e3', 16), 'hex');
         DELETE FROM _orna_kernel.function_artifacts
         WHERE function_revision_id = decode(repeat('e3', 16), 'hex')",
    ];
    for (index, statement) in cases.into_iter().enumerate() {
        reject_function_tamper(statement).await.map_err(|error| {
            failure(format!(
                "function tamper case {index} failed before recovery rejection: {error}"
            ))
        })?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_crossed_write_reference_updates_at_the_v6_compatibility_check() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_function_revision(&database).await?;

        let session = database.open().await?;
        let operation_result: TestResult<()> = async {
            for (ordinal, original_kind, crossed_kind) in [
                (0_i64, "query_object", "write_field"),
                (1_i64, "query_field", "write_object"),
            ] {
                session.client().batch_execute("BEGIN").await?;
                let update_result = session
                    .client()
                    .execute(
                        "UPDATE _orna_kernel.definition_references
                         SET reference_kind = $1
                         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
                           AND ordinal = $2",
                        &[&crossed_kind, &ordinal],
                    )
                    .await;
                let rollback_result = session.client().batch_execute("ROLLBACK").await;
                let constraint = update_result
                    .as_ref()
                    .err()
                    .and_then(|error| error.as_db_error())
                    .and_then(|error| error.constraint());
                require(
                    update_result.is_err(),
                    format!("crossed write reference update {crossed_kind} at ordinal {ordinal} was accepted"),
                )?;
                require(
                    constraint == Some("definition_references_reference_target_compatibility_check"),
                    format!(
                        "crossed write reference update {crossed_kind} at ordinal {ordinal} failed for {constraint:?}"
                    ),
                )?;
                rollback_result?;

                let row = session
                    .client()
                    .query_one(
                        "SELECT reference_kind
                         FROM _orna_kernel.definition_references
                         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
                           AND ordinal = $1",
                        &[&ordinal],
                    )
                    .await?;
                let recovered_kind: String = row.try_get(0)?;
                require(
                    recovered_kind == original_kind,
                    format!(
                        "rolled-back crossed write reference update changed ordinal {ordinal} to {recovered_kind:?}"
                    ),
                )?;
            }
            Ok(())
        }
        .await;
        finish_session(
            operation_result,
            session.shutdown().await,
            "crossed write-reference constraint probe",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_crossed_write_reference_kinds_before_catalogue_hash_validation() -> TestResult<()>
{
    for (statement, crossed_kind) in [
        (
            "ALTER TABLE _orna_kernel.definition_references
                 DROP CONSTRAINT definition_references_reference_target_compatibility_check;
             UPDATE _orna_kernel.definition_references
             SET reference_kind = 'write_field'
             WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
               AND ordinal = 0",
            "write_field",
        ),
        (
            "ALTER TABLE _orna_kernel.definition_references
                 DROP CONSTRAINT definition_references_reference_target_compatibility_check;
             UPDATE _orna_kernel.definition_references
             SET reference_kind = 'write_object'
             WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
               AND ordinal = 1",
            "write_object",
        ),
    ] {
        reject_function_tamper_expected(
            statement,
            ExpectedRecoveryError::DurableExact {
                relation: "_orna_kernel.definition_references",
                rule: "reference kind must be compatible with its exact target kind",
            },
        )
        .await
        .map_err(|error| {
            failure(format!(
                "crossed {crossed_kind} recovery case failed: {error}"
            ))
        })?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_unknown_reference_kind_before_catalogue_hash_validation() -> TestResult<()> {
    reject_function_tamper_expected(
        "ALTER TABLE _orna_kernel.definition_references
             DROP CONSTRAINT definition_references_reference_kind_check,
             DROP CONSTRAINT definition_references_reference_target_compatibility_check;
         UPDATE _orna_kernel.definition_references
         SET reference_kind = 'future_reference_kind'
         WHERE source_function_revision_id = decode(repeat('e1', 16), 'hex')
           AND ordinal = 0",
        ExpectedRecoveryError::DurableExact {
            relation: "_orna_kernel.definition_references",
            rule: "reference kind must be one exact supported semantic relation",
        },
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_void_parameters_and_rows_columns_at_their_decoder_relations() -> TestResult<()> {
    for (statement, relation) in [
        (
            "ALTER TABLE _orna_kernel.catalogue_function_parameters
                 DROP CONSTRAINT catalogue_function_parameters_scalar_type_check;
             UPDATE _orna_kernel.catalogue_function_parameters
             SET scalar_type = 'void'
             WHERE function_id = decode(repeat('d1', 16), 'hex')
               AND parameter_id = decode(repeat('b1', 16), 'hex')",
            "_orna_kernel.catalogue_function_parameters",
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_function_return_columns
                 DROP CONSTRAINT catalogue_function_return_columns_scalar_type_check;
             UPDATE _orna_kernel.catalogue_function_return_columns
             SET scalar_type = 'void'
             WHERE function_id = decode(repeat('d1', 16), 'hex') AND ordinal = 0",
            "_orna_kernel.catalogue_function_return_columns",
        ),
    ] {
        reject_function_tamper_expected(statement, ExpectedRecoveryError::Durable(relation))
            .await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_retained_introduction_source_catalogue_and_history_tampering() -> TestResult<()> {
    let cases = [
        "UPDATE _orna_kernel.function_revisions
         SET status = 'active'
         WHERE id = decode(repeat('e3', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_revisions
         SET content_hash = decode(repeat('ff', 32), 'hex')
         WHERE id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         )",
        "UPDATE _orna_kernel.source_revisions
         SET content_hash = decode(repeat('ff', 32), 'hex')
         WHERE id = (
             SELECT parent_source_revision_id
             FROM _orna_kernel.source_revisions
             WHERE id = (SELECT source_revision_id FROM _orna_kernel.active_revision)
         )",
        "UPDATE _orna_kernel.catalogue_functions
         SET security_mode = 'definer'
         WHERE catalogue_revision_id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         ) AND function_id = decode(repeat('d3', 16), 'hex')",
        "UPDATE _orna_kernel.definition_references
         SET target_definition_id = decode(repeat('82', 16), 'hex')
         WHERE catalogue_revision_id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         ) AND source_function_revision_id = decode(repeat('e1', 16), 'hex')
           AND ordinal = 0",
        "UPDATE _orna_kernel.catalogue_functions
         SET source_start = 1
         WHERE catalogue_revision_id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         ) AND function_id = decode(repeat('d3', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_functions
         SET source_end = 999
         WHERE catalogue_revision_id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         ) AND function_id = decode(repeat('d3', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_functions
         SET source_start = 11
         WHERE catalogue_revision_id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         ) AND function_id = decode(repeat('d3', 16), 'hex')",
        "UPDATE _orna_kernel.catalogue_functions
         SET source_unit_id = decode(repeat('f4', 16), 'hex')
         WHERE catalogue_revision_id = (
             SELECT parent_catalogue_revision_id
             FROM _orna_kernel.catalogue_revisions
             WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)
         ) AND function_id = decode(repeat('d3', 16), 'hex')",
        "ALTER TABLE _orna_kernel.catalogue_revisions DISABLE TRIGGER ALL;
         UPDATE _orna_kernel.catalogue_revisions
         SET parent_catalogue_revision_id = NULL
         WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)",
    ];
    for (index, statement) in cases.into_iter().enumerate() {
        reject_history_tamper(statement).await.map_err(|error| {
            failure(format!(
                "history tamper case {index} failed before recovery rejection: {error}"
            ))
        })?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_a_valid_function_introduction_from_a_sibling_branch() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        let introduction = install_function_revision(&database).await?;
        install_reused_function_catalogue(&database, &introduction).await?;
        install_valid_sibling_introduction(&database, &introduction).await?;

        require_expected_error(
            recovery_error(&database).await?,
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_tampered_schema_name_and_incomplete_origin() -> TestResult<()> {
    reject_schema_tamper(
        "UPDATE _orna_kernel.catalogue_schemas
         SET name_parts = ARRAY['tampered']",
        ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
    )
    .await?;
    reject_schema_tamper(
        "ALTER TABLE _orna_kernel.catalogue_schemas
             DROP CONSTRAINT catalogue_schemas_source_origin_check;
         UPDATE _orna_kernel.catalogue_schemas SET source_end = NULL",
        ExpectedRecoveryError::Durable("_orna_kernel.catalogue_schemas"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_schema_origin_from_another_bundle_or_invalid_span() -> TestResult<()> {
    reject_schema_tamper(
        "INSERT INTO _orna_kernel.source_bundles (id, content_hash)
         VALUES (
             decode(repeat('71', 16), 'hex'),
             decode(repeat('00', 32), 'hex')
         );
         INSERT INTO _orna_kernel.source_units
             (id, bundle_id, ordinal, logical_path, content, content_hash)
         VALUES (
             decode(repeat('72', 16), 'hex'),
             decode(repeat('71', 16), 'hex'),
             0,
             'other.orna',
             'other',
             decode(repeat('00', 32), 'hex')
         );
         INSERT INTO _orna_kernel.source_bundle_units
             (bundle_id, source_unit_id, ordinal)
         VALUES (
             decode(repeat('71', 16), 'hex'),
             decode(repeat('72', 16), 'hex'),
             0
         );
         UPDATE _orna_kernel.catalogue_schemas
         SET source_unit_id = decode(repeat('72', 16), 'hex')",
        ExpectedRecoveryError::Revision,
    )
    .await?;
    reject_schema_tamper(
        "UPDATE _orna_kernel.catalogue_schemas SET source_end = 999",
        ExpectedRecoveryError::Revision,
    )
    .await?;
    reject_schema_tamper(
        "UPDATE _orna_kernel.catalogue_schemas SET source_start = 11",
        ExpectedRecoveryError::Revision,
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_tampered_schema_catalogue_hash() -> TestResult<()> {
    reject_schema_tamper(
        "UPDATE _orna_kernel.catalogue_revisions
         SET content_hash = decode(repeat('73', 32), 'hex')
         WHERE id = (SELECT catalogue_revision_id FROM _orna_kernel.active_revision)",
        ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_enum_label_name_and_origin_tampering() -> TestResult<()> {
    reject_enum_tamper(
        "UPDATE _orna_kernel.catalogue_enum_types
         SET labels = ARRAY['customer', 'owner''s', 'lead']",
        ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
    )
    .await?;
    reject_enum_tamper(
        "UPDATE _orna_kernel.catalogue_enum_types
         SET labels = ARRAY['lead', 'lead', 'customer']",
        ExpectedRecoveryError::Catalogue,
    )
    .await?;
    reject_enum_tamper(
        "UPDATE _orna_kernel.catalogue_enum_types
         SET name_parts = ARRAY['wrong', 'stage']",
        ExpectedRecoveryError::Durable("_orna_kernel.catalogue_enum_types"),
    )
    .await?;
    reject_enum_tamper(
        "UPDATE _orna_kernel.catalogue_enum_types
         SET source_start = source_start + 1",
        ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_object_field_expression_and_origin_tampering() -> TestResult<()> {
    for (statement, expected) in [
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_check;
             UPDATE _orna_kernel.catalogue_fields
             SET target_type_id = decode(repeat('82', 16), 'hex')
             WHERE field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_catalogue_revision_id_owner_type_id_fkey;
             UPDATE _orna_kernel.catalogue_fields
             SET owner_type_id = decode(repeat('ee', 16), 'hex')
             WHERE field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "UPDATE _orna_kernel.catalogue_fields SET ordinal = 99
             WHERE field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Catalogue,
        ),
        (
            "INSERT INTO _orna_kernel.catalogue_schemas
                (catalogue_revision_id, schema_id, name_parts,
                 source_unit_id, source_start, source_end)
             SELECT catalogue_revision_id, decode(repeat('62', 16), 'hex'), ARRAY['other'],
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.catalogue_schemas LIMIT 1;
             UPDATE _orna_kernel.catalogue_object_types
             SET schema_id = decode(repeat('62', 16), 'hex')
             WHERE type_id = decode(repeat('81', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_object_types"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_catalogue_revision_id_target_type_id_fkey;
             UPDATE _orna_kernel.catalogue_fields
             SET target_type_id = decode(repeat('ee', 16), 'hex')
             WHERE field_id = decode(repeat('a1', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_check1;
             UPDATE _orna_kernel.catalogue_fields SET on_delete = 'restrict'
             WHERE field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_check2;
             UPDATE _orna_kernel.catalogue_fields SET on_delete = 'set_null'
             WHERE owner_type_id = decode(repeat('81', 16), 'hex')
               AND field_id = decode(repeat('a1', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_scalar_type_check;
             UPDATE _orna_kernel.catalogue_fields SET scalar_type = 'void'
             WHERE field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
        ),
        (
            "UPDATE _orna_kernel.catalogue_fields SET type_kind = 'named'
             WHERE owner_type_id = decode(repeat('81', 16), 'hex')
               AND field_id = decode(repeat('a0', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "DO $drop$
             DECLARE generated_name name;
             BEGIN
                 SELECT conname INTO STRICT generated_name
                 FROM pg_catalog.pg_constraint
                 WHERE conrelid = '_orna_kernel.catalogue_fields'::regclass
                   AND contype = 'f'
                   AND conname LIKE '%default%';
                 EXECUTE pg_catalog.format(
                     'ALTER TABLE _orna_kernel.catalogue_fields DROP CONSTRAINT %I',
                     generated_name
                 );
             END
             $drop$;
             UPDATE _orna_kernel.catalogue_fields
             SET default_expression_id = decode(repeat('ee', 16), 'hex')
             WHERE field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "UPDATE _orna_kernel.catalogue_expressions SET payload = payload || decode('00', 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_expressions"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_expressions
                 DROP CONSTRAINT catalogue_expressions_hash_algorithm_check;
             UPDATE _orna_kernel.catalogue_expressions SET hash_algorithm = 'sha512'",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_expressions"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_expressions
                 DROP CONSTRAINT catalogue_expressions_hash_contract_version_check;
             UPDATE _orna_kernel.catalogue_expressions SET hash_contract_version = 2",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_expressions"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_expressions
                 DROP CONSTRAINT catalogue_expressions_source_origin_check;
             UPDATE _orna_kernel.catalogue_expressions SET source_end = NULL",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_expressions"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_fields
                 DROP CONSTRAINT catalogue_fields_field_id_check;
             UPDATE _orna_kernel.catalogue_fields
             SET field_id = decode(repeat('ef', 15), 'hex')
             WHERE owner_type_id = decode(repeat('81', 16), 'hex')
               AND field_id = decode(repeat('91', 16), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_fields"),
        ),
        (
            "ALTER TABLE _orna_kernel.catalogue_expressions
                 DROP CONSTRAINT catalogue_expressions_expression_id_check;
             UPDATE _orna_kernel.catalogue_expressions
             SET expression_id = decode(repeat('ef', 15), 'hex')",
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_expressions"),
        ),
    ] {
        reject_object_tamper(statement, expected).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_exact_physical_catalogue_tampering() -> TestResult<()> {
    const TABLE: &str = "_orna_data.t_81818181818181818181818181818181";
    const TARGET: &str = "_orna_data.t_82828282828282828282828282828282";
    const UNIQUE: &str = "uq_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
    const UNIQUE_FIELD: &str = "f_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
    const OTHER_REFERENCE: &str = "f_a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    let statements = [
        format!("DROP TABLE {TABLE} CASCADE"),
        "CREATE TABLE _orna_data.extra_relation (value integer)".to_owned(),
        format!("ALTER TABLE {TABLE} RENAME TO wrong_name"),
        format!("ALTER TABLE {TABLE} ALTER COLUMN f_91919191919191919191919191919191 TYPE bigint"),
        format!(
            "ALTER TABLE {TABLE} ALTER COLUMN f_91919191919191919191919191919191 DROP NOT NULL"
        ),
        format!(
            "ALTER TABLE {TABLE} ALTER COLUMN f_91919191919191919191919191919191 SET DEFAULT 1"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT ck_81818181818181818181818181818181_object_id"
        ),
        format!("ALTER TABLE {TABLE} DROP CONSTRAINT pk_81818181818181818181818181818181 CASCADE"),
        format!("ALTER TABLE {TABLE} DROP CONSTRAINT fk_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0"),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT fk_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0;
             ALTER TABLE {TABLE} ADD CONSTRAINT fk_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0
             FOREIGN KEY (f_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0)
             REFERENCES {TARGET} (_orna_object_id) ON DELETE CASCADE"
        ),
        format!("ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE}"),
        format!("ALTER TABLE {TABLE} RENAME CONSTRAINT {UNIQUE} TO wrong_unique_name"),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             ALTER TABLE {TABLE} ADD CONSTRAINT {UNIQUE} UNIQUE ({OTHER_REFERENCE})"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             ALTER TABLE {TABLE} ADD CONSTRAINT {UNIQUE} UNIQUE ({UNIQUE_FIELD})
             DEFERRABLE INITIALLY DEFERRED"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             ALTER TABLE {TABLE} ADD CONSTRAINT {UNIQUE}
             UNIQUE ({UNIQUE_FIELD}, {OTHER_REFERENCE})"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             ALTER TABLE {TABLE} ADD CONSTRAINT {UNIQUE}
             UNIQUE NULLS NOT DISTINCT ({UNIQUE_FIELD})"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             ALTER TABLE {TABLE} ADD CONSTRAINT {UNIQUE}
             UNIQUE ({UNIQUE_FIELD}) INCLUDE ({OTHER_REFERENCE})"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             CREATE UNIQUE INDEX {UNIQUE} ON {TABLE} ({UNIQUE_FIELD})
             WHERE {UNIQUE_FIELD} IS NOT NULL"
        ),
        format!(
            "ALTER TABLE {TABLE} DROP CONSTRAINT {UNIQUE};
             CREATE UNIQUE INDEX {UNIQUE} ON {TABLE} ((octet_length({UNIQUE_FIELD})))"
        ),
        format!("CREATE UNIQUE INDEX unexpected_unique_index ON {TABLE} ({UNIQUE_FIELD})"),
        format!("CREATE INDEX unexpected_index ON {TABLE} (f_91919191919191919191919191919191)"),
        format!("ALTER TABLE {TABLE} ENABLE ROW LEVEL SECURITY"),
        format!("GRANT MAINTAIN ON TABLE {TABLE} TO PUBLIC"),
        format!(
            "ALTER TABLE {TABLE} ADD COLUMN dropped integer; ALTER TABLE {TABLE} DROP COLUMN dropped"
        ),
        format!("ALTER TABLE {TABLE} DISABLE TRIGGER ALL"),
        format!("CREATE TABLE public.inbound (value bytea REFERENCES {TABLE} (_orna_object_id))"),
        format!("CREATE TABLE public.inherited () INHERITS ({TABLE})"),
        format!(
            "CREATE FUNCTION public.noop_trigger() RETURNS trigger LANGUAGE plpgsql
             AS 'BEGIN RETURN NEW; END';
             CREATE TRIGGER unexpected_trigger BEFORE INSERT ON {TABLE}
             FOR EACH ROW EXECUTE FUNCTION public.noop_trigger()"
        ),
        format!("CREATE POLICY unexpected_policy ON {TABLE} USING (true)"),
        format!("CREATE RULE unexpected_rule AS ON INSERT TO {TABLE} DO NOTHING"),
    ];
    for statement in statements {
        reject_object_tamper(&statement, ExpectedRecoveryError::AnyDurable).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn trusted_catalogue_search_path_ignores_shadow_privilege_functions() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_object_revision(&database, false).await?;
        run_batch(
            &database,
            "CREATE FUNCTION public.has_table_privilege(name, oid, text)
             RETURNS boolean LANGUAGE sql IMMUTABLE AS 'SELECT true';
             CREATE FUNCTION public.octet_length(bytea)
             RETURNS integer LANGUAGE sql IMMUTABLE AS 'SELECT 16'",
        )
        .await?;

        let mut hostile_config = database.config()?;
        hostile_config.options("-c search_path=public,pg_catalog");
        PostgresKernel::new(hostile_config).recover().await?;
        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn trusted_catalogue_rejects_checks_bound_to_a_shadow_function() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        install_object_revision(&database, false).await?;
        run_batch(
            &database,
            "CREATE FUNCTION public.octet_length(bytea)
             RETURNS integer LANGUAGE sql IMMUTABLE AS 'SELECT 16'",
        )
        .await?;

        let mut hostile_config = database.config()?;
        hostile_config.options("-c search_path=public,pg_catalog");
        let (client, connection) = hostile_config.connect(tokio_postgres::NoTls).await?;
        let driver = tokio::spawn(connection);
        client
            .batch_execute(
                "ALTER TABLE _orna_data.t_81818181818181818181818181818181
                     DROP CONSTRAINT ck_81818181818181818181818181818181_object_id;
                 ALTER TABLE _orna_data.t_81818181818181818181818181818181
                     ADD CONSTRAINT ck_81818181818181818181818181818181_object_id
                     CHECK (octet_length(_orna_object_id) = 16)",
            )
            .await?;
        drop(client);
        driver.await??;

        let mut hostile_recovery = database.config()?;
        hostile_recovery.options("-c search_path=public,pg_catalog");
        let error = match PostgresKernel::new(hostile_recovery).recover().await {
            Ok(_) => {
                return Err(failure(
                    "shadow-bound physical check recovered successfully",
                ));
            }
            Err(error) => error,
        };
        require_expected_error(error, ExpectedRecoveryError::AnyDurable)
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_tampered_migration_history_before_durable_state() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        run_batch(
            &database,
            "UPDATE _orna_kernel.schema_migrations
             SET checksum = decode(repeat('00', 32), 'hex')
             WHERE version = 5;
             UPDATE _orna_kernel.source_bundles
             SET content_hash = decode(repeat('ff', 32), 'hex')",
        )
        .await?;

        let error = recovery_error(&database).await?;
        require(
            matches!(error, PostgresKernelError::MigrationMismatch { version: 5 }),
            format!("tampered migration history produced the wrong failure: {error}"),
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_source_content_ordinals_encoding_and_contract_tampering() -> TestResult<()> {
    reject_source_tamper(
        "UPDATE _orna_kernel.source_units SET content = content || 'tampered'",
        ExpectedRecoveryError::Durable("_orna_kernel.source_units"),
    )
    .await?;
    reject_source_tamper(
        "UPDATE _orna_kernel.source_bundle_units SET ordinal = 1",
        ExpectedRecoveryError::Canonical,
    )
    .await?;
    reject_source_tamper(
        "UPDATE _orna_kernel.source_bundles
         SET content_hash = decode(repeat('fe', 32), 'hex')",
        ExpectedRecoveryError::Durable("_orna_kernel.source_bundles"),
    )
    .await?;
    reject_source_tamper(
        "UPDATE _orna_kernel.source_revisions
         SET content_hash = decode(repeat('fd', 32), 'hex')",
        ExpectedRecoveryError::Durable("_orna_kernel.source_revisions"),
    )
    .await?;
    reject_source_tamper(
        "ALTER TABLE _orna_kernel.source_units
             DROP CONSTRAINT source_units_encoding_check;
         UPDATE _orna_kernel.source_units SET encoding = 'latin-1'",
        ExpectedRecoveryError::Durable("_orna_kernel.source_units"),
    )
    .await?;
    reject_source_tamper(
        "ALTER TABLE _orna_kernel.source_units
             DROP CONSTRAINT source_units_hash_contract_version_check;
         UPDATE _orna_kernel.source_units SET hash_contract_version = 2",
        ExpectedRecoveryError::Durable("_orna_kernel.source_units"),
    )
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_functions_and_unexpected_physical_relations() -> TestResult<()> {
    reject_unsupported_state(UNSUPPORTED_FUNCTION_SQL, "_orna_kernel.catalogue_functions").await?;
    reject_unsupported_state(
        "CREATE TABLE _orna_data.unexpected (value integer)",
        "_orna_data",
    )
    .await?;
    reject_unsupported_state("CREATE SEQUENCE _orna_data.unexpected", "_orna_data").await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_a_multi_revision_catalogue_and_source_ancestry_cycle() -> TestResult<()> {
    with_test_database(|database| async move {
        kernel(&database)?.bootstrap().await?;
        run_batch(
            &database,
            "INSERT INTO _orna_kernel.source_bundles
                (id, content_hash)
             VALUES (
                decode(repeat('b1', 16), 'hex'),
                decode(repeat('00', 32), 'hex')
             );
             INSERT INTO _orna_kernel.source_revisions
                (id, parent_source_revision_id, bundle_id, content_hash)
             SELECT
                decode(repeat('b2', 16), 'hex'),
                source_revision_id,
                decode(repeat('b1', 16), 'hex'),
                decode(repeat('00', 32), 'hex')
             FROM _orna_kernel.active_revision;
             INSERT INTO _orna_kernel.catalogue_revisions
                (id, source_revision_id, parent_catalogue_revision_id, content_hash)
             SELECT
                decode(repeat('b3', 16), 'hex'),
                decode(repeat('b2', 16), 'hex'),
                catalogue_revision_id,
                decode(repeat('00', 32), 'hex')
             FROM _orna_kernel.active_revision;
             UPDATE _orna_kernel.source_revisions
             SET parent_source_revision_id = decode(repeat('b2', 16), 'hex')
             WHERE id = (
                SELECT source_revision_id FROM _orna_kernel.active_revision
             );
             UPDATE _orna_kernel.catalogue_revisions
             SET parent_catalogue_revision_id = decode(repeat('b3', 16), 'hex')
             WHERE id = (
                SELECT catalogue_revision_id FROM _orna_kernel.active_revision
             )",
        )
        .await?;

        require_expected_error(
            recovery_error(&database).await?,
            ExpectedRecoveryError::Durable("_orna_kernel.catalogue_revisions"),
        )
    })
    .await
}
