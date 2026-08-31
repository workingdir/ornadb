use std::{str::FromStr, sync::Arc};

use super::{
    MIGRATIONS, PostgresKernel, legacy_migration_checksum, migration_checksum,
    migration_checksum_matches, validated_migration_registry,
};
use super::migrations::{
    migration_sql_contains_anonymous_do_block,
    migration_sql_contains_anonymous_do_block_with_standard_conforming_strings,
};

#[test]
fn migration_registry_is_a_strict_contiguous_sequence() {
    assert_eq!(
        validated_migration_registry()
            .expect("registry is valid")
            .len(),
        47
    );
    assert_eq!(MIGRATIONS[0].version, 1);
    assert_eq!(MIGRATIONS[1].version, 2);
    assert_eq!(MIGRATIONS[2].version, 3);
    assert_eq!(MIGRATIONS[3].version, 4);
    assert_eq!(MIGRATIONS[4].version, 5);
    assert_eq!(MIGRATIONS[5].version, 6);
    assert_eq!(MIGRATIONS[6].version, 7);
    assert_eq!(MIGRATIONS[7].version, 8);
    assert_eq!(MIGRATIONS[8].version, 9);
    assert_eq!(MIGRATIONS[9].version, 10);
    assert_eq!(MIGRATIONS[10].version, 11);
    assert_eq!(MIGRATIONS[11].version, 12);
    assert_eq!(MIGRATIONS[12].version, 13);
    assert_eq!(MIGRATIONS[13].version, 14);
    assert_eq!(MIGRATIONS[14].version, 15);
    assert_eq!(MIGRATIONS[15].version, 16);
    assert_eq!(MIGRATIONS[16].version, 17);
    assert_eq!(MIGRATIONS[17].version, 18);
    assert_eq!(MIGRATIONS[18].version, 19);
    assert_eq!(MIGRATIONS[19].version, 20);
    assert_eq!(MIGRATIONS[20].version, 21);
    assert_eq!(MIGRATIONS[21].version, 22);
    assert_eq!(MIGRATIONS[22].version, 23);
    assert_eq!(MIGRATIONS[23].version, 24);
    assert_eq!(MIGRATIONS[24].version, 25);
    assert_eq!(MIGRATIONS[25].version, 26);
    assert_eq!(MIGRATIONS[26].version, 27);
    assert_eq!(MIGRATIONS[27].version, 28);
    assert_eq!(MIGRATIONS[28].version, 29);
    assert_eq!(MIGRATIONS[29].version, 30);
    assert_eq!(MIGRATIONS[30].version, 31);
    assert_eq!(MIGRATIONS[31].version, 32);
    assert_eq!(MIGRATIONS[32].version, 33);
    assert_eq!(MIGRATIONS[33].version, 34);
    assert_eq!(MIGRATIONS[34].version, 35);
    assert_eq!(MIGRATIONS[35].version, 36);
    assert_eq!(MIGRATIONS[36].version, 37);
    assert_eq!(MIGRATIONS[37].version, 38);
    assert_eq!(MIGRATIONS[38].version, 39);
    assert_eq!(MIGRATIONS[39].version, 40);
    assert_eq!(MIGRATIONS[40].version, 41);
    assert_eq!(MIGRATIONS[41].version, 42);
    assert_eq!(MIGRATIONS[42].version, 43);
    assert_eq!(MIGRATIONS[43].version, 44);
    assert_eq!(MIGRATIONS[44].version, 45);
    assert_eq!(MIGRATIONS[33].name, "resource request identity history");
    assert_eq!(MIGRATIONS[34].name, "resource audit target authorities");
    assert_eq!(MIGRATIONS[35].name, "sealed Inspector value types");
    assert_eq!(MIGRATIONS[36].name, "source apply audit");
    assert_eq!(MIGRATIONS[37].name, "source apply principal binding");
    assert_eq!(
        MIGRATIONS[38].name,
        "sealed invocation SECURITY DEFINER denial audit"
    );
    assert_eq!(
        MIGRATIONS[39].name,
        "security admin class-wide grant boundary"
    );
    assert_eq!(
        MIGRATIONS[40].name,
        "nullable resource audit nested invocation"
    );
    assert_eq!(
        MIGRATIONS[41].name,
        "non-empty security principal identities"
    );
    assert_eq!(MIGRATIONS[42].name, "source bundle unit memberships");
    assert_eq!(
        MIGRATIONS[43].name,
        "standard table and CSV executable formats"
    );
    assert_eq!(MIGRATIONS[44].name, "inspect snapshot observer context");
    assert_eq!(MIGRATIONS[45].name, "application_migrations");
    assert_eq!(MIGRATIONS[46].name, "application migration ledger baseline");
    assert_eq!(MIGRATIONS[5].name, "definition reference write evidence");
    assert_eq!(MIGRATIONS[6].name, "standard catalogue type storage");
    assert_eq!(MIGRATIONS[7].name, "resolved value type storage");
    assert_eq!(MIGRATIONS[8].name, "security decision snapshot");
    assert_eq!(MIGRATIONS[9].name, "local peer credentials");
    assert_eq!(MIGRATIONS[10].name, "protected security audit");
    assert_eq!(MIGRATIONS[11].name, "catalogue enum type storage");
    assert_eq!(MIGRATIONS[12].name, "resolved enum type storage");
    assert_eq!(MIGRATIONS[13].name, "catalogue enum reference targets");
    assert_eq!(MIGRATIONS[14].name, "catalogue record value storage");
    assert_eq!(MIGRATIONS[15].name, "resolved record value type storage");
    assert_eq!(MIGRATIONS[16].name, "record value field reference targets");
    assert_eq!(MIGRATIONS[17].name, "disjoint field reference targets");
    assert_eq!(MIGRATIONS[18].name, "standard opaque value storage");
    assert_eq!(MIGRATIONS[19].name, "standard enum record field storage");
    assert_eq!(MIGRATIONS[20].name, "nested record field targets");
    assert_eq!(MIGRATIONS[21].name, "protected invocation audit");
    assert_eq!(MIGRATIONS[22].name, "executable standard relations");
    assert_eq!(MIGRATIONS[23].name, "capability audit decisions");
    assert_eq!(MIGRATIONS[24].name, "durable user state cells");
    assert_eq!(MIGRATIONS[25].name, "user state audit decisions");
    assert_eq!(MIGRATIONS[26].name, "inspect snapshots and trace");
    assert_eq!(MIGRATIONS[27].name, "security admin privilege grants");
    assert_eq!(MIGRATIONS[28].name, "sealed system invocation authorities");
    assert_eq!(
        MIGRATIONS[29].name,
        "active roles system invocation authority"
    );
    assert_eq!(MIGRATIONS[30].name, "standard JSON executable format");
    assert_eq!(MIGRATIONS[31].name, "protected resource audit");
    assert!(MIGRATIONS[6].data_step.is_none());
    assert!(MIGRATIONS[7].data_step.is_none());
    assert!(MIGRATIONS[8].data_step.is_none());
    assert!(MIGRATIONS[9].data_step.is_none());
    assert!(MIGRATIONS[10].data_step.is_none());
    assert!(MIGRATIONS[11].data_step.is_none());
    assert!(MIGRATIONS[12].data_step.is_none());
    assert!(MIGRATIONS[13].data_step.is_none());
    assert!(MIGRATIONS[14].data_step.is_none());
    assert!(MIGRATIONS[15].data_step.is_none());
    assert!(MIGRATIONS[16].data_step.is_none());
    assert!(MIGRATIONS[17].data_step.is_none());
    assert!(MIGRATIONS[18].data_step.is_none());
    assert!(MIGRATIONS[19].data_step.is_none());
    assert!(MIGRATIONS[20].data_step.is_none());
    assert!(MIGRATIONS[21].data_step.is_none());
    assert!(MIGRATIONS[22].data_step.is_none());
    assert!(MIGRATIONS[23].data_step.is_none());
    assert!(MIGRATIONS[24].data_step.is_none());
    assert!(MIGRATIONS[25].data_step.is_none());
    assert!(MIGRATIONS[26].data_step.is_none());
    assert!(MIGRATIONS[27].data_step.is_none());
    assert!(MIGRATIONS[28].data_step.is_none());
    assert!(MIGRATIONS[29].data_step.is_none());
    assert!(MIGRATIONS[30].data_step.is_none());
    assert!(MIGRATIONS[31].data_step.is_none());
    assert!(MIGRATIONS[32].data_step.is_none());
    assert!(MIGRATIONS[33].data_step.is_none());
    assert!(MIGRATIONS[34].data_step.is_none());
    assert!(MIGRATIONS[35].data_step.is_none());
    assert!(MIGRATIONS[36].data_step.is_none());
    assert!(MIGRATIONS[39].data_step.is_none());
    assert!(MIGRATIONS[40].data_step.is_none());
    assert!(MIGRATIONS[41].data_step.is_none());
    assert!(MIGRATIONS[42].data_step.is_none());
    assert!(MIGRATIONS[43].data_step.is_none());
    assert!(MIGRATIONS[44].data_step.is_none());
    assert!(MIGRATIONS[45].data_step.is_none());
    assert!(MIGRATIONS[46].data_step.is_some());
}

