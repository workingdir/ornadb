use super::*;

#[path = "scenarios.rs"]
mod scenarios;

async fn inspect_bootstrap_state(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    let inspection_result = inspect_client(session.client()).await;
    let shutdown_result = session.shutdown().await;

    match (inspection_result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(inspection_error), Err(shutdown_error)) => Err(failure(format!(
            "bootstrap inspection failed: {inspection_error}; inspection driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn inspect_client(client: &Client) -> TestResult<()> {
    inspect_migrations(client).await?;
    require_count(
        client,
        "_orna_kernel.source_bundles",
        "SELECT count(*) FROM _orna_kernel.source_bundles",
        1,
    )
    .await?;
    require_count(
        client,
        "_orna_kernel.source_revisions",
        "SELECT count(*) FROM _orna_kernel.source_revisions",
        1,
    )
    .await?;
    require_count(
        client,
        "_orna_kernel.catalogue_revisions",
        "SELECT count(*) FROM _orna_kernel.catalogue_revisions",
        1,
    )
    .await?;
    require_count(
        client,
        "_orna_kernel.active_revision",
        "SELECT count(*) FROM _orna_kernel.active_revision",
        1,
    )
    .await?;
    require_count(
        client,
        "_orna_kernel.source_units",
        "SELECT count(*) FROM _orna_kernel.source_units",
        0,
    )
    .await?;

    inspect_empty_aggregate_hashes(client).await?;
    inspect_hash_contract_columns(client).await?;
    inspect_origin_columns(client).await?;
    inspect_owner_qualified_catalogue_members(client).await?;
    inspect_definition_references(client).await?;
    inspect_function_revision_constraints(client).await?;
    inspect_standard_catalogue_schema(client).await?;
    inspect_resolved_value_storage(client, true).await?;
    inspect_resolved_enum_storage(client, true).await?;
    inspect_record_value_storage(client).await?;
    inspect_security_snapshot_schema(client).await?;
    inspect_resource_audit_schema(client).await?;

    for schema in ["_orna_kernel", "_orna_data"] {
        let role = "public";
        let privilege = "USAGE";
        let row = client
            .query_one(
                "SELECT has_schema_privilege($1, $2, $3)",
                &[&role, &schema, &privilege],
            )
            .await?;
        let has_public_usage: bool = value(&row, 0)?;
        require(
            !has_public_usage,
            format!("PUBLIC has USAGE on protected schema {schema}"),
        )?;
    }

    let table_schema = "_orna_kernel";
    let table_type = "BASE TABLE";
    let rows = client
        .query(
            "SELECT table_name
             FROM information_schema.tables
             WHERE table_schema = $1 AND table_type = $2
             ORDER BY table_name",
            &[&table_schema, &table_type],
        )
        .await?;
    let actual_tables = rows
        .iter()
        .map(|row| value::<String>(row, 0))
        .collect::<TestResult<BTreeSet<_>>>()?;
    let expected_tables = EXPECTED_KERNEL_TABLES
        .iter()
        .map(|table| (*table).to_owned())
        .collect::<BTreeSet<_>>();
    require(
        actual_tables == expected_tables,
        format!(
            "protected table set differs; expected {expected_tables:?}, found {actual_tables:?}"
        ),
    )
}

async fn inspect_security_snapshot_schema(client: &Client) -> TestResult<()> {
    inspect_columns(
        client,
        "security_principals",
        &[
            ("id", "bytea", "bytea", "NO", Some("")),
            ("kind", "text", "text", "NO", Some("")),
            ("status", "text", "text", "NO", Some("")),
        ],
    )
    .await?;
    inspect_columns(
        client,
        "security_role_memberships",
        &[
            ("role_id", "bytea", "bytea", "NO", Some("")),
            ("member_id", "bytea", "bytea", "NO", Some("")),
        ],
    )
    .await?;
    inspect_columns(
        client,
        "security_execute_grants",
        &[
            ("grantee_id", "bytea", "bytea", "NO", Some("")),
            ("function_id", "bytea", "bytea", "NO", Some("")),
        ],
    )
    .await?;
    inspect_columns(
        client,
        "security_local_peer_credentials",
        &[
            ("uid", "bigint", "int8", "NO", Some("")),
            ("principal_id", "bytea", "bytea", "NO", Some("")),
        ],
    )
    .await?;
    inspect_columns(
        client,
        "security_audit_events",
        &[
            ("sequence", "bigint", "int8", "NO", None),
            ("event_id", "bytea", "bytea", "NO", Some("")),
            (
                "recorded_at",
                "timestamp without time zone",
                "timestamp",
                "NO",
                None,
            ),
            ("event_kind", "text", "text", "NO", Some("")),
            ("outcome", "text", "text", "NO", Some("")),
            ("session_principal_id", "bytea", "bytea", "YES", Some("")),
            ("effective_principal_id", "bytea", "bytea", "YES", Some("")),
            (
                "authorising_principal_id",
                "bytea",
                "bytea",
                "YES",
                Some(""),
            ),
            ("function_id", "bytea", "bytea", "YES", Some("")),
            ("source_revision_id", "bytea", "bytea", "YES", Some("")),
            ("catalogue_revision_id", "bytea", "bytea", "YES", Some("")),
            ("denial_reason", "text", "text", "YES", Some("")),
        ],
    )
    .await?;

    for (table, constraint, expected) in [
        (
            "security_principals",
            "security_principals_id_length",
            "octet_length(id) = 16",
        ),
        (
            "security_principals",
            "security_principals_kind_check",
            "kind = ANY",
        ),
        (
            "security_principals",
            "security_principals_status_check",
            "status = ANY",
        ),
        (
            "security_role_memberships",
            "security_role_memberships_not_self",
            "role_id <> member_id",
        ),
        (
            "security_role_memberships",
            "security_role_memberships_role_fk",
            "FOREIGN KEY (role_id)",
        ),
        (
            "security_role_memberships",
            "security_role_memberships_member_fk",
            "FOREIGN KEY (member_id)",
        ),
        (
            "security_execute_grants",
            "security_execute_grants_function_id_length",
            "octet_length(function_id) = 16",
        ),
        (
            "security_execute_grants",
            "security_execute_grants_grantee_fk",
            "FOREIGN KEY (grantee_id)",
        ),
        (
            "security_local_peer_credentials",
            "security_local_peer_credentials_principal_key",
            "UNIQUE (principal_id)",
        ),
        (
            "security_local_peer_credentials",
            "security_local_peer_credentials_principal_fk",
            "FOREIGN KEY (principal_id)",
        ),
        (
            "security_audit_events",
            "security_audit_events_event_id_key",
            "UNIQUE (event_id)",
        ),
        (
            "security_audit_events",
            "security_audit_events_identity_lengths",
            "octet_length(event_id) = 16",
        ),
        (
            "security_audit_events",
            "security_audit_events_kind_check",
            "event_kind = ANY",
        ),
        (
            "security_audit_events",
            "security_audit_events_outcome_check",
            "outcome = ANY",
        ),
        (
            "security_audit_events",
            "security_audit_events_revision_pair_check",
            "(source_revision_id IS NULL) = (catalogue_revision_id IS NULL)",
        ),
        (
            "security_audit_events",
            "security_audit_events_denial_reason_check",
            "source_apply:committed",
        ),
        (
            "security_audit_events",
            "security_audit_events_shape_check",
            "event_kind = 'source_apply'::text",
        ),
    ] {
        require_constraint(client, table, constraint, expected).await?;
    }
    let uid_range = constraint_definition(
        client,
        "security_local_peer_credentials",
        "security_local_peer_credentials_uid_range",
    )
    .await?;
    require(
        uid_range.contains("uid >= 0") && uid_range.contains("uid <= '4294967295'::bigint"),
        format!("local peer UID range is not exact: {uid_range:?}"),
    )?;
    require_index(
        client,
        "security_role_memberships_member_index",
        "(member_id, role_id)",
    )
    .await?;
    require_index(
        client,
        "security_execute_grants_function_index",
        "(function_id, grantee_id)",
    )
    .await?;

    let identity = client
        .query_one(
            "SELECT is_identity, identity_generation
             FROM information_schema.columns
             WHERE table_schema = '_orna_kernel'
               AND table_name = 'security_audit_events'
               AND column_name = 'sequence'",
            &[],
        )
        .await?;
    require(
        value::<String>(&identity, 0)? == "YES" && value::<String>(&identity, 1)? == "ALWAYS",
        "security audit sequence is not an always-generated identity",
    )?;
    require_count(
        client,
        "_orna_kernel.security_audit_events",
        "SELECT count(*) FROM _orna_kernel.security_audit_events",
        0,
    )
    .await?;

    for table in [
        "security_audit_events",
        "security_principals",
        "security_role_memberships",
        "security_execute_grants",
        "security_local_peer_credentials",
    ] {
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
            let relation = format!("_orna_kernel.{table}");
            let row = client
                .query_one(
                    "SELECT has_table_privilege('public', $1, $2)",
                    &[&relation, &privilege],
                )
                .await?;
            let granted: bool = value(&row, 0)?;
            require(
                !granted,
                format!("PUBLIC has {privilege} on protected table {relation}"),
            )?;
        }
    }

    for privilege in ["USAGE", "SELECT", "UPDATE"] {
        let row = client
            .query_one(
                "SELECT has_sequence_privilege(
                    'public',
                    '_orna_kernel.security_audit_events_sequence_seq',
                    $1
                 )",
                &[&privilege],
            )
            .await?;
        require(
            !value::<bool>(&row, 0)?,
            format!("PUBLIC has {privilege} on the protected audit sequence"),
        )?;
    }

    Ok(())
}

async fn inspect_resource_audit_schema(client: &Client) -> TestResult<()> {
    inspect_columns(
        client,
        "resource_audit_events",
        &[
            ("sequence", "bigint", "int8", "NO", None),
            ("event_id", "bytea", "bytea", "NO", Some("")),
            (
                "recorded_at",
                "timestamp without time zone",
                "timestamp",
                "NO",
                None,
            ),
            ("request_id", "bytea", "bytea", "NO", Some("")),
            ("nested_invocation_id", "bytea", "bytea", "YES", Some("")),
            ("parent_invocation_id", "bytea", "bytea", "NO", Some("")),
            ("call_site_id", "bytea", "bytea", "NO", Some("")),
            ("target_function_id", "bytea", "bytea", "YES", Some("")),
            ("source_revision_id", "bytea", "bytea", "YES", Some("")),
            ("catalogue_revision_id", "bytea", "bytea", "YES", Some("")),
            ("session_principal_id", "bytea", "bytea", "NO", Some("")),
            ("decision_outcome", "text", "text", "NO", Some("")),
            ("terminal_outcome", "text", "text", "NO", Some("")),
            ("item_count", "bigint", "int8", "YES", Some("")),
            ("byte_count", "bigint", "int8", "YES", Some("")),
        ],
    )
    .await?;

    for (constraint, expected) in [
        ("resource_audit_events_pkey", "PRIMARY KEY (sequence)"),
        ("resource_audit_events_event_id_key", "UNIQUE (event_id)"),
        (
            "resource_audit_events_request_id_key",
            "UNIQUE (request_id)",
        ),
        (
            "resource_audit_events_nested_invocation_id_key",
            "UNIQUE (nested_invocation_id)",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(event_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(request_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "nested_invocation_id IS NULL",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(nested_invocation_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(parent_invocation_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(call_site_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(session_principal_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(target_function_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(source_revision_id) = 16",
        ),
        (
            "resource_audit_events_identity_lengths",
            "octet_length(catalogue_revision_id) = 16",
        ),
        (
            "resource_audit_events_nested_invocation_presence_check",
            "nested_invocation_id IS NOT NULL",
        ),
        (
            "resource_audit_events_nested_invocation_presence_check",
            "decision_outcome = 'denied'",
        ),
        (
            "resource_audit_events_nested_invocation_presence_check",
            "terminal_outcome = ANY",
        ),
        (
            "resource_audit_events_target_pair_check",
            "(target_function_id IS NULL) = (source_revision_id IS NULL)",
        ),
        (
            "resource_audit_events_target_pair_check",
            "(target_function_id IS NULL) = (catalogue_revision_id IS NULL)",
        ),
        (
            "resource_audit_events_decision_outcome_check",
            "decision_outcome = ANY",
        ),
        (
            "resource_audit_events_terminal_outcome_check",
            "terminal_outcome = ANY",
        ),
        ("resource_audit_events_counts_check", "item_count >= 0"),
        ("resource_audit_events_counts_check", "byte_count >= 0"),
        (
            "resource_audit_events_target_fk",
            "FOREIGN KEY (catalogue_revision_id, target_function_id) REFERENCES _orna_kernel.invocation_target_authorities(catalogue_revision_id, function_id)",
        ),
        (
            "resource_audit_events_revision_pair_fk",
            "FOREIGN KEY (catalogue_revision_id, source_revision_id) REFERENCES _orna_kernel.catalogue_revisions(id, source_revision_id)",
        ),
        (
            "resource_audit_events_nested_invocation_fk",
            "FOREIGN KEY (nested_invocation_id) REFERENCES _orna_kernel.invocation_audit_events(invocation_id)",
        ),
    ] {
        require_constraint(client, "resource_audit_events", constraint, expected).await?;
    }

    let identity = client
        .query_one(
            "SELECT is_identity, identity_generation
             FROM information_schema.columns
             WHERE table_schema = '_orna_kernel'
               AND table_name = 'resource_audit_events'
               AND column_name = 'sequence'",
            &[],
        )
        .await?;
    require(
        value::<String>(&identity, 0)? == "YES" && value::<String>(&identity, 1)? == "ALWAYS",
        "resource audit sequence is not an always-generated identity",
    )?;
    require_count(
        client,
        "_orna_kernel.resource_audit_events",
        "SELECT count(*) FROM _orna_kernel.resource_audit_events",
        0,
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
        let relation = "_orna_kernel.resource_audit_events";
        let row = client
            .query_one(
                "SELECT has_table_privilege('public', $1, $2)",
                &[&relation, &privilege],
            )
            .await?;
        let granted: bool = value(&row, 0)?;
        require(
            !granted,
            format!("PUBLIC has {privilege} on protected table {relation}"),
        )?;
    }

    for privilege in ["USAGE", "SELECT", "UPDATE"] {
        let row = client
            .query_one(
                "SELECT has_sequence_privilege(
                    'public',
                    '_orna_kernel.resource_audit_events_sequence_seq',
                    $1
                 )",
                &[&privilege],
            )
            .await?;
        require(
            !value::<bool>(&row, 0)?,
            format!("PUBLIC has {privilege} on the protected resource audit sequence"),
        )?;
    }

    Ok(())
}

async fn inspect_standard_catalogue_schema(client: &Client) -> TestResult<()> {
    for (table, expected_columns) in [
        (
            "standard_library_revisions",
            &[
                ("id", "bytea", "bytea", "NO", Some("")),
                ("source_revision_id", "bytea", "bytea", "NO", Some("")),
                ("catalogue_revision_id", "bytea", "bytea", "NO", Some("")),
                ("digest_version", "smallint", "int2", "NO", Some("1")),
                ("language_version", "text", "text", "NO", Some("")),
                ("content_hash", "bytea", "bytea", "NO", Some("")),
                (
                    "hash_algorithm",
                    "text",
                    "text",
                    "NO",
                    Some("'sha256'::text"),
                ),
                (
                    "created_at",
                    "timestamp with time zone",
                    "timestamptz",
                    "NO",
                    Some("transaction_timestamp()"),
                ),
            ][..],
        ),
        (
            "standard_catalogue_schemas",
            &[
                (
                    "standard_library_revision_id",
                    "bytea",
                    "bytea",
                    "NO",
                    Some(""),
                ),
                ("schema_id", "bytea", "bytea", "NO", Some("")),
                ("name_parts", "ARRAY", "_text", "NO", Some("")),
                ("source_unit_id", "bytea", "bytea", "NO", Some("")),
                ("source_start", "bigint", "int8", "NO", Some("")),
                ("source_end", "bigint", "int8", "NO", Some("")),
            ][..],
        ),
        (
            "standard_catalogue_value_types",
            &[
                (
                    "standard_library_revision_id",
                    "bytea",
                    "bytea",
                    "NO",
                    Some(""),
                ),
                ("type_id", "bytea", "bytea", "NO", Some("")),
                ("schema_id", "bytea", "bytea", "NO", Some("")),
                ("name_parts", "ARRAY", "_text", "NO", Some("")),
                ("value_kind", "text", "text", "NO", Some("")),
                ("mutability", "text", "text", "NO", Some("")),
                ("persistence", "text", "text", "NO", Some("")),
                ("representation_contract", "text", "text", "NO", Some("")),
                ("source_unit_id", "bytea", "bytea", "NO", Some("")),
                ("source_start", "bigint", "int8", "NO", Some("")),
                ("source_end", "bigint", "int8", "NO", Some("")),
            ][..],
        ),
        (
            "standard_catalogue_enum_types",
            &[
                (
                    "standard_library_revision_id",
                    "bytea",
                    "bytea",
                    "NO",
                    Some(""),
                ),
                ("type_id", "bytea", "bytea", "NO", Some("")),
                ("schema_id", "bytea", "bytea", "NO", Some("")),
                ("name_parts", "ARRAY", "_text", "NO", Some("")),
                ("labels", "ARRAY", "_text", "NO", Some("")),
                ("source_unit_id", "bytea", "bytea", "NO", Some("")),
                ("source_start", "bigint", "int8", "NO", Some("")),
                ("source_end", "bigint", "int8", "NO", Some("")),
            ][..],
        ),
        (
            "standard_catalogue_type_bindings",
            &[
                (
                    "standard_library_revision_id",
                    "bytea",
                    "bytea",
                    "NO",
                    Some(""),
                ),
                ("type_binding_id", "bytea", "bytea", "NO", Some("")),
                ("kind", "text", "text", "NO", Some("")),
                ("name_parts", "ARRAY", "_text", "NO", Some("")),
                ("target_type_id", "bytea", "bytea", "YES", Some("")),
                ("source_unit_id", "bytea", "bytea", "NO", Some("")),
                ("source_start", "bigint", "int8", "NO", Some("")),
                ("source_end", "bigint", "int8", "NO", Some("")),
                (
                    "target_type_kind",
                    "text",
                    "text",
                    "NO",
                    Some("'value'::text"),
                ),
                ("target_enum_type_id", "bytea", "bytea", "YES", Some("")),
            ][..],
        ),
    ] {
        inspect_columns(client, table, expected_columns).await?;
        require_count(
            client,
            table,
            &format!("SELECT count(*) FROM _orna_kernel.{table}"),
            0,
        )
        .await?;
    }

    inspect_column_contract(
        client,
        "catalogue_revisions",
        &[
            (
                "canonical_hash_version",
                "smallint",
                "int2",
                "NO",
                Some("1"),
            ),
            (
                "standard_library_revision_id",
                "bytea",
                "bytea",
                "YES",
                Some(""),
            ),
        ],
    )
    .await?;
    inspect_column_contract(
        client,
        "function_revisions",
        &[("semantic_hash_version", "smallint", "int2", "NO", Some("1"))],
    )
    .await?;
    inspect_column_contract(
        client,
        "definition_references",
        &[(
            "target_standard_library_revision_id",
            "bytea",
            "bytea",
            "YES",
            Some(""),
        )],
    )
    .await?;

    let catalogue_version = client
        .query_one(
            "SELECT canonical_hash_version, standard_library_revision_id
             FROM _orna_kernel.catalogue_revisions",
            &[],
        )
        .await?;
    let canonical_hash_version: i16 = value(&catalogue_version, 0)?;
    let standard_library_revision_id: Option<Vec<u8>> = value(&catalogue_version, 1)?;
    require(
        canonical_hash_version == 1 && standard_library_revision_id.is_none(),
        format!(
            "application catalogue standard context is ({canonical_hash_version}, {standard_library_revision_id:?}); expected (1, NULL)"
        ),
    )?;
    let semantic_versions = client
        .query(
            "SELECT semantic_hash_version
             FROM _orna_kernel.function_revisions
             ORDER BY id",
            &[],
        )
        .await?;
    for row in semantic_versions {
        let semantic_hash_version: i16 = value(&row, 0)?;
        require(
            semantic_hash_version == 1,
            format!("function semantic hash version is {semantic_hash_version}; expected 1"),
        )?;
    }

    inspect_standard_catalogue_constraints(client).await?;
    inspect_standard_catalogue_indexes(client).await?;
    inspect_standard_catalogue_privileges(client).await
}

async fn inspect_resolved_value_storage(
    client: &Client,
    require_null_values: bool,
) -> TestResult<()> {
    for (table, columns) in [
        (
            "catalogue_fields",
            ["value_type_id", "value_standard_library_revision_id"],
        ),
        (
            "catalogue_function_parameters",
            ["value_type_id", "value_standard_library_revision_id"],
        ),
        (
            "catalogue_function_return_columns",
            ["value_type_id", "value_standard_library_revision_id"],
        ),
        (
            "catalogue_functions",
            [
                "return_value_type_id",
                "return_standard_library_revision_id",
            ],
        ),
    ] {
        for column in columns {
            inspect_column_contract(
                client,
                table,
                &[(column, "bytea", "bytea", "YES", Some(""))],
            )
            .await?;
        }
        if require_null_values {
            let row = client
                .query_one(
                    &format!(
                        "SELECT count(*) FROM _orna_kernel.{table}
                         WHERE {} IS NOT NULL OR {} IS NOT NULL",
                        columns[0], columns[1]
                    ),
                    &[],
                )
                .await?;
            let non_null_rows: i64 = value(&row, 0)?;
            require(
                non_null_rows == 0,
                format!("{table} contains {non_null_rows} resolved value rows"),
            )?;
        }
    }

    for (table, constraint, expected_deferrable, expected_deferred) in [
        ("catalogue_fields", "cat_fields_val_pin_fk", true, true),
        (
            "catalogue_fields",
            "cat_fields_val_std_rev_len",
            false,
            false,
        ),
        ("catalogue_fields", "cat_fields_val_type_fk", true, true),
        ("catalogue_fields", "cat_fields_val_type_len", false, false),
        ("catalogue_fields", "catalogue_fields_check", false, false),
        (
            "catalogue_fields",
            "catalogue_fields_type_kind_check",
            false,
            false,
        ),
        (
            "catalogue_function_parameters",
            "cat_fn_params_val_pin_fk",
            true,
            true,
        ),
        (
            "catalogue_function_parameters",
            "cat_fn_params_val_std_rev_len",
            false,
            false,
        ),
        (
            "catalogue_function_parameters",
            "cat_fn_params_val_type_fk",
            true,
            true,
        ),
        (
            "catalogue_function_parameters",
            "cat_fn_params_val_type_len",
            false,
            false,
        ),
        (
            "catalogue_function_parameters",
            "catalogue_function_parameters_check",
            false,
            false,
        ),
        (
            "catalogue_function_parameters",
            "catalogue_function_parameters_value_pin_check",
            false,
            false,
        ),
        (
            "catalogue_function_parameters",
            "catalogue_function_parameters_type_kind_check",
            false,
            false,
        ),
        (
            "catalogue_function_return_columns",
            "cat_fn_ret_cols_val_pin_fk",
            true,
            true,
        ),
        (
            "catalogue_function_return_columns",
            "cat_fn_ret_cols_val_std_rev_len",
            false,
            false,
        ),
        (
            "catalogue_function_return_columns",
            "cat_fn_ret_cols_val_type_fk",
            true,
            true,
        ),
        (
            "catalogue_function_return_columns",
            "cat_fn_ret_cols_val_type_len",
            false,
            false,
        ),
        (
            "catalogue_function_return_columns",
            "catalogue_function_return_columns_check",
            false,
            false,
        ),
        (
            "catalogue_function_return_columns",
            "catalogue_function_return_columns_type_kind_check",
            false,
            false,
        ),
        (
            "catalogue_functions",
            "cat_funcs_ret_val_pin_fk",
            true,
            true,
        ),
        (
            "catalogue_functions",
            "cat_funcs_ret_val_std_rev_len",
            false,
            false,
        ),
        (
            "catalogue_functions",
            "cat_funcs_ret_val_type_fk",
            true,
            true,
        ),
        (
            "catalogue_functions",
            "cat_funcs_ret_val_type_len",
            false,
            false,
        ),
        (
            "catalogue_functions",
            "catalogue_functions_check1",
            false,
            false,
        ),
        (
            "catalogue_functions",
            "catalogue_functions_return_type_kind_check",
            false,
            false,
        ),
        (
            "catalogue_functions",
            "catalogue_functions_return_shape_check",
            false,
            false,
        ),
        (
            "catalogue_functions",
            "catalogue_functions_return_value_pin_check",
            false,
            false,
        ),
    ] {
        if let Some((value_type_column, standard_revision_column, require_shape)) = match constraint
        {
            "catalogue_function_parameters_check"
            | "catalogue_function_parameters_value_pin_check" => Some((
                "value_type_id",
                "value_standard_library_revision_id",
                constraint == "catalogue_function_parameters_check",
            )),
            "catalogue_functions_check1" | "catalogue_functions_return_value_pin_check" => Some((
                "return_value_type_id",
                "return_standard_library_revision_id",
                constraint == "catalogue_functions_check1",
            )),
            _ => None,
        } {
            inspect_sealed_value_type_constraint(
                client,
                table,
                constraint,
                value_type_column,
                standard_revision_column,
                require_shape,
            )
            .await?;
            continue;
        }

        let definition =
            exact_resolved_type_constraint_definition(constraint).ok_or_else(|| {
                failure(format!(
                    "missing exact resolved-type contract for {constraint}"
                ))
            })?;
        require_exact_constraint(
            client,
            table,
            constraint,
            definition,
            expected_deferrable,
            expected_deferred,
        )
        .await?;
    }
    inspect_resolved_value_public_privileges(client).await
}

async fn inspect_sealed_value_type_constraint(
    client: &Client,
    table: &str,
    constraint: &str,
    value_type_column: &str,
    standard_revision_column: &str,
    require_shape: bool,
) -> TestResult<()> {
    let definition = constraint_definition(client, table, constraint).await?;
    let value_type_not_null = format!("{value_type_column} IS NOT NULL");
    let value_type_null = format!("{value_type_column} IS NULL");
    let value_type_exclusion = format!("{value_type_column} <> ALL");
    let value_type_inclusion = format!("{value_type_column} = ANY");
    let standard_revision_not_null = format!("{standard_revision_column} IS NOT NULL");
    let standard_revision_null = format!("{standard_revision_column} IS NULL");
    let shape_is_valid = !require_shape
        || (value_type_column == "value_type_id"
            && definition.contains("type_kind = 'scalar'::text")
            && definition.contains("type_kind = 'value'::text")
            && definition.contains("type_kind = 'enum'::text")
            && definition.contains("type_kind = 'record'::text"))
        || (value_type_column == "return_value_type_id"
            && definition.contains("return_shape = 'rows'::text")
            && definition.contains("return_shape = 'single'::text")
            && definition.contains("return_shape = 'stream'::text"));
    require(
        definition.contains(&value_type_not_null)
            && (!require_shape || definition.contains(&value_type_null))
            && definition.contains(&value_type_exclusion)
            && definition.contains(&value_type_inclusion)
            && definition.contains(&standard_revision_not_null)
            && definition.contains(&standard_revision_null)
            && definition.contains("decode('000000000000000000000000000000f3'::text, 'hex'::text)")
            && definition.contains("decode('000000000000000000000000000000ff'::text, 'hex'::text)")
            && shape_is_valid,
        format!(
            "{table} constraint {constraint} has an incomplete sealed value-type contract: {definition:?}"
        ),
    )
}

fn exact_resolved_type_constraint_definition(constraint: &str) -> Option<&'static str> {
    Some(match constraint {
        "catalogue_fields_type_kind_check"
        | "catalogue_function_parameters_type_kind_check"
        | "catalogue_function_return_columns_type_kind_check" => {
            "CHECK ((type_kind = ANY (ARRAY['scalar'::text, 'named'::text, 'reference'::text, 'value'::text, 'enum'::text, 'record'::text])))"
        }
        "catalogue_fields_check" | "catalogue_function_return_columns_check" => {
            "CHECK ((((type_kind = 'scalar'::text) AND (scalar_type IS NOT NULL) AND (target_type_id IS NULL) AND (value_type_id IS NULL) AND (value_standard_library_revision_id IS NULL) AND (enum_type_id IS NULL) AND (record_type_id IS NULL)) OR ((type_kind = ANY (ARRAY['named'::text, 'reference'::text])) AND (scalar_type IS NULL) AND (target_type_id IS NOT NULL) AND (value_type_id IS NULL) AND (value_standard_library_revision_id IS NULL) AND (enum_type_id IS NULL) AND (record_type_id IS NULL)) OR ((type_kind = 'value'::text) AND (scalar_type IS NULL) AND (target_type_id IS NULL) AND (value_type_id IS NOT NULL) AND (value_standard_library_revision_id IS NOT NULL) AND (enum_type_id IS NULL) AND (record_type_id IS NULL)) OR ((type_kind = 'enum'::text) AND (scalar_type IS NULL) AND (target_type_id IS NULL) AND (value_type_id IS NULL) AND (value_standard_library_revision_id IS NULL) AND (enum_type_id IS NOT NULL) AND (record_type_id IS NULL)) OR ((type_kind = 'record'::text) AND (scalar_type IS NULL) AND (target_type_id IS NULL) AND (value_type_id IS NULL) AND (value_standard_library_revision_id IS NULL) AND (enum_type_id IS NULL) AND (record_type_id IS NOT NULL))))"
        }
        "catalogue_functions_return_type_kind_check" => {
            "CHECK ((return_type_kind = ANY (ARRAY['scalar'::text, 'named'::text, 'reference'::text, 'value'::text, 'enum'::text, 'record'::text])))"
        }
        "catalogue_functions_return_shape_check" => {
            "CHECK ((return_shape = ANY (ARRAY['single'::text, 'rows'::text, 'stream'::text])))"
        }
        "cat_fields_val_type_len"
        | "cat_fn_params_val_type_len"
        | "cat_fn_ret_cols_val_type_len" => {
            "CHECK (((value_type_id IS NULL) OR (octet_length(value_type_id) = 16)))"
        }
        "cat_fields_val_std_rev_len"
        | "cat_fn_params_val_std_rev_len"
        | "cat_fn_ret_cols_val_std_rev_len" => {
            "CHECK (((value_standard_library_revision_id IS NULL) OR (octet_length(value_standard_library_revision_id) = 16)))"
        }
        "cat_funcs_ret_val_type_len" => {
            "CHECK (((return_value_type_id IS NULL) OR (octet_length(return_value_type_id) = 16)))"
        }
        "cat_funcs_ret_val_std_rev_len" => {
            "CHECK (((return_standard_library_revision_id IS NULL) OR (octet_length(return_standard_library_revision_id) = 16)))"
        }
        "cat_fields_val_pin_fk" | "cat_fn_params_val_pin_fk" | "cat_fn_ret_cols_val_pin_fk" => {
            "FOREIGN KEY (catalogue_revision_id, value_standard_library_revision_id) REFERENCES _orna_kernel.catalogue_revisions(id, standard_library_revision_id) DEFERRABLE INITIALLY DEFERRED"
        }
        "cat_fields_val_type_fk" | "cat_fn_params_val_type_fk" | "cat_fn_ret_cols_val_type_fk" => {
            "FOREIGN KEY (value_standard_library_revision_id, value_type_id) REFERENCES _orna_kernel.standard_catalogue_value_types(standard_library_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED"
        }
        "cat_funcs_ret_val_type_fk" => {
            "FOREIGN KEY (return_standard_library_revision_id, return_value_type_id) REFERENCES _orna_kernel.standard_catalogue_value_types(standard_library_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED"
        }
        "cat_funcs_ret_val_pin_fk" => {
            "FOREIGN KEY (catalogue_revision_id, return_standard_library_revision_id) REFERENCES _orna_kernel.catalogue_revisions(id, standard_library_revision_id) DEFERRABLE INITIALLY DEFERRED"
        }
        _ => return None,
    })
}

async fn inspect_resolved_enum_storage(
    client: &Client,
    require_null_values: bool,
) -> TestResult<()> {
    for (table, column, length_constraint, foreign_key) in [
        (
            "catalogue_fields",
            "enum_type_id",
            "cat_fields_enum_type_len",
            "cat_fields_enum_type_fk",
        ),
        (
            "catalogue_function_parameters",
            "enum_type_id",
            "cat_fn_params_enum_type_len",
            "cat_fn_params_enum_type_fk",
        ),
        (
            "catalogue_function_return_columns",
            "enum_type_id",
            "cat_fn_ret_cols_enum_type_len",
            "cat_fn_ret_cols_enum_type_fk",
        ),
        (
            "catalogue_functions",
            "return_enum_type_id",
            "cat_funcs_ret_enum_type_len",
            "cat_funcs_ret_enum_type_fk",
        ),
    ] {
        inspect_column_contract(
            client,
            table,
            &[(column, "bytea", "bytea", "YES", Some(""))],
        )
        .await?;
        if require_null_values {
            let row = client
                .query_one(
                    &format!(
                        "SELECT count(*) FROM _orna_kernel.{table} WHERE {column} IS NOT NULL"
                    ),
                    &[],
                )
                .await?;
            require(
                value::<i64>(&row, 0)? == 0,
                format!("{table} contains a resolved enum tuple"),
            )?;
        }
        require_exact_constraint(
            client,
            table,
            length_constraint,
            &format!("CHECK ((({column} IS NULL) OR (octet_length({column}) = 16)))"),
            false,
            false,
        )
        .await?;
        require_exact_constraint(
            client,
            table,
            foreign_key,
            &format!(
                "FOREIGN KEY (catalogue_revision_id, {column}) REFERENCES _orna_kernel.catalogue_enum_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED"
            ),
            true,
            true,
        )
        .await?;
    }
    Ok(())
}

async fn inspect_record_value_storage(client: &Client) -> TestResult<()> {
    inspect_columns(
        client,
        "catalogue_record_value_types",
        &[
            ("catalogue_revision_id", "bytea", "bytea", "NO", Some("")),
            ("type_id", "bytea", "bytea", "NO", Some("")),
            ("schema_id", "bytea", "bytea", "NO", Some("")),
            ("name_parts", "ARRAY", "_text", "NO", Some("")),
            ("value_kind", "text", "text", "NO", Some("")),
            ("mutability", "text", "text", "NO", Some("")),
            ("persistence", "text", "text", "NO", Some("")),
            ("source_unit_id", "bytea", "bytea", "NO", Some("")),
            ("source_start", "bigint", "int8", "NO", Some("")),
            ("source_end", "bigint", "int8", "NO", Some("")),
        ],
    )
    .await?;
    inspect_columns(
        client,
        "catalogue_record_value_fields",
        &[
            ("catalogue_revision_id", "bytea", "bytea", "NO", Some("")),
            ("owner_type_id", "bytea", "bytea", "NO", Some("")),
            ("field_id", "bytea", "bytea", "NO", Some("")),
            ("name", "text", "text", "NO", Some("")),
            ("ordinal", "bigint", "int8", "NO", Some("")),
            ("type_kind", "text", "text", "NO", Some("")),
            ("value_type_id", "bytea", "bytea", "YES", Some("")),
            (
                "value_standard_library_revision_id",
                "bytea",
                "bytea",
                "YES",
                Some(""),
            ),
            ("enum_type_id", "bytea", "bytea", "YES", Some("")),
            ("source_unit_id", "bytea", "bytea", "NO", Some("")),
            ("source_start", "bigint", "int8", "NO", Some("")),
            ("source_end", "bigint", "int8", "NO", Some("")),
            (
                "enum_standard_library_revision_id",
                "bytea",
                "bytea",
                "YES",
                Some(""),
            ),
            ("standard_enum_type_id", "bytea", "bytea", "YES", Some("")),
            ("record_type_id", "bytea", "bytea", "YES", Some("")),
        ],
    )
    .await?;

    for (table, constraint, fragment) in [
        (
            "catalogue_record_value_types",
            "cat_record_value_types_value_kind_check",
            "value_kind = 'record'::text",
        ),
        (
            "catalogue_record_value_types",
            "cat_record_value_types_mutability_check",
            "mutability = 'immutable'::text",
        ),
        (
            "catalogue_record_value_types",
            "cat_record_value_types_persistence_check",
            "persistence = 'persistable'::text",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_type_kind_check",
            "type_kind = ANY (ARRAY['value'::text, 'enum'::text, 'record'::text])",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_type_check",
            "enum_standard_library_revision_id IS NOT NULL",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_type_check",
            "type_kind = 'record'::text) AND (value_type_id IS NULL)",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_enum_std_rev_length",
            "octet_length(enum_standard_library_revision_id) = 16",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_std_enum_id_length",
            "octet_length(standard_enum_type_id) = 16",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_record_type_id_length",
            "octet_length(record_type_id) = 16",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_owner_fk",
            "REFERENCES _orna_kernel.catalogue_record_value_types",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_value_pin_fk",
            "REFERENCES _orna_kernel.catalogue_revisions",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_value_type_fk",
            "REFERENCES _orna_kernel.standard_catalogue_value_types",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_enum_type_fk",
            "REFERENCES _orna_kernel.catalogue_enum_types",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_enum_pin_fk",
            "REFERENCES _orna_kernel.catalogue_revisions",
        ),
        (
            "catalogue_record_value_fields",
            "cat_record_value_fields_std_enum_fk",
            "REFERENCES _orna_kernel.standard_catalogue_enum_types",
        ),
    ] {
        require_constraint(client, table, constraint, fragment).await?;
    }

    require_exact_constraint(
        client,
        "catalogue_record_value_fields",
        "cat_record_value_fields_record_type_fk",
        "FOREIGN KEY (catalogue_revision_id, record_type_id) REFERENCES _orna_kernel.catalogue_record_value_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED",
        true,
        true,
    )
    .await?;

    for table in [
        "catalogue_record_value_types",
        "catalogue_record_value_fields",
    ] {
        require_count(
            client,
            &format!("_orna_kernel.{table}"),
            &format!("SELECT count(*) FROM _orna_kernel.{table}"),
            0,
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
            let relation = format!("_orna_kernel.{table}");
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

    Ok(())
}

async fn inspect_resolved_value_public_privileges(client: &Client) -> TestResult<()> {
    for table in [
        "catalogue_fields",
        "catalogue_function_parameters",
        "catalogue_function_return_columns",
        "catalogue_functions",
    ] {
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
            let relation = format!("_orna_kernel.{table}");
            let row = client
                .query_one(
                    "SELECT has_table_privilege('public', $1, $2)",
                    &[&relation, &privilege],
                )
                .await?;
            let granted: bool = value(&row, 0)?;
            require(
                !granted,
                format!("PUBLIC has {privilege} on protected table {relation}"),
            )?;
        }
    }
    Ok(())
}

async fn inspect_columns(
    client: &Client,
    table: &str,
    expected_columns: &[(&str, &str, &str, &str, Option<&str>)],
) -> TestResult<()> {
    let rows = client
        .query(
            "SELECT column_name, data_type, udt_name, is_nullable, column_default
             FROM information_schema.columns
             WHERE table_schema = '_orna_kernel' AND table_name = $1
             ORDER BY ordinal_position",
            &[&table],
        )
        .await?;
    let expected_names = expected_columns
        .iter()
        .map(|column| column.0)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let actual_names = rows
        .iter()
        .map(|row| value::<String>(row, 0))
        .collect::<TestResult<Vec<_>>>()?;
    for expected in expected_columns {
        let column = expected.0;
        let row = rows
            .iter()
            .find(|row| row.get::<_, String>(0) == column)
            .ok_or_else(|| failure(format!("missing {table}.{column}")))?;
        let actual = (
            value::<String>(row, 1)?,
            value::<String>(row, 2)?,
            value::<String>(row, 3)?,
            value::<Option<String>>(row, 4)?,
        );
        require(
            actual.0 == expected.1
                && actual.1 == expected.2
                && actual.2 == expected.3
                && match expected.4 {
                    Some("") => actual.3.is_none(),
                    Some(default) => actual.3.as_deref() == Some(default),
                    None => true,
                },
            format!(
                "{table}.{column} is ({:?}, {:?}, {:?}, {:?}); expected ({:?}, {:?}, {:?}, {:?})",
                actual.0,
                actual.1,
                actual.2,
                actual.3,
                expected.1,
                expected.2,
                expected.3,
                expected.4,
            ),
        )?;
    }
    require(
        actual_names == expected_names,
        format!("{table} columns differ: {actual_names:?}"),
    )
}

async fn inspect_column_contract(
    client: &Client,
    table: &str,
    expected_columns: &[(&str, &str, &str, &str, Option<&str>)],
) -> TestResult<()> {
    for expected in expected_columns {
        let row = client
            .query_opt(
                "SELECT data_type, udt_name, is_nullable, column_default
                 FROM information_schema.columns
                 WHERE table_schema = '_orna_kernel'
                   AND table_name = $1
                   AND column_name = $2",
                &[&table, &expected.0],
            )
            .await?
            .ok_or_else(|| failure(format!("missing {table}.{}", expected.0)))?;
        let actual = (
            value::<String>(&row, 0)?,
            value::<String>(&row, 1)?,
            value::<String>(&row, 2)?,
            value::<Option<String>>(&row, 3)?,
        );
        require(
            actual.0 == expected.1
                && actual.1 == expected.2
                && actual.2 == expected.3
                && match expected.4 {
                    Some("") => actual.3.is_none(),
                    Some(default) => actual.3.as_deref() == Some(default),
                    None => true,
                },
            format!(
                "{table}.{} is ({:?}, {:?}, {:?}, {:?}); expected ({:?}, {:?}, {:?}, {:?})",
                expected.0,
                actual.0,
                actual.1,
                actual.2,
                actual.3,
                expected.1,
                expected.2,
                expected.3,
                expected.4,
            ),
        )?;
    }
    Ok(())
}

fn exact_standard_catalogue_constraint_definition(constraint: &str) -> Option<&'static str> {
    Some(match constraint {
        "std_lib_rev_pkey" => "PRIMARY KEY (id)",
        "std_lib_rev_id_length" => "CHECK ((octet_length(id) = 16))",
        "std_lib_rev_source_revision_id_length" => {
            "CHECK ((octet_length(source_revision_id) = 16))"
        }
        "std_lib_rev_source_revision_key" => "UNIQUE (source_revision_id)",
        "std_lib_rev_source_revision_fk" => {
            "FOREIGN KEY (source_revision_id) REFERENCES _orna_kernel.source_revisions(id)"
        }
        "std_lib_rev_catalogue_revision_id_length" => {
            "CHECK ((octet_length(catalogue_revision_id) = 16))"
        }
        "std_lib_rev_catalogue_revision_key" => "UNIQUE (catalogue_revision_id)",
        "std_lib_rev_digest_version_check" => "CHECK ((digest_version = ANY (ARRAY[1, 2])))",
        "std_lib_rev_language_version_check" => "CHECK ((length(language_version) > 0))",
        "std_lib_rev_content_hash_length" => "CHECK ((octet_length(content_hash) = 32))",
        "std_lib_rev_hash_algorithm_check" => "CHECK ((hash_algorithm = 'sha256'::text))",
        "std_cat_schemas_pkey" => "PRIMARY KEY (standard_library_revision_id, schema_id)",
        "std_cat_schemas_std_lib_rev_id_length" => {
            "CHECK ((octet_length(standard_library_revision_id) = 16))"
        }
        "std_cat_schemas_std_lib_rev_fk" => {
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)"
        }
        "std_cat_schemas_schema_id_length" => "CHECK ((octet_length(schema_id) = 16))",
        "std_cat_schemas_name_parts_check" => {
            "CHECK (((cardinality(name_parts) > 0) AND (array_position(name_parts, NULL::text) IS NULL) AND (array_position(name_parts, ''::text) IS NULL)))"
        }
        "std_cat_schemas_name_key" => "UNIQUE (standard_library_revision_id, name_parts)",
        "std_cat_schemas_source_origin_check" => {
            "CHECK (((octet_length(source_unit_id) = 16) AND (source_start >= 0) AND (source_start <= '4294967295'::bigint) AND (source_end >= source_start) AND (source_end <= '4294967295'::bigint)))"
        }
        "std_cat_schemas_source_unit_fk" => {
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)"
        }
        "std_cat_value_types_pkey" => "PRIMARY KEY (standard_library_revision_id, type_id)",
        "std_cat_value_types_std_lib_rev_id_length" => {
            "CHECK ((octet_length(standard_library_revision_id) = 16))"
        }
        "std_cat_value_types_std_lib_rev_fk" => {
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)"
        }
        "std_cat_value_types_type_id_length" => "CHECK ((octet_length(type_id) = 16))",
        "std_cat_value_types_schema_id_length" => "CHECK ((octet_length(schema_id) = 16))",
        "std_cat_value_types_schema_fk" => {
            "FOREIGN KEY (standard_library_revision_id, schema_id) REFERENCES _orna_kernel.standard_catalogue_schemas(standard_library_revision_id, schema_id)"
        }
        "std_cat_value_types_name_parts_check" => {
            "CHECK (((cardinality(name_parts) >= 2) AND (array_position(name_parts, NULL::text) IS NULL) AND (array_position(name_parts, ''::text) IS NULL)))"
        }
        "std_cat_value_types_name_key" => "UNIQUE (standard_library_revision_id, name_parts)",
        "std_cat_value_types_value_kind_check" => {
            "CHECK ((value_kind = ANY (ARRAY['primitive'::text, 'opaque'::text])))"
        }
        "std_cat_value_types_opaque_contract_check" => {
            "CHECK (((value_kind <> 'opaque'::text) OR ((persistence = 'transient'::text) AND (octet_length(representation_contract) <= 128) AND (representation_contract !~ '[^ -~]'::text))))"
        }
        "std_cat_value_types_mutability_check" => "CHECK ((mutability = 'immutable'::text))",
        "std_cat_value_types_persistence_check" => {
            "CHECK ((persistence = ANY (ARRAY['persistable'::text, 'transient'::text])))"
        }
        "std_cat_value_types_representation_contract_check" => {
            "CHECK ((length(representation_contract) > 0))"
        }
        "std_cat_value_types_source_origin_check" => {
            "CHECK (((octet_length(source_unit_id) = 16) AND (source_start >= 0) AND (source_start <= '4294967295'::bigint) AND (source_end >= source_start) AND (source_end <= '4294967295'::bigint)))"
        }
        "std_cat_value_types_source_unit_fk" => {
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)"
        }
        "std_cat_enum_types_pkey" => "PRIMARY KEY (standard_library_revision_id, type_id)",
        "std_cat_enum_types_std_lib_rev_id_length" => {
            "CHECK ((octet_length(standard_library_revision_id) = 16))"
        }
        "std_cat_enum_types_std_lib_rev_fk" => {
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)"
        }
        "std_cat_enum_types_type_id_length" => "CHECK ((octet_length(type_id) = 16))",
        "std_cat_enum_types_schema_id_length" => "CHECK ((octet_length(schema_id) = 16))",
        "std_cat_enum_types_schema_fk" => {
            "FOREIGN KEY (standard_library_revision_id, schema_id) REFERENCES _orna_kernel.standard_catalogue_schemas(standard_library_revision_id, schema_id)"
        }
        "std_cat_enum_types_name_parts_check" => {
            "CHECK (((cardinality(name_parts) >= 2) AND (array_position(name_parts, NULL::text) IS NULL) AND (array_position(name_parts, ''::text) IS NULL)))"
        }
        "std_cat_enum_types_name_key" => "UNIQUE (standard_library_revision_id, name_parts)",
        "std_cat_enum_types_labels_check" => {
            "CHECK (((cardinality(labels) > 0) AND (array_position(labels, NULL::text) IS NULL)))"
        }
        "standard_catalogue_enum_types_source_origin_check" => {
            "CHECK (((octet_length(source_unit_id) = 16) AND (source_start >= 0) AND (source_start <= '4294967295'::bigint) AND (source_end >= source_start) AND (source_end <= '4294967295'::bigint)))"
        }
        "standard_catalogue_enum_types_source_unit_fk" => {
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)"
        }
        "std_cat_type_bindings_pkey" => {
            "PRIMARY KEY (standard_library_revision_id, type_binding_id)"
        }
        "std_cat_type_bindings_std_lib_rev_id_length" => {
            "CHECK ((octet_length(standard_library_revision_id) = 16))"
        }
        "std_cat_type_bindings_std_lib_rev_fk" => {
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)"
        }
        "std_cat_type_bindings_type_binding_id_length" => {
            "CHECK ((octet_length(type_binding_id) = 16))"
        }
        "std_cat_type_bindings_kind_check" => {
            "CHECK ((kind = ANY (ARRAY['qualified'::text, 'prelude'::text])))"
        }
        "std_cat_type_bindings_name_parts_check" => {
            "CHECK ((((kind = 'qualified'::text) AND (cardinality(name_parts) >= 2) AND (array_position(name_parts, NULL::text) IS NULL) AND (array_position(name_parts, ''::text) IS NULL)) OR ((kind = 'prelude'::text) AND (cardinality(name_parts) >= 1) AND (array_position(name_parts, NULL::text) IS NULL) AND (array_position(name_parts, ''::text) IS NULL))))"
        }
        "std_cat_type_bindings_name_key" => {
            "UNIQUE (standard_library_revision_id, kind, name_parts)"
        }
        "std_cat_type_bindings_target_type_id_length" => {
            "CHECK ((octet_length(target_type_id) = 16))"
        }
        "std_cat_type_bindings_target_type_fk" => {
            "FOREIGN KEY (standard_library_revision_id, target_type_id) REFERENCES _orna_kernel.standard_catalogue_value_types(standard_library_revision_id, type_id)"
        }
        "std_cat_type_bindings_target_type_kind_check" => {
            "CHECK ((target_type_kind = ANY (ARRAY['value'::text, 'enum'::text])))"
        }
        "std_cat_type_bindings_target_shape_check" => {
            "CHECK ((((target_type_kind = 'value'::text) AND (target_type_id IS NOT NULL) AND (target_enum_type_id IS NULL)) OR ((target_type_kind = 'enum'::text) AND (target_type_id IS NULL) AND (target_enum_type_id IS NOT NULL))))"
        }
        "std_cat_type_bindings_target_enum_id_length" => {
            "CHECK (((target_enum_type_id IS NULL) OR (octet_length(target_enum_type_id) = 16)))"
        }
        "std_cat_type_bindings_target_enum_fk" => {
            "FOREIGN KEY (standard_library_revision_id, target_enum_type_id) REFERENCES _orna_kernel.standard_catalogue_enum_types(standard_library_revision_id, type_id)"
        }
        "std_cat_type_bindings_source_origin_check" => {
            "CHECK (((octet_length(source_unit_id) = 16) AND (source_start >= 0) AND (source_start <= '4294967295'::bigint) AND (source_end >= source_start) AND (source_end <= '4294967295'::bigint)))"
        }
        "std_cat_type_bindings_source_unit_fk" => {
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)"
        }
        "catalogue_revisions_canonical_hash_version_check" => {
            "CHECK ((canonical_hash_version = ANY (ARRAY[1, 2])))"
        }
        "catalogue_revisions_std_lib_rev_id_length" => {
            "CHECK (((standard_library_revision_id IS NULL) OR (octet_length(standard_library_revision_id) = 16)))"
        }
        "catalogue_revisions_std_lib_rev_fk" => {
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)"
        }
        "catalogue_revisions_standard_context_check" => {
            "CHECK ((((canonical_hash_version = 1) AND (standard_library_revision_id IS NULL)) OR ((canonical_hash_version = 2) AND (standard_library_revision_id IS NOT NULL))))"
        }
        "catalogue_revisions_id_std_lib_rev_key" => "UNIQUE (id, standard_library_revision_id)",
        "function_revisions_semantic_hash_version_check" => {
            "CHECK ((semantic_hash_version = ANY (ARRAY[1, 2])))"
        }
        _ => return None,
    })
}

