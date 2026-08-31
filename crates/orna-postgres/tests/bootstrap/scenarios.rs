use super::*;

#[test]
fn registered_v4_semantic_fixture_is_a_valid_active_database_revision() -> TestResult<()> {
    let fixture = registered_v4_semantic_fixture()?;

    require(
        fixture.catalogue().object_types().len() == 2
            && fixture.catalogue().functions().len() == 2
            && fixture.function_revisions().len() == 2
            && fixture.references().len() == 2,
        "registered v4 fixture lost required semantic rows",
    )
}

#[test]
fn supported_reference_kind_sql_maps_every_legacy_fixture_kind() -> TestResult<()> {
    assert_eq!(
        SUPPORTED_REFERENCE_KINDS,
        &[
            (DefinitionReferenceKind::FunctionCall, "function_call"),
            (DefinitionReferenceKind::NamedType, "named_type"),
            (DefinitionReferenceKind::ObjectReference, "object_reference"),
            (DefinitionReferenceKind::ParameterRead, "parameter_read"),
            (DefinitionReferenceKind::QueryObject, "query_object"),
            (DefinitionReferenceKind::QueryField, "query_field"),
            (DefinitionReferenceKind::Expression, "expression"),
        ]
    );
    for (kind, expected) in SUPPORTED_REFERENCE_KINDS {
        assert_eq!(supported_reference_kind_sql(*kind)?, *expected);
    }
    Ok(())
}

#[test]
fn legacy_migration_epoch_is_order_contiguous() -> TestResult<()> {
    require(
        MIGRATIONS.len() == 47,
        format!(
            "migration registry has {} entries; expected 47",
            MIGRATIONS.len()
        ),
    )?;
    for (index, (version, _, _)) in MIGRATIONS.iter().enumerate() {
        require(
            *version == (index + 1) as i64,
            format!(
                "legacy migration at index {index} is version {version}; expected {}",
                index + 1
            ),
        )?;
    }
    Ok(())
}

#[test]
fn write_reference_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(6, MIGRATIONS[5].2)),
        "e831811c0f42d6f4b3ab2601cf480fabaaed03b5547e2615400b9eec4b6b53bf"
    );
}

#[test]
fn application_migration_baseline_checksum_binds_v46_and_v47_contracts() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(46, MIGRATIONS[45].2)),
        "bcbe71c0c5d2c18890f1aacab9e09389ffdba3f2789f88f7e0df95562fad6685"
    );
    assert_eq!(
        hex_bytes(expected_migration_checksum(47, MIGRATIONS[46].2)),
        "ac92c5acb0388c652ab130db481ad051f1b893e91d0d232e35001d5ffaa0345d"
    );
}

#[test]
fn standard_catalogue_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(7, MIGRATIONS[6].2)),
        "da58e39fb08edf1c214f6c041c792adb1446a6acb2939560d9091759a218c90f"
    );
}

#[test]
fn resolved_value_type_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(8, MIGRATIONS[7].2)),
        "2ef8d844814dafd7d70d40fb39ce7e5e6c52dea3cfc668e84c74c2c5c1dd06e7"
    );
}

#[test]
fn security_snapshot_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(9, MIGRATIONS[8].2)),
        "101413b9478a975b08099cda32bd26e4c41ad0bc00b8c473c5ca281a7e2690ef"
    );
}

#[test]
fn local_peer_credential_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(10, MIGRATIONS[9].2)),
        "0c6d158eb85209c8d0413e3871c5f56840936026f4f80d1325c079d3723e9099"
    );
}

#[test]
fn source_apply_audit_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(37, MIGRATIONS[36].2)),
        "23afc307eefed842ea24b0eab50d21a8108f20983da24454792fe4fc44e2d66b"
    );
}

#[test]
fn source_apply_principal_binding_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(38, MIGRATIONS[37].2)),
        "ada9f1e5b7080ab8955484a3e2c602ba1966f344f6577bf2a48bbc7e444d7179"
    );
}

#[test]
fn protected_security_audit_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(11, MIGRATIONS[10].2)),
        "54288defeebde1621805eed6ac0b2653669a658938e6c707f0665d430d639575"
    );
}

#[test]
fn catalogue_enum_type_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(12, MIGRATIONS[11].2)),
        "87635d3052423176b969ce860e0c3e0fec665199259c14c1dbf5a0e3e385d3ff"
    );
}

#[test]
fn resolved_enum_type_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(13, MIGRATIONS[12].2)),
        "850a85e034cc7548c4d70f35763356492af4d2c227506bb79aca0c346b4a3f75"
    );
}

#[test]
fn enum_reference_target_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(14, MIGRATIONS[13].2)),
        "c130918d3a24a386d78c61cae41775df3b57f5a0b070afac19b9fb143088e38d"
    );
}

#[test]
fn catalogue_record_value_migration_checksum_binds_exact_sql_bytes() {
    assert_eq!(
        hex_bytes(expected_migration_checksum(15, MIGRATIONS[14].2)),
        "31891de1fe93086185d54aa8d995bb5f1f569c8906596e16a007c13ef48385a3"
    );
}

#[test]
fn security_snapshot_migration_is_the_registered_version_nine() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[8];

    require(
        version == 9,
        format!("security snapshot migration is version {version}"),
    )?;
    require(
        name == "security decision snapshot",
        format!("security snapshot migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.security_principals"),
        "security migration does not create the principal table",
    )
}

#[test]
fn local_peer_credential_migration_is_the_registered_version_ten() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[9];

    require(
        version == 10,
        format!("local peer credential migration is version {version}"),
    )?;
    require(
        name == "local peer credentials",
        format!("local peer credential migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.security_local_peer_credentials"),
        "local peer credential migration does not create its protected table",
    )
}

#[test]
fn source_apply_audit_is_the_registered_version_thirty_seven() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[36];

    require(
        version == 37,
        format!("source apply audit migration is version {version}"),
    )?;
    require(
        name == "source apply audit",
        format!("source apply audit migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("event_kind = 'source_apply'")
            && sql.contains("denial_reason = 'source_apply:committed'")
            && sql.contains("source_revision_id IS NOT NULL")
            && sql.contains("catalogue_revision_id IS NOT NULL"),
        "source apply audit migration does not constrain the committed candidate shape",
    )
}

#[test]
fn source_apply_principal_binding_is_the_registered_version_thirty_eight() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[37];

    require(
        version == 38,
        format!("source apply principal binding migration is version {version}"),
    )?;
    require(
        name == "source apply principal binding",
        format!("source apply principal binding migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("event_kind <> 'source_apply'")
            && sql.contains(
                "session_principal_id = decode('00000000000000000000000000000001', 'hex')",
            ),
        "source apply principal binding migration does not bind the fixed service principal",
    )
}

#[test]
fn protected_security_audit_is_the_registered_version_eleven() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[10];

    require(
        version == 11,
        format!("last migration is version {version}"),
    )?;
    require(
        name == "protected security audit",
        format!("last migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.security_audit_events"),
        "protected security audit migration does not create its table",
    )
}

#[test]
fn catalogue_enum_type_storage_is_the_registered_version_twelve() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[11];

    require(
        version == 12,
        format!("catalogue enum migration is version {version}"),
    )?;
    require(
        name == "catalogue enum type storage",
        format!("catalogue enum migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.catalogue_enum_types")
            && sql.contains("labels text[] NOT NULL")
            && sql.contains("cardinality(labels) > 0")
            && sql.contains("REVOKE ALL ON TABLE _orna_kernel.catalogue_enum_types FROM PUBLIC"),
        "catalogue enum migration does not preserve protected ordered label storage",
    )
}

#[test]
fn resolved_enum_type_storage_is_the_registered_version_thirteen() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[12];

    require(
        version == 13,
        format!("resolved enum migration is version {version}"),
    )?;
    require(
        name == "resolved enum type storage",
        format!("resolved enum migration has unexpected name {name:?}"),
    )?;
    for column in ["enum_type_id", "return_enum_type_id"] {
        require(
            sql.contains(column),
            format!("resolved enum migration omits {column}"),
        )?;
    }
    require(
        sql.matches("REFERENCES _orna_kernel.catalogue_enum_types")
            .count()
            == 4,
        "resolved enum migration does not bind every type position to the enum catalogue",
    )
}

#[test]
fn enum_reference_targets_are_the_registered_version_fourteen() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[13];

    require(
        version == 14,
        format!("enum reference migration is version {version}"),
    )?;
    require(
        name == "catalogue enum reference targets",
        format!("enum reference migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("target_enum_catalogue_revision_id")
            && sql.contains("target_kind = 'enum_type'")
            && sql.contains("REFERENCES _orna_kernel.catalogue_enum_types"),
        "enum reference migration does not bind named evidence to its catalogue enum",
    )
}

#[test]
fn record_value_storage_is_the_registered_version_fifteen() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[14];

    require(
        version == 15,
        format!("last migration is version {version}"),
    )?;
    require(
        name == "catalogue record value storage",
        format!("last migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.catalogue_record_value_types")
            && sql.contains("CREATE TABLE _orna_kernel.catalogue_record_value_fields")
            && sql.contains("CHECK (value_kind = 'record')")
            && sql.contains("CHECK (mutability = 'immutable')")
            && sql.contains("CHECK (persistence = 'persistable')")
            && sql.contains("CHECK (type_kind IN ('value', 'enum'))")
            && sql.contains("REFERENCES _orna_kernel.standard_catalogue_value_types")
            && sql.contains("REFERENCES _orna_kernel.catalogue_enum_types")
            && sql.contains(
                "REVOKE ALL ON TABLE _orna_kernel.catalogue_record_value_types FROM PUBLIC",
            )
            && sql.contains(
                "REVOKE ALL ON TABLE _orna_kernel.catalogue_record_value_fields FROM PUBLIC",
            ),
        "record value migration does not preserve the complete protected definition contract",
    )
}

