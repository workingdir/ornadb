use super::*;

#[test]
fn prepares_the_accepted_standard_upgrade_from_an_empty_active_revision() {
    let active = empty_active_revision();

    let upgrade = prepare_standard_upgrade(&active).expect("the standard upgrade prepares");

    assert_eq!(
        upgrade
            .checked_standard_library()
            .verified_snapshot()
            .revision(),
        STANDARD_LIBRARY_REVISION_ID
    );
    assert_eq!(
        upgrade.verified_standard_snapshot().revision(),
        STANDARD_LIBRARY_REVISION_ID
    );
    assert_eq!(
        upgrade.application_revision().expected_base(),
        active.pair()
    );
    assert_eq!(
        upgrade
            .application_revision()
            .catalogue_hash_context()
            .standard()
            .map(|snapshot| snapshot.revision()),
        Some(STANDARD_LIBRARY_REVISION_ID)
    );
}

#[test]
fn standard_upgrade_stops_before_compiler_callbacks_when_accepted_verification_fails() {
    let accepted =
        retained_standard_library_snapshot().expect("the retained standard source is valid");
    let wrong_digest = orna_core::revision::StandardLibrarySnapshot::new(
        accepted.revision(),
        accepted.digest_version(),
        accepted.source().clone(),
        accepted.language_version(),
        accepted.catalogue().clone(),
        accepted.origins().to_vec(),
        orna_core::revision::Sha256Digest::from_bytes([0; 32]),
    )
    .expect("a different standard digest remains structurally valid");
    let active = empty_active_revision();
    let checker_calls = Cell::new(0);
    let preparation_calls = Cell::new(0);

    let error = prepare_standard_upgrade_with(
        &active,
        || Ok(wrong_digest),
        verify_standard_library_snapshot,
        |snapshot| {
            checker_calls.set(checker_calls.get() + 1);
            orna_compiler::check_standard_library_source(snapshot)
        },
        |standard, active| {
            preparation_calls.set(preparation_calls.get() + 1);
            orna_compiler::prepare_checked_standard_upgrade(standard, active)
        },
    )
    .expect_err("the accepted verifier rejects the different digest");

    assert!(matches!(
        error,
        StandardUpgradeError::StandardLibrary {
            source: StandardLibraryError::AcceptedDigestMismatch { expected, actual }
        } if expected == super::super::ACCEPTED_STANDARD_LIBRARY_DIGEST
            && actual == orna_core::revision::Sha256Digest::from_bytes([0; 32])
    ));
    assert_eq!(checker_calls.get(), 0);
    assert_eq!(preparation_calls.get(), 0);
}

#[test]
fn standard_upgrade_maps_the_compiler_installed_gate_after_standard_acceptance() {
    let snapshot =
        retained_standard_library_snapshot().expect("the retained standard source is valid");
    let verified =
        verify_standard_library_snapshot(snapshot).expect("the accepted standard source verifies");
    let active = empty_version_two_active_revision(&verified);

    let error = prepare_standard_upgrade(&active).expect_err("the standard is already installed");

    assert!(matches!(
        &error,
        StandardUpgradeError::Prepare {
            source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                revision
            }
        } if *revision == STANDARD_LIBRARY_REVISION_ID
    ));
    assert_eq!(
        error.to_string(),
        format!("standard library {STANDARD_LIBRARY_REVISION_ID} is already installed")
    );
    assert_eq!(
        error.source().map(ToString::to_string),
        Some(format!(
            "standard library {STANDARD_LIBRARY_REVISION_ID} is already installed"
        ))
    );
}

#[test]
fn standard_upgrade_errors_are_transparent_and_preserve_their_fields() {
    let standard_library = StandardUpgradeError::StandardLibrary {
        source: StandardLibraryError::Unavailable,
    };
    let standard_source = StandardUpgradeError::StandardSource {
        source: orna_compiler::StandardLibraryCheckError::SourceUnitCount { actual: 9 },
    };
    let preparation = StandardUpgradeError::Prepare {
        source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
            revision: STANDARD_LIBRARY_REVISION_ID,
        },
    };

    assert!(matches!(
        standard_library,
        StandardUpgradeError::StandardLibrary {
            source: StandardLibraryError::Unavailable
        }
    ));
    assert!(matches!(
        standard_source,
        StandardUpgradeError::StandardSource {
            source: orna_compiler::StandardLibraryCheckError::SourceUnitCount { actual: 9 }
        }
    ));
    assert!(matches!(
        preparation,
        StandardUpgradeError::Prepare {
            source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                revision
            }
        } if revision == STANDARD_LIBRARY_REVISION_ID
    ));

    for (error, expected) in [
        (
            &standard_library,
            "the standard library is not installed".to_owned(),
        ),
        (
            &standard_source,
            "the verified standard library has 9 source units, expected exactly one".to_owned(),
        ),
        (
            &preparation,
            format!("standard library {STANDARD_LIBRARY_REVISION_ID} is already installed"),
        ),
    ] {
        assert_eq!(error.to_string(), expected);
        assert_eq!(error.source().map(ToString::to_string), Some(expected));
    }
}

#[test]
fn manifest_exposes_the_reserved_staging_identities() {
    let manifest = standard_library_manifest().expect("the accepted manifest must be valid");
    let cloned = manifest.clone();

    assert_eq!(STANDARD_LIBRARY_VERSION_IDENTITY, "orna.std/1");
    assert_eq!(LANGUAGE_VERSION_IDENTITY, "orna.language/1");
    assert_eq!(SOURCE_LOGICAL_PATH, "std/types.orna");
    assert_eq!(
        manifest.standard_library_version(),
        STANDARD_LIBRARY_VERSION_IDENTITY
    );
    assert_eq!(
        manifest.standard_library_revision(),
        STANDARD_LIBRARY_REVISION_ID
    );
    assert_eq!(manifest.language_version(), LANGUAGE_VERSION_IDENTITY);
    assert_eq!(manifest.source_bundle(), STANDARD_SOURCE_BUNDLE_ID);
    assert_eq!(manifest.source_revision(), STANDARD_SOURCE_REVISION_ID);
    assert_eq!(manifest.source_unit(), STANDARD_SOURCE_UNIT_ID);
    assert_eq!(manifest.source_logical_path(), SOURCE_LOGICAL_PATH);
    assert_eq!(
        manifest.catalogue().revision(),
        STANDARD_CATALOGUE_REVISION_ID
    );
    assert_eq!(manifest.catalogue().schemas().len(), 2);
    assert_eq!(manifest.catalogue().schemas()[0].id(), STD_SCHEMA_ID);
    assert_eq!(manifest.catalogue().schemas()[1].id(), STD_TYPES_SCHEMA_ID);
    assert_eq!(
        cloned.catalogue().revision(),
        STANDARD_CATALOGUE_REVISION_ID
    );
    assert_eq!(
        STANDARD_LIBRARY_REVISION_ID.to_bytes(),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
    );
    assert_eq!(
        STANDARD_CATALOGUE_REVISION_ID.to_bytes(),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
    );
    assert_eq!(
        STANDARD_SOURCE_BUNDLE_ID.to_bytes(),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
    );
    assert_eq!(
        STANDARD_SOURCE_REVISION_ID.to_bytes(),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
    );
    assert_eq!(
        STANDARD_SOURCE_UNIT_ID.to_bytes(),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
    );
    assert_eq!(
        STD_SCHEMA_ID.to_bytes(),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
    );
    assert_eq!(
        STD_TYPES_SCHEMA_ID.to_bytes(),
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]
    );
}