async fn inspect_standard_catalogue_constraints(client: &Client) -> TestResult<()> {
    for (table, constraint, _fragment) in [
        (
            "standard_library_revisions",
            "std_lib_rev_pkey",
            "PRIMARY KEY (id)",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_id_length",
            "octet_length(id) = 16",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_source_revision_id_length",
            "octet_length(source_revision_id) = 16",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_source_revision_key",
            "UNIQUE (source_revision_id)",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_source_revision_fk",
            "FOREIGN KEY (source_revision_id) REFERENCES _orna_kernel.source_revisions(id)",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_catalogue_revision_id_length",
            "octet_length(catalogue_revision_id) = 16",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_catalogue_revision_key",
            "UNIQUE (catalogue_revision_id)",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_digest_version_check",
            "digest_version = ANY (ARRAY[1, 2])",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_language_version_check",
            "length(language_version) > 0",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_content_hash_length",
            "octet_length(content_hash) = 32",
        ),
        (
            "standard_library_revisions",
            "std_lib_rev_hash_algorithm_check",
            "hash_algorithm = 'sha256'::text",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_pkey",
            "PRIMARY KEY (standard_library_revision_id, schema_id)",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_std_lib_rev_id_length",
            "octet_length(standard_library_revision_id) = 16",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_std_lib_rev_fk",
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_schema_id_length",
            "octet_length(schema_id) = 16",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_name_parts_check",
            "cardinality(name_parts) > 0",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_name_key",
            "UNIQUE (standard_library_revision_id, name_parts)",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_source_origin_check",
            "source_start <= '4294967295'::bigint",
        ),
        (
            "standard_catalogue_schemas",
            "std_cat_schemas_source_unit_fk",
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_pkey",
            "PRIMARY KEY (standard_library_revision_id, type_id)",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_std_lib_rev_id_length",
            "octet_length(standard_library_revision_id) = 16",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_std_lib_rev_fk",
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_type_id_length",
            "octet_length(type_id) = 16",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_schema_id_length",
            "octet_length(schema_id) = 16",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_schema_fk",
            "FOREIGN KEY (standard_library_revision_id, schema_id) REFERENCES _orna_kernel.standard_catalogue_schemas(standard_library_revision_id, schema_id)",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_name_parts_check",
            "cardinality(name_parts) >= 2",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_name_key",
            "UNIQUE (standard_library_revision_id, name_parts)",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_value_kind_check",
            "value_kind = ANY (ARRAY['primitive'::text, 'opaque'::text])",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_opaque_contract_check",
            "value_kind <> 'opaque'::text",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_mutability_check",
            "mutability = 'immutable'::text",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_persistence_check",
            "persistence = ANY (ARRAY['persistable'::text, 'transient'::text])",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_representation_contract_check",
            "length(representation_contract) > 0",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_source_origin_check",
            "source_start <= '4294967295'::bigint",
        ),
        (
            "standard_catalogue_value_types",
            "std_cat_value_types_source_unit_fk",
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_pkey",
            "PRIMARY KEY (standard_library_revision_id, type_id)",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_std_lib_rev_id_length",
            "octet_length(standard_library_revision_id) = 16",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_std_lib_rev_fk",
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_type_id_length",
            "octet_length(type_id) = 16",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_schema_id_length",
            "octet_length(schema_id) = 16",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_schema_fk",
            "FOREIGN KEY (standard_library_revision_id, schema_id) REFERENCES _orna_kernel.standard_catalogue_schemas(standard_library_revision_id, schema_id)",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_name_parts_check",
            "cardinality(name_parts) >= 2",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_name_key",
            "UNIQUE (standard_library_revision_id, name_parts)",
        ),
        (
            "standard_catalogue_enum_types",
            "std_cat_enum_types_labels_check",
            "cardinality(labels) > 0",
        ),
        (
            "standard_catalogue_enum_types",
            "standard_catalogue_enum_types_source_origin_check",
            "source_start <= '4294967295'::bigint",
        ),
        (
            "standard_catalogue_enum_types",
            "standard_catalogue_enum_types_source_unit_fk",
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_pkey",
            "PRIMARY KEY (standard_library_revision_id, type_binding_id)",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_std_lib_rev_id_length",
            "octet_length(standard_library_revision_id) = 16",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_std_lib_rev_fk",
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_type_binding_id_length",
            "octet_length(type_binding_id) = 16",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_kind_check",
            "kind = ANY (ARRAY['qualified'::text, 'prelude'::text])",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_name_parts_check",
            "kind = 'qualified'::text",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_name_key",
            "UNIQUE (standard_library_revision_id, kind, name_parts)",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_target_type_id_length",
            "octet_length(target_type_id) = 16",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_target_type_fk",
            "FOREIGN KEY (standard_library_revision_id, target_type_id) REFERENCES _orna_kernel.standard_catalogue_value_types(standard_library_revision_id, type_id)",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_target_type_kind_check",
            "target_type_kind = ANY (ARRAY['value'::text, 'enum'::text])",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_target_shape_check",
            "target_enum_type_id IS NOT NULL",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_target_enum_id_length",
            "octet_length(target_enum_type_id) = 16",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_target_enum_fk",
            "FOREIGN KEY (standard_library_revision_id, target_enum_type_id) REFERENCES _orna_kernel.standard_catalogue_enum_types(standard_library_revision_id, type_id)",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_source_origin_check",
            "source_start <= '4294967295'::bigint",
        ),
        (
            "standard_catalogue_type_bindings",
            "std_cat_type_bindings_source_unit_fk",
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)",
        ),
        (
            "catalogue_revisions",
            "catalogue_revisions_canonical_hash_version_check",
            "canonical_hash_version = ANY (ARRAY[1, 2])",
        ),
        (
            "catalogue_revisions",
            "catalogue_revisions_std_lib_rev_id_length",
            "octet_length(standard_library_revision_id) = 16",
        ),
        (
            "catalogue_revisions",
            "catalogue_revisions_std_lib_rev_fk",
            "FOREIGN KEY (standard_library_revision_id) REFERENCES _orna_kernel.standard_library_revisions(id)",
        ),
        (
            "catalogue_revisions",
            "catalogue_revisions_standard_context_check",
            "canonical_hash_version = 1",
        ),
        (
            "catalogue_revisions",
            "catalogue_revisions_id_std_lib_rev_key",
            "UNIQUE (id, standard_library_revision_id)",
        ),
        (
            "function_revisions",
            "function_revisions_semantic_hash_version_check",
            "semantic_hash_version = ANY (ARRAY[1, 2])",
        ),
    ] {
        let expected_definition = exact_standard_catalogue_constraint_definition(constraint)
            .ok_or_else(|| failure(format!("missing exact standard contract for {constraint}")))?;
        require_exact_constraint(client, table, constraint, expected_definition, false, false)
            .await?;
    }

    require_no_foreign_key_to(
        client,
        "standard_library_revisions",
        "_orna_kernel.catalogue_revisions",
    )
    .await?;
    Ok(())
}