#[test]
fn production_migration_registry_has_no_anonymous_do_blocks() {
    for migration in validated_migration_registry()
        .expect("production migration registry is valid")
    {
        assert!(
            !migration_sql_contains_anonymous_do_block(migration.sql),
            "migration {} ({}) contains an anonymous DO block",
            migration.version,
            migration.name
        );
        assert!(
            !migration.sql.to_ascii_lowercase().contains("plpgsql"),
            "migration {} ({}) depends on PL/pgSQL",
            migration.version,
            migration.name
        );
    }
}

#[test]
fn anonymous_do_scanner_covers_postgres_forms_and_ignores_quoted_text() {
    for sql in [
        "DO $$ BEGIN NULL; END $$;",
        "Do $body$ BEGIN NULL; END $body$;",
        "DO 'BEGIN NULL; END';",
        "DO E'BEGIN NULL; END';",
        "DO LANGUAGE sql 'SELECT 1';",
        "DO /* before */ LANGUAGE /* between */ sql $body$SELECT 1$body$;",
        "DO LANGUAGE \"sql\" E'SELECT 1';",
        "DO LANGUAGE 'sql' $$SELECT 1$$;",
        "DO $body$ SELECT 1 $body$ LANGUAGE sql;",
        "DO $body$ SELECT 'DO LANGUAGE sql $$not-a-block$$'; $body$;",
    ] {
        assert!(
            migration_sql_contains_anonymous_do_block(sql),
            "scanner missed anonymous DO form: {sql:?}"
        );
    }

    for sql in [
        "-- DO $$ BEGIN NULL; END $$;\nSELECT 1;",
        "/* DO LANGUAGE sql 'not a block'; */ SELECT 1;",
        "/* outer /* DO $$ nested comment $$ */ comment */ SELECT 1;",
        "SELECT 'DO LANGUAGE sql $$not-a-block$$';",
        "SELECT $$DO 'not-a-block'$$;",
        "SELECT E'DO LANGUAGE sql $$not-a-block$$';",
        "SELECT \"DO\";",
        r#"SELECT U&"DO $$ BEGIN NULL; END $$;" UESCAPE '!';"#,
        "SELECT do FROM _orna_kernel.example;",
    ] {
        assert!(
            !migration_sql_contains_anonymous_do_block(sql),
            "scanner treated quoted/commented text as anonymous DO: {sql:?}"
        );
    }
}