#[test]
fn retains_the_canonical_standard_source_as_an_unverified_snapshot() {
    let snapshot =
        retained_standard_library_snapshot().expect("the retained standard source is valid");

    assert_eq!(snapshot.revision(), STANDARD_LIBRARY_REVISION_ID);
    assert_eq!(
        snapshot.digest_version(),
        orna_core::revision::StandardLibraryDigestVersion::Version1
    );
    assert_eq!(snapshot.language_version(), LANGUAGE_VERSION_IDENTITY);
    assert_eq!(
        snapshot.catalogue().revision(),
        STANDARD_CATALOGUE_REVISION_ID
    );
    assert_eq!(snapshot.source().id(), STANDARD_SOURCE_REVISION_ID);
    assert_eq!(snapshot.source().bundle(), STANDARD_SOURCE_BUNDLE_ID);
    assert_eq!(snapshot.source().parent(), None);
    assert_eq!(snapshot.source().units().len(), 1);
    assert_eq!(snapshot.source().units()[0].id(), STANDARD_SOURCE_UNIT_ID);
    assert_eq!(snapshot.source().units()[0].ordinal(), 0);
    assert_eq!(
        snapshot.source().units()[0].logical_path(),
        SOURCE_LOGICAL_PATH
    );
}

#[test]
fn retained_source_has_the_exact_literal_bytes_parse_and_hash_goldens() {
    let snapshot =
        retained_standard_library_snapshot().expect("the retained standard source is valid");
    let source = snapshot.source().units()[0].content();
    let parsed = orna_syntax::parse(source);

    assert_eq!(source, EXPECTED_RETAINED_STANDARD_SOURCE);
    assert_eq!(source.len(), 3463);
    assert!(source.is_ascii());
    assert!(!source.as_bytes().starts_with(&[0xef, 0xbb, 0xbf]));
    assert!(!source.contains('\r'));
    assert!(source.ends_with('\n'));
    assert!(!source[..source.len() - 1].ends_with('\n'));
    assert_eq!(source.matches(';').count(), 47);
    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), source);
    assert_eq!(parsed.schemas().len(), 2);
    assert_eq!(parsed.primitive_value_types().len(), 13);
    assert_eq!(parsed.opaque_value_types().len(), 1);
    assert_eq!(parsed.type_exports().len(), 31);
    assert!(parsed.object_types().is_empty());
    assert!(parsed.field_renames().is_empty());
    assert!(parsed.server_functions().is_empty());
    assert!(parsed.client_functions().is_empty());
    assert_eq!(
        snapshot.source().units()[0].content_hash().to_bytes(),
        [
            0x5d, 0x53, 0x60, 0x01, 0xab, 0xc7, 0x54, 0xcf, 0x2c, 0xde, 0x9f, 0xf4, 0xed, 0x50,
            0xb2, 0x2d, 0xe8, 0xbb, 0x70, 0x04, 0x0a, 0x69, 0x1b, 0xc2, 0xec, 0x50, 0xbd, 0x6c,
            0x65, 0xe5, 0x25, 0xf4,
        ]
    );
    assert_eq!(
        snapshot.source().bundle_hash().to_bytes(),
        [
            0xd8, 0x0e, 0x8f, 0x73, 0x88, 0x78, 0x2d, 0x73, 0x0e, 0x4d, 0x6c, 0x5a, 0x6f, 0xcd,
            0x4a, 0x56, 0x42, 0xa4, 0x81, 0xcb, 0x65, 0x6d, 0x6e, 0x5f, 0xca, 0x35, 0x9a, 0x69,
            0xf3, 0x72, 0x63, 0xeb,
        ]
    );
    assert_eq!(
        snapshot.source().revision_hash().to_bytes(),
        [
            0x40, 0x0e, 0xb4, 0x35, 0x5d, 0xa2, 0x8f, 0x41, 0xf4, 0xd4, 0xae, 0x8c, 0x06, 0x21,
            0x24, 0x89, 0xbe, 0x60, 0xf6, 0xd8, 0x7c, 0x6d, 0x8e, 0xf3, 0x0c, 0x29, 0x1c, 0xc8,
            0x3b, 0x2c, 0xfb, 0x6b,
        ]
    );
    assert_eq!(
        snapshot.digest().to_bytes(),
        [
            0xbe, 0x61, 0x9c, 0xaa, 0xf6, 0xb2, 0x0b, 0xb7, 0xf8, 0xbc, 0x8d, 0xf9, 0x56, 0xd4,
            0x89, 0xad, 0xe4, 0x9b, 0xc8, 0xdf, 0xe0, 0x3c, 0xd6, 0xd9, 0x64, 0x70, 0x5b, 0x30,
            0x23, 0x5b, 0x08, 0x1d,
        ]
    );
}

#[test]
fn retained_source_rejects_quoted_and_reordered_manifest_facts() {
    let quoted =
        EXPECTED_RETAINED_STANDARD_SOURCE.replacen("std.types.BOOLEAN", "std.types.\"BOOLEAN\"", 1);
    let reordered = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
        "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;\nEXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL;",
        "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL;\nEXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
        1,
    );
    let changed_schema = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
        "CREATE SCHEMA std.types;",
        "CREATE SCHEMA std.other;",
        1,
    );
    let changed_contract = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
        "orna.kernel.value.boolean@1",
        "orna.kernel.value.boolean@2",
        1,
    );
    let changed_persistence =
        EXPECTED_RETAINED_STANDARD_SOURCE.replacen("    PERSISTABLE;", "    TRANSIENT;", 1);
    let changed_qualified_target = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
        "EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;",
        "EXPORT TYPE std.types.BOOLEAN AS std.BOOL;",
        1,
    );
    let changed_prelude_source = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
        "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
        "EXPORT TYPE std.BOOL TO PRELUDE AS BOOLEAN;",
        1,
    );
    let changed_prelude_target = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
        "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL;",
        "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
        1,
    );

    for source in [
        quoted,
        reordered,
        changed_schema,
        changed_contract,
        changed_persistence,
        changed_qualified_target,
        changed_prelude_source,
        changed_prelude_target,
    ] {
        assert!(matches!(
            retained_standard_library_snapshot_from_source(&source),
            Err(super::super::StandardLibraryError::RetainedSourceMismatch)
        ));
    }
}