async fn inspect_standard_catalogue_indexes(client: &Client) -> TestResult<()> {
    for (index, relation, columns) in [
        (
            "catalogue_schemas_identity_index",
            "catalogue_schemas",
            "(schema_id, catalogue_revision_id)",
        ),
        (
            "catalogue_object_types_identity_index",
            "catalogue_object_types",
            "(type_id, catalogue_revision_id)",
        ),
        (
            "standard_catalogue_schemas_identity_index",
            "standard_catalogue_schemas",
            "(schema_id, standard_library_revision_id)",
        ),
        (
            "standard_catalogue_value_types_identity_index",
            "standard_catalogue_value_types",
            "(type_id, standard_library_revision_id)",
        ),
        (
            "standard_catalogue_type_bindings_identity_index",
            "standard_catalogue_type_bindings",
            "(type_binding_id, standard_library_revision_id)",
        ),
    ] {
        require_index_shape(client, index, relation, columns, None).await?;
    }
    require_index_shape(
        client,
        "definition_references_value_type_target_index",
        "definition_references",
        "(target_standard_library_revision_id, target_definition_id, catalogue_revision_id)",
        Some("(target_kind = 'value_type'::text)"),
    )
    .await
}

async fn inspect_standard_catalogue_privileges(client: &Client) -> TestResult<()> {
    for table in [
        "standard_library_revisions",
        "standard_catalogue_enum_types",
        "standard_catalogue_schemas",
        "standard_catalogue_value_types",
        "standard_catalogue_type_bindings",
    ] {
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
            let relation = format!("_orna_kernel.{table}");
            let row = client
                .query_one(
                    "SELECT has_table_privilege('public', $1, $2)",
                    &[&relation, &privilege],
                )
                .await?;
            let granted: bool = value(&row, 0)?;
            require(
                !granted,
                format!("PUBLIC has {privilege} on protected table {relation}"),
            )?;
        }
    }
    Ok(())
}