#[test]
fn anonymous_do_scanner_ends_line_comments_at_cr_and_crlf() {
    for sql in [
        "-- ignored DO text\rDO $$ BEGIN NULL; END $$;",
        "-- ignored DO text\r\nDO $$ BEGIN NULL; END $$;",
    ] {
        assert!(
            migration_sql_contains_anonymous_do_block(sql),
            "scanner missed DO after line comment terminator: {sql:?}"
        );
    }
}

#[test]
fn anonymous_do_scanner_does_not_swallow_do_after_unicode_identifier() {
    let sql = r#"SELECT U&"backslash\" UESCAPE '!'; DO $$ BEGIN NULL; END $$;"#;

    assert!(
        migration_sql_contains_anonymous_do_block(sql),
        "scanner swallowed DO after a U& identifier with alternate UESCAPE: {sql:?}"
    );
}

#[test]
fn anonymous_do_scanner_honors_standard_conforming_strings_mode() {
    let sql = r#"'escaped \'; DO $$ BEGIN NULL; END $$';"#;

    assert!(
        !migration_sql_contains_anonymous_do_block_with_standard_conforming_strings(sql, false),
        "scanner treated DO text inside a standard-conforming-off string as executable: {sql:?}"
    );
    assert!(
        migration_sql_contains_anonymous_do_block_with_standard_conforming_strings(sql, true),
        "scanner ignored a one-byte backslash before a quote with standard-conforming-strings on: {sql:?}"
    );

    let sql =
        r#"SET standard_conforming_strings = off; SELECT 'escaped \'; DO $$ BEGIN NULL; END $$;"#;

    assert!(
        migration_sql_contains_anonymous_do_block(sql),
        "an in-batch SET changed the scanner mode despite batch_execute parsing mode being fixed: {sql:?}"
    );
}