#[test]
fn retained_source_rejects_a_catalogue_value_kind_mismatch() {
    // Work ADR 0016 requires each standard scalar source declaration to
    // reconcile with a primitive catalogue value definition, not merely a
    // matching name, contract, and persistence policy.
    let manifest = super::super::standard_library_manifest().expect("the manifest is valid");
    let mut value_types = manifest.catalogue().value_types().to_vec();
    let void_index = value_types
        .iter()
        .position(|definition| definition.id() == super::super::VOID_TYPE_ID)
        .expect("the VOID definition is retained");
    let void = &value_types[void_index];
    let void_id = void.id();
    let void_name = void.name().clone();
    let void_contract = void.representation_contract().to_owned();
    value_types[void_index] = ValueTypeDefinition::opaque(void_id, void_name, void_contract);
    let catalogue = CatalogueSnapshot::new_with_types(
        manifest.catalogue().revision(),
        manifest.catalogue().schemas().to_vec(),
        Vec::new(),
        value_types,
        manifest.catalogue().type_bindings().to_vec(),
    )
    .expect("the kind-tampered catalogue remains structurally valid");
    let tampered_manifest = super::super::StandardLibraryManifest { catalogue };

    assert!(matches!(
        super::super::reconcile_retained_source_with_unit(
            EXPECTED_RETAINED_STANDARD_SOURCE,
            &tampered_manifest,
            super::super::STANDARD_SOURCE_UNIT_ID,
        ),
        Err(super::super::StandardLibraryError::RetainedSourceMismatch)
    ));
}

#[test]
fn retained_source_rejects_a_missing_complete_declaration() {
    let missing = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
        "CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE\n    KERNEL CONTRACT 'orna.kernel.value.boolean@1'\n    IMMUTABLE\n    PERSISTABLE;\n\n",
        "",
        1,
    );

    assert!(matches!(
        retained_standard_library_snapshot_from_source(&missing),
        Err(super::super::StandardLibraryError::RetainedSourceMismatch)
    ));
}

#[test]
fn retained_source_rejects_an_extra_declaration_or_category() {
    let extra_schema = format!("{EXPECTED_RETAINED_STANDARD_SOURCE}CREATE SCHEMA std.extra;\n");
    let extra_field_rename = format!(
        "{EXPECTED_RETAINED_STANDARD_SOURCE}ALTER TYPE std.types.BOOLEAN RENAME FIELD old TO new;\n"
    );

    for source in [extra_schema, extra_field_rename] {
        assert!(matches!(
            retained_standard_library_snapshot_from_source(&source),
            Err(super::super::StandardLibraryError::RetainedSourceMismatch)
        ));
    }
}

#[test]
fn retained_source_rejects_a_valid_cross_type_export_association() {
    let crossed = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
        "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
        "EXPORT TYPE std.INTEGER TO PRELUDE AS BOOLEAN;",
        1,
    );
    let parsed = orna_syntax::parse(&crossed);

    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.type_exports().len(), 31);
    assert!(matches!(
        retained_standard_library_snapshot_from_source(&crossed),
        Err(super::super::StandardLibraryError::RetainedSourceMismatch)
    ));
}

#[test]
fn retained_source_rejects_duplicate_schema_and_prelude_declarations() {
    let duplicate_schema = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
        "CREATE SCHEMA std.types;",
        "CREATE SCHEMA std;",
        1,
    );
    let duplicate_prelude = EXPECTED_RETAINED_STANDARD_SOURCE.replacen(
        "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL;",
        "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
        1,
    );

    for source in [duplicate_schema, duplicate_prelude] {
        let parsed = orna_syntax::parse(&source);
        assert!(parsed.diagnostics().is_empty());
        assert!(matches!(
            retained_standard_library_snapshot_from_source(&source),
            Err(super::super::StandardLibraryError::RetainedSourceMismatch)
        ));
    }
}