pub(super) async fn inspect_migrations(client: &Client) -> TestResult<()> {
    let rows = client
        .query(
            "SELECT version, name, checksum
             FROM _orna_kernel.schema_migrations
             ORDER BY version",
            &[],
        )
        .await?;
    require(
        rows.len() == MIGRATIONS.len(),
        format!(
            "migration count is {}; expected {}",
            rows.len(),
            MIGRATIONS.len()
        ),
    )?;

    for (row, (expected_version, expected_name, migration_sql)) in rows.iter().zip(MIGRATIONS) {
        let version: i64 = value(row, 0)?;
        let name: String = value(row, 1)?;
        let checksum: Vec<u8> = value(row, 2)?;
        require(
            version == *expected_version,
            format!("migration version is {version}; expected {expected_version}"),
        )?;
        require(
            name == *expected_name,
            format!("migration {version} name is {name:?}; expected {expected_name:?}"),
        )?;
        require(
            checksum == expected_migration_checksum(*expected_version, migration_sql),
            format!("migration {version} checksum does not match its registered contract"),
        )?;
        require(
            checksum.len() == 32,
            format!("migration {version} checksum is not 32 bytes"),
        )?;
    }
    Ok(())
}

pub(super) fn expected_migration_checksum(version: i64, sql: &str) -> Vec<u8> {
    let mut hash = Sha256::new();
    hash.update(sql.as_bytes());
    if version == 4 {
        hash.update(MIGRATION_DATA_STEP_SEPARATOR);
        hash.update(CANONICAL_HASH_V1_EMPTY_SEED_STEP);
    } else if version == 47 {
        hash.update(MIGRATION_DATA_STEP_SEPARATOR);
        hash.update(APPLICATION_MIGRATION_LEDGER_BASELINE_STEP);
    }
    hash.finalize().to_vec()
}