#[test]
fn legacy_migration_checksums_are_scoped_to_versions_23_29_30_and_43() {
    let expected_legacy_checksums = [
        (
            23_i64,
            [
                0x3c, 0xa6, 0x3b, 0x0c, 0xc4, 0xf2, 0x6d, 0x91, 0x30, 0x5d, 0xcc, 0xd7, 0xda, 0xdc,
                0x50, 0x64, 0xfe, 0xfc, 0xfc, 0xf0, 0x7c, 0x5b, 0x2b, 0x22, 0x6e, 0x92, 0x0b, 0xbf,
                0x88, 0xd0, 0xed, 0x89,
            ],
        ),
        (
            29_i64,
            [
                0xc4, 0x08, 0xd9, 0xfa, 0xeb, 0x56, 0x22, 0x76, 0xb9, 0x19, 0x1d, 0xbd, 0x9d, 0xc1,
                0xe4, 0xce, 0xea, 0x02, 0xd7, 0x94, 0xb9, 0x4e, 0x48, 0x14, 0xf4, 0xfa, 0xb4, 0x0e,
                0x62, 0x96, 0x76, 0xef,
            ],
        ),
        (
            30_i64,
            [
                0x90, 0x36, 0x2d, 0x04, 0x93, 0xf2, 0xbd, 0xd7, 0x9b, 0xcb, 0xf8, 0x6c, 0x23, 0x66,
                0x1d, 0xdc, 0xe5, 0xc0, 0xa4, 0x06, 0x6a, 0x79, 0xe8, 0xed, 0xcc, 0xd7, 0x1b, 0x19,
                0x3e, 0xfe, 0x81, 0x01,
            ],
        ),
        (
            43_i64,
            [
                0x02, 0x26, 0x8f, 0x8d, 0x50, 0xe4, 0x46, 0xba, 0x5e, 0x21, 0xce, 0x24, 0x18, 0x6c,
                0xc8, 0xcc, 0x0c, 0xa7, 0x6c, 0x9b, 0x45, 0xa7, 0x48, 0xb1, 0x7b, 0x25, 0x35, 0xaf,
                0xa4, 0xce, 0x04, 0x20,
            ],
        ),
    ];

    for (version, expected_checksum) in expected_legacy_checksums {
        let migration = &MIGRATIONS[usize::try_from(version - 1).expect("valid version")];
        assert_eq!(legacy_migration_checksum(version), Some(&expected_checksum));
        assert!(migration_checksum_matches(migration, &expected_checksum));
        assert!(migration_checksum_matches(
            migration,
            &migration_checksum(migration)
        ));

        let mut drifted_legacy_checksum = expected_checksum;
        drifted_legacy_checksum[0] ^= 0xff;
        assert!(!migration_checksum_matches(
            migration,
            &drifted_legacy_checksum
        ));
    }

    assert!(legacy_migration_checksum(22).is_none());
    assert!(legacy_migration_checksum(24).is_none());
    assert!(legacy_migration_checksum(28).is_none());
    assert!(legacy_migration_checksum(31).is_none());
    assert!(legacy_migration_checksum(44).is_none());
}

#[test]
fn unrelated_migration_checksum_drift_is_rejected() {
    for migration in MIGRATIONS {
        if matches!(migration.version, 23 | 29 | 30 | 43) {
            continue;
        }

        let mut drifted_checksum = migration_checksum(migration);
        drifted_checksum[0] ^= 0xff;
        assert!(!migration_checksum_matches(migration, &drifted_checksum));
    }
    for version in [23_i64, 29, 30, 43] {
        let migration = &MIGRATIONS[usize::try_from(version - 1).expect("valid version")];
        let mut drifted_current_checksum = migration_checksum(migration);
        drifted_current_checksum[0] ^= 0xff;
        assert!(!migration_checksum_matches(
            migration,
            &drifted_current_checksum
        ));
    }

    let legacy_23_checksum = legacy_migration_checksum(23).expect("version 23 compatibility");
    assert!(!migration_checksum_matches(
        &MIGRATIONS[28],
        legacy_23_checksum
    ));
}