#[test]
fn retained_source_assigns_every_complete_declaration_its_exact_origin() {
    let snapshot =
        retained_standard_library_snapshot().expect("the retained standard source is valid");
    let source = snapshot.source().units()[0].content();
    let expected_spans = [
        (0, 18),
        (19, 43),
        (45, 174),
        (176, 221),
        (223, 269),
        (270, 313),
        (315, 444),
        (446, 491),
        (493, 539),
        (540, 582),
        (584, 711),
        (713, 756),
        (758, 802),
        (804, 929),
        (931, 972),
        (974, 1016),
        (1018, 1147),
        (1149, 1194),
        (1196, 1242),
        (1244, 1403),
        (1405, 1480),
        (1482, 1558),
        (1559, 1617),
        (1619, 1772),
        (1774, 1843),
        (1845, 1915),
        (1916, 1972),
        (1974, 2097),
        (2099, 2138),
        (2140, 2180),
        (2182, 2305),
        (2307, 2346),
        (2348, 2388),
        (2390, 2513),
        (2515, 2554),
        (2556, 2596),
        (2598, 2731),
        (2733, 2782),
        (2784, 2834),
        (2836, 2967),
        (2969, 3016),
        (3018, 3066),
        (3068, 3189),
        (3191, 3230),
        (3232, 3272),
        (3274, 3405),
        (3407, 3462),
    ];
    let expected_source_unit =
        orna_core::SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    let expected_identities = [
        DefinitionIdentity::Schema(orna_core::SchemaId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
        ])),
        DefinitionIdentity::Schema(orna_core::SchemaId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1, 0xdd,
            0x4d, 0x31,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0xfc, 0x31, 0x05, 0xaf, 0xaf, 0x25, 0x20, 0xd7, 0xc7, 0x7c, 0xdd, 0x6b, 0x0e, 0xf8,
            0x15, 0xaa,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x7b, 0x20, 0xca, 0xb3, 0x61, 0x23, 0x35, 0x61, 0x03, 0xad, 0xab, 0x48, 0x61, 0x11,
            0x0c, 0xad,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0xf9, 0x2a, 0x68, 0x3c, 0xa4, 0x2b, 0x48, 0x2f, 0x77, 0x7a, 0x79, 0x86, 0xb2, 0xdf,
            0x25, 0x93,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x19, 0x40, 0x9c, 0x7b, 0x37, 0x81, 0x68, 0xf8, 0x30, 0x0b, 0x44, 0x0c, 0xaf, 0x18,
            0x57, 0x78,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x97, 0x0a, 0xa4, 0x1b, 0xb9, 0xb1, 0x99, 0xa3, 0xcb, 0xa3, 0x46, 0x8c, 0x9e, 0x7c,
            0x58, 0x89,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x08, 0x52, 0xa1, 0xcb, 0xbe, 0x1c, 0x5b, 0x78, 0xb4, 0xfa, 0xd2, 0x9e, 0xed, 0x5b,
            0x0d, 0x1e,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0xa0, 0x50, 0x06, 0x28, 0xc9, 0x77, 0x06, 0xb2, 0xbd, 0x8f, 0x29, 0xf7, 0x8b, 0xaa,
            0x5e, 0x88,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x30, 0x1f, 0x53, 0xba, 0x6e, 0xe1, 0xea, 0xd1, 0xe3, 0x18, 0x6b, 0x6b, 0x71, 0x9e,
            0xfc, 0xb5,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x31, 0x03, 0xa7, 0xca, 0xfc, 0xc6, 0x3e, 0xd7, 0x2a, 0x10, 0x58, 0x00, 0x87, 0x97,
            0xb5, 0xe6,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x28, 0x5c, 0x9a, 0x60, 0x1c, 0x08, 0x5b, 0xfa, 0xe9, 0x48, 0x5c, 0x9c, 0xb8, 0x6b,
            0x45, 0xf9,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0xdf, 0x8e, 0x7b, 0x74, 0x41, 0xca, 0xe1, 0xf8, 0xfd, 0x56, 0xd8, 0x83, 0xa3, 0x10,
            0x6e, 0xd5,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x28, 0x67, 0x4f, 0xd2, 0x8e, 0x8a, 0x68, 0x08, 0x1e, 0x26, 0x3f, 0xb3, 0x1b, 0xc2,
            0xd8, 0x70,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0xf6, 0xd0, 0xd3, 0xb6, 0x31, 0x1b, 0x6b, 0xdc, 0xe6, 0x01, 0xd3, 0xcf, 0xc3, 0xa6,
            0x89, 0x1a,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x72, 0x0f, 0xf6, 0x30, 0x3e, 0xf0, 0x01, 0x8c, 0x81, 0xd2, 0xa6, 0x73, 0x99, 0xf0,
            0xdb, 0xc2,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0xa9, 0x31, 0x64, 0x64, 0xe3, 0x52, 0xb5, 0x6a, 0x56, 0xa1, 0x4b, 0x38, 0x4c, 0x7d,
            0x81, 0x34,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x15, 0x24, 0xb4, 0xca, 0x63, 0xbc, 0xe7, 0xf8, 0x9b, 0x24, 0xba, 0xf1, 0x8d, 0x33,
            0xaf, 0xbf,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x84, 0xe0, 0x46, 0xbd, 0x87, 0xde, 0xc7, 0x0a, 0x1b, 0x73, 0x13, 0xae, 0x51, 0xb6,
            0x9d, 0xb7,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x89, 0xea, 0x05, 0xd7, 0x14, 0xdc, 0x5d, 0x2f, 0x0a, 0x8e, 0x09, 0xf7, 0x5f, 0x31,
            0x66, 0x00,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x73, 0xda, 0x8e, 0x2f, 0xac, 0xe9, 0x8a, 0x17, 0xa6, 0x63, 0xec, 0x97, 0xe6, 0x7c,
            0x79, 0x7f,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0xf9, 0x7c, 0x60, 0xa7, 0x50, 0x6b, 0x9e, 0x79, 0xa8, 0xa8, 0xd7, 0x84, 0xa1, 0x71,
            0xf7, 0xac,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0xf3, 0x2c, 0xab, 0x58, 0xdb, 0xdf, 0x3d, 0xc6, 0xfe, 0x7c, 0xb1, 0x74, 0x8e, 0x1f,
            0x93, 0x56,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x15, 0x11, 0xd9, 0x2f, 0x12, 0xc3, 0x4c, 0x1b, 0x0c, 0x4c, 0x53, 0x26, 0xa8, 0xa0,
            0x34, 0x8d,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x8b, 0xd8, 0x9d, 0x33, 0x32, 0x97, 0x8f, 0x32, 0xa7, 0xd0, 0xe1, 0xd6, 0x72, 0xd2,
            0x33, 0xd4,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 11,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x47, 0xb0, 0x08, 0xa2, 0xdc, 0x0b, 0x20, 0xd1, 0x2b, 0x3e, 0x68, 0x9a, 0x30, 0xfc,
            0xff, 0x04,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x84, 0x1f, 0xc4, 0xfb, 0x35, 0x7f, 0xf8, 0xc3, 0x10, 0x74, 0x4b, 0xfc, 0x97, 0x9c,
            0x8a, 0xa1,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 12,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x36, 0x29, 0x37, 0xf6, 0x5e, 0x81, 0xf4, 0xa9, 0x45, 0x85, 0x47, 0xb4, 0xeb, 0x62,
            0x14, 0x9a,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x6b, 0xdd, 0xb3, 0xa5, 0xf1, 0x4a, 0xc6, 0xf8, 0x42, 0x57, 0x35, 0xb8, 0x80, 0x2d,
            0xdc, 0x37,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 13,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x82, 0xae, 0x45, 0x04, 0x07, 0xcf, 0xfa, 0xa6, 0x87, 0xe8, 0x1f, 0xa7, 0xdc, 0xbf,
            0x94, 0x0f,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x56, 0xc5, 0x04, 0xe2, 0xf8, 0x07, 0xce, 0x24, 0xd3, 0x61, 0x11, 0xe6, 0x4a, 0x01,
            0x73, 0xfb,
        ])),
        DefinitionIdentity::ValueType(orna_core::TypeId::from_bytes([
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 14,
        ])),
        DefinitionIdentity::TypeBinding(orna_core::TypeBindingId::from_bytes([
            0x4d, 0xab, 0x42, 0x83, 0x03, 0x1f, 0xcd, 0x81, 0xb5, 0x8d, 0x09, 0xd8, 0x87, 0x63,
            0x46, 0xae,
        ])),
    ];

    assert_eq!(expected_spans.len(), 47);
    assert_eq!(expected_identities.len(), 47);
    assert_eq!(snapshot.origins().len(), 47);
    for ((origin, identity), (start, end)) in snapshot
        .origins()
        .iter()
        .zip(expected_identities)
        .zip(expected_spans)
    {
        assert_eq!(origin.identity(), identity);
        assert_eq!(origin.source().source_unit(), expected_source_unit);
        assert_eq!(origin.source().byte_start(), start);
        assert_eq!(origin.source().byte_end(), end);
        assert_eq!(
            &source[start as usize..end as usize],
            &EXPECTED_RETAINED_STANDARD_SOURCE[start as usize..end as usize]
        );
    }
    assert_eq!(snapshot.origins()[0].source().byte_start(), 0);
    assert_eq!(snapshot.origins()[1].source().byte_start(), 19);
    assert_eq!(snapshot.origins()[46].source().byte_end(), 3462);
    assert_eq!(&source[3462..], "\n");
}