fn require_database_constraint(
    error: &PostgresKernelError,
    expected_sqlstate: &str,
    expected_constraint: Option<&str>,
    context: &str,
) -> TestResult<()> {
    let PostgresKernelError::Database(error) = error else {
        return Err(failure(format!(
            "{context} produced a non-database failure: {error}"
        )));
    };
    let database_error = error
        .as_db_error()
        .ok_or_else(|| failure(format!("{context} has no PostgreSQL error fields: {error}")))?;
    require(
        database_error.code().code() == expected_sqlstate
            && database_error.constraint() == expected_constraint,
        format!(
            "{context} failed with SQLSTATE {} and constraint {:?}; expected {expected_sqlstate} and {expected_constraint:?}",
            database_error.code().code(),
            database_error.constraint(),
        ),
    )
}

fn hex_bytes(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn snapshot_upgrade_state(database: &TestDatabase) -> TestResult<UpgradeSnapshot> {
    let session = database.open().await?;
    let snapshot_result = async {
        let active = session
            .client()
            .query_one(
                "SELECT source_revision_id, catalogue_revision_id
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true",
                &[],
            )
            .await?;
        let active_pair = (value(&active, 0)?, value(&active, 1)?);
        let source_unit_count = session
            .client()
            .query_one("SELECT count(*) FROM _orna_kernel.source_units", &[])
            .await?
            .get(0);
        let migrations = session
            .client()
            .query(
                "SELECT version, name, checksum
                 FROM _orna_kernel.schema_migrations
                 ORDER BY version",
                &[],
            )
            .await?
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?, value(row, 2)?)))
            .collect::<TestResult<Vec<(i64, String, Vec<u8>)>>>()?;
        let references = session
            .client()
            .query(
                "SELECT catalogue_revision_id, source_function_id,
                        source_function_revision_id, ordinal,
                        target_definition_id, target_kind, reference_kind,
                        source_subobject_id, source_unit_id, source_start,
                        source_end, target_owner_type_id,
                        target_owner_function_id, xmin::text
                 FROM _orna_kernel.definition_references
                 ORDER BY ordinal",
                &[],
            )
            .await?
            .iter()
            .map(|row| {
                Ok(DefinitionReferenceSnapshot {
                    catalogue_revision_id: value(row, 0)?,
                    source_function_id: value(row, 1)?,
                    source_function_revision_id: value(row, 2)?,
                    ordinal: value(row, 3)?,
                    target_definition_id: value(row, 4)?,
                    target_kind: value(row, 5)?,
                    reference_kind: value(row, 6)?,
                    source_subobject_id: value(row, 7)?,
                    source_unit_id: value(row, 8)?,
                    source_start: value(row, 9)?,
                    source_end: value(row, 10)?,
                    target_owner_type_id: value(row, 11)?,
                    target_owner_function_id: value(row, 12)?,
                    xmin: value(row, 13)?,
                })
            })
            .collect::<TestResult<Vec<DefinitionReferenceSnapshot>>>()?;
        let catalogue_hashes = session
            .client()
            .query(
                "SELECT id, content_hash
                 FROM _orna_kernel.catalogue_revisions
                 ORDER BY id",
                &[],
            )
            .await?
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?)))
            .collect::<TestResult<Vec<(Vec<u8>, Vec<u8>)>>>()?;
        let function_hashes = session
            .client()
            .query(
                "SELECT id, semantic_ir_hash
                 FROM _orna_kernel.function_revisions
                 ORDER BY id",
                &[],
            )
            .await?
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?)))
            .collect::<TestResult<Vec<(Vec<u8>, Vec<u8>)>>>()?;
        Ok(UpgradeSnapshot {
            active_pair,
            source_unit_count,
            migrations,
            references,
            catalogue_hashes,
            function_hashes,
        })
    }
    .await;
    let shutdown_result = session.shutdown().await;

    match (snapshot_result, shutdown_result) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(snapshot_error), Err(shutdown_error)) => Err(failure(format!(
            "upgrade snapshot failed: {snapshot_error}; snapshot driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn snapshot_catalogue_surface(
    database: &TestDatabase,
) -> TestResult<CatalogueSurfaceSnapshot> {
    let session = database.open().await?;
    let snapshot_result = async {
        let relations_and_indexes = session
            .client()
            .query(
                "SELECT namespace.nspname, relation.relname, relation.relkind::text
                 FROM pg_class AS relation
                 JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
                 WHERE namespace.nspname IN ('_orna_kernel', '_orna_data')
                 ORDER BY namespace.nspname, relation.relname, relation.relkind",
                &[],
            )
            .await?
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?, value(row, 2)?)))
            .collect::<TestResult<Vec<(String, String, String)>>>()?;
        let triggers = session
            .client()
            .query(
                "SELECT namespace.nspname, relation.relname, trigger_row.tgname,
                        trigger_row.tgisinternal
                 FROM pg_trigger AS trigger_row
                 JOIN pg_class AS relation ON relation.oid = trigger_row.tgrelid
                 JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
                 WHERE namespace.nspname IN ('_orna_kernel', '_orna_data')
                   AND NOT trigger_row.tgisinternal
                 ORDER BY namespace.nspname, relation.relname, trigger_row.tgname",
                &[],
            )
            .await?
            .iter()
            .map(|row| {
                Ok((
                    value(row, 0)?,
                    value(row, 1)?,
                    value(row, 2)?,
                    value(row, 3)?,
                ))
            })
            .collect::<TestResult<Vec<(String, String, String, bool)>>>()?;
        let relation_acls = session
            .client()
            .query(
                "SELECT namespace.nspname, relation.relname,
                        COALESCE(relation.relacl::text, '')
                 FROM pg_class AS relation
                 JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
                 WHERE namespace.nspname IN ('_orna_kernel', '_orna_data')
                 ORDER BY namespace.nspname, relation.relname",
                &[],
            )
            .await?
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?, value(row, 2)?)))
            .collect::<TestResult<Vec<(String, String, String)>>>()?;
        let schema_acls = session
            .client()
            .query(
                "SELECT namespace.nspname, COALESCE(namespace.nspacl::text, '')
                 FROM pg_namespace AS namespace
                 WHERE namespace.nspname IN ('_orna_kernel', '_orna_data')
                 ORDER BY namespace.nspname",
                &[],
            )
            .await?
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?)))
            .collect::<TestResult<Vec<(String, String)>>>()?;
        Ok(CatalogueSurfaceSnapshot {
            relations_and_indexes,
            triggers,
            relation_acls,
            schema_acls,
        })
    }
    .await;
    let shutdown_result = session.shutdown().await;
    match (snapshot_result, shutdown_result) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(snapshot_error), Err(shutdown_error)) => Err(failure(format!(
            "catalogue surface snapshot failed: {snapshot_error}; snapshot driver shutdown failed: {shutdown_error}"
        ))),
    }
}