#[test]
fn source_bundle_unit_memberships_is_the_registered_version_forty_three() {
    let migration = &MIGRATIONS[42];

    assert_eq!(migration.version, 43);
    assert_eq!(migration.name, "source bundle unit memberships");
    assert!(migration.data_step.is_none());
    assert!(
        migration
            .sql
            .contains("CREATE TABLE _orna_kernel.source_bundle_units")
    );
    assert!(
        migration
            .sql
            .contains("bundle_id bytea NOT NULL REFERENCES _orna_kernel.source_bundles(id)")
    );
    assert!(
        migration
            .sql
            .contains("source_unit_id bytea NOT NULL REFERENCES _orna_kernel.source_units(id)")
    );
    assert!(
        migration
            .sql
            .contains("ordinal bigint NOT NULL CHECK (ordinal >= 0)")
    );
    assert!(
        migration
            .sql
            .contains("PRIMARY KEY (bundle_id, source_unit_id)")
    );
    assert!(migration.sql.contains("UNIQUE (bundle_id, ordinal)"));
    assert!(migration.sql.contains(
        "INSERT INTO _orna_kernel.source_bundle_units (bundle_id, source_unit_id, ordinal)"
    ));
    assert!(migration.sql.contains("SELECT bundle_id, id, ordinal"));
    assert!(
        migration
            .sql
            .contains("REVOKE ALL ON TABLE _orna_kernel.source_bundle_units FROM PUBLIC")
    );
    assert!(
        migration
            .sql
            .contains("CREATE TEMP TABLE _orna_migration_0043_membership_guard")
    );
    assert!(
        migration
            .sql
            .contains("valid boolean NOT NULL CHECK (valid)")
    );
}

#[test]
fn non_empty_security_principal_identity_is_the_registered_version_forty_two() {
    let migration = &MIGRATIONS[41];

    assert_eq!(migration.version, 42);
    assert_eq!(migration.name, "non-empty security principal identities");
    assert!(migration.data_step.is_none());
    assert!(migration.sql.contains("security_principals_id_not_empty"));
    assert!(
        migration
            .sql
            .contains("decode('00000000000000000000000000000000', 'hex')")
    );
}

#[test]
fn nullable_resource_audit_nested_invocation_is_the_registered_version_forty_one() {
    let migration = &MIGRATIONS[40];

    assert_eq!(migration.version, 41);
    assert_eq!(migration.name, "nullable resource audit nested invocation");
    assert!(migration.data_step.is_none());
    assert!(
        migration
            .sql
            .contains("ALTER COLUMN nested_invocation_id DROP NOT NULL")
    );
    assert!(
        migration
            .sql
            .contains("nested_invocation_id IS NULL OR octet_length(nested_invocation_id) = 16")
    );
    assert!(
        migration
            .sql
            .contains("resource_audit_events_nested_invocation_presence_check")
    );
    assert!(
        migration
            .sql
            .contains("terminal_outcome IN ('failed', 'cancelled')")
    );
}

#[test]
fn security_admin_class_wide_grant_boundary_is_the_registered_version_forty() {
    let migration = &MIGRATIONS[39];

    assert_eq!(migration.version, 40);
    assert_eq!(migration.name, "security admin class-wide grant boundary");
    assert!(migration.data_step.is_none());
    assert!(
        migration
            .sql
            .contains("security_privilege_grants_security_admin_class_wide_check")
    );
    assert!(
        migration
            .sql
            .contains("CHECK (privilege_class <> 'security_admin' OR object_id = '')")
    );
}

#[test]
fn source_apply_audit_migration_admits_only_committed_candidates() {
    let migration = &MIGRATIONS[36];

    assert_eq!(migration.version, 37);
    assert_eq!(migration.name, "source apply audit");
    assert!(migration.sql.contains("event_kind = 'source_apply'"));
    assert!(
        migration
            .sql
            .contains("denial_reason = 'source_apply:committed'")
    );
    assert!(migration.sql.contains("source_revision_id IS NOT NULL"));
    assert!(migration.sql.contains("catalogue_revision_id IS NOT NULL"));
}