#[test]
fn standard_library_error_preserves_its_exact_public_contract() {
    let unavailable = StandardLibraryError::Unavailable;
    let manifest = super::super::StandardLibraryError::Manifest {
        source: StandardLibraryManifestError::TypeBindingCountMismatch {
            expected: 30,
            actual: 29,
        },
    };
    let retained = super::super::StandardLibraryError::RetainedSourceMismatch;
    let revision = super::super::StandardLibraryError::Revision {
        source: orna_core::revision::RevisionInvariantError::EmptyLogicalPath {
            source_unit: STANDARD_SOURCE_UNIT_ID,
        },
    };
    let canonical = super::super::StandardLibraryError::CanonicalHash {
        source: orna_core::canonical_hash::CanonicalHashError::SourceContentHashMismatch {
            source_unit: STANDARD_SOURCE_UNIT_ID,
        },
    };
    let catalogue = super::super::StandardLibraryError::CatalogueIdentityMismatch {
        expected: STANDARD_CATALOGUE_REVISION_ID,
        actual: orna_core::CatalogueRevisionId::from_bytes([2; 16]),
    };
    let digest = super::super::StandardLibraryError::AcceptedDigestMismatch {
        expected: orna_core::revision::Sha256Digest::from_bytes([3; 32]),
        actual: orna_core::revision::Sha256Digest::from_bytes([4; 32]),
    };

    assert_eq!(
        manifest.to_string(),
        "the standard library manifest is invalid: the standard library manifest has 29 type bindings, expected 30"
    );
    assert_eq!(
        unavailable.to_string(),
        "the standard library is not installed"
    );
    assert_eq!(
        retained.to_string(),
        "the retained standard library source does not match its manifest"
    );
    assert_eq!(
        revision.to_string(),
        "the retained standard library revision is invalid: stored source unit has an empty logical path"
    );
    assert_eq!(
        canonical.to_string(),
        "the standard library canonical hashes are invalid: stored source content hash differs from exact content"
    );
    assert_eq!(
        catalogue.to_string(),
        "the standard library catalogue identity does not match the reserved identity"
    );
    assert_eq!(
        digest.to_string(),
        "the standard library digest does not match the hard-coded accepted digest"
    );
    assert_eq!(
        manifest.source().map(ToString::to_string),
        Some("the standard library manifest has 29 type bindings, expected 30".to_owned())
    );
    assert!(unavailable.source().is_none());
    assert!(retained.source().is_none());
    assert_eq!(
        revision.source().map(ToString::to_string),
        Some("stored source unit has an empty logical path".to_owned())
    );
    assert_eq!(
        canonical.source().map(ToString::to_string),
        Some("stored source content hash differs from exact content".to_owned())
    );
    assert!(catalogue.source().is_none());
    assert!(digest.source().is_none());
}

#[test]
fn verification_enforces_catalogue_digest_and_core_gates_in_order() {
    let accepted =
        retained_standard_library_snapshot().expect("the retained standard source is valid");
    let alternate_catalogue_id = orna_core::CatalogueRevisionId::from_bytes([2; 16]);
    let alternate_catalogue = orna_core::catalogue::CatalogueSnapshot::new_with_types(
        alternate_catalogue_id,
        accepted.catalogue().schemas().to_vec(),
        Vec::new(),
        accepted.catalogue().value_types().to_vec(),
        accepted.catalogue().type_bindings().to_vec(),
    )
    .expect("alternate catalogue remains structurally valid");
    let wrong_catalogue = orna_core::revision::StandardLibrarySnapshot::new(
        accepted.revision(),
        accepted.digest_version(),
        accepted.source().clone(),
        accepted.language_version(),
        alternate_catalogue,
        accepted.origins().to_vec(),
        orna_core::revision::Sha256Digest::from_bytes([0; 32]),
    )
    .expect("alternate snapshot remains structurally valid");
    let wrong_digest = orna_core::revision::StandardLibrarySnapshot::new(
        accepted.revision(),
        accepted.digest_version(),
        accepted.source().clone(),
        accepted.language_version(),
        accepted.catalogue().clone(),
        accepted.origins().to_vec(),
        orna_core::revision::Sha256Digest::from_bytes([0; 32]),
    )
    .expect("different digest does not affect structural validation");
    let invalid_source = orna_core::revision::StoredSourceRevision::new(
        accepted.source().bundle(),
        accepted.source().id(),
        accepted.source().parent(),
        accepted.source().units().to_vec(),
        orna_core::revision::Sha256Digest::from_bytes([0; 32]),
        accepted.source().revision_hash(),
    )
    .expect("incorrect source hash does not affect structural validation");
    let wrong_core = orna_core::revision::StandardLibrarySnapshot::new(
        accepted.revision(),
        accepted.digest_version(),
        invalid_source,
        accepted.language_version(),
        accepted.catalogue().clone(),
        accepted.origins().to_vec(),
        accepted.digest(),
    )
    .expect("incorrect source hash does not affect structural validation");

    assert!(matches!(
        verify_standard_library_snapshot(wrong_catalogue),
        Err(super::super::StandardLibraryError::CatalogueIdentityMismatch { expected, actual })
            if expected == STANDARD_CATALOGUE_REVISION_ID && actual == alternate_catalogue_id
    ));
    assert!(matches!(
        verify_standard_library_snapshot(wrong_digest),
        Err(super::super::StandardLibraryError::AcceptedDigestMismatch { .. })
    ));
    assert!(matches!(
        verify_standard_library_snapshot(wrong_core),
        Err(super::super::StandardLibraryError::CanonicalHash { .. })
    ));
    let verified = verify_standard_library_snapshot(accepted)
        .expect("the accepted retained snapshot must grant authority");
    assert_eq!(verified.revision(), STANDARD_LIBRARY_REVISION_ID);
}

#[test]
fn verification_rejects_a_core_accepted_self_consistent_non_golden_snapshot() {
    let accepted =
        retained_standard_library_snapshot().expect("the retained standard source is valid");
    let non_golden = orna_core::revision::StandardLibrarySnapshot::new(
        accepted.revision(),
        accepted.digest_version(),
        accepted.source().clone(),
        "orna.language/2",
        accepted.catalogue().clone(),
        accepted.origins().to_vec(),
        orna_core::revision::Sha256Digest::from_bytes([
            0x19, 0x65, 0xe6, 0xcb, 0xeb, 0x68, 0x77, 0xa6, 0xab, 0xea, 0x13, 0x14, 0xe9, 0x12,
            0xbe, 0xc5, 0xef, 0x12, 0xa9, 0x5b, 0xd3, 0x57, 0xdc, 0xee, 0xc9, 0xef, 0xb4, 0x54,
            0xf8, 0x4a, 0x98, 0xb2,
        ]),
    )
    .expect("the alternate standard snapshot is structurally valid");

    let core_verified =
        orna_core::canonical_hash::verify_standard_library_snapshot(non_golden.clone())
            .expect("the alternate standard is canonically self-consistent");
    assert_eq!(
        registered_opaque_codecs(&core_verified).unwrap_err(),
        super::super::RegisteredOpaqueCodecsError::UnacceptedStandardSnapshot
    );
    assert!(matches!(
        verify_standard_library_snapshot(non_golden),
        Err(super::super::StandardLibraryError::AcceptedDigestMismatch { expected, actual })
            if expected == orna_core::revision::Sha256Digest::from_bytes([
                0xbe, 0x61, 0x9c, 0xaa, 0xf6, 0xb2, 0x0b, 0xb7, 0xf8, 0xbc, 0x8d, 0xf9, 0x56,
                0xd4, 0x89, 0xad, 0xe4, 0x9b, 0xc8, 0xdf, 0xe0, 0x3c, 0xd6, 0xd9, 0x64, 0x70,
                0x5b, 0x30, 0x23, 0x5b, 0x08, 0x1d,
            ])
            && actual == orna_core::revision::Sha256Digest::from_bytes([
                0x19, 0x65, 0xe6, 0xcb, 0xeb, 0x68, 0x77, 0xa6, 0xab, 0xea, 0x13, 0x14, 0xe9,
                0x12, 0xbe, 0xc5, 0xef, 0x12, 0xa9, 0x5b, 0xd3, 0x57, 0xdc, 0xee, 0xc9, 0xef,
                0xb4, 0x54, 0xf8, 0x4a, 0x98, 0xb2,
            ])
    ));
}