async fn snapshot_application_target_foreign_keys(
    database: &TestDatabase,
) -> TestResult<TargetForeignKeySnapshot> {
    let session = database.open().await?;
    let snapshot_result = session
        .client()
        .query(
            "SELECT relation.relname, constraint_row.conname,
                    pg_get_constraintdef(constraint_row.oid),
                    constraint_row.condeferrable,
                    constraint_row.condeferred
             FROM pg_constraint AS constraint_row
             JOIN pg_class AS relation ON relation.oid = constraint_row.conrelid
             JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
             WHERE namespace.nspname = '_orna_kernel'
               AND constraint_row.contype = 'f'
               AND (
                   (relation.relname = 'catalogue_fields'
                    AND constraint_row.conname = 'catalogue_fields_catalogue_revision_id_target_type_id_fkey')
                   OR (relation.relname = 'catalogue_function_parameters'
                    AND constraint_row.conname = 'catalogue_function_parameters_catalogue_revision_id_target_fkey')
                   OR (relation.relname = 'catalogue_function_return_columns'
                    AND constraint_row.conname = 'catalogue_function_return_col_catalogue_revision_id_target_fkey')
                   OR (relation.relname = 'catalogue_functions'
                    AND constraint_row.conname = 'catalogue_functions_catalogue_revision_id_return_target_ty_fkey')
               )
             ORDER BY relation.relname, constraint_row.conname",
            &[],
        )
        .await
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    Ok((
                        value(row, 0)?,
                        value(row, 1)?,
                        value(row, 2)?,
                        value(row, 3)?,
                        value(row, 4)?,
                    ))
                })
                .collect::<TestResult<Vec<(String, String, String, bool, bool)>>>()
                .map(TargetForeignKeySnapshot)
        })?;
    let shutdown_result = session.shutdown().await;
    match (snapshot_result, shutdown_result) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(snapshot_error), Err(shutdown_error)) => Err(failure(format!(
            "target foreign-key snapshot failed: {snapshot_error}; snapshot driver shutdown failed: {shutdown_error}"
        ))),
    }
}