#[test]
fn sealed_inspect_value_migration_preserves_strict_ref_targets() {
    let migration = &MIGRATIONS[35];

    assert_eq!(migration.version, 36);
    assert_eq!(migration.name, "sealed Inspector value types");
    assert!(migration.sql.contains(
        "DROP CONSTRAINT catalogue_function_parameters_catalogue_revision_id_target_fkey"
    ));
    assert!(migration.sql.contains(
        "DROP CONSTRAINT catalogue_functions_catalogue_revision_id_return_target_ty_fkey"
    ));
    assert!(
        migration
            .sql
            .contains("ADD COLUMN target_type_id_fk bytea\n        GENERATED ALWAYS AS")
    );
    assert!(
        migration
            .sql
            .contains("ADD COLUMN return_target_type_id_fk bytea\n        GENERATED ALWAYS AS")
    );
    assert!(
        migration
            .sql
            .contains("FOREIGN KEY (catalogue_revision_id, target_type_id_fk)")
    );
    assert!(
        migration
            .sql
            .contains("FOREIGN KEY (catalogue_revision_id, return_target_type_id_fk)")
    );
    assert!(
        migration
            .sql
            .contains("target_type_id = decode('000000000000000000000000000000f3', 'hex')")
    );
    assert!(
        migration
            .sql
            .contains("return_target_type_id = decode('000000000000000000000000000000f3', 'hex')")
    );
    assert!(!migration.sql.to_ascii_lowercase().contains("plpgsql"));
    assert!(!migration.sql.contains("CREATE CONSTRAINT TRIGGER"));
    assert!(!migration.sql.contains("FOR KEY SHARE"));
}

#[test]
fn executable_standard_relations_is_the_registered_version_twenty_three() {
    let migration = &MIGRATIONS[22];

    assert_eq!(migration.version, 23);
    assert_eq!(migration.name, "executable standard relations");
    assert!(migration.data_step.is_none());
    assert!(
        migration
            .sql
            .contains("CREATE TABLE _orna_kernel.standard_catalogue_functions")
    );
    assert!(
        migration
            .sql
            .contains("CREATE TABLE _orna_kernel.standard_catalogue_function_parameters")
    );
    assert!(
        migration
            .sql
            .contains("CREATE TABLE _orna_kernel.standard_function_revisions")
    );
    assert!(
        migration
            .sql
            .contains("CREATE TABLE _orna_kernel.standard_function_artifacts")
    );
    assert!(
        migration
            .sql
            .contains("CREATE TABLE _orna_kernel.standard_definition_references")
    );
    assert!(
        migration
            .sql
            .contains("CREATE TABLE _orna_kernel.invocation_target_authorities")
    );
    assert!(
        migration
            .sql
            .contains("INSERT INTO _orna_kernel.invocation_target_authorities")
    );
    assert!(
        migration
            .sql
            .contains("DROP CONSTRAINT invocation_audit_events_target_fk")
    );
    assert!(migration.sql.contains(
        "REFERENCES _orna_kernel.invocation_target_authorities(
        catalogue_revision_id,
        function_id
    )"
    ));
}

#[test]
fn protected_invocation_audit_is_the_registered_version_twenty_two() {
    let migration = &MIGRATIONS[21];

    assert_eq!(migration.version, 22);
    assert_eq!(migration.name, "protected invocation audit");
    assert!(migration.data_step.is_none());
    assert!(
        migration
            .sql
            .contains("CREATE TABLE _orna_kernel.invocation_audit_events")
    );
    assert!(migration.sql.contains("UNIQUE (invocation_id)"));
    assert!(
        migration
            .sql
            .contains("invocation_audit_events_target_evidence_pair_check")
    );
    assert!(
        migration
            .sql
            .contains("security_audit_events_invocation_evidence_key")
    );
    assert!(
        migration
            .sql
            .contains("invocation_audit_events_security_evidence_fk")
    );
}