#[test]
fn registered_opaque_codec_is_bound_to_the_accepted_active_standard() {
    let verified = verify_standard_library_snapshot(
        retained_standard_library_snapshot().expect("the retained standard source is valid"),
    )
    .expect("the accepted standard snapshot verifies");
    let registry = registered_opaque_codecs(&verified)
        .expect("the checked-in opaque codec matches the accepted standard");
    let active = empty_version_two_active_revision(&verified);
    let payload = [0xa5; 16];

    let value = OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, payload)
        .expect("the exact registered payload is accepted");

    assert_eq!(value.opaque_type(), OPAQUE_TOKEN_TYPE_ID);
    assert_eq!(value.canonical_payload(), payload);
}

#[test]
fn manifest_contains_the_exact_standard_value_type_facts() {
    let manifest = standard_library_manifest().expect("the accepted manifest must be valid");
    let catalogue = manifest.catalogue();
    let expected = [
        (
            BOOLEAN_TYPE_ID,
            "std.types.boolean",
            "orna.kernel.value.boolean@1",
            ValueTypeKind::Primitive,
            ValueTypePersistence::Persistable,
        ),
        (
            INTEGER_TYPE_ID,
            "std.types.integer",
            "orna.kernel.value.integer@1",
            ValueTypeKind::Primitive,
            ValueTypePersistence::Persistable,
        ),
        (
            BIGINT_TYPE_ID,
            "std.types.bigint",
            "orna.kernel.value.bigint@1",
            ValueTypeKind::Primitive,
            ValueTypePersistence::Persistable,
        ),
        (
            FLOAT_TYPE_ID,
            "std.types.float",
            "orna.kernel.value.float@1",
            ValueTypeKind::Primitive,
            ValueTypePersistence::Persistable,
        ),
        (
            DECIMAL_TYPE_ID,
            "std.types.decimal",
            "orna.kernel.value.decimal@1",
            ValueTypeKind::Primitive,
            ValueTypePersistence::Persistable,
        ),
        (
            CHARACTER_LARGE_OBJECT_TYPE_ID,
            "std.types.character_large_object",
            "orna.kernel.value.character-large-object@1",
            ValueTypeKind::Primitive,
            ValueTypePersistence::Persistable,
        ),
        (
            BINARY_LARGE_OBJECT_TYPE_ID,
            "std.types.binary_large_object",
            "orna.kernel.value.binary-large-object@1",
            ValueTypeKind::Primitive,
            ValueTypePersistence::Persistable,
        ),
        (
            UUID_TYPE_ID,
            "std.types.uuid",
            "orna.kernel.value.uuid@1",
            ValueTypeKind::Primitive,
            ValueTypePersistence::Persistable,
        ),
        (
            DATE_TYPE_ID,
            "std.types.date",
            "orna.kernel.value.date@1",
            ValueTypeKind::Primitive,
            ValueTypePersistence::Persistable,
        ),
        (
            TIME_TYPE_ID,
            "std.types.time",
            "orna.kernel.value.time@1",
            ValueTypeKind::Primitive,
            ValueTypePersistence::Persistable,
        ),
        (
            TIMESTAMP_TYPE_ID,
            "std.types.timestamp",
            "orna.kernel.value.timestamp@1",
            ValueTypeKind::Primitive,
            ValueTypePersistence::Persistable,
        ),
        (
            DURATION_TYPE_ID,
            "std.types.duration",
            "orna.kernel.value.duration@1",
            ValueTypeKind::Primitive,
            ValueTypePersistence::Persistable,
        ),
        (
            VOID_TYPE_ID,
            "std.types.void",
            "orna.kernel.value.void@1",
            ValueTypeKind::Primitive,
            ValueTypePersistence::Transient,
        ),
        (
            OPAQUE_TOKEN_TYPE_ID,
            "std.types.opaque_token",
            "orna.std.value.opaque-token@1",
            ValueTypeKind::Opaque,
            ValueTypePersistence::Transient,
        ),
    ];

    assert_eq!(STANDARD_TYPE_IDS, expected.map(|fact| fact.0));
    assert_eq!(
        STANDARD_TYPE_IDS.map(|id| id.to_bytes()),
        [
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 11],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 12],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 13],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 14],
        ]
    );
    assert_eq!(catalogue.schemas()[0].name().to_string(), "std");
    assert_eq!(catalogue.schemas()[1].name().to_string(), "std.types");
    assert!(catalogue.object_types().is_empty());
    assert!(catalogue.functions().is_empty());
    assert_eq!(catalogue.value_types().len(), expected.len());
    for (definition, (id, name, contract, kind, persistence)) in
        catalogue.value_types().iter().zip(expected)
    {
        assert_eq!(definition.id(), id);
        assert_eq!(definition.name().to_string(), name);
        assert_eq!(definition.kind(), kind);
        assert_eq!(definition.mutability(), ValueTypeMutability::Immutable);
        assert_eq!(definition.persistence(), persistence);
        assert_eq!(definition.representation_contract(), contract);
        let primary = TypeLookupName::qualified(definition.name().clone());
        assert_eq!(catalogue.type_id_by_name(&primary), Some(id));
    }
}