fn expected_application_target_foreign_keys() -> TargetForeignKeySnapshot {
    TargetForeignKeySnapshot(vec![
        (
            "catalogue_fields".to_owned(),
            "catalogue_fields_catalogue_revision_id_target_type_id_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, target_type_id) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
        (
            "catalogue_function_parameters".to_owned(),
            "catalogue_function_parameters_catalogue_revision_id_target_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, target_type_id) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
        (
            "catalogue_function_return_columns".to_owned(),
            "catalogue_function_return_col_catalogue_revision_id_target_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, target_type_id) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
        (
            "catalogue_functions".to_owned(),
            "catalogue_functions_catalogue_revision_id_return_target_ty_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, return_target_type_id) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
    ])
}

fn expected_application_target_foreign_keys_after_sealed_inspector() -> TargetForeignKeySnapshot {
    TargetForeignKeySnapshot(vec![
        (
            "catalogue_fields".to_owned(),
            "catalogue_fields_catalogue_revision_id_target_type_id_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, target_type_id) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
        (
            "catalogue_function_parameters".to_owned(),
            "catalogue_function_parameters_catalogue_revision_id_target_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, target_type_id_fk) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
        (
            "catalogue_function_return_columns".to_owned(),
            "catalogue_function_return_col_catalogue_revision_id_target_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, target_type_id) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
        (
            "catalogue_functions".to_owned(),
            "catalogue_functions_catalogue_revision_id_return_target_ty_fkey".to_owned(),
            "FOREIGN KEY (catalogue_revision_id, return_target_type_id_fk) REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED".to_owned(),
            true,
            true,
        ),
    ])
}

async fn inspect_empty_aggregate_hashes(client: &Client) -> TestResult<()> {
    let row = client
        .query_one(
            "SELECT
                bundle.id,
                bundle.content_hash,
                bundle.hash_algorithm,
                bundle.hash_contract_version,
                source.content_hash,
                source.hash_algorithm,
                source.hash_contract_version,
                catalogue.id,
                catalogue.content_hash,
                catalogue.hash_algorithm,
                catalogue.hash_contract_version
             FROM _orna_kernel.source_bundles AS bundle
             CROSS JOIN _orna_kernel.source_revisions AS source
             CROSS JOIN _orna_kernel.catalogue_revisions AS catalogue",
            &[],
        )
        .await?;
    let bundle = SourceBundleId::from_bytes(exact_id(value(&row, 0)?, "source bundle")?);
    let catalogue = CatalogueRevisionId::from_bytes(exact_id(value(&row, 7)?, "catalogue")?);
    let bundle_hash = source_bundle_digest(&[])?;
    let source_hash = source_revision_record_digest(bundle, None, bundle_hash)?;
    let snapshot = CatalogueSnapshot::new(catalogue, Vec::new(), Vec::new())?;
    let catalogue_hash = catalogue_digest(&snapshot, &[], &[], &[], &[])?;

    require(
        value::<Vec<u8>>(&row, 1)? == bundle_hash.to_bytes(),
        "source bundle does not store the canonical empty bundle hash",
    )?;
    require(
        value::<Vec<u8>>(&row, 4)? == source_hash.to_bytes(),
        "source revision does not store the canonical empty source revision hash",
    )?;
    require(
        value::<Vec<u8>>(&row, 8)? == catalogue_hash.to_bytes(),
        "catalogue revision does not store the canonical empty catalogue hash",
    )?;
    for (relation, algorithm_index, contract_version_index) in [
        ("source bundle", 2, 3),
        ("source revision", 5, 6),
        ("catalogue revision", 9, 10),
    ] {
        let hash_algorithm: String = value(&row, algorithm_index)?;
        let contract_version: i16 = value(&row, contract_version_index)?;
        require(
            hash_algorithm == "sha256",
            format!("{relation} hash algorithm is {hash_algorithm:?}; expected sha256"),
        )?;
        require(
            contract_version == 1,
            format!("{relation} hash contract version is {contract_version}; expected 1"),
        )?;
    }
    Ok(())
}

async fn inspect_hash_contract_columns(client: &Client) -> TestResult<()> {
    for table in HASH_CONTRACT_TABLES {
        let row = client
            .query_opt(
                "SELECT data_type, is_nullable, column_default
                 FROM information_schema.columns
                 WHERE table_schema = '_orna_kernel'
                   AND table_name = $1
                   AND column_name = 'hash_contract_version'",
                &[table],
            )
            .await?
            .ok_or_else(|| failure(format!("missing {table}.hash_contract_version")))?;
        let data_type: String = value(&row, 0)?;
        let is_nullable: String = value(&row, 1)?;
        let default: Option<String> = value(&row, 2)?;
        require(
            data_type == "smallint" && is_nullable == "NO" && default.as_deref() == Some("1"),
            format!(
                "{table}.hash_contract_version contract is ({data_type:?}, {is_nullable:?}, {default:?})"
            ),
        )?;
        require_constraint(
            client,
            table,
            &format!("{table}_hash_contract_version_check"),
            "hash_contract_version = 1",
        )
        .await?;
    }
    Ok(())
}

async fn inspect_origin_columns(client: &Client) -> TestResult<()> {
    let schema = "_orna_kernel";

    for table in ORIGIN_TABLES {
        let nullability = match *table {
            "catalogue_enum_types"
            | "catalogue_record_value_fields"
            | "catalogue_record_value_types"
            | "standard_catalogue_enum_types" => "NO",
            _ => "YES",
        };
        let expected_columns = BTreeSet::from([
            ("source_end".to_owned(), nullability.to_owned()),
            ("source_start".to_owned(), nullability.to_owned()),
            ("source_unit_id".to_owned(), nullability.to_owned()),
        ]);
        let rows = client
            .query(
                "SELECT column_name, is_nullable
                 FROM information_schema.columns
                 WHERE table_schema = $1
                   AND table_name = $2
                   AND column_name IN ('source_unit_id', 'source_start', 'source_end')",
                &[&schema, table],
            )
            .await?;
        let actual_columns = rows
            .iter()
            .map(|row| Ok((value(row, 0)?, value(row, 1)?)))
            .collect::<TestResult<BTreeSet<(String, String)>>>()?;
        require(
            actual_columns == expected_columns,
            format!("{table} source-origin columns differ: {actual_columns:?}"),
        )?;
        require_constraint(
            client,
            table,
            &format!("{table}_source_origin_check"),
            "CHECK",
        )
        .await?;
        require_constraint(
            client,
            table,
            &format!("{table}_source_unit_fk"),
            "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)",
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn inspect_owner_qualified_catalogue_members(client: &Client) -> TestResult<()> {
    require_constraint(
        client,
        "catalogue_fields",
        "catalogue_fields_pkey",
        "PRIMARY KEY (catalogue_revision_id, owner_type_id, field_id)",
    )
    .await?;
    require_constraint(
        client,
        "catalogue_function_parameters",
        "catalogue_function_parameters_pkey",
        "PRIMARY KEY (catalogue_revision_id, function_id, parameter_id)",
    )
    .await
}

pub(super) async fn inspect_definition_references(client: &Client) -> TestResult<()> {
    let rows = client
        .query(
            "SELECT column_name, is_nullable
             FROM information_schema.columns
             WHERE table_schema = '_orna_kernel'
               AND table_name = 'definition_references'
             ORDER BY ordinal_position",
            &[],
        )
        .await?;
    let actual_columns = rows
        .iter()
        .map(|row| Ok((value(row, 0)?, value(row, 1)?)))
        .collect::<TestResult<Vec<(String, String)>>>()?;
    let expected_columns = vec![
        ("catalogue_revision_id".to_owned(), "NO".to_owned()),
        ("source_function_id".to_owned(), "NO".to_owned()),
        ("source_function_revision_id".to_owned(), "NO".to_owned()),
        ("ordinal".to_owned(), "NO".to_owned()),
        ("target_definition_id".to_owned(), "NO".to_owned()),
        ("target_kind".to_owned(), "NO".to_owned()),
        ("reference_kind".to_owned(), "NO".to_owned()),
        ("source_subobject_id".to_owned(), "YES".to_owned()),
        ("source_unit_id".to_owned(), "NO".to_owned()),
        ("source_start".to_owned(), "NO".to_owned()),
        ("source_end".to_owned(), "NO".to_owned()),
        ("target_owner_type_id".to_owned(), "YES".to_owned()),
        ("target_owner_function_id".to_owned(), "YES".to_owned()),
        (
            "target_standard_library_revision_id".to_owned(),
            "YES".to_owned(),
        ),
        (
            "target_enum_catalogue_revision_id".to_owned(),
            "YES".to_owned(),
        ),
        (
            "target_record_catalogue_revision_id".to_owned(),
            "YES".to_owned(),
        ),
        (
            "target_record_field_catalogue_revision_id".to_owned(),
            "YES".to_owned(),
        ),
        (
            "target_record_field_owner_type_id".to_owned(),
            "YES".to_owned(),
        ),
    ];
    require(
        actual_columns == expected_columns,
        format!("definition_references columns differ: {actual_columns:?}"),
    )?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_catalogue_function_revision_fk",
        "FOREIGN KEY (catalogue_revision_id, source_function_id, source_function_revision_id) REFERENCES _orna_kernel.catalogue_functions(catalogue_revision_id, function_id, current_function_revision_id)",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_function_revision_fk",
        "FOREIGN KEY (source_function_id, source_function_revision_id) REFERENCES _orna_kernel.function_revisions(function_id, id)",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_source_unit_fk",
        "FOREIGN KEY (source_unit_id) REFERENCES _orna_kernel.source_units(id)",
    )
    .await?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_target_kind_check",
        "CHECK ((target_kind = ANY (ARRAY['object_type'::text, 'field'::text, 'record_field'::text, 'function'::text, 'parameter'::text, 'expression'::text, 'value_type'::text, 'enum_type'::text, 'record_type'::text])))",
        false,
        false,
    )
    .await?;
    let reference_kind_constraint = constraint_definition(
        client,
        "definition_references",
        "definition_references_reference_kind_check",
    )
    .await?;
    for reference_kind in [
        "function_call",
        "named_type",
        "object_reference",
        "parameter_read",
        "query_object",
        "query_field",
        "expression",
        "write_object",
        "write_field",
    ] {
        require(
            reference_kind_constraint.contains(&format!("'{reference_kind}'::text")),
            format!(
                "definition_references reference kind constraint omits {reference_kind:?}: {reference_kind_constraint:?}"
            ),
        )?;
    }

    require_constraint(
        client,
        "definition_references",
        "definition_references_target_owner_type_id_check",
        "octet_length(target_owner_type_id) = 16",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_target_owner_function_id_check",
        "octet_length(target_owner_function_id) = 16",
    )
    .await?;

    require_constraint(
        client,
        "definition_references",
        "definition_references_target_owner_shape_check",
        "(target_kind = 'record_field'::text) AND (target_owner_type_id IS NULL) AND (target_record_field_owner_type_id IS NOT NULL)",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_reference_target_compatibility_check",
        "(reference_kind = 'write_field'::text) AND (target_kind = ANY (ARRAY['field'::text, 'record_field'::text]))",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_field_target_fk",
        "FOREIGN KEY (catalogue_revision_id, target_owner_type_id, target_definition_id) REFERENCES _orna_kernel.catalogue_fields(catalogue_revision_id, owner_type_id, field_id) DEFERRABLE INITIALLY DEFERRED",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_record_field_target_fk",
        "FOREIGN KEY (target_record_field_catalogue_revision_id, target_record_field_owner_type_id, target_definition_id) REFERENCES _orna_kernel.catalogue_record_value_fields(catalogue_revision_id, owner_type_id, field_id) DEFERRABLE INITIALLY DEFERRED",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_parameter_target_fk",
        "FOREIGN KEY (catalogue_revision_id, target_owner_function_id, target_definition_id) REFERENCES _orna_kernel.catalogue_function_parameters(catalogue_revision_id, function_id, parameter_id) DEFERRABLE INITIALLY DEFERRED",
    )
    .await?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_target_std_lib_rev_id_length",
        "CHECK (((target_standard_library_revision_id IS NULL) OR (octet_length(target_standard_library_revision_id) = 16)))",
        false,
        false,
    )
    .await?;
    let target_std_lib_rev_shape = constraint_definition(
        client,
        "definition_references",
        "definition_references_target_std_lib_rev_shape_check",
    )
    .await?;
    require(
        target_std_lib_rev_shape.contains("target_kind = 'value_type'::text")
            && target_std_lib_rev_shape.contains("target_standard_library_revision_id IS NOT NULL")
            && target_std_lib_rev_shape.contains("target_standard_library_revision_id IS NULL")
            && target_std_lib_rev_shape.contains("target_definition_id <> ALL")
            && target_std_lib_rev_shape.contains("target_definition_id = ANY")
            && target_std_lib_rev_shape
                .contains("decode('000000000000000000000000000000f3'::text, 'hex'::text)")
            && target_std_lib_rev_shape
                .contains("decode('000000000000000000000000000000ff'::text, 'hex'::text)"),
        format!(
            "definition_references sealed value-type shape is not closed: {target_std_lib_rev_shape:?}"
        ),
    )?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_catalogue_std_lib_rev_fk",
        "FOREIGN KEY (catalogue_revision_id, target_standard_library_revision_id) REFERENCES _orna_kernel.catalogue_revisions(id, standard_library_revision_id) DEFERRABLE INITIALLY DEFERRED",
        true,
        true,
    )
    .await?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_std_value_type_target_fk",
        "FOREIGN KEY (target_standard_library_revision_id, target_definition_id) REFERENCES _orna_kernel.standard_catalogue_value_types(standard_library_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED",
        true,
        true,
    )
    .await?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_target_enum_revision_length",
        "CHECK (((target_enum_catalogue_revision_id IS NULL) OR (octet_length(target_enum_catalogue_revision_id) = 16)))",
        false,
        false,
    )
    .await?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_target_enum_revision_shape",
        "CHECK ((((target_kind = 'enum_type'::text) AND (target_enum_catalogue_revision_id = catalogue_revision_id)) OR ((target_kind <> 'enum_type'::text) AND (target_enum_catalogue_revision_id IS NULL))))",
        false,
        false,
    )
    .await?;
    require_exact_constraint(
        client,
        "definition_references",
        "definition_references_enum_type_target_fk",
        "FOREIGN KEY (target_enum_catalogue_revision_id, target_definition_id) REFERENCES _orna_kernel.catalogue_enum_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED",
        true,
        true,
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_target_record_revision_length",
        "octet_length(target_record_catalogue_revision_id) = 16",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_target_record_revision_shape",
        "target_kind = 'record_type'::text",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_record_type_target_fk",
        "FOREIGN KEY (target_record_catalogue_revision_id, target_definition_id) REFERENCES _orna_kernel.catalogue_record_value_types(catalogue_revision_id, type_id) DEFERRABLE INITIALLY DEFERRED",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_target_record_field_revision_length",
        "octet_length(target_record_field_catalogue_revision_id) = 16",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_target_record_field_owner_type_id_check",
        "octet_length(target_record_field_owner_type_id) = 16",
    )
    .await?;
    require_constraint(
        client,
        "definition_references",
        "definition_references_target_field_revision_shape",
        "target_kind = 'record_field'::text",
    )
    .await?;
    require_index(
        client,
        "definition_references_field_target_index",
        "(target_owner_type_id, target_definition_id, catalogue_revision_id) WHERE (target_kind = 'field'::text)",
    )
    .await?;
    require_index(
        client,
        "definition_references_record_field_target_index",
        "(target_record_field_catalogue_revision_id, target_record_field_owner_type_id, target_definition_id) WHERE (target_kind = 'record_field'::text)",
    )
    .await?;
    require_index(
        client,
        "definition_references_parameter_target_index",
        "(target_owner_function_id, target_definition_id, catalogue_revision_id) WHERE (target_kind = 'parameter'::text)",
    )
    .await?;
    require_index(
        client,
        "definition_references_direct_target_index",
        "(target_kind, target_definition_id, catalogue_revision_id) WHERE (target_kind <> ALL (ARRAY['field'::text, 'record_field'::text, 'parameter'::text]))",
    )
    .await?;
    require_index(
        client,
        "definition_references_enum_type_target_index",
        "(target_enum_catalogue_revision_id, target_definition_id) WHERE (target_kind = 'enum_type'::text)",
    )
    .await?;
    require_index(
        client,
        "definition_references_record_type_target_index",
        "(target_record_catalogue_revision_id, target_definition_id) WHERE (target_kind = 'record_type'::text)",
    )
    .await?;
    require_index_absent(client, "definition_references_target_index").await?;
    require_index_absent(client, "definition_references_owner_qualified_target_index").await?;
    Ok(())
}

async fn inspect_function_revision_constraints(client: &Client) -> TestResult<()> {
    require_constraint(
        client,
        "function_revisions",
        "function_revisions_introduced_catalogue_revision_fk",
        "FOREIGN KEY (introduced_catalogue_revision_id) REFERENCES _orna_kernel.catalogue_revisions(id)",
    )
    .await?;
    require_constraint(
        client,
        "function_revisions",
        "function_revisions_introduced_function_fk",
        "FOREIGN KEY (introduced_catalogue_revision_id, function_id) REFERENCES _orna_kernel.catalogue_functions(catalogue_revision_id, function_id)",
    )
    .await?;
    require_constraint(
        client,
        "function_revisions",
        "function_revisions_function_id_id_key",
        "UNIQUE (function_id, id)",
    )
    .await?;
    require_constraint(
        client,
        "function_revisions",
        "function_revisions_function_content_semantic_key",
        "UNIQUE (function_id, content_hash, semantic_ir_hash)",
    )
    .await?;
    require_constraint_absent(
        client,
        "function_revisions",
        "function_revisions_function_id_content_hash_key",
    )
    .await?;
    require_constraint(
        client,
        "catalogue_functions",
        "catalogue_functions_current_revision_fk",
        "FOREIGN KEY (function_id, current_function_revision_id) REFERENCES _orna_kernel.function_revisions(function_id, id)",
    )
    .await
}

async fn require_constraint_absent(
    client: &Client,
    table: &str,
    constraint: &str,
) -> TestResult<()> {
    let row = client
        .query_one(
            "SELECT count(*)
             FROM pg_constraint AS constraint_row
             WHERE constraint_row.conrelid = to_regclass($1)
               AND constraint_row.conname = $2",
            &[&format!("_orna_kernel.{table}"), &constraint],
        )
        .await?;
    let count: i64 = value(&row, 0)?;
    require(
        count == 0,
        format!("unexpected {table} constraint {constraint}"),
    )
}

pub(super) async fn require_constraint(
    client: &Client,
    table: &str,
    constraint: &str,
    expected_fragment: &str,
) -> TestResult<()> {
    let definition = constraint_definition(client, table, constraint).await?;
    require(
        definition.contains(expected_fragment),
        format!(
            "{table} constraint {constraint} is {definition:?}; expected {expected_fragment:?}"
        ),
    )
}

async fn require_exact_constraint(
    client: &Client,
    table: &str,
    constraint: &str,
    expected_definition: &str,
    expected_deferrable: bool,
    expected_deferred: bool,
) -> TestResult<()> {
    let row = client
        .query_opt(
            "SELECT pg_get_constraintdef(constraint_row.oid),
                    constraint_row.condeferrable,
                    constraint_row.condeferred
             FROM pg_constraint AS constraint_row
             WHERE constraint_row.conrelid = to_regclass($1)
               AND constraint_row.conname = $2",
            &[&format!("_orna_kernel.{table}"), &constraint],
        )
        .await?
        .ok_or_else(|| failure(format!("missing {table} constraint {constraint}")))?;
    let definition: String = value(&row, 0)?;
    let deferrable: bool = value(&row, 1)?;
    let deferred: bool = value(&row, 2)?;
    require(
        definition == expected_definition
            && deferrable == expected_deferrable
            && deferred == expected_deferred,
        format!(
            "{table} constraint {constraint} is ({definition:?}, deferrable={deferrable}, deferred={deferred}); expected ({expected_definition:?}, deferrable={expected_deferrable}, deferred={expected_deferred})"
        ),
    )
}

async fn require_index(client: &Client, index: &str, expected_fragment: &str) -> TestResult<()> {
    let row = client
        .query_opt(
            "SELECT pg_get_indexdef(to_regclass($1))",
            &[&format!("_orna_kernel.{index}")],
        )
        .await?
        .ok_or_else(|| failure(format!("missing index {index}")))?;
    let definition: Option<String> = value(&row, 0)?;
    let definition = definition.ok_or_else(|| failure(format!("missing index {index}")))?;
    require(
        definition.contains(expected_fragment),
        format!("index {index} is {definition:?}; expected {expected_fragment:?}"),
    )
}

async fn require_index_shape(
    client: &Client,
    index: &str,
    relation: &str,
    expected_columns: &str,
    expected_predicate: Option<&str>,
) -> TestResult<()> {
    let row = client
        .query_opt(
            "SELECT index_class.relname,
                    index_namespace.nspname,
                    table_class.relname,
                    table_namespace.nspname,
                    pg_get_indexdef(index_row.indexrelid),
                    pg_get_expr(index_row.indpred, index_row.indrelid),
                    index_row.indisunique
             FROM pg_index AS index_row
             JOIN pg_class AS index_class
               ON index_class.oid = index_row.indexrelid
             JOIN pg_namespace AS index_namespace
               ON index_namespace.oid = index_class.relnamespace
             JOIN pg_class AS table_class
               ON table_class.oid = index_row.indrelid
             JOIN pg_namespace AS table_namespace
               ON table_namespace.oid = table_class.relnamespace
             WHERE index_row.indexrelid = to_regclass($1)",
            &[&format!("_orna_kernel.{index}")],
        )
        .await?
        .ok_or_else(|| failure(format!("missing index {index}")))?;
    let actual_index: String = value(&row, 0)?;
    let actual_index_schema: String = value(&row, 1)?;
    let actual_relation: String = value(&row, 2)?;
    let actual_relation_schema: String = value(&row, 3)?;
    let definition: String = value(&row, 4)?;
    let predicate: Option<String> = value(&row, 5)?;
    let unique: bool = value(&row, 6)?;
    let expected_definition = format!(
        "CREATE INDEX {index} ON _orna_kernel.{relation} USING btree {expected_columns}{}",
        expected_predicate
            .map(|predicate| format!(" WHERE {predicate}"))
            .unwrap_or_default()
    );
    require(
        actual_index == index
            && actual_index_schema == "_orna_kernel"
            && actual_relation == relation
            && actual_relation_schema == "_orna_kernel"
            && !unique
            && definition == expected_definition,
        format!(
            "index {index} is ({actual_index_schema}.{actual_index} on {actual_relation_schema}.{actual_relation}, unique={unique}, definition={definition:?}); expected {expected_definition:?}"
        ),
    )?;
    require(
        predicate.as_deref() == expected_predicate,
        format!("index {index} predicate is {predicate:?}; expected {expected_predicate:?}"),
    )
}

async fn require_no_foreign_key_to(
    client: &Client,
    table: &str,
    target_table: &str,
) -> TestResult<()> {
    let row = client
        .query_one(
            "SELECT count(*)
             FROM pg_constraint AS constraint_row
             WHERE constraint_row.conrelid = to_regclass($1)
               AND constraint_row.confrelid = to_regclass($2)
               AND constraint_row.contype = 'f'",
            &[&format!("_orna_kernel.{table}"), &target_table.to_owned()],
        )
        .await?;
    let count: i64 = value(&row, 0)?;
    require(
        count == 0,
        format!("{table} has {count} foreign keys to {target_table}; expected none"),
    )
}

async fn require_index_absent(client: &Client, index: &str) -> TestResult<()> {
    let row = client
        .query_one(
            "SELECT to_regclass($1)::text",
            &[&format!("_orna_kernel.{index}")],
        )
        .await?;
    let relation: Option<String> = value(&row, 0)?;
    require(relation.is_none(), format!("unexpected index {index}"))
}

async fn constraint_definition(
    client: &Client,
    table: &str,
    constraint: &str,
) -> TestResult<String> {
    let row = client
        .query_opt(
            "SELECT pg_get_constraintdef(constraint_row.oid)
             FROM pg_constraint AS constraint_row
             WHERE constraint_row.conrelid = to_regclass($1)
               AND constraint_row.conname = $2",
            &[&format!("_orna_kernel.{table}"), &constraint],
        )
        .await?
        .ok_or_else(|| failure(format!("missing {table} constraint {constraint}")))?;
    value(&row, 0)
}