#[test]
fn record_field_reference_targets_are_the_registered_version_seventeen() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[16];
    require(version == 17, format!("migration is version {version}"))?;
    require(
        name == "record value field reference targets",
        format!("migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("target_kind = 'record_field'")
            && sql.contains("definition_references_record_field_target_fk")
            && sql.contains("REFERENCES _orna_kernel.catalogue_record_value_fields")
            && sql.contains("DEFERRABLE INITIALLY DEFERRED")
            && !sql.contains("LANGUAGE plpgsql"),
        "record-field reference migration does not preserve exact relational integrity",
    )
}

#[test]
fn disjoint_field_reference_targets_are_the_registered_version_eighteen() -> TestResult<()> {
    let (version, name, sql) = MIGRATIONS[17];
    require(version == 18, format!("migration is version {version}"))?;
    require(
        name == "disjoint field reference targets",
        format!("migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("target_record_field_owner_type_id")
            && sql.contains("WHERE target_kind = 'record_field'")
            && sql.contains("definition_references_record_field_target_fk")
            && sql.contains("REFERENCES _orna_kernel.catalogue_record_value_fields")
            && sql.contains("DEFERRABLE INITIALLY DEFERRED")
            && !sql.contains("LANGUAGE plpgsql"),
        "disjoint field-reference migration does not preserve exact relational integrity",
    )
}

#[test]
fn standard_opaque_value_storage_is_the_registered_version_nineteen() -> TestResult<()> {
    let Some((version, name, sql)) = MIGRATIONS.get(18).copied() else {
        return Err(failure(
            "standard opaque value storage migration is not registered",
        ));
    };
    require(version == 19, format!("migration is version {version}"))?;
    require(
        name == "standard opaque value storage",
        format!("migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("value_kind IN ('primitive', 'opaque')")
            && sql.contains("value_kind <> 'opaque'")
            && sql.contains("persistence = 'transient'")
            && sql.contains("octet_length(representation_contract) <= 128")
            && sql.contains("representation_contract !~ '[^ -~]'")
            && !sql.contains("CREATE TYPE")
            && !sql.contains("LANGUAGE"),
        "opaque value migration does not preserve the closed definition-only contract",
    )
}

#[test]
fn standard_enum_record_fields_are_the_registered_version_twenty() -> TestResult<()> {
    let Some((version, name, sql)) = MIGRATIONS.get(19).copied() else {
        return Err(failure(
            "standard enum record field storage migration is not registered",
        ));
    };
    require(version == 20, format!("migration is version {version}"))?;
    require(
        name == "standard enum record field storage",
        format!("migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.standard_catalogue_enum_types")
            && sql.contains("target_type_kind IN ('value', 'enum')")
            && sql.contains("target_enum_type_id")
            && sql.contains("enum_standard_library_revision_id")
            && sql.contains("standard_enum_type_id")
            && sql.contains("cat_record_value_fields_std_enum_fk")
            && sql.contains("DEFERRABLE INITIALLY DEFERRED")
            && !sql.contains("CREATE TYPE")
            && !sql.contains("LANGUAGE"),
        "standard enum migration does not preserve its protected relational contract",
    )
}

#[test]
fn durable_user_state_cells_are_the_registered_version_twenty_five() -> TestResult<()> {
    let Some((version, name, sql)) = MIGRATIONS.get(24).copied() else {
        return Err(failure(
            "durable user state cells migration is not registered",
        ));
    };
    require(version == 25, format!("migration is version {version}"))?;
    require(
        name == "durable user state cells",
        format!("migration has unexpected name {name:?}"),
    )?;
    require(
        sql.contains("CREATE TABLE _orna_kernel.user_state_cells")
            && sql.contains("principal_id bytea NOT NULL")
            && sql.contains("root_function_id bytea NOT NULL")
            && sql.contains("root_state_profile text NOT NULL")
            && sql.contains("function_id bytea NOT NULL")
            && sql.contains("function_instance_key text NOT NULL")
            && sql.contains("state_slot_id bytea NOT NULL")
            && sql.contains("value_bytes bytea NOT NULL")
            && sql.contains("value_type_id bytea NOT NULL")
            && sql.contains("revision bigint NOT NULL")
            && sql.contains("updated_at timestamp with time zone NOT NULL")
            && sql.contains("DEFAULT transaction_timestamp()")
            && sql.contains("CONSTRAINT user_state_cells_pkey")
            && sql.contains("CONSTRAINT user_state_cells_identity_lengths CHECK")
            && sql.contains("octet_length(principal_id) = 16")
            && sql.contains("octet_length(value_type_id) = 16")
            && sql.contains("CONSTRAINT user_state_cells_revision_check CHECK (revision > 0)")
            && sql.contains(
                "CREATE INDEX user_state_cells_principal_root_state_profile_idx\n    ON _orna_kernel.user_state_cells (principal_id, root_function_id, root_state_profile)",
            )
            && sql.contains("REVOKE ALL ON TABLE _orna_kernel.user_state_cells FROM PUBLIC"),
        "user state migration does not preserve the complete protected cell contract",
    )
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_creates_one_recoverable_empty_revision() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = Arc::new(PostgresKernel::from_str(&database.connection_string())?);

        let first_kernel = Arc::clone(&kernel);
        let second_kernel = Arc::clone(&kernel);
        let (first_result, second_result) =
            tokio::join!(first_kernel.bootstrap(), second_kernel.bootstrap(),);
        let first = first_result?;
        let second = second_result?;
        require(
            first == second,
            "concurrent bootstrap calls returned different revisions",
        )?;

        let reconnected = PostgresKernel::new(database.config()?);
        let recovered = reconnected.bootstrap().await?;
        require(
            recovered == first,
            "a newly constructed kernel did not recover the active revision",
        )?;

        inspect_bootstrap_state(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_the_seeded_initial_catalogue() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_initial_catalogue(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        kernel.bootstrap().await?;
        inspect_bootstrap_state(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_the_registered_v2_empty_catalogue() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v2_empty_catalogue(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        kernel.bootstrap().await?;
        inspect_bootstrap_state(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_the_registered_v3_empty_catalogue() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v3_empty_catalogue(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        kernel.bootstrap().await?;
        inspect_bootstrap_state(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_the_registered_v20_empty_catalogue() -> TestResult<()> {
    with_test_database(|database| async move {
        let session = database.open().await?;
        let seed_result = async {
            seed_initial_catalogue_client(session.client()).await?;
            apply_and_register_migrations(session.client(), &MIGRATIONS[1..20]).await
        }
        .await;
        let shutdown_result = session.shutdown().await;
        match (seed_result, shutdown_result) {
            (Ok(()), Ok(())) => {}
            (Err(error), Ok(())) | (Ok(()), Err(error)) => return Err(error),
            (Err(seed_error), Err(shutdown_error)) => {
                return Err(failure(format!(
                    "registered v20 catalogue seed failed: {seed_error}; seed driver shutdown failed: {shutdown_error}"
                )))
            }
        }

        let before = snapshot_upgrade_state(&database).await?;
        require(
            before.migrations.len() == 20,
            format!(
                "registered v20 setup produced unexpected migrations: {:?}",
                before.migrations
            ),
        )?;

        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;

        let after = snapshot_upgrade_state(&database).await?;
        require(
            after.migrations.len() == MIGRATIONS.len() && after.migrations[..20] == before.migrations[..],
            format!("v21-v45 changed prior migration records: {:?}", after.migrations),
        )?;
        require(
            after.migrations[20]
                == (
                    21,
                    "nested record field targets".to_owned(),
                    expected_migration_checksum(21, MIGRATIONS[20].2),
                ),
            format!("v21 migration record is not exact: {:?}", after.migrations[20]),
        )?;
        require(
            after.migrations[21]
                == (
                    22,
                    "protected invocation audit".to_owned(),
                    expected_migration_checksum(22, MIGRATIONS[21].2),
                ),
            format!("v22 migration record is not exact: {:?}", after.migrations[21]),
        )?;
        require(
            after.migrations[22]
                == (
                    23,
                    "executable standard relations".to_owned(),
                    expected_migration_checksum(23, MIGRATIONS[22].2),
                ),
            format!("v23 migration record is not exact: {:?}", after.migrations[22]),
        )?;
        require(
            after.migrations[23]
                == (
                    24,
                    "capability audit decisions".to_owned(),
                    expected_migration_checksum(24, MIGRATIONS[23].2),
                ),
            format!("v24 migration record is not exact: {:?}", after.migrations[23]),
        )?;
        require(
            after.migrations[24]
                == (
                    25,
                    "durable user state cells".to_owned(),
                    expected_migration_checksum(25, MIGRATIONS[24].2),
                ),
            format!("v25 migration record is not exact: {:?}", after.migrations[24]),
        )?;
        require(
            after.migrations[25]
                == (
                    26,
                    "user state audit decisions".to_owned(),
                    expected_migration_checksum(26, MIGRATIONS[25].2),
                ),
            format!("v26 migration record is not exact: {:?}", after.migrations[25]),
        )?;
        require(
            after.migrations[26]
                == (
                    27,
                    "inspect snapshots and trace".to_owned(),
                    expected_migration_checksum(27, MIGRATIONS[26].2),
                ),
            format!("v27 migration record is not exact: {:?}", after.migrations[26]),
        )?;
        require(
            after.migrations[27]
                == (
                    28,
                    "security admin privilege grants".to_owned(),
                    expected_migration_checksum(28, MIGRATIONS[27].2),
                ),
            format!("v28 migration record is not exact: {:?}", after.migrations[27]),
        )?;
        require(
            after.migrations[28]
                == (
                    29,
                    "sealed system invocation authorities".to_owned(),
                    expected_migration_checksum(29, MIGRATIONS[28].2),
                ),
            format!("v29 migration record is not exact: {:?}", after.migrations[28]),
        )?;
        require(
            after.migrations[29]
                == (
                    30,
                    "active roles system invocation authority".to_owned(),
                    expected_migration_checksum(30, MIGRATIONS[29].2),
                ),
            format!("v30 migration record is not exact: {:?}", after.migrations[29]),
        )?;
        require(
            after.migrations[30]
                == (
                    31,
                    "standard JSON executable format".to_owned(),
                    expected_migration_checksum(31, MIGRATIONS[30].2),
                ),
            format!("v31 migration record is not exact: {:?}", after.migrations[30]),
        )?;
        require(
            after.migrations[31]
                == (
                    32,
                    "protected resource audit".to_owned(),
                    expected_migration_checksum(32, MIGRATIONS[31].2),
                ),
            format!("v32 migration record is not exact: {:?}", after.migrations[31]),
        )?;
        require(
            after.migrations[32]
                == (
                    33,
                    "stream function returns".to_owned(),
                    expected_migration_checksum(33, MIGRATIONS[32].2),
                ),
            format!("v33 migration record is not exact: {:?}", after.migrations[32]),
        )?;
        require(
            after.migrations[33]
                == (
                    34,
                    "resource request identity history".to_owned(),
                    expected_migration_checksum(34, MIGRATIONS[33].2),
                ),
            format!("v34 migration record is not exact: {:?}", after.migrations[33]),
        )?;
        require(
            after.migrations[34]
                == (
                    35,
                    "resource audit target authorities".to_owned(),
                    expected_migration_checksum(35, MIGRATIONS[34].2),
                ),
            format!("v35 migration record is not exact: {:?}", after.migrations[34]),
        )?;
        require(
            after.migrations[35]
                == (
                    36,
                    "sealed Inspector value types".to_owned(),
                    expected_migration_checksum(36, MIGRATIONS[35].2),
                ),
            format!("v36 migration record is not exact: {:?}", after.migrations[35]),
        )?;
        require(
            after.migrations[36]
                == (
                    37,
                    "source apply audit".to_owned(),
                    expected_migration_checksum(37, MIGRATIONS[36].2),
                ),
            format!("v37 migration record is not exact: {:?}", after.migrations[36]),
        )?;
        for (index, (version, name)) in [
            (37, (38, "source apply principal binding")),
            (38, (39, "sealed invocation SECURITY DEFINER denial audit")),
            (39, (40, "security admin class-wide grant boundary")),
            (40, (41, "nullable resource audit nested invocation")),
        ] {
            require(
                after.migrations[index]
                    == (
                        version,
                        name.to_owned(),
                        expected_migration_checksum(version, MIGRATIONS[index].2),
                    ),
                format!("v{version} migration record is not exact: {:?}", after.migrations[index]),
            )?;
        }
        require(
            after.active_pair == before.active_pair,
            "v21-v45 changed the active revision pair",
        )?;

        let recovered = kernel.recover().await?;
        let (source_revision_id, catalogue_revision_id) = after.active_pair;
        require(
            recovered.pair().source().to_bytes().to_vec() == source_revision_id
                && recovered.pair().catalogue().to_bytes().to_vec() == catalogue_revision_id,
            "v21-v45 recovery does not preserve the active revision pair",
        )?;
        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn user_state_cells_migration_applies_cleanly_and_relation_is_closed() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;
        inspect_user_state_cells_storage(&database).await
    })
    .await
}

async fn inspect_user_state_cells_storage(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let inspection_result = async {
        let client = session.client();
        inspect_columns(
            client,
            "user_state_cells",
            &[
                ("principal_id", "bytea", "bytea", "NO", Some("")),
                ("root_function_id", "bytea", "bytea", "NO", Some("")),
                ("root_state_profile", "text", "text", "NO", Some("")),
                ("function_id", "bytea", "bytea", "NO", Some("")),
                ("function_instance_key", "text", "text", "NO", Some("")),
                ("state_slot_id", "bytea", "bytea", "NO", Some("")),
                ("value_bytes", "bytea", "bytea", "NO", Some("")),
                ("value_type_id", "bytea", "bytea", "NO", Some("")),
                ("revision", "bigint", "int8", "NO", Some("")),
                (
                    "updated_at",
                    "timestamp with time zone",
                    "timestamptz",
                    "NO",
                    Some("transaction_timestamp()"),
                ),
            ],
        )
        .await?;
        require_exact_constraint(
            client,
            "user_state_cells",
            "user_state_cells_pkey",
            "PRIMARY KEY (principal_id, root_function_id, root_state_profile, function_id, function_instance_key, state_slot_id)",
            false,
            false,
        )
        .await?;
        require_constraint(
            client,
            "user_state_cells",
            "user_state_cells_identity_lengths",
            "octet_length(principal_id) = 16",
        )
        .await?;
        require_constraint(
            client,
            "user_state_cells",
            "user_state_cells_revision_check",
            "revision > 0",
        )
        .await?;
        require_index_shape(
            client,
            "user_state_cells_principal_root_state_profile_idx",
            "user_state_cells",
            "(principal_id, root_function_id, root_state_profile)",
            None,
        )
        .await?;
        for privilege in [
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "TRUNCATE",
            "REFERENCES",
            "TRIGGER",
            "MAINTAIN",
        ] {
            let relation = "_orna_kernel.user_state_cells";
            let row = client
                .query_one(
                    "SELECT has_table_privilege('public', $1, $2)",
                    &[&relation, &privilege],
                )
                .await?;
            require(
                !value::<bool>(&row, 0)?,
                format!("PUBLIC has {privilege} on protected table {relation}"),
            )?;
        }

        // The closed domains are enforced, not merely declared: a valid cell
        // writes with its default timestamp, a duplicate full key is
        // rejected, a zero revision is rejected, and a short identity is
        // rejected.
        client
            .batch_execute(
                "INSERT INTO _orna_kernel.user_state_cells
                     (principal_id, root_function_id, root_state_profile,
                      function_id, function_instance_key, state_slot_id,
                      value_bytes, value_type_id, revision)
                 VALUES
                     (decode(repeat('a1', 16), 'hex'),
                      decode(repeat('a2', 16), 'hex'), '',
                      decode(repeat('a3', 16), 'hex'), '',
                      decode(repeat('a4', 16), 'hex'),
                      decode('00aabb', 'hex'),
                      decode(repeat('a5', 16), 'hex'), 1);",
            )
            .await?;
        let row = client
            .query_one(
                "SELECT updated_at IS NOT NULL
                 FROM _orna_kernel.user_state_cells
                 WHERE principal_id = decode(repeat('a1', 16), 'hex')",
                &[],
            )
            .await?;
        let stamped: bool = value(&row, 0)?;
        require(
            stamped,
            "user_state_cells write did not stamp updated_at",
        )?;

        let duplicate = client
            .batch_execute(
                "INSERT INTO _orna_kernel.user_state_cells
                     (principal_id, root_function_id, root_state_profile,
                      function_id, function_instance_key, state_slot_id,
                      value_bytes, value_type_id, revision)
                 VALUES
                     (decode(repeat('a1', 16), 'hex'),
                      decode(repeat('a2', 16), 'hex'), '',
                      decode(repeat('a3', 16), 'hex'), '',
                      decode(repeat('a4', 16), 'hex'),
                      decode('00ccdd', 'hex'),
                      decode(repeat('a5', 16), 'hex'), 2);",
            )
            .await
            .err();
        require(
            duplicate
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("user_state_cells_pkey"),
            format!("duplicate user state key failed for the wrong reason: {duplicate:?}"),
        )?;

        let zero_revision = client
            .batch_execute(
                "INSERT INTO _orna_kernel.user_state_cells
                     (principal_id, root_function_id, root_state_profile,
                      function_id, function_instance_key, state_slot_id,
                      value_bytes, value_type_id, revision)
                 VALUES
                     (decode(repeat('b1', 16), 'hex'),
                      decode(repeat('b2', 16), 'hex'), '',
                      decode(repeat('b3', 16), 'hex'), '',
                      decode(repeat('b4', 16), 'hex'),
                      decode('00aabb', 'hex'),
                      decode(repeat('b5', 16), 'hex'), 0);",
            )
            .await
            .err();
        require(
            zero_revision
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("user_state_cells_revision_check"),
            format!("zero revision failed for the wrong reason: {zero_revision:?}"),
        )?;

        let short_identity = client
            .batch_execute(
                "INSERT INTO _orna_kernel.user_state_cells
                     (principal_id, root_function_id, root_state_profile,
                      function_id, function_instance_key, state_slot_id,
                      value_bytes, value_type_id, revision)
                 VALUES
                     (decode(repeat('c1', 15), 'hex'),
                      decode(repeat('c2', 16), 'hex'), '',
                      decode(repeat('c3', 16), 'hex'), '',
                      decode(repeat('c4', 16), 'hex'),
                      decode('00aabb', 'hex'),
                      decode(repeat('c5', 16), 'hex'), 1);",
            )
            .await
            .err();
        require(
            short_identity
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("user_state_cells_identity_lengths"),
            format!("short principal identity failed for the wrong reason: {short_identity:?}"),
        )?;
        Ok(())
    }
    .await;
    let shutdown_result = session.shutdown().await;
    match (inspection_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(inspection_error), Err(shutdown_error)) => Err(failure(format!(
            "user state storage inspection failed: {inspection_error}; shutdown failed: {shutdown_error}"
        ))),
    }
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn inspect_snapshots_migration_applies_cleanly_and_relations_are_closed() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;
        inspect_inspect_storage(&database).await
    })
    .await
}

async fn inspect_inspect_storage(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let inspection_result = async {
        let client = session.client();
        inspect_columns(
            client,
            "inspect_snapshots",
            &[
                ("epoch_id", "bytea", "bytea", "NO", Some("")),
                ("invocation_id", "bytea", "bytea", "NO", Some("")),
                (
                    "recorded_at",
                    "timestamp with time zone",
                    "timestamptz",
                    "NO",
                    Some("transaction_timestamp()"),
                ),
                ("owner_principal_id", "bytea", "bytea", "NO", Some("")),
                ("source_revision_id", "bytea", "bytea", "NO", Some("")),
                ("catalogue_revision_id", "bytea", "bytea", "NO", Some("")),
                ("summary_bytes", "bytea", "bytea", "NO", Some("")),
                (
                    "observer_root_invocation_id",
                    "bytea",
                    "bytea",
                    "YES",
                    Some(""),
                ),
                (
                    "observer_parent_invocation_id",
                    "bytea",
                    "bytea",
                    "YES",
                    Some(""),
                ),
                ("observer_purpose", "text", "text", "YES", Some("")),
            ],
        )
        .await?;
        inspect_columns(
            client,
            "inspect_trace_events",
            &[
                ("invocation_id", "bytea", "bytea", "NO", Some("")),
                ("sequence", "bigint", "int8", "NO", Some("")),
                ("kind", "text", "text", "NO", Some("")),
                ("payload_bytes", "bytea", "bytea", "NO", Some("")),
                ("observer_invocation_id", "bytea", "bytea", "YES", Some("")),
                (
                    "recorded_at",
                    "timestamp with time zone",
                    "timestamptz",
                    "NO",
                    Some("transaction_timestamp()"),
                ),
            ],
        )
        .await?;
        require_exact_constraint(
            client,
            "inspect_snapshots",
            "inspect_snapshots_pkey",
            "PRIMARY KEY (epoch_id)",
            false,
            false,
        )
        .await?;
        require_exact_constraint(
            client,
            "inspect_trace_events",
            "inspect_trace_events_pkey",
            "PRIMARY KEY (invocation_id, sequence)",
            false,
            false,
        )
        .await?;
        require_constraint(
            client,
            "inspect_snapshots",
            "inspect_snapshots_identity_lengths",
            "octet_length(epoch_id) = 16",
        )
        .await?;
        require_constraint(
            client,
            "inspect_trace_events",
            "inspect_trace_events_identity_lengths",
            "octet_length(invocation_id) = 16",
        )
        .await?;
        require_constraint(
            client,
            "inspect_trace_events",
            "inspect_trace_events_sequence_check",
            "sequence >= 0",
        )
        .await?;
        require_constraint(
            client,
            "inspect_trace_events",
            "inspect_trace_events_kind_check",
            "'inspect_snapshot'",
        )
        .await?;
        for privilege in [
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "TRUNCATE",
            "REFERENCES",
            "TRIGGER",
            "MAINTAIN",
        ] {
            for relation in [
                "_orna_kernel.inspect_snapshots",
                "_orna_kernel.inspect_trace_events",
            ] {
                let row = client
                    .query_one(
                        "SELECT has_table_privilege('public', $1, $2)",
                        &[&relation, &privilege],
                    )
                    .await?;
                require(
                    !value::<bool>(&row, 0)?,
                    format!("PUBLIC has {privilege} on protected table {relation}"),
                )?;
            }
        }

        // The closed domains are enforced, not merely declared: a valid
        // snapshot writes with its default timestamp, a duplicate epoch id
        // is rejected, a short identity is rejected, an unknown invocation
        // is rejected, and trace rows enforce the composite key, the closed
        // kind set, non-negative sequences, and identity lengths.
        client
            .batch_execute(
                "INSERT INTO _orna_kernel.invocation_audit_events
                     (event_id, invocation_id, outcome, session_principal_id)
                 VALUES
                     (decode(repeat('e1', 16), 'hex'),
                      decode(repeat('f1', 16), 'hex'), 'denied',
                      decode(repeat('71', 16), 'hex'));",
            )
            .await?;
        client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_snapshots
                     (epoch_id, invocation_id, recorded_at, owner_principal_id,
                      source_revision_id, catalogue_revision_id, summary_bytes)
                 VALUES
                     (decode(repeat('d1', 16), 'hex'),
                      decode(repeat('f1', 16), 'hex'),
                      transaction_timestamp(),
                      decode(repeat('71', 16), 'hex'),
                      decode(repeat('d2', 16), 'hex'),
                      decode(repeat('d3', 16), 'hex'),
                      decode('00aabb', 'hex'));",
            )
            .await?;
        let row = client
            .query_one(
                "SELECT recorded_at IS NOT NULL
                 FROM _orna_kernel.inspect_snapshots
                 WHERE epoch_id = decode(repeat('d1', 16), 'hex')",
                &[],
            )
            .await?;
        require(
            value::<bool>(&row, 0)?,
            "inspect_snapshots write did not stamp recorded_at",
        )?;

        let duplicate_epoch = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_snapshots
                     (epoch_id, invocation_id, recorded_at, owner_principal_id,
                      source_revision_id, catalogue_revision_id, summary_bytes)
                 VALUES
                     (decode(repeat('d1', 16), 'hex'),
                      decode(repeat('f1', 16), 'hex'),
                      transaction_timestamp(),
                      decode(repeat('71', 16), 'hex'),
                      decode(repeat('d2', 16), 'hex'),
                      decode(repeat('d3', 16), 'hex'),
                      decode('00ccdd', 'hex'));",
            )
            .await
            .err();
        require(
            duplicate_epoch
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_snapshots_pkey"),
            format!("duplicate epoch id failed for the wrong reason: {duplicate_epoch:?}"),
        )?;

        let short_epoch = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_snapshots
                     (epoch_id, invocation_id, recorded_at, owner_principal_id,
                      source_revision_id, catalogue_revision_id, summary_bytes)
                 VALUES
                     (decode(repeat('d1', 15), 'hex'),
                      decode(repeat('f1', 16), 'hex'),
                      transaction_timestamp(),
                      decode(repeat('71', 16), 'hex'),
                      decode(repeat('d2', 16), 'hex'),
                      decode(repeat('d3', 16), 'hex'),
                      decode('00aabb', 'hex'));",
            )
            .await
            .err();
        require(
            short_epoch
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_snapshots_identity_lengths"),
            format!("short epoch identity failed for the wrong reason: {short_epoch:?}"),
        )?;

        let unknown_invocation = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_snapshots
                     (epoch_id, invocation_id, recorded_at, owner_principal_id,
                      source_revision_id, catalogue_revision_id, summary_bytes)
                 VALUES
                     (decode(repeat('d4', 16), 'hex'),
                      decode(repeat('f9', 16), 'hex'),
                      transaction_timestamp(),
                      decode(repeat('71', 16), 'hex'),
                      decode(repeat('d2', 16), 'hex'),
                      decode(repeat('d3', 16), 'hex'),
                      decode('00aabb', 'hex'));",
            )
            .await
            .err();
        require(
            unknown_invocation
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_snapshots_invocation_fk"),
            format!("unknown invocation failed for the wrong reason: {unknown_invocation:?}"),
        )?;

        client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES
                     (decode(repeat('f1', 16), 'hex'), 0, 'started',
                      decode('00aa', 'hex'), NULL, transaction_timestamp()),
                     (decode(repeat('f1', 16), 'hex'), 1, 'value_batch',
                      decode('00bb', 'hex'),
                      decode(repeat('f2', 16), 'hex'), transaction_timestamp()),
                     (decode(repeat('f1', 16), 'hex'), 2, 'completed',
                      decode('00cc', 'hex'), NULL, transaction_timestamp()),
                     (decode(repeat('f1', 16), 'hex'), 3, 'inspect_snapshot',
                      decode('00dd', 'hex'), NULL, transaction_timestamp());",
            )
            .await?;

        let duplicate_trace = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES
                     (decode(repeat('f1', 16), 'hex'), 0, 'started',
                      decode('00ee', 'hex'), NULL, transaction_timestamp());",
            )
            .await
            .err();
        require(
            duplicate_trace
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_trace_events_pkey"),
            format!("duplicate trace key failed for the wrong reason: {duplicate_trace:?}"),
        )?;

        let bad_kind = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES
                     (decode(repeat('f1', 16), 'hex'), 4, 'snapshot',
                      decode('00ee', 'hex'), NULL, transaction_timestamp());",
            )
            .await
            .err();
        require(
            bad_kind
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_trace_events_kind_check"),
            format!("unclosed trace kind failed for the wrong reason: {bad_kind:?}"),
        )?;

        let negative_sequence = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES
                     (decode(repeat('f1', 16), 'hex'), -1, 'started',
                      decode('00ee', 'hex'), NULL, transaction_timestamp());",
            )
            .await
            .err();
        require(
            negative_sequence
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_trace_events_sequence_check"),
            format!("negative trace sequence failed for the wrong reason: {negative_sequence:?}"),
        )?;

        let short_observer = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES
                     (decode(repeat('f1', 16), 'hex'), 4, 'started',
                      decode('00ee', 'hex'),
                      decode(repeat('f3', 15), 'hex'), transaction_timestamp());",
            )
            .await
            .err();
        require(
            short_observer
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_trace_events_identity_lengths"),
            format!("short observer identity failed for the wrong reason: {short_observer:?}"),
        )?;

        let unknown_trace_invocation = client
            .batch_execute(
                "INSERT INTO _orna_kernel.inspect_trace_events
                     (invocation_id, sequence, kind, payload_bytes,
                      observer_invocation_id, recorded_at)
                 VALUES
                     (decode(repeat('fa', 16), 'hex'), 0, 'started',
                      decode('00ee', 'hex'), NULL, transaction_timestamp());",
            )
            .await
            .err();
        require(
            unknown_trace_invocation
                .as_ref()
                .and_then(|error| error.as_db_error())
                .and_then(|error| error.constraint())
                == Some("inspect_trace_events_invocation_fk"),
            format!(
                "unknown trace invocation failed for the wrong reason: {unknown_trace_invocation:?}"
            ),
        )?;
        Ok(())
    }
    .await;
    let shutdown_result = session.shutdown().await;
    match (inspection_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(inspection_error), Err(shutdown_error)) => Err(failure(format!(
            "inspect storage inspection failed: {inspection_error}; shutdown failed: {shutdown_error}"
        ))),
    }
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_enforces_nested_record_field_target_storage() -> TestResult<()> {
    with_test_database(|database| async move {
        let session = database.open().await?;
        let result = async {
            let kernel = PostgresKernel::from_str(&database.connection_string())?;
            kernel.bootstrap().await?;
            verify_nested_record_field_target_storage(session.client()).await
        }
        .await;
        let shutdown_result = session.shutdown().await;
        match (result, shutdown_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(verify_error), Err(shutdown_error)) => Err(failure(format!(
                "nested record field target storage verification failed: {verify_error}; verification driver shutdown failed: {shutdown_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_owner_qualifies_registered_v4_semantic_references() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v4_semantic_catalogue(&database, false).await?;
        seed_registered_v4_physical_catalogue(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        kernel.bootstrap().await?;
        verify_owner_qualified_reference_backfill(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_v5_write_reference_evidence_without_mutating_semantics()
-> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v5_semantic_catalogue(&database).await?;
        seed_registered_v4_physical_catalogue(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        let expected_revision = registered_v4_semantic_fixture()?;
        let before = snapshot_upgrade_state(&database).await?;

        require(
            before.migrations.len() == 5
                && before.migrations.last().map(|migration| migration.0) == Some(5),
            format!("manual v5 setup produced unexpected migrations: {:?}", before.migrations),
        )?;
        require(
            before.active_pair
                == (
                    expected_revision.pair().source().to_bytes().to_vec(),
                    expected_revision.pair().catalogue().to_bytes().to_vec(),
                ),
            format!("manual v5 setup changed the active pair: {:?}", before.active_pair),
        )?;

        kernel.bootstrap().await?;

        let after = snapshot_upgrade_state(&database).await?;
        require(
            after.migrations.len() == MIGRATIONS.len() && after.migrations[..5] == before.migrations[..],
            format!("v6-v45 changed prior migration records: {:?}", after.migrations),
        )?;
        require(
            after.migrations[5]
                == (
                    6,
                    "definition reference write evidence".to_owned(),
                    expected_migration_checksum(6, MIGRATIONS[5].2),
                ),
            format!("v6 migration record is not exact: {:?}", after.migrations[5]),
        )?;
        require(
            after.migrations[6]
                == (
                    7,
                    "standard catalogue type storage".to_owned(),
                    expected_migration_checksum(7, MIGRATIONS[6].2),
                ),
            format!("v7 migration record is not exact: {:?}", after.migrations[6]),
        )?;
        require(
            after.migrations[7]
                == (
                    8,
                    "resolved value type storage".to_owned(),
                    expected_migration_checksum(8, MIGRATIONS[7].2),
                ),
            format!("v8 migration record is not exact: {:?}", after.migrations[7]),
        )?;
        require(
            after.migrations[8]
                == (
                    9,
                    "security decision snapshot".to_owned(),
                    expected_migration_checksum(9, MIGRATIONS[8].2),
                ),
            format!("v9 migration record is not exact: {:?}", after.migrations[8]),
        )?;
        require(
            after.migrations[9]
                == (
                    10,
                    "local peer credentials".to_owned(),
                    expected_migration_checksum(10, MIGRATIONS[9].2),
                ),
            format!("v10 migration record is not exact: {:?}", after.migrations[9]),
        )?;
        require(
            after.migrations[10]
                == (
                    11,
                    "protected security audit".to_owned(),
                    expected_migration_checksum(11, MIGRATIONS[10].2),
                ),
            format!("v11 migration record is not exact: {:?}", after.migrations[10]),
        )?;
        require(
            after.migrations[11]
                == (
                    12,
                    "catalogue enum type storage".to_owned(),
                    expected_migration_checksum(12, MIGRATIONS[11].2),
                ),
            format!("v12 migration record is not exact: {:?}", after.migrations[11]),
        )?;
        require(
            after.migrations[12]
                == (
                    13,
                    "resolved enum type storage".to_owned(),
                    expected_migration_checksum(13, MIGRATIONS[12].2),
                ),
            format!("v13 migration record is not exact: {:?}", after.migrations[12]),
        )?;
        require(
            after.migrations[13]
                == (
                    14,
                    "catalogue enum reference targets".to_owned(),
                    expected_migration_checksum(14, MIGRATIONS[13].2),
                ),
            format!("v14 migration record is not exact: {:?}", after.migrations[13]),
        )?;
        require(
            after.migrations[14]
                == (
                    15,
                    "catalogue record value storage".to_owned(),
                    expected_migration_checksum(15, MIGRATIONS[14].2),
                ),
            format!("v15 migration record is not exact: {:?}", after.migrations[14]),
        )?;
        require(
            after.migrations[15]
                == (
                    16,
                    "resolved record value type storage".to_owned(),
                    expected_migration_checksum(16, MIGRATIONS[15].2),
                ),
            format!("v16 migration record is not exact: {:?}", after.migrations[15]),
        )?;
        require(
            after.migrations[16]
                == (
                    17,
                    "record value field reference targets".to_owned(),
                    expected_migration_checksum(17, MIGRATIONS[16].2),
                ),
            format!("v17 migration record is not exact: {:?}", after.migrations[16]),
        )?;
        require(
            after.migrations[17]
                == (
                    18,
                    "disjoint field reference targets".to_owned(),
                    expected_migration_checksum(18, MIGRATIONS[17].2),
                ),
            format!("v18 migration record is not exact: {:?}", after.migrations[17]),
        )?;
        require(
            after.migrations[18]
                == (
                    19,
                    "standard opaque value storage".to_owned(),
                    expected_migration_checksum(19, MIGRATIONS[18].2),
                ),
            format!("v19 migration record is not exact: {:?}", after.migrations[18]),
        )?;
        require(
            after.migrations[19]
                == (
                    20,
                    "standard enum record field storage".to_owned(),
                    expected_migration_checksum(20, MIGRATIONS[19].2),
                ),
            format!("v20 migration record is not exact: {:?}", after.migrations[19]),
        )?;
        require(
            after.migrations[20]
                == (
                    21,
                    "nested record field targets".to_owned(),
                    expected_migration_checksum(21, MIGRATIONS[20].2),
                ),
            format!("v21 migration record is not exact: {:?}", after.migrations[20]),
        )?;
        require(
            after.migrations[21]
                == (
                    22,
                    "protected invocation audit".to_owned(),
                    expected_migration_checksum(22, MIGRATIONS[21].2),
                ),
            format!("v22 migration record is not exact: {:?}", after.migrations[21]),
        )?;
        require(
            after.migrations[22]
                == (
                    23,
                    "executable standard relations".to_owned(),
                    expected_migration_checksum(23, MIGRATIONS[22].2),
                ),
            format!("v23 migration record is not exact: {:?}", after.migrations[22]),
        )?;
        require(
            after.migrations[23]
                == (
                    24,
                    "capability audit decisions".to_owned(),
                    expected_migration_checksum(24, MIGRATIONS[23].2),
                ),
            format!("v24 migration record is not exact: {:?}", after.migrations[23]),
        )?;
        require(
            after.migrations[24]
                == (
                    25,
                    "durable user state cells".to_owned(),
                    expected_migration_checksum(25, MIGRATIONS[24].2),
                ),
            format!("v25 migration record is not exact: {:?}", after.migrations[24]),
        )?;
        require(
            after.migrations[25]
                == (
                    26,
                    "user state audit decisions".to_owned(),
                    expected_migration_checksum(26, MIGRATIONS[25].2),
                ),
            format!("v26 migration record is not exact: {:?}", after.migrations[25]),
        )?;
        require(
            after.migrations[26]
                == (
                    27,
                    "inspect snapshots and trace".to_owned(),
                    expected_migration_checksum(27, MIGRATIONS[26].2),
                ),
            format!("v27 migration record is not exact: {:?}", after.migrations[26]),
        )?;
        require(
            after.migrations[27]
                == (
                    28,
                    "security admin privilege grants".to_owned(),
                    expected_migration_checksum(28, MIGRATIONS[27].2),
                ),
            format!("v28 migration record is not exact: {:?}", after.migrations[27]),
        )?;
        require(
            after.migrations[28]
                == (
                    29,
                    "sealed system invocation authorities".to_owned(),
                    expected_migration_checksum(29, MIGRATIONS[28].2),
                ),
            format!("v29 migration record is not exact: {:?}", after.migrations[28]),
        )?;
        require(
            after.migrations[29]
                == (
                    30,
                    "active roles system invocation authority".to_owned(),
                    expected_migration_checksum(30, MIGRATIONS[29].2),
                ),
            format!("v30 migration record is not exact: {:?}", after.migrations[29]),
        )?;
        require(
            after.migrations[30]
                == (
                    31,
                    "standard JSON executable format".to_owned(),
                    expected_migration_checksum(31, MIGRATIONS[30].2),
                ),
            format!("v31 migration record is not exact: {:?}", after.migrations[30]),
        )?;
        require(
            after.migrations[31]
                == (
                    32,
                    "protected resource audit".to_owned(),
                    expected_migration_checksum(32, MIGRATIONS[31].2),
                ),
            format!("v32 migration record is not exact: {:?}", after.migrations[31]),
        )?;
        require(
            after.migrations[32]
                == (
                    33,
                    "stream function returns".to_owned(),
                    expected_migration_checksum(33, MIGRATIONS[32].2),
                ),
            format!("v33 migration record is not exact: {:?}", after.migrations[32]),
        )?;
        require(
            after.active_pair == before.active_pair,
            "v6 changed the active revision pair",
        )?;
        require(
            after.references == before.references,
            "v6 changed existing definition-reference rows or xmin values",
        )?;
        require(
            after.catalogue_hashes == before.catalogue_hashes
                && after.function_hashes == before.function_hashes,
            "v6 changed catalogue or function semantic hash bytes",
        )?;
        let after_revision = kernel.recover().await?;
        let pair_matches = expected_revision.pair() == after_revision.pair();
        let source_matches = expected_revision.source() == after_revision.source();
        let catalogue_hash_matches =
            expected_revision.catalogue_hash() == after_revision.catalogue_hash();
        let catalogue_revision_matches =
            expected_revision.catalogue().revision() == after_revision.catalogue().revision();
        let schemas_match =
            expected_revision.catalogue().schemas() == after_revision.catalogue().schemas();
        let object_types_match = expected_revision.catalogue().object_types()
            == after_revision.catalogue().object_types();
        let functions_match =
            expected_revision.catalogue().functions() == after_revision.catalogue().functions();
        let expressions_match = expected_revision.expressions() == after_revision.expressions();
        let function_revisions_match =
            expected_revision.function_revisions() == after_revision.function_revisions();
        let historical_revisions_match = expected_revision.historical_function_revisions()
            == after_revision.historical_function_revisions();
        let origins_match = same_members(expected_revision.origins(), after_revision.origins());
        let references_match = expected_revision.references() == after_revision.references();
        require(
            pair_matches
                && source_matches
                && catalogue_hash_matches
                && catalogue_revision_matches
                && schemas_match
                && object_types_match
                && functions_match
                && expressions_match
                && function_revisions_match
                && historical_revisions_match
                && origins_match
                && references_match,
            format!(
                "v6 recovery differs: pair={pair_matches}, source={source_matches}, catalogue_hash={catalogue_hash_matches}, catalogue_revision={catalogue_revision_matches}, schemas={schemas_match}, object_types={object_types_match}, functions={functions_match}, expressions={expressions_match}, function_revisions={function_revisions_match}, historical={historical_revisions_match}, origins={origins_match}, references={references_match}"
            ),
        )?;

        let session = database.open().await?;
        let verification_result = verify_write_reference_compatibility(session.client()).await;
        let shutdown_result = session.shutdown().await;
        match (verification_result, shutdown_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(verification_error), Err(shutdown_error)) => Err(failure(format!(
                "write-reference compatibility verification failed: {verification_error}; verification driver shutdown failed: {shutdown_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_registered_v6_without_standard_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v6_catalogue(&database).await?;
        let expected_revision = registered_v4_semantic_fixture()?;
        let before = snapshot_upgrade_state(&database).await?;
        require(
            before.migrations.len() == 6
                && before.migrations.last().map(|migration| migration.0) == Some(6),
            format!(
                "manual v6 setup produced unexpected migrations: {:?}",
                before.migrations
            ),
        )?;

        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;

        let after = snapshot_upgrade_state(&database).await?;
        require(
            after.migrations.len() == MIGRATIONS.len()
                && after.migrations[..6] == before.migrations[..]
                && after.migrations[6]
                    == (
                        7,
                        "standard catalogue type storage".to_owned(),
                        expected_migration_checksum(7, MIGRATIONS[6].2),
                    )
                && after.migrations[20]
                    == (
                        21,
                        "nested record field targets".to_owned(),
                        expected_migration_checksum(21, MIGRATIONS[20].2),
                    )
                && after.migrations[21]
                    == (
                        22,
                        "protected invocation audit".to_owned(),
                        expected_migration_checksum(22, MIGRATIONS[21].2),
                    )
                && after.migrations[22]
                    == (
                        23,
                        "executable standard relations".to_owned(),
                        expected_migration_checksum(23, MIGRATIONS[22].2),
                    )
                && after.migrations[23]
                    == (
                        24,
                        "capability audit decisions".to_owned(),
                        expected_migration_checksum(24, MIGRATIONS[23].2),
                    )
                && after.migrations[24]
                    == (
                        25,
                        "durable user state cells".to_owned(),
                        expected_migration_checksum(25, MIGRATIONS[24].2),
                    )
                && after.migrations[25]
                    == (
                        26,
                        "user state audit decisions".to_owned(),
                        expected_migration_checksum(26, MIGRATIONS[25].2),
                    )
                && after.migrations[26]
                    == (
                        27,
                        "inspect snapshots and trace".to_owned(),
                        expected_migration_checksum(27, MIGRATIONS[26].2),
                    )
                && after.migrations[27]
                    == (
                        28,
                        "security admin privilege grants".to_owned(),
                        expected_migration_checksum(28, MIGRATIONS[27].2),
                    )
                && after.migrations[28]
                    == (
                        29,
                        "sealed system invocation authorities".to_owned(),
                        expected_migration_checksum(29, MIGRATIONS[28].2),
                    )
                && after.migrations[29]
                    == (
                        30,
                        "active roles system invocation authority".to_owned(),
                        expected_migration_checksum(30, MIGRATIONS[29].2),
                    )
                && after.migrations[30]
                    == (
                        31,
                        "standard JSON executable format".to_owned(),
                        expected_migration_checksum(31, MIGRATIONS[30].2),
                    )
                && after.migrations[31]
                    == (
                        32,
                        "protected resource audit".to_owned(),
                        expected_migration_checksum(32, MIGRATIONS[31].2),
                    )
                && after.migrations[32]
                    == (
                        33,
                        "stream function returns".to_owned(),
                        expected_migration_checksum(33, MIGRATIONS[32].2),
                    ),
            format!("v6 upgrade produced unexpected migrations: {:?}", after.migrations),
        )?;
        require(
            after.migrations[7]
                == (
                    8,
                    "resolved value type storage".to_owned(),
                    expected_migration_checksum(8, MIGRATIONS[7].2),
                ),
            format!("v8 migration record is not exact: {:?}", after.migrations[7]),
        )?;
        require(
            after.migrations[8]
                == (
                    9,
                    "security decision snapshot".to_owned(),
                    expected_migration_checksum(9, MIGRATIONS[8].2),
                ),
            format!("v9 migration record is not exact: {:?}", after.migrations[8]),
        )?;
        require(
            after.migrations[9]
                == (
                    10,
                    "local peer credentials".to_owned(),
                    expected_migration_checksum(10, MIGRATIONS[9].2),
                ),
            format!("v10 migration record is not exact: {:?}", after.migrations[9]),
        )?;
        require(
            after.migrations[10]
                == (
                    11,
                    "protected security audit".to_owned(),
                    expected_migration_checksum(11, MIGRATIONS[10].2),
                ),
            format!("v11 migration record is not exact: {:?}", after.migrations[10]),
        )?;
        require(
            after.migrations[11]
                == (
                    12,
                    "catalogue enum type storage".to_owned(),
                    expected_migration_checksum(12, MIGRATIONS[11].2),
                ),
            format!("v12 migration record is not exact: {:?}", after.migrations[11]),
        )?;
        require(
            after.migrations[12]
                == (
                    13,
                    "resolved enum type storage".to_owned(),
                    expected_migration_checksum(13, MIGRATIONS[12].2),
                ),
            format!("v13 migration record is not exact: {:?}", after.migrations[12]),
        )?;
        require(
            after.migrations[13]
                == (
                    14,
                    "catalogue enum reference targets".to_owned(),
                    expected_migration_checksum(14, MIGRATIONS[13].2),
                ),
            format!("v14 migration record is not exact: {:?}", after.migrations[13]),
        )?;
        require(
            after.migrations[14]
                == (
                    15,
                    "catalogue record value storage".to_owned(),
                    expected_migration_checksum(15, MIGRATIONS[14].2),
                ),
            format!("v15 migration record is not exact: {:?}", after.migrations[14]),
        )?;
        require(
            after.migrations[15]
                == (
                    16,
                    "resolved record value type storage".to_owned(),
                    expected_migration_checksum(16, MIGRATIONS[15].2),
                ),
            format!("v16 migration record is not exact: {:?}", after.migrations[15]),
        )?;
        require(
            after.migrations[16]
                == (
                    17,
                    "record value field reference targets".to_owned(),
                    expected_migration_checksum(17, MIGRATIONS[16].2),
                ),
            format!("v17 migration record is not exact: {:?}", after.migrations[16]),
        )?;
        require(
            after.migrations[17]
                == (
                    18,
                    "disjoint field reference targets".to_owned(),
                    expected_migration_checksum(18, MIGRATIONS[17].2),
                ),
            format!("v18 migration record is not exact: {:?}", after.migrations[17]),
        )?;
        require(
            after.migrations[18]
                == (
                    19,
                    "standard opaque value storage".to_owned(),
                    expected_migration_checksum(19, MIGRATIONS[18].2),
                ),
            format!("v19 migration record is not exact: {:?}", after.migrations[18]),
        )?;
        require(
            after.migrations[19]
                == (
                    20,
                    "standard enum record field storage".to_owned(),
                    expected_migration_checksum(20, MIGRATIONS[19].2),
                ),
            format!("v20 migration record is not exact: {:?}", after.migrations[19]),
        )?;
        require(
            after.active_pair == before.active_pair
                && after.source_unit_count == before.source_unit_count
                && after.references == before.references
                && after.catalogue_hashes == before.catalogue_hashes
                && after.function_hashes == before.function_hashes,
            "migration 0007 changed the active pair, references, or semantic hashes",
        )?;
        let recovered = kernel.recover().await?;
        let catalogue_matches = recovered.catalogue().revision()
            == expected_revision.catalogue().revision()
            && recovered.catalogue().schemas() == expected_revision.catalogue().schemas()
            && recovered.catalogue().object_types() == expected_revision.catalogue().object_types()
            && recovered.catalogue().functions() == expected_revision.catalogue().functions();
        require(
            recovered.pair() == expected_revision.pair()
                && recovered.source() == expected_revision.source()
                && recovered.catalogue_hash() == expected_revision.catalogue_hash()
                && catalogue_matches
                && recovered.expressions() == expected_revision.expressions()
                && recovered.function_revisions() == expected_revision.function_revisions()
                && same_members(recovered.origins(), expected_revision.origins())
                && recovered.references() == expected_revision.references(),
            "migration 0007 changed recoverable application revision facts",
        )?;
        let session = database.open().await?;
        let inspection_result = inspect_standard_catalogue_schema(session.client()).await;
        let shutdown_result = session.shutdown().await;
        match (inspection_result, shutdown_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(inspection_error), Err(shutdown_error)) => Err(failure(format!(
                "v6 standard schema inspection failed: {inspection_error}; shutdown failed: {shutdown_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_upgrades_registered_v7_without_resolved_value_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v7_catalogue(&database).await?;
        let expected_revision = registered_v7_rows_fixture()?;
        let before = snapshot_upgrade_state(&database).await?;
        let before_surface = snapshot_catalogue_surface(&database).await?;
        let before_target_fks = snapshot_application_target_foreign_keys(&database).await?;
        let expected_target_fks = expected_application_target_foreign_keys();
        require(
            before_target_fks == expected_target_fks,
            format!("v7 application target foreign keys are not exact: {before_target_fks:?}"),
        )?;
        require(
            before.migrations.len() == 7
                && before.migrations.last().map(|migration| migration.0) == Some(7),
            format!("manual v7 setup produced unexpected migrations: {:?}", before.migrations),
        )?;
        require(
            before.active_pair
                == (
                    expected_revision.pair().source().to_bytes().to_vec(),
                    expected_revision.pair().catalogue().to_bytes().to_vec(),
                ),
            format!("manual v7 setup changed the active pair: {:?}", before.active_pair),
        )?;

        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;

        let after = snapshot_upgrade_state(&database).await?;
        let after_surface = snapshot_catalogue_surface(&database).await?;
        let after_target_fks = snapshot_application_target_foreign_keys(&database).await?;
        require(
            after.migrations.len() == MIGRATIONS.len()
                && after.migrations[..7] == before.migrations[..]
                && after.migrations[7]
                    == (
                        8,
                        "resolved value type storage".to_owned(),
                        expected_migration_checksum(8, MIGRATIONS[7].2),
                    )
                && after.migrations[8]
                    == (
                        9,
                        "security decision snapshot".to_owned(),
                        expected_migration_checksum(9, MIGRATIONS[8].2),
                    )
                && after.migrations[9]
                    == (
                        10,
                        "local peer credentials".to_owned(),
                        expected_migration_checksum(10, MIGRATIONS[9].2),
                    )
                && after.migrations[10]
                    == (
                        11,
                        "protected security audit".to_owned(),
                        expected_migration_checksum(11, MIGRATIONS[10].2),
                    )
                && after.migrations[11]
                    == (
                        12,
                        "catalogue enum type storage".to_owned(),
                        expected_migration_checksum(12, MIGRATIONS[11].2),
                    )
                && after.migrations[12]
                    == (
                        13,
                        "resolved enum type storage".to_owned(),
                        expected_migration_checksum(13, MIGRATIONS[12].2),
                    )
                && after.migrations[13]
                    == (
                        14,
                        "catalogue enum reference targets".to_owned(),
                        expected_migration_checksum(14, MIGRATIONS[13].2),
                    )
                && after.migrations[14]
                    == (
                        15,
                        "catalogue record value storage".to_owned(),
                        expected_migration_checksum(15, MIGRATIONS[14].2),
                    )
                && after.migrations[15]
                    == (
                        16,
                        "resolved record value type storage".to_owned(),
                        expected_migration_checksum(16, MIGRATIONS[15].2),
                    )
                && after.migrations[16]
                    == (
                        17,
                        "record value field reference targets".to_owned(),
                        expected_migration_checksum(17, MIGRATIONS[16].2),
                    )
                && after.migrations[17]
                    == (
                        18,
                        "disjoint field reference targets".to_owned(),
                        expected_migration_checksum(18, MIGRATIONS[17].2),
                    )
                && after.migrations[18]
                    == (
                        19,
                        "standard opaque value storage".to_owned(),
                        expected_migration_checksum(19, MIGRATIONS[18].2),
                    )
                && after.migrations[19]
                    == (
                        20,
                        "standard enum record field storage".to_owned(),
                        expected_migration_checksum(20, MIGRATIONS[19].2),
                    )
                && after.migrations[20]
                    == (
                        21,
                        "nested record field targets".to_owned(),
                        expected_migration_checksum(21, MIGRATIONS[20].2),
                    )
                && after.migrations[21]
                    == (
                        22,
                        "protected invocation audit".to_owned(),
                        expected_migration_checksum(22, MIGRATIONS[21].2),
                    )
                && after.migrations[22]
                    == (
                        23,
                        "executable standard relations".to_owned(),
                        expected_migration_checksum(23, MIGRATIONS[22].2),
                    )
                && after.migrations[23]
                    == (
                        24,
                        "capability audit decisions".to_owned(),
                        expected_migration_checksum(24, MIGRATIONS[23].2),
                    )
                && after.migrations[24]
                    == (
                        25,
                        "durable user state cells".to_owned(),
                        expected_migration_checksum(25, MIGRATIONS[24].2),
                    )
                && after.migrations[25]
                    == (
                        26,
                        "user state audit decisions".to_owned(),
                        expected_migration_checksum(26, MIGRATIONS[25].2),
                    )
                && after.migrations[26]
                    == (
                        27,
                        "inspect snapshots and trace".to_owned(),
                        expected_migration_checksum(27, MIGRATIONS[26].2),
                    )
                && after.migrations[27]
                    == (
                        28,
                        "security admin privilege grants".to_owned(),
                        expected_migration_checksum(28, MIGRATIONS[27].2),
                    )
                && after.migrations[28]
                    == (
                        29,
                        "sealed system invocation authorities".to_owned(),
                        expected_migration_checksum(29, MIGRATIONS[28].2),
                    )
                && after.migrations[29]
                    == (
                        30,
                        "active roles system invocation authority".to_owned(),
                        expected_migration_checksum(30, MIGRATIONS[29].2),
                    )
                && after.migrations[30]
                    == (
                        31,
                        "standard JSON executable format".to_owned(),
                        expected_migration_checksum(31, MIGRATIONS[30].2),
                    )
                && after.migrations[31]
                    == (
                        32,
                        "protected resource audit".to_owned(),
                        expected_migration_checksum(32, MIGRATIONS[31].2),
                    )
                && after.migrations[32]
                    == (
                        33,
                        "stream function returns".to_owned(),
                        expected_migration_checksum(33, MIGRATIONS[32].2),
                    ),
            format!("v7-v45 upgrade produced unexpected migrations: {:?}", after.migrations),
        )?;
        require(
            after.active_pair == before.active_pair
                && after.source_unit_count == before.source_unit_count
                && after.references == before.references
                && after.catalogue_hashes == before.catalogue_hashes
                && after.function_hashes == before.function_hashes,
            "migration 0008 changed the active pair, references, or semantic hashes",
        )?;
        require(
            before_surface == without_later_relations(&after_surface),
            format!(
                "migration 0008 changed a relation, index, trigger, or ACL: before={before_surface:?}, after={after_surface:?}"
            ),
        )?;
        require(
            after_target_fks == expected_application_target_foreign_keys_after_sealed_inspector(),
            format!(
                "migration 0036 changed application target foreign keys unexpectedly: before={before_target_fks:?}, after={after_target_fks:?}"
            ),
        )?;

        let recovered = kernel.recover().await?;
        let catalogue_matches = recovered.catalogue().revision()
            == expected_revision.catalogue().revision()
            && recovered.catalogue().schemas() == expected_revision.catalogue().schemas()
            && recovered.catalogue().object_types() == expected_revision.catalogue().object_types()
            && recovered.catalogue().functions() == expected_revision.catalogue().functions();
        require(
            recovered.pair() == expected_revision.pair()
                && recovered.source() == expected_revision.source()
                && recovered.catalogue_hash() == expected_revision.catalogue_hash()
                && catalogue_matches
                && recovered.expressions() == expected_revision.expressions()
                && recovered.function_revisions() == expected_revision.function_revisions()
                && same_members(recovered.origins(), expected_revision.origins())
                && recovered.references() == expected_revision.references(),
            "migration 0008 changed recoverable application revision facts",
        )?;

        let session = database.open().await?;
        let inspection_result = async {
            inspect_resolved_value_storage(session.client(), true).await?;
            inspect_resolved_enum_storage(session.client(), true).await?;
            inspect_standard_catalogue_schema(session.client()).await
        }
        .await;
        let shutdown_result = session.shutdown().await;
        match (inspection_result, shutdown_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(inspection_error), Err(shutdown_error)) => Err(failure(format!(
                "v7 resolved-value inspection failed: {inspection_error}; shutdown failed: {shutdown_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn standard_catalogue_zero_catalogue_id_is_schema_valid_without_activation() -> TestResult<()>
{
    with_test_database(|database| async move {
        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;
        let session = database.open().await?;
        let result = async {
            let source_revision_id = session
                .client()
                .query_one(
                    "SELECT source_revision_id
                     FROM _orna_kernel.active_revision
                     WHERE singleton = true",
                    &[],
                )
                .await?
                .get::<_, Vec<u8>>(0);
            let standard_library_revision_id = vec![0x71_u8; 16];
            let zero_catalogue_revision_id = vec![0_u8; 16];
            session
                .client()
                .execute(
                    "INSERT INTO _orna_kernel.standard_library_revisions
                        (id, source_revision_id, catalogue_revision_id,
                         language_version, content_hash)
                     VALUES ($1, $2, $3, 'standard-v1', $4)",
                    &[
                        &standard_library_revision_id,
                        &source_revision_id,
                        &zero_catalogue_revision_id,
                        &vec![0x72_u8; 32],
                    ],
                )
                .await?;
            let row = session
                .client()
                .query_one(
                    "SELECT catalogue_revision_id, digest_version, hash_algorithm
                     FROM _orna_kernel.standard_library_revisions
                     WHERE id = $1",
                    &[&standard_library_revision_id],
                )
                .await?;
            let catalogue_revision_id: Vec<u8> = row.get(0);
            let digest_version: i16 = row.get(1);
            let hash_algorithm: String = row.get(2);
            require(
                catalogue_revision_id == zero_catalogue_revision_id
                    && digest_version == 1
                    && hash_algorithm == "sha256",
                "the all-zero standard catalogue ID did not remain schema-valid",
            )?;
            let active_pin: Option<Vec<u8>> = session
                .client()
                .query_one(
                    "SELECT standard_library_revision_id
                     FROM _orna_kernel.catalogue_revisions",
                    &[],
                )
                .await?
                .get(0);
            require(
                active_pin.is_none(),
                "the raw sentinel fixture changed the application catalogue pin",
            )?;
            session
                .client()
                .execute(
                    "DELETE FROM _orna_kernel.standard_library_revisions WHERE id = $1",
                    &[&standard_library_revision_id],
                )
                .await?;
            Ok(())
        }
        .await;
        let shutdown_result = session.shutdown().await;
        match (result, shutdown_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(insert_error), Err(shutdown_error)) => Err(failure(format!(
                "all-zero standard catalogue fixture failed: {insert_error}; shutdown failed: {shutdown_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_rolls_back_v5_for_a_dangling_legacy_reference() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v4_semantic_catalogue(&database, true).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        let error = kernel
            .bootstrap()
            .await
            .expect_err("v5 must reject a dangling legacy field reference");
        require_database_constraint(
            &error,
            "23514",
            Some("definition_references_target_owner_shape_check"),
            "dangling legacy field reference",
        )?;
        inspect_v5_rollback(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_rolls_back_v5_for_an_ambiguous_legacy_reference() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v4_semantic_catalogue(&database, false).await?;
        insert_ambiguous_legacy_field_target(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        let error = kernel
            .bootstrap()
            .await
            .expect_err("v5 must reject an ambiguous legacy field reference");
        require_database_constraint(&error, "21000", None, "ambiguous legacy field reference")?;
        inspect_v5_rollback(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_rolls_back_v4_when_legacy_empty_hashes_are_tampered() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v3_empty_catalogue(&database).await?;
        let session = database.open().await?;
        let tamper_result = session
            .client()
            .execute(
                "UPDATE _orna_kernel.source_bundles SET content_hash = $1",
                &[&vec![0_u8; 32]],
            )
            .await
            .map(|_| ())
            .map_err(|error| Box::new(error) as _);
        let shutdown_result = session.shutdown().await;
        match (tamper_result, shutdown_result) {
            (Ok(()), Ok(())) => {}
            (Err(error), Ok(())) | (Ok(()), Err(error)) => return Err(error),
            (Err(tamper_error), Err(shutdown_error)) => {
                return Err(failure(format!(
                    "legacy hash tamper failed: {tamper_error}; tamper driver shutdown failed: {shutdown_error}"
                )));
            }
        }

        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        let error = kernel
            .bootstrap()
            .await
            .expect_err("a tampered legacy hash must fail closed");
        require(
            matches!(error, PostgresKernelError::CatalogueInvariant(_)),
            format!("tampered legacy hash produced the wrong failure: {error}"),
        )?;
        inspect_v4_rollback(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_rejects_registered_v3_semantic_rows_and_rolls_back_v4() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_registered_v3_empty_catalogue(&database).await?;
        insert_unsupported_initial_schema(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        let error = kernel
            .bootstrap()
            .await
            .expect_err("v4 must reject a registered legacy catalogue with semantic rows");
        require(
            matches!(error, PostgresKernelError::CatalogueInvariant(_)),
            format!("registered v3 semantic row produced the wrong failure: {error}"),
        )?;
        inspect_v4_rollback(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn function_revisions_allow_distinct_semantics_for_one_declaration() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = PostgresKernel::from_str(&database.connection_string())?;
        kernel.bootstrap().await?;

        let session = database.open().await?;
        let verification_result = verify_function_revision_semantic_hash_uniqueness(session.client()).await;
        let shutdown_result = session.shutdown().await;
        match (verification_result, shutdown_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(verification_error), Err(shutdown_error)) => Err(failure(format!(
                "function revision uniqueness verification failed: {verification_error}; verification driver shutdown failed: {shutdown_error}"
            ))),
        }
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_rejects_a_seeded_initial_catalogue_with_semantic_rows() -> TestResult<()> {
    with_test_database(|database| async move {
        seed_initial_catalogue(&database).await?;
        insert_unsupported_initial_schema(&database).await?;
        let kernel = PostgresKernel::from_str(&database.connection_string())?;

        let error = kernel
            .bootstrap()
            .await
            .expect_err("migration 0002 must reject an unhashable initial catalogue");
        require_database_constraint(
            &error,
            "23514",
            Some("migration_0002_legacy_state_valid_check"),
            "non-empty migration 0001 catalogue",
        )?;
        inspect_v2_rollback(&database).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_rejects_tampered_gapped_and_newer_migration_history() -> TestResult<()> {
    reject_migration_history(
        1,
        "renamed migration",
        Sha256::digest(MIGRATIONS[0].2.as_bytes()).to_vec(),
    )
    .await?;
    reject_migration_history(1, MIGRATIONS[0].1, vec![0; 32]).await?;
    reject_migration_history(
        2,
        MIGRATIONS[1].1,
        Sha256::digest(MIGRATIONS[1].2.as_bytes()).to_vec(),
    )
    .await?;
    reject_migration_history(26, "future migration", vec![0; 32]).await
}