#[test]
fn manifest_contains_the_exact_direct_binding_facts() {
    struct ExpectedBinding {
        kind: TypeBindingKind,
        name: &'static str,
        target: orna_core::TypeId,
        id: [u8; 16],
    }

    let expected = [
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.boolean",
            target: BOOLEAN_TYPE_ID,
            id: [
                0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1, 0xdd,
                0x4d, 0x31,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "boolean",
            target: BOOLEAN_TYPE_ID,
            id: [
                0xfc, 0x31, 0x05, 0xaf, 0xaf, 0x25, 0x20, 0xd7, 0xc7, 0x7c, 0xdd, 0x6b, 0x0e, 0xf8,
                0x15, 0xaa,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "bool",
            target: BOOLEAN_TYPE_ID,
            id: [
                0x7b, 0x20, 0xca, 0xb3, 0x61, 0x23, 0x35, 0x61, 0x03, 0xad, 0xab, 0x48, 0x61, 0x11,
                0x0c, 0xad,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.integer",
            target: INTEGER_TYPE_ID,
            id: [
                0xf9, 0x2a, 0x68, 0x3c, 0xa4, 0x2b, 0x48, 0x2f, 0x77, 0x7a, 0x79, 0x86, 0xb2, 0xdf,
                0x25, 0x93,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "integer",
            target: INTEGER_TYPE_ID,
            id: [
                0x19, 0x40, 0x9c, 0x7b, 0x37, 0x81, 0x68, 0xf8, 0x30, 0x0b, 0x44, 0x0c, 0xaf, 0x18,
                0x57, 0x78,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "int",
            target: INTEGER_TYPE_ID,
            id: [
                0x97, 0x0a, 0xa4, 0x1b, 0xb9, 0xb1, 0x99, 0xa3, 0xcb, 0xa3, 0x46, 0x8c, 0x9e, 0x7c,
                0x58, 0x89,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.bigint",
            target: BIGINT_TYPE_ID,
            id: [
                0x08, 0x52, 0xa1, 0xcb, 0xbe, 0x1c, 0x5b, 0x78, 0xb4, 0xfa, 0xd2, 0x9e, 0xed, 0x5b,
                0x0d, 0x1e,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "bigint",
            target: BIGINT_TYPE_ID,
            id: [
                0xa0, 0x50, 0x06, 0x28, 0xc9, 0x77, 0x06, 0xb2, 0xbd, 0x8f, 0x29, 0xf7, 0x8b, 0xaa,
                0x5e, 0x88,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.float",
            target: FLOAT_TYPE_ID,
            id: [
                0x30, 0x1f, 0x53, 0xba, 0x6e, 0xe1, 0xea, 0xd1, 0xe3, 0x18, 0x6b, 0x6b, 0x71, 0x9e,
                0xfc, 0xb5,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "float",
            target: FLOAT_TYPE_ID,
            id: [
                0x31, 0x03, 0xa7, 0xca, 0xfc, 0xc6, 0x3e, 0xd7, 0x2a, 0x10, 0x58, 0x00, 0x87, 0x97,
                0xb5, 0xe6,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.decimal",
            target: DECIMAL_TYPE_ID,
            id: [
                0x28, 0x5c, 0x9a, 0x60, 0x1c, 0x08, 0x5b, 0xfa, 0xe9, 0x48, 0x5c, 0x9c, 0xb8, 0x6b,
                0x45, 0xf9,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "decimal",
            target: DECIMAL_TYPE_ID,
            id: [
                0xdf, 0x8e, 0x7b, 0x74, 0x41, 0xca, 0xe1, 0xf8, 0xfd, 0x56, 0xd8, 0x83, 0xa3, 0x10,
                0x6e, 0xd5,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.character_large_object",
            target: CHARACTER_LARGE_OBJECT_TYPE_ID,
            id: [
                0x28, 0x67, 0x4f, 0xd2, 0x8e, 0x8a, 0x68, 0x08, 0x1e, 0x26, 0x3f, 0xb3, 0x1b, 0xc2,
                0xd8, 0x70,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "character large object",
            target: CHARACTER_LARGE_OBJECT_TYPE_ID,
            id: [
                0xf6, 0xd0, 0xd3, 0xb6, 0x31, 0x1b, 0x6b, 0xdc, 0xe6, 0x01, 0xd3, 0xcf, 0xc3, 0xa6,
                0x89, 0x1a,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "text",
            target: CHARACTER_LARGE_OBJECT_TYPE_ID,
            id: [
                0x72, 0x0f, 0xf6, 0x30, 0x3e, 0xf0, 0x01, 0x8c, 0x81, 0xd2, 0xa6, 0x73, 0x99, 0xf0,
                0xdb, 0xc2,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.binary_large_object",
            target: BINARY_LARGE_OBJECT_TYPE_ID,
            id: [
                0xa9, 0x31, 0x64, 0x64, 0xe3, 0x52, 0xb5, 0x6a, 0x56, 0xa1, 0x4b, 0x38, 0x4c, 0x7d,
                0x81, 0x34,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "binary large object",
            target: BINARY_LARGE_OBJECT_TYPE_ID,
            id: [
                0x15, 0x24, 0xb4, 0xca, 0x63, 0xbc, 0xe7, 0xf8, 0x9b, 0x24, 0xba, 0xf1, 0x8d, 0x33,
                0xaf, 0xbf,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "bytes",
            target: BINARY_LARGE_OBJECT_TYPE_ID,
            id: [
                0x84, 0xe0, 0x46, 0xbd, 0x87, 0xde, 0xc7, 0x0a, 0x1b, 0x73, 0x13, 0xae, 0x51, 0xb6,
                0x9d, 0xb7,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.uuid",
            target: UUID_TYPE_ID,
            id: [
                0x89, 0xea, 0x05, 0xd7, 0x14, 0xdc, 0x5d, 0x2f, 0x0a, 0x8e, 0x09, 0xf7, 0x5f, 0x31,
                0x66, 0x00,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "uuid",
            target: UUID_TYPE_ID,
            id: [
                0x73, 0xda, 0x8e, 0x2f, 0xac, 0xe9, 0x8a, 0x17, 0xa6, 0x63, 0xec, 0x97, 0xe6, 0x7c,
                0x79, 0x7f,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.date",
            target: DATE_TYPE_ID,
            id: [
                0xf9, 0x7c, 0x60, 0xa7, 0x50, 0x6b, 0x9e, 0x79, 0xa8, 0xa8, 0xd7, 0x84, 0xa1, 0x71,
                0xf7, 0xac,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "date",
            target: DATE_TYPE_ID,
            id: [
                0xf3, 0x2c, 0xab, 0x58, 0xdb, 0xdf, 0x3d, 0xc6, 0xfe, 0x7c, 0xb1, 0x74, 0x8e, 0x1f,
                0x93, 0x56,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.time",
            target: TIME_TYPE_ID,
            id: [
                0x15, 0x11, 0xd9, 0x2f, 0x12, 0xc3, 0x4c, 0x1b, 0x0c, 0x4c, 0x53, 0x26, 0xa8, 0xa0,
                0x34, 0x8d,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "time",
            target: TIME_TYPE_ID,
            id: [
                0x8b, 0xd8, 0x9d, 0x33, 0x32, 0x97, 0x8f, 0x32, 0xa7, 0xd0, 0xe1, 0xd6, 0x72, 0xd2,
                0x33, 0xd4,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.timestamp",
            target: TIMESTAMP_TYPE_ID,
            id: [
                0x47, 0xb0, 0x08, 0xa2, 0xdc, 0x0b, 0x20, 0xd1, 0x2b, 0x3e, 0x68, 0x9a, 0x30, 0xfc,
                0xff, 0x04,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "timestamp",
            target: TIMESTAMP_TYPE_ID,
            id: [
                0x84, 0x1f, 0xc4, 0xfb, 0x35, 0x7f, 0xf8, 0xc3, 0x10, 0x74, 0x4b, 0xfc, 0x97, 0x9c,
                0x8a, 0xa1,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.duration",
            target: DURATION_TYPE_ID,
            id: [
                0x36, 0x29, 0x37, 0xf6, 0x5e, 0x81, 0xf4, 0xa9, 0x45, 0x85, 0x47, 0xb4, 0xeb, 0x62,
                0x14, 0x9a,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "duration",
            target: DURATION_TYPE_ID,
            id: [
                0x6b, 0xdd, 0xb3, 0xa5, 0xf1, 0x4a, 0xc6, 0xf8, 0x42, 0x57, 0x35, 0xb8, 0x80, 0x2d,
                0xdc, 0x37,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.void",
            target: VOID_TYPE_ID,
            id: [
                0x82, 0xae, 0x45, 0x04, 0x07, 0xcf, 0xfa, 0xa6, 0x87, 0xe8, 0x1f, 0xa7, 0xdc, 0xbf,
                0x94, 0x0f,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Prelude,
            name: "void",
            target: VOID_TYPE_ID,
            id: [
                0x56, 0xc5, 0x04, 0xe2, 0xf8, 0x07, 0xce, 0x24, 0xd3, 0x61, 0x11, 0xe6, 0x4a, 0x01,
                0x73, 0xfb,
            ],
        },
        ExpectedBinding {
            kind: TypeBindingKind::Qualified,
            name: "std.opaque_token",
            target: OPAQUE_TOKEN_TYPE_ID,
            id: [
                0x4d, 0xab, 0x42, 0x83, 0x03, 0x1f, 0xcd, 0x81, 0xb5, 0x8d, 0x09, 0xd8, 0x87, 0x63,
                0x46, 0xae,
            ],
        },
    ];
    let manifest = standard_library_manifest().expect("the accepted manifest must be valid");
    let catalogue = manifest.catalogue();

    assert_eq!(catalogue.type_bindings().len(), expected.len());
    assert_eq!(
        catalogue
            .type_bindings()
            .iter()
            .filter(|binding| binding.kind() == TypeBindingKind::Qualified)
            .count(),
        14
    );
    assert_eq!(
        catalogue
            .type_bindings()
            .iter()
            .filter(|binding| binding.kind() == TypeBindingKind::Prelude)
            .count(),
        17
    );
    for (binding, fact) in catalogue.type_bindings().iter().zip(expected) {
        assert_eq!(binding.kind(), fact.kind);
        assert_eq!(binding.name().to_string(), fact.name);
        assert_eq!(binding.target(), fact.target);
        assert_eq!(binding.id().to_bytes(), fact.id);
        assert_eq!(
            catalogue
                .value_type_by_id(binding.target())
                .map(|definition| definition.id()),
            Some(fact.target)
        );

        let lookup = if fact.kind == TypeBindingKind::Qualified {
            TypeLookupName::qualified(QualifiedSemanticName::new(fact.name.split('.')).unwrap())
        } else {
            assert_eq!(fact.kind, TypeBindingKind::Prelude);
            TypeLookupName::prelude(PreludeTypeName::new(fact.name.split(' ')).unwrap())
        };
        assert_eq!(catalogue.type_id_by_name(&lookup), Some(fact.target));
        assert_eq!(
            catalogue
                .type_binding_by_name(&lookup)
                .map(|item| item.id()),
            Some(binding.id())
        );
    }

    for absent in ["std.bool", "std.int", "std.text", "std.bytes"] {
        let lookup =
            TypeLookupName::qualified(QualifiedSemanticName::new(absent.split('.')).unwrap());
        assert_eq!(catalogue.type_id_by_name(&lookup), None);
    }
}

#[test]
fn binding_identity_drift_is_a_typed_human_readable_error() {
    let mut changed_ids = EXPECTED_TYPE_BINDING_IDS;
    changed_ids[0] = [0; 16];

    let error = build_type_bindings(&changed_ids).unwrap_err();

    assert_eq!(
        error,
        StandardLibraryManifestError::TypeBindingIdentityMismatch {
            name: TypeLookupName::qualified(
                QualifiedSemanticName::new(["std", "boolean"]).unwrap()
            ),
            expected: orna_core::TypeBindingId::from_bytes([0; 16]),
            actual: orna_core::TypeBindingId::from_bytes([
                0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1, 0xdd,
                0x4d, 0x31,
            ]),
        }
    );
    assert_eq!(
        error.to_string(),
        "standard library type binding std.boolean has identity type-binding:afrke7nfxydead3z2nef3qad64, expected type-binding:00000000000000000000000000"
    );
    assert!(error.source().is_none());
}

#[test]
fn binding_identity_count_drift_fails_before_identity_comparison() {
    let shorter = build_type_bindings(&EXPECTED_TYPE_BINDING_IDS[..30]).unwrap_err();
    assert_eq!(
        shorter,
        StandardLibraryManifestError::TypeBindingCountMismatch {
            expected: 30,
            actual: 31,
        }
    );
    assert_eq!(
        shorter.to_string(),
        "the standard library manifest has 31 type bindings, expected 30"
    );
    assert!(shorter.source().is_none());

    let mut longer = EXPECTED_TYPE_BINDING_IDS.to_vec();
    longer.push([0; 16]);
    let longer = build_type_bindings(&longer).unwrap_err();
    assert_eq!(
        longer,
        StandardLibraryManifestError::TypeBindingCountMismatch {
            expected: 32,
            actual: 31,
        }
    );
    assert_eq!(
        longer.to_string(),
        "the standard library manifest has 31 type bindings, expected 32"
    );
    assert!(longer.source().is_none());
}

#[test]
fn manifest_errors_preserve_exact_context_and_sources() {
    let semantic = StandardLibraryManifestError::SemanticName {
        name: "std.types.boolean".to_owned(),
        source: SemanticNameError::EmptyName,
    };
    assert_eq!(
        semantic.to_string(),
        "the standard library manifest contains an invalid semantic name std.types.boolean: a semantic name must contain at least one part"
    );
    assert_eq!(
        semantic.source().map(ToString::to_string),
        Some("a semantic name must contain at least one part".to_owned())
    );

    let prelude = StandardLibraryManifestError::PreludeName {
        name: "LARGE-OBJECT".to_owned(),
        source: PreludeTypeNameError::InvalidWord { index: 0 },
    };
    assert_eq!(
        prelude.to_string(),
        "the standard library manifest contains an invalid prelude name LARGE-OBJECT: prelude type name word 0 is not an unquoted SQL word"
    );
    assert_eq!(
        prelude.source().map(ToString::to_string),
        Some("prelude type name word 0 is not an unquoted SQL word".to_owned())
    );

    let unqualified = QualifiedSemanticName::new(["boolean"]).unwrap();
    let binding = StandardLibraryManifestError::TypeBinding {
        name: TypeLookupName::qualified(unqualified.clone()),
        source: TypeBindingError::QualifiedNameIsNotQualified { name: unqualified },
    };
    assert_eq!(
        binding.to_string(),
        "the standard library manifest contains an invalid type binding boolean: qualified type binding boolean has no schema namespace"
    );
    assert_eq!(
        binding.source().map(ToString::to_string),
        Some("qualified type binding boolean has no schema namespace".to_owned())
    );

    let count = StandardLibraryManifestError::TypeBindingCountMismatch {
        expected: 30,
        actual: 29,
    };
    assert_eq!(
        count.to_string(),
        "the standard library manifest has 29 type bindings, expected 30"
    );
    assert!(count.source().is_none());

    let catalogue = StandardLibraryManifestError::Catalogue {
        source: CatalogueSnapshotError::DuplicateSchemaId { id: STD_SCHEMA_ID },
    };
    assert_eq!(
        catalogue.to_string(),
        format!(
            "the standard library manifest cannot form a catalogue: duplicate schema identity {STD_SCHEMA_ID}"
        )
    );
    assert_eq!(
        catalogue.source().map(ToString::to_string),
        Some(format!("duplicate schema identity {STD_SCHEMA_ID}"))
    );
}

pub(super) const EXPECTED_RETAINED_INVOKE_SOURCE: &str = r#"CREATE SCHEMA std.invoke;

CREATE SERVER FUNCTION std.invoke.echo(
    p_value INTEGER
)
RETURNS INTEGER
SECURITY INVOKER
TRANSACTION READ ONLY
VOLATILITY STABLE
AS
    SELECT p_value;
"#;