#[test]
fn capability_audit_decisions_is_the_registered_version_twenty_four() {
    let migration = &MIGRATIONS[23];

    assert_eq!(migration.version, 24);
    assert_eq!(migration.name, "capability audit decisions");
    assert!(migration.data_step.is_none());
    assert!(
        migration
            .sql
            .contains("ALTER TABLE _orna_kernel.security_audit_events")
    );
    assert!(
        migration
            .sql
            .contains("event_kind IN ('authentication', 'execute', 'capability')")
    );
    assert!(migration.sql.contains("denial_reason LIKE 'capability:%'"));
    assert!(migration.sql.contains("event_kind = 'capability'"));
    assert!(
        migration
            .sql
            .contains("DROP CONSTRAINT security_audit_events_shape_check")
    );
}

#[test]
fn durable_user_state_cells_is_the_registered_version_twenty_five() {
    let migration = &MIGRATIONS[24];

    assert_eq!(migration.version, 25);
    assert_eq!(migration.name, "durable user state cells");
    assert!(migration.data_step.is_none());
    assert!(
        migration
            .sql
            .contains("CREATE TABLE _orna_kernel.user_state_cells")
    );
    assert!(
        migration.sql.contains("PRIMARY KEY (")
            && migration.sql.contains("principal_id")
            && migration.sql.contains("root_function_id")
            && migration.sql.contains("root_state_profile")
            && migration.sql.contains("function_id")
            && migration.sql.contains("function_instance_key")
            && migration.sql.contains("state_slot_id")
    );
    assert!(migration.sql.contains("user_state_cells_identity_lengths"));
    assert!(migration.sql.contains("octet_length(value_type_id) = 16"));
    assert!(migration.sql.contains("user_state_cells_revision_check"));
    assert!(migration.sql.contains("revision > 0"));
    assert!(
        migration
            .sql
            .contains("updated_at timestamp with time zone")
    );
    assert!(
        migration
            .sql
            .contains("REVOKE ALL ON TABLE _orna_kernel.user_state_cells FROM PUBLIC")
    );
}

#[test]
fn security_admin_privilege_grants_is_the_registered_version_twenty_eight() {
    let migration = &MIGRATIONS[27];

    assert_eq!(migration.version, 28);
    assert_eq!(migration.name, "security admin privilege grants");
    assert!(migration.data_step.is_none());
    assert!(
        migration
            .sql
            .contains("CREATE TABLE _orna_kernel.security_privilege_grants")
    );
    assert!(
        migration
            .sql
            .contains("PRIMARY KEY (grantee_id, privilege_class, object_id)")
            && migration.sql.contains("grantee_id bytea NOT NULL")
            && migration.sql.contains("privilege_class text NOT NULL")
            && migration.sql.contains("object_id bytea NOT NULL")
    );
    // The class-wide sentinel keeps the composite key total: PostgreSQL
    // treats NULLs as distinct in unique keys, so a nullable object_id
    // would admit duplicate class-wide grants.
    assert!(
        migration
            .sql
            .contains("object_id = '' OR octet_length(object_id) = 16")
    );
    assert!(
        migration
            .sql
            .contains("privilege_class IN ('execute', 'security_admin')")
    );
    assert!(migration.sql.contains("'inspect:own-invocation'"));
    assert!(migration.sql.contains("'inspect:runtime-internals'"));
    assert!(
        migration
            .sql
            .contains("REFERENCES _orna_kernel.security_principals(id)")
    );
    assert!(
        migration
            .sql
            .contains("REVOKE ALL ON TABLE _orna_kernel.security_privilege_grants FROM PUBLIC")
    );

    // The audit extension admits the closed security_admin kind and both
    // allowed/denied shape rows.
    assert!(
            migration.sql.contains(
                "event_kind IN (\n            'authentication',\n            'execute',\n            'capability',\n            'user_state',\n            'inspect',\n            'security_admin'\n        )"
            )
        );
    assert!(
        migration
            .sql
            .contains("denial_reason LIKE 'security_admin:%'")
    );
    assert!(migration.sql.contains("event_kind = 'security_admin'"));
    assert!(
        migration
            .sql
            .contains("denial_reason NOT LIKE '%:missing-privilege'")
    );
    assert!(
        migration
            .sql
            .contains("denial_reason LIKE 'security_admin:%:missing-privilege'")
    );
    assert!(
        migration
            .sql
            .contains("DROP CONSTRAINT security_audit_events_shape_check")
    );
    assert!(
        migration
            .sql
            .contains("DROP CONSTRAINT security_audit_events_kind_check")
    );
    assert!(
        migration
            .sql
            .contains("DROP CONSTRAINT security_audit_events_denial_reason_check")
    );
}

#[tokio::test]
#[ignore = "requires an empty private PostgreSQL test database"]
async fn bootstrap_is_idempotent_under_concurrency() {
    let connection_string = std::env::var("ORNA_TEST_POSTGRES_URL")
        .expect("ORNA_TEST_POSTGRES_URL must identify the test kernel");
    let kernel = Arc::new(PostgresKernel::from_str(&connection_string).expect("config parses"));

    let first_kernel = Arc::clone(&kernel);
    let second_kernel = Arc::clone(&kernel);
    let (first, second) = tokio::join!(first_kernel.bootstrap(), second_kernel.bootstrap());
    let first = first.expect("first bootstrap succeeds");
    let second = second.expect("second bootstrap succeeds");

    assert_eq!(first, second);
    assert_eq!(kernel.bootstrap().await.expect("restart succeeds"), first);
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn bootstrap_rejects_tampered_current_catalogue_hash_without_mutation() {
    let connection_string = std::env::var("ORNA_TEST_POSTGRES_URL")
        .expect("ORNA_TEST_POSTGRES_URL must identify the test kernel");
    let kernel = PostgresKernel::from_str(&connection_string).expect("config parses");
    let active = kernel
        .bootstrap()
        .await
        .expect("initial bootstrap succeeds");
    let catalogue_id = active.catalogue().to_bytes().to_vec();

    let session = kernel.open().await.expect("snapshot session opens");
    let active_before = session
        .client
        .query_one(
            "SELECT source_revision_id, catalogue_revision_id
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true",
            &[],
        )
        .await
        .expect("active pointer is readable");
    let active_before = (
        active_before.get::<_, Vec<u8>>(0),
        active_before.get::<_, Vec<u8>>(1),
    );
    let migrations_before: Vec<(i64, Vec<u8>)> = session
        .client
        .query(
            "SELECT version, checksum
                 FROM _orna_kernel.schema_migrations
                 ORDER BY version",
            &[],
        )
        .await
        .expect("migration state is readable")
        .into_iter()
        .map(|row| (row.get::<_, i64>(0), row.get::<_, Vec<u8>>(1)))
        .collect();
    let original_hash: Vec<u8> = session
        .client
        .query_one(
            "SELECT content_hash
                 FROM _orna_kernel.catalogue_revisions
                 WHERE id = $1",
            &[&catalogue_id],
        )
        .await
        .expect("active catalogue hash is readable")
        .get(0);
    let updated = session
        .client
        .execute(
            "UPDATE _orna_kernel.catalogue_revisions
                 SET content_hash = decode(repeat('00', 32), 'hex')
                 WHERE id = $1",
            &[&catalogue_id],
        )
        .await
        .expect("catalogue hash tamper succeeds");
    assert_eq!(updated, 1);
    session.shutdown().await.expect("tamper session shuts down");

    assert!(
        kernel.bootstrap().await.is_err(),
        "bootstrap must fail closed when the current catalogue hash is tampered"
    );

    let verification = kernel.open().await.expect("verification session opens");
    let active_after = verification
        .client
        .query_one(
            "SELECT source_revision_id, catalogue_revision_id
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true",
            &[],
        )
        .await
        .expect("active pointer remains readable");
    let active_after = (
        active_after.get::<_, Vec<u8>>(0),
        active_after.get::<_, Vec<u8>>(1),
    );
    let migrations_after: Vec<(i64, Vec<u8>)> = verification
        .client
        .query(
            "SELECT version, checksum
                 FROM _orna_kernel.schema_migrations
                 ORDER BY version",
            &[],
        )
        .await
        .expect("migration state remains readable")
        .into_iter()
        .map(|row| (row.get::<_, i64>(0), row.get::<_, Vec<u8>>(1)))
        .collect();
    assert_eq!(active_after, active_before);
    assert_eq!(migrations_after, migrations_before);

    let restored = verification
        .client
        .execute(
            "UPDATE _orna_kernel.catalogue_revisions
                 SET content_hash = $1
                 WHERE id = $2",
            &[&original_hash, &catalogue_id],
        )
        .await
        .expect("catalogue hash restoration succeeds");
    assert_eq!(restored, 1);
    verification
        .shutdown()
        .await
        .expect("verification session shuts down");
}
