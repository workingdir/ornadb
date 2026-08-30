use super::*;

fn tampered_v2_snapshot(types: &str, invoke: &str) -> StandardLibrarySnapshot {
    // A structurally valid V2 snapshot whose unit content differs from the
    // retained source. The catalogue, origins, executable, and retained
    // digest are the accepted ones; only the source bytes and the
    // recomputed source hashes change, so the canonical digest encoder
    // must reject the resulting snapshot.
    let snapshot =
        retained_standard_library_v2_snapshot().expect("the retained V2 standard source is valid");
    let types_unit = StoredSourceUnit::new(
        STD_TYPES_SOURCE_UNIT_ID,
        0,
        SOURCE_LOGICAL_PATH,
        types,
        source_unit_content_digest(types).expect("the tampered types digest is valid"),
    )
    .expect("the tampered types unit is valid");
    let invoke_unit = StoredSourceUnit::new(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        STD_INVOKE_SOURCE_LOGICAL_PATH,
        invoke,
        source_unit_content_digest(invoke).expect("the tampered invoke digest is valid"),
    )
    .expect("the tampered invoke unit is valid");
    let units = vec![types_unit, invoke_unit];
    let bundle_hash = source_bundle_digest(&units).expect("the tampered bundle digest is valid");
    let source = StoredSourceRevision::new(
        STANDARD_SOURCE_V2_BUNDLE_ID,
        STANDARD_SOURCE_V2_REVISION_ID,
        Some(STANDARD_SOURCE_REVISION_ID),
        units,
        bundle_hash,
        source_revision_record_digest(
            STANDARD_SOURCE_V2_BUNDLE_ID,
            Some(STANDARD_SOURCE_REVISION_ID),
            bundle_hash,
        )
        .expect("the tampered source revision digest is valid"),
    )
    .expect("the tampered stored source revision is valid");
    StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V2_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        source,
        LANGUAGE_VERSION_IDENTITY,
        snapshot.catalogue().clone(),
        snapshot.executables().to_vec(),
        snapshot.origins().to_vec(),
        snapshot.digest(),
    )
    .expect("the tampered V2 snapshot remains structurally valid")
}

#[test]
fn manifest_v2_exposes_the_reserved_executable_standard_facts() {
    let manifest = standard_library_v2_manifest().expect("the accepted V2 manifest is valid");
    let cloned = manifest.clone();

    assert_eq!(STANDARD_LIBRARY_V2_VERSION_IDENTITY, "orna.std/2");
    assert_eq!(
        manifest.standard_library_version(),
        STANDARD_LIBRARY_V2_VERSION_IDENTITY
    );
    assert_eq!(
        manifest.standard_library_revision(),
        STANDARD_LIBRARY_V2_REVISION_ID
    );
    assert_eq!(manifest.language_version(), LANGUAGE_VERSION_IDENTITY);
    assert_eq!(manifest.source_bundle(), STANDARD_SOURCE_V2_BUNDLE_ID);
    assert_eq!(manifest.source_revision(), STANDARD_SOURCE_V2_REVISION_ID);
    assert_eq!(manifest.types_source_unit(), STD_TYPES_SOURCE_UNIT_ID);
    assert_eq!(manifest.invoke_source_unit(), STD_INVOKE_SOURCE_UNIT_ID);
    assert_eq!(manifest.types_source_logical_path(), SOURCE_LOGICAL_PATH);
    assert_eq!(
        manifest.invoke_source_logical_path(),
        STD_INVOKE_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        manifest.catalogue().revision(),
        STANDARD_CATALOGUE_V2_REVISION_ID
    );
    assert_eq!(manifest.catalogue().schemas().len(), 3);
    assert_eq!(manifest.catalogue().schemas()[0].id(), STD_SCHEMA_ID);
    assert_eq!(manifest.catalogue().schemas()[1].id(), STD_TYPES_SCHEMA_ID);
    assert_eq!(manifest.catalogue().schemas()[2].id(), STD_INVOKE_SCHEMA_ID);
    assert_eq!(manifest.catalogue().value_types().len(), 14);
    assert_eq!(manifest.catalogue().type_bindings().len(), 31);
    assert_eq!(manifest.catalogue().functions().len(), 1);
    assert_eq!(
        manifest.catalogue().functions()[0].id(),
        STD_INVOKE_ECHO_FUNCTION_ID
    );
    assert_eq!(
        cloned.catalogue().revision(),
        STANDARD_CATALOGUE_V2_REVISION_ID
    );

    for (actual, expected) in [
        (
            STANDARD_LIBRARY_V2_REVISION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
        ),
        (
            STANDARD_CATALOGUE_V2_REVISION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
        ),
        (
            STANDARD_SOURCE_V2_BUNDLE_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
        ),
        (
            STANDARD_SOURCE_V2_REVISION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
        ),
        (
            STD_TYPES_SOURCE_UNIT_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
        ),
        (
            STD_INVOKE_SOURCE_UNIT_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
        ),
        (
            STD_INVOKE_SCHEMA_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
        ),
        (
            STD_INVOKE_ECHO_FUNCTION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10],
        ),
        (
            STD_INVOKE_ECHO_PARAMETER_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10],
        ),
        (
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10],
        ),
        (
            INTEGER_TYPE_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
        ),
    ] {
        assert_eq!(actual, expected);
    }
    assert_eq!(STD_INVOKE_ECHO_REVISION_NUMBER, 1);
    assert_eq!(INTEGER_TYPE_ID, STD_INTEGER_TYPE_ID);
}

#[test]
fn retains_the_v2_executable_standard_snapshot() {
    let snapshot =
        retained_standard_library_v2_snapshot().expect("the retained V2 standard source is valid");

    assert_eq!(snapshot.revision(), STANDARD_LIBRARY_V2_REVISION_ID);
    assert_eq!(
        snapshot.digest_version(),
        StandardLibraryDigestVersion::Version2
    );
    assert_eq!(snapshot.language_version(), LANGUAGE_VERSION_IDENTITY);
    assert_eq!(
        snapshot.catalogue().revision(),
        STANDARD_CATALOGUE_V2_REVISION_ID
    );
    assert_eq!(snapshot.source().id(), STANDARD_SOURCE_V2_REVISION_ID);
    assert_eq!(snapshot.source().bundle(), STANDARD_SOURCE_V2_BUNDLE_ID);
    assert_eq!(
        snapshot.source().parent(),
        Some(STANDARD_SOURCE_REVISION_ID)
    );
    assert_eq!(snapshot.source().units().len(), 2);
    assert_eq!(snapshot.source().units()[0].id(), STD_TYPES_SOURCE_UNIT_ID);
    assert_eq!(snapshot.source().units()[0].ordinal(), 0);
    assert_eq!(
        snapshot.source().units()[0].logical_path(),
        SOURCE_LOGICAL_PATH
    );
    assert_eq!(snapshot.source().units()[1].id(), STD_INVOKE_SOURCE_UNIT_ID);
    assert_eq!(snapshot.source().units()[1].ordinal(), 1);
    assert_eq!(
        snapshot.source().units()[1].logical_path(),
        STD_INVOKE_SOURCE_LOGICAL_PATH
    );
    assert_eq!(snapshot.catalogue().schemas().len(), 3);
    assert_eq!(snapshot.catalogue().functions().len(), 1);
    assert_eq!(snapshot.origins().len(), 50);

    let [executable] = snapshot.executables() else {
        panic!("the V2 snapshot must retain exactly one executable");
    };
    assert_eq!(executable.function(), STD_INVOKE_ECHO_FUNCTION_ID);
    assert_eq!(
        executable.revision().id(),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID
    );
    assert_eq!(
        executable.revision().revision_number(),
        STD_INVOKE_ECHO_REVISION_NUMBER
    );
    assert_eq!(
        executable.revision().semantic_hash_version(),
        FunctionSemanticHashVersion::Version2
    );
    assert_eq!(
        executable.revision().language_version(),
        LANGUAGE_VERSION_IDENTITY
    );
    assert_eq!(
        executable.revision().declaration_origin().source_unit(),
        STD_INVOKE_SOURCE_UNIT_ID
    );
    assert_eq!(executable.references().len(), 3);
    for (ordinal, reference) in executable.references().iter().enumerate() {
        assert_eq!(reference.ordinal(), ordinal as u32);
        assert_eq!(reference.source_function(), STD_INVOKE_ECHO_FUNCTION_ID);
        assert_eq!(
            reference.source_revision(),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID
        );
        assert_eq!(
            reference.source_origin().source_unit(),
            STD_INVOKE_SOURCE_UNIT_ID
        );
    }
    assert_eq!(
        executable.references()[0].target(),
        DefinitionReferenceTarget::ValueType(INTEGER_TYPE_ID)
    );
    assert_eq!(
        executable.references()[0].kind(),
        DefinitionReferenceKind::NamedType
    );
    assert_eq!(
        executable.references()[1].target(),
        DefinitionReferenceTarget::ValueType(INTEGER_TYPE_ID)
    );
    assert_eq!(
        executable.references()[1].kind(),
        DefinitionReferenceKind::NamedType
    );
    assert_eq!(
        executable.references()[2].target(),
        DefinitionReferenceTarget::Parameter {
            owner: STD_INVOKE_ECHO_FUNCTION_ID,
            parameter: STD_INVOKE_ECHO_PARAMETER_ID,
        }
    );
    assert_eq!(
        executable.references()[2].kind(),
        DefinitionReferenceKind::ParameterRead
    );
}

#[test]
fn v2_retained_invoke_source_has_the_exact_literal_bytes_and_parse() {
    let snapshot =
        retained_standard_library_v2_snapshot().expect("the retained V2 standard source is valid");
    let types = snapshot.source().units()[0].content();
    let invoke = snapshot.source().units()[1].content();

    assert_eq!(invoke, EXPECTED_RETAINED_INVOKE_SOURCE);
    assert_eq!(invoke.len(), 185);
    assert!(invoke.is_ascii());
    assert!(!invoke.as_bytes().starts_with(&[0xef, 0xbb, 0xbf]));
    assert!(!invoke.contains('\r'));
    assert!(invoke.ends_with('\n'));
    assert!(!invoke[..invoke.len() - 1].ends_with('\n'));
    assert_eq!(invoke.matches(';').count(), 2);
    assert_eq!(
        snapshot.source().units()[0].content(),
        super::super::RETAINED_STANDARD_SOURCE
    );
    assert_eq!(
        types,
        super::super::RETAINED_STANDARD_SOURCE,
        "the V2 types unit must retain the V1 bytes byte-for-byte"
    );

    let parsed = orna_syntax::parse(invoke);
    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), invoke);
    assert_eq!(parsed.schemas().len(), 1);
    assert_eq!(parsed.server_functions().len(), 1);
    assert!(parsed.object_types().is_empty());
    assert!(parsed.field_renames().is_empty());
    assert!(parsed.primitive_value_types().is_empty());
    assert!(parsed.opaque_value_types().is_empty());
    assert!(parsed.type_exports().is_empty());
    assert!(parsed.client_functions().is_empty());
}

#[test]
fn v2_invoke_origins_cover_the_exact_declaration_ranges() {
    let snapshot =
        retained_standard_library_v2_snapshot().expect("the retained V2 standard source is valid");
    let invoke = snapshot.source().units()[1].content();
    let invoke_origins = snapshot
        .origins()
        .iter()
        .filter(|origin| origin.source().source_unit() == STD_INVOKE_SOURCE_UNIT_ID)
        .collect::<Vec<_>>();
    assert_eq!(invoke_origins.len(), 3);

    let schema_origin = invoke_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID))
        .expect("the schema origin is retained")
        .source();
    let function_origin = invoke_origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
        })
        .expect("the function origin is retained")
        .source();
    let parameter_origin = invoke_origins
        .iter()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::Parameter {
                    owner: STD_INVOKE_ECHO_FUNCTION_ID,
                    parameter: STD_INVOKE_ECHO_PARAMETER_ID,
                }
        })
        .expect("the parameter origin is retained")
        .source();

    let schema_end = "CREATE SCHEMA std.invoke;".len();
    let parameter_start = invoke.find("p_value").expect("the parameter is retained");
    let parameter_end = parameter_start + "p_value INTEGER".len();
    let function_start = invoke
        .find("CREATE SERVER FUNCTION")
        .expect("the function is retained");
    let function_end = invoke.rfind(';').expect("the declaration ends") + 1;

    assert_eq!(schema_origin.byte_start(), 0);
    assert_eq!(schema_origin.byte_end(), schema_end as u32);
    assert_eq!(&invoke[0..schema_end], "CREATE SCHEMA std.invoke;");
    assert_eq!(function_origin.byte_start(), function_start as u32);
    assert_eq!(function_origin.byte_end(), function_end as u32);
    assert_eq!(parameter_origin.byte_start(), parameter_start as u32);
    assert_eq!(parameter_origin.byte_end(), parameter_end as u32);
    assert_eq!(&invoke[parameter_start..parameter_end], "p_value INTEGER");

    let executable = &snapshot.executables()[0];
    let references = executable.references();
    let parameter_integer = invoke
        .find("INTEGER")
        .expect("the parameter type is retained");
    let result_integer = invoke
        .rfind("INTEGER")
        .expect("the result type is retained");
    let body_p_value = invoke
        .rfind("p_value")
        .expect("the body identifier is retained");
    assert_eq!(
        references[0].source_origin().byte_start(),
        parameter_integer as u32
    );
    assert_eq!(
        references[0].source_origin().byte_end(),
        parameter_integer as u32 + 7
    );
    assert_eq!(
        references[1].source_origin().byte_start(),
        result_integer as u32
    );
    assert_eq!(
        references[1].source_origin().byte_end(),
        result_integer as u32 + 7
    );
    assert_eq!(
        references[2].source_origin().byte_start(),
        body_p_value as u32
    );
    assert_eq!(
        references[2].source_origin().byte_end(),
        body_p_value as u32 + 7
    );
    assert_eq!(&invoke[parameter_integer..parameter_integer + 7], "INTEGER");
    assert_eq!(&invoke[result_integer..result_integer + 7], "INTEGER");
    assert_eq!(&invoke[body_p_value..body_p_value + 7], "p_value");
}

#[test]
fn v2_digest_goldens_are_computed_from_the_retained_source() {
    let snapshot =
        retained_standard_library_v2_snapshot().expect("the retained V2 standard source is valid");
    let types = snapshot.source().units()[0].content();
    let invoke = snapshot.source().units()[1].content();
    let units = snapshot.source().units().to_vec();
    let executable = &snapshot.executables()[0];
    let function = snapshot
        .catalogue()
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .expect("the echo function is retained");
    let artifact = executable.revision().artifact();
    let references = executable.references();

    assert_eq!(
        source_unit_content_digest(types).expect("the types content digest is valid"),
        super::super::ACCEPTED_V2_TYPES_CONTENT_DIGEST
    );
    assert_eq!(
        source_unit_content_digest(invoke).expect("the invoke content digest is valid"),
        super::super::ACCEPTED_V2_INVOKE_CONTENT_DIGEST
    );
    assert_eq!(
        source_bundle_digest(&units).expect("the bundle digest is valid"),
        super::super::ACCEPTED_V2_SOURCE_BUNDLE_DIGEST
    );
    assert_eq!(
        source_revision_record_digest(
            STANDARD_SOURCE_V2_BUNDLE_ID,
            Some(STANDARD_SOURCE_REVISION_ID),
            snapshot.source().bundle_hash(),
        )
        .expect("the source revision digest is valid"),
        super::super::ACCEPTED_V2_SOURCE_REVISION_DIGEST
    );
    assert_eq!(
        artifact_payload_digest(artifact.payload()).expect("the artifact digest is valid"),
        super::super::ACCEPTED_V2_ARTIFACT_DIGEST
    );
    assert_eq!(
        function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            function,
            LANGUAGE_VERSION_IDENTITY,
            artifact,
            &[],
            references,
        )
        .expect("the semantic digest is valid"),
        super::super::ACCEPTED_V2_SEMANTIC_DIGEST
    );
    assert_eq!(
        snapshot.digest(),
        super::super::ACCEPTED_V2_STANDARD_LIBRARY_DIGEST
    );
    assert_eq!(
        standard_library_digest(&snapshot).expect("the retained digest recomputes"),
        super::super::ACCEPTED_V2_STANDARD_LIBRARY_DIGEST
    );
}

#[test]
fn v2_standard_digest_binds_every_retained_byte() {
    let snapshot =
        retained_standard_library_v2_snapshot().expect("the retained V2 standard source is valid");
    let types = snapshot.source().units()[0].content();
    let invoke = snapshot.source().units()[1].content();

    let tampered_types = format!("{types} ");
    let tampered_invoke = format!("{invoke} ");
    for tampered in [
        tampered_v2_snapshot(&tampered_types, invoke),
        tampered_v2_snapshot(types, &tampered_invoke),
    ] {
        assert_eq!(
            tampered.digest(),
            super::super::ACCEPTED_V2_STANDARD_LIBRARY_DIGEST
        );
        assert!(matches!(
            standard_library_digest(&tampered),
            Err(
                orna_core::canonical_hash::CanonicalHashError::StandardLibraryDigestMismatch { .. }
            )
        ));
        assert!(verify_standard_library_v2_snapshot(tampered).is_err());
    }
}

#[test]
fn v2_snapshot_verifies_and_the_compiler_reconciles_the_bundle() {
    let snapshot =
        retained_standard_library_v2_snapshot().expect("the retained V2 standard source is valid");
    let verified = verify_standard_library_v2_snapshot(snapshot)
        .expect("the retained V2 standard source verifies");
    assert_eq!(verified.revision(), STANDARD_LIBRARY_V2_REVISION_ID);
    assert_eq!(
        verified.digest_version(),
        StandardLibraryDigestVersion::Version2
    );

    let checked = orna_compiler::check_standard_library_source(&verified)
        .expect("the V2 standard source reconciles");
    assert_eq!(checked.schemas().len(), 2);
    assert_eq!(checked.value_types().len(), 14);
    assert_eq!(checked.type_bindings().len(), 31);
    let executable = checked
        .checked_executable()
        .expect("the V2 check retains the executable");
    assert_eq!(executable.function_id(), STD_INVOKE_ECHO_FUNCTION_ID);
    assert_eq!(executable.parameter_ids(), &[STD_INVOKE_ECHO_PARAMETER_ID]);
    assert_eq!(
        executable.revision_id(),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID
    );
    assert_eq!(
        executable.revision_number(),
        STD_INVOKE_ECHO_REVISION_NUMBER
    );
    assert_eq!(executable.references().len(), 3);
    assert_eq!(
        verified.executables()[0].references().len(),
        executable.references().len()
    );
}

#[test]
fn v1_and_v2_snapshots_reject_each_others_verifiers() {
    let version_one =
        retained_standard_library_snapshot().expect("the retained V1 source is valid");
    let version_two =
        retained_standard_library_v2_snapshot().expect("the retained V2 source is valid");

    assert!(verify_standard_library_snapshot(version_one.clone()).is_ok());
    // The V2 wrapper rejects a V1 snapshot closed at the reserved
    // catalogue-identity gate before it reaches the canonical verifier.
    assert!(matches!(
        verify_standard_library_v2_snapshot(version_one.clone()),
        Err(super::super::StandardLibraryError::CatalogueIdentityMismatch {
            expected,
            actual
        }) if expected == STANDARD_CATALOGUE_V2_REVISION_ID
            && actual == STANDARD_CATALOGUE_REVISION_ID
    ));
    // The core V2 canonical verifier itself rejects the V1 digest version.
    assert!(matches!(
        orna_core::canonical_hash::verify_standard_library_v2_snapshot(version_one),
        Err(
            orna_core::canonical_hash::CanonicalHashError::StandardLibraryDigestVersionMismatch {
                expected: StandardLibraryDigestVersion::Version2,
                actual: StandardLibraryDigestVersion::Version1,
                ..
            }
        )
    ));

    assert!(verify_standard_library_v2_snapshot(version_two.clone()).is_ok());
    assert!(matches!(
        verify_standard_library_snapshot(version_two.clone()),
        Err(super::super::StandardLibraryError::CatalogueIdentityMismatch {
            expected,
            actual
        }) if expected == STANDARD_CATALOGUE_REVISION_ID
            && actual == STANDARD_CATALOGUE_V2_REVISION_ID
    ));
    assert!(matches!(
        orna_core::canonical_hash::verify_standard_library_snapshot(version_two),
        Err(
            orna_core::canonical_hash::CanonicalHashError::StandardLibraryDigestVersionMismatch {
                expected: StandardLibraryDigestVersion::Version1,
                actual: StandardLibraryDigestVersion::Version2,
                ..
            }
        )
    ));
}

#[test]
fn v2_artifact_is_the_exact_44_byte_parameter_echo() {
    let snapshot =
        retained_standard_library_v2_snapshot().expect("the retained V2 standard source is valid");
    let artifact = snapshot.executables()[0].revision().artifact();

    assert_eq!(artifact.kind(), ExecutableArtifactKind::Server);
    assert_eq!(artifact.format(), "orna.server-parameter-echo");
    assert_eq!(artifact.version(), 1);
    let payload = artifact.payload();
    assert_eq!(payload.len(), 44);
    assert_eq!(&payload[0..8], b"ORNAPE\0\0");
    assert_eq!(&payload[8..12], &1_u32.to_be_bytes());
    assert_eq!(&payload[12..28], STD_INVOKE_ECHO_PARAMETER_ID.to_bytes());
    assert_eq!(&payload[28..44], INTEGER_TYPE_ID.to_bytes());
    assert_eq!(
        artifact.content_hash(),
        artifact_payload_digest(payload).expect("the artifact digest is valid")
    );
    assert_eq!(
        snapshot.executables()[0].revision().semantic_hash_version(),
        FunctionSemanticHashVersion::Version2
    );
}

#[test]
fn v2_retained_source_rejects_modified_invoke_bytes() {
    let modified =
        EXPECTED_RETAINED_INVOKE_SOURCE.replacen("VOLATILITY STABLE", "VOLATILITY VOLATILE", 1);
    assert!(matches!(
        retained_standard_library_v2_snapshot_from_source(
            super::super::RETAINED_STANDARD_SOURCE,
            &modified,
        ),
        Err(super::super::StandardLibraryError::RetainedSourceMismatch)
    ));

    let extra_schema = format!("{EXPECTED_RETAINED_INVOKE_SOURCE}CREATE SCHEMA std.extra;\n");
    assert!(matches!(
        retained_standard_library_v2_snapshot_from_source(
            super::super::RETAINED_STANDARD_SOURCE,
            &extra_schema,
        ),
        Err(super::super::StandardLibraryError::RetainedSourceMismatch)
    ));
}

#[test]
fn prepares_the_v1_to_v2_standard_upgrade_from_an_empty_active_revision() {
    let active = empty_active_revision();

    let upgrade =
        prepare_standard_upgrade_v1_to_v2(&active).expect("the V1-to-V2 standard upgrade prepares");

    assert_eq!(
        upgrade
            .checked_standard_library()
            .verified_snapshot()
            .revision(),
        STANDARD_LIBRARY_V2_REVISION_ID
    );
    assert_eq!(
        upgrade.verified_standard_snapshot().revision(),
        STANDARD_LIBRARY_V2_REVISION_ID
    );
    assert_eq!(
        upgrade.verified_standard_snapshot().source().parent(),
        Some(STANDARD_SOURCE_REVISION_ID),
        "V2 must be the append-only child of the retained V1 source revision"
    );
    let executable = upgrade
        .checked_standard_library()
        .checked_executable()
        .expect("the V2 upgrade retains the executable");
    assert_eq!(executable.function_id(), STD_INVOKE_ECHO_FUNCTION_ID);
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
        Some(STANDARD_LIBRARY_V2_REVISION_ID)
    );
    assert_eq!(
        upgrade
            .application_revision()
            .catalogue_hash_context()
            .standard()
            .map(|snapshot| snapshot.digest_version()),
        Some(StandardLibraryDigestVersion::Version2)
    );
}

#[test]
fn v1_to_v2_standard_upgrade_fails_when_v2_is_already_installed() {
    let version_two = verify_standard_library_v2_snapshot(
        retained_standard_library_v2_snapshot().expect("the retained V2 standard source is valid"),
    )
    .expect("the retained V2 standard source verifies");
    let active = empty_version_two_active_revision(&version_two);

    let error = prepare_standard_upgrade_v1_to_v2(&active)
        .expect_err("an installed V2 standard must close the upgrade");

    assert!(matches!(
        &error,
        StandardUpgradeError::Prepare {
            source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                revision
            }
        } if *revision == STANDARD_LIBRARY_V2_REVISION_ID
    ));
    assert_eq!(
        error.to_string(),
        format!("standard library {STANDARD_LIBRARY_V2_REVISION_ID} is already installed")
    );
}

#[test]
fn v1_to_v2_standard_upgrade_fails_when_v1_is_pinned_or_the_base_is_not_expected() {
    let version_one = verify_standard_library_snapshot(
        retained_standard_library_snapshot().expect("the retained V1 source is valid"),
    )
    .expect("the retained V1 standard source verifies");
    let pinned = empty_version_two_active_revision(&version_one);

    let error = prepare_standard_upgrade_v1_to_v2(&pinned)
        .expect_err("a pinned V1 standard must close the upgrade");
    assert!(matches!(
        &error,
        StandardUpgradeError::Prepare {
            source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                revision
            }
        } if *revision == STANDARD_LIBRARY_REVISION_ID
    ));

    // A non-empty active revision with a reserved standard identity is not
    // the expected empty base and must fail closed.
    let occupied_source_unit = SourceUnitId::from_bytes([0x94; 16]);
    let occupied_catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x92; 16]),
        vec![orna_core::catalogue::SchemaDefinition::new(
            STD_INVOKE_SCHEMA_ID,
            QualifiedSemanticName::new(["app"]).expect("the app schema name is valid"),
        )],
        Vec::new(),
    )
    .expect("the occupied catalogue is valid");
    let occupied_origin = orna_core::revision::DefinitionOrigin::new(
        DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID),
        orna_core::revision::SourceOrigin::new(occupied_source_unit, 0, 1)
            .expect("the occupied origin is valid"),
    );
    let occupied_unit = StoredSourceUnit::new(
        occupied_source_unit,
        0,
        "occupied.orna",
        " ",
        source_unit_content_digest(" ").expect("the occupied unit digest is valid"),
    )
    .expect("the occupied source unit is valid");
    let occupied = ActiveDatabaseRevision::new(
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x91; 16]),
            CatalogueRevisionId::from_bytes([0x92; 16]),
        ),
        StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x93; 16]),
            SourceRevisionId::from_bytes([0x91; 16]),
            None,
            vec![occupied_unit],
            source_bundle_digest(std::slice::from_ref(
                &StoredSourceUnit::new(
                    occupied_source_unit,
                    0,
                    "occupied.orna",
                    " ",
                    source_unit_content_digest(" ").expect("the occupied unit digest is valid"),
                )
                .expect("the occupied source unit is valid"),
            ))
            .expect("the occupied source bundle digest is valid"),
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x93; 16]),
                None,
                source_bundle_digest(std::slice::from_ref(
                    &StoredSourceUnit::new(
                        occupied_source_unit,
                        0,
                        "occupied.orna",
                        " ",
                        source_unit_content_digest(" ").expect("the occupied unit digest is valid"),
                    )
                    .expect("the occupied source unit is valid"),
                ))
                .expect("the occupied source bundle digest is valid"),
            )
            .expect("the occupied source revision digest is valid"),
        )
        .expect("the occupied stored source revision is valid"),
        occupied_catalogue.clone(),
        catalogue_digest(
            &occupied_catalogue,
            &[],
            &[],
            std::slice::from_ref(&occupied_origin),
            &[],
        )
        .expect("the occupied catalogue digest is valid"),
        Vec::new(),
        Vec::new(),
        vec![occupied_origin],
        Vec::new(),
    )
    .expect("the occupied active revision is valid");

    let error = prepare_standard_upgrade_v1_to_v2(&occupied)
        .expect_err("a reserved identity must close the upgrade");
    assert!(matches!(
        &error,
        StandardUpgradeError::Prepare {
            source: orna_compiler::PrepareStandardUpgradeError::ReservedIdentity { .. }
        }
    ));
}

const EXPECTED_RETAINED_OUTPUT_SOURCE: &str = r#"CREATE SCHEMA std.terminal;
CREATE SCHEMA std.io;

CREATE TYPE std.terminal.Document AS VALUE OPAQUE
    KERNEL CONTRACT 'orna.std.value.terminal-document@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.terminal.Document AS std.Document;

CREATE TYPE std.io.ByteStream AS VALUE OPAQUE
    KERNEL CONTRACT 'orna.std.value.byte-stream@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.io.ByteStream AS std.ByteStream;
"#;

const EXPECTED_RETAINED_UI_SOURCE: &str = r#"CREATE SCHEMA std.ui;

CREATE TYPE std.ui.UI AS VALUE
    OPAQUE
    KERNEL CONTRACT 'orna.std.value.ui@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.ui.UI AS std.UI;
"#;

pub(super) const EXPECTED_RETAINED_ACTION_SOURCE: &str = r#"CREATE SCHEMA std.action;

CREATE TYPE std.action.Action AS VALUE
    OPAQUE
    KERNEL CONTRACT 'orna.std.value.action@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.action.Action AS std.Action;
"#;

fn tampered_v4_snapshot(
    types: &str,
    invoke: &str,
    output: &str,
    ui: &str,
) -> StandardLibrarySnapshot {
    // A structurally valid V4 snapshot whose unit content differs from the
    // retained source. The catalogue, origins, executable, and retained
    // digest are the accepted ones; only the source bytes and the
    // recomputed source hashes change, so the canonical digest encoder
    // must reject the resulting snapshot.
    let snapshot =
        retained_standard_library_v4_snapshot().expect("the retained V4 standard source is valid");
    let types_unit = StoredSourceUnit::new(
        STD_TYPES_SOURCE_UNIT_ID,
        0,
        SOURCE_LOGICAL_PATH,
        types,
        source_unit_content_digest(types).expect("the tampered types digest is valid"),
    )
    .expect("the tampered types unit is valid");
    let invoke_unit = StoredSourceUnit::new(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        STD_INVOKE_SOURCE_LOGICAL_PATH,
        invoke,
        source_unit_content_digest(invoke).expect("the tampered invoke digest is valid"),
    )
    .expect("the tampered invoke unit is valid");
    let output_unit = StoredSourceUnit::new(
        STD_OUTPUT_SOURCE_UNIT_ID,
        2,
        STD_OUTPUT_SOURCE_LOGICAL_PATH,
        output,
        source_unit_content_digest(output).expect("the tampered output digest is valid"),
    )
    .expect("the tampered output unit is valid");
    let ui_unit = StoredSourceUnit::new(
        STD_UI_SOURCE_UNIT_ID,
        3,
        STD_UI_SOURCE_LOGICAL_PATH,
        ui,
        source_unit_content_digest(ui).expect("the tampered ui digest is valid"),
    )
    .expect("the tampered ui unit is valid");
    let units = vec![types_unit, invoke_unit, output_unit, ui_unit];
    let bundle_hash = source_bundle_digest(&units).expect("the tampered bundle digest is valid");
    let source = StoredSourceRevision::new(
        STANDARD_SOURCE_V4_BUNDLE_ID,
        STANDARD_SOURCE_V4_REVISION_ID,
        Some(STANDARD_SOURCE_V3_REVISION_ID),
        units,
        bundle_hash,
        source_revision_record_digest(
            STANDARD_SOURCE_V4_BUNDLE_ID,
            Some(STANDARD_SOURCE_V3_REVISION_ID),
            bundle_hash,
        )
        .expect("the tampered source revision digest is valid"),
    )
    .expect("the tampered stored source revision is valid");
    StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V4_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        source,
        LANGUAGE_VERSION_IDENTITY,
        snapshot.catalogue().clone(),
        snapshot.executables().to_vec(),
        snapshot.origins().to_vec(),
        snapshot.digest(),
    )
    .expect("the tampered V4 snapshot remains structurally valid")
}

fn tampered_v3_snapshot(types: &str, invoke: &str, output: &str) -> StandardLibrarySnapshot {
    // A structurally valid V3 snapshot whose unit content differs from the
    // retained source. The catalogue, origins, executable, and retained
    // digest are the accepted ones; only the source bytes and the
    // recomputed source hashes change, so the canonical digest encoder
    // must reject the resulting snapshot.
    let snapshot =
        retained_standard_library_v3_snapshot().expect("the retained V3 standard source is valid");
    let types_unit = StoredSourceUnit::new(
        STD_TYPES_SOURCE_UNIT_ID,
        0,
        SOURCE_LOGICAL_PATH,
        types,
        source_unit_content_digest(types).expect("the tampered types digest is valid"),
    )
    .expect("the tampered types unit is valid");
    let invoke_unit = StoredSourceUnit::new(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        STD_INVOKE_SOURCE_LOGICAL_PATH,
        invoke,
        source_unit_content_digest(invoke).expect("the tampered invoke digest is valid"),
    )
    .expect("the tampered invoke unit is valid");
    let output_unit = StoredSourceUnit::new(
        STD_OUTPUT_SOURCE_UNIT_ID,
        2,
        STD_OUTPUT_SOURCE_LOGICAL_PATH,
        output,
        source_unit_content_digest(output).expect("the tampered output digest is valid"),
    )
    .expect("the tampered output unit is valid");
    let units = vec![types_unit, invoke_unit, output_unit];
    let bundle_hash = source_bundle_digest(&units).expect("the tampered bundle digest is valid");
    let source = StoredSourceRevision::new(
        STANDARD_SOURCE_V3_BUNDLE_ID,
        STANDARD_SOURCE_V3_REVISION_ID,
        Some(STANDARD_SOURCE_V2_REVISION_ID),
        units,
        bundle_hash,
        source_revision_record_digest(
            STANDARD_SOURCE_V3_BUNDLE_ID,
            Some(STANDARD_SOURCE_V2_REVISION_ID),
            bundle_hash,
        )
        .expect("the tampered source revision digest is valid"),
    )
    .expect("the tampered stored source revision is valid");
    StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V3_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        source,
        LANGUAGE_VERSION_IDENTITY,
        snapshot.catalogue().clone(),
        snapshot.executables().to_vec(),
        snapshot.origins().to_vec(),
        snapshot.digest(),
    )
    .expect("the tampered V3 snapshot remains structurally valid")
}

#[test]
fn manifest_v3_exposes_the_reserved_output_standard_facts() {
    let manifest = standard_library_v3_manifest().expect("the accepted V3 manifest is valid");
    let cloned = manifest.clone();

    assert_eq!(STANDARD_LIBRARY_V3_VERSION_IDENTITY, "orna.std/3");
    assert_eq!(
        manifest.standard_library_version(),
        STANDARD_LIBRARY_V3_VERSION_IDENTITY
    );
    assert_eq!(
        manifest.standard_library_revision(),
        STANDARD_LIBRARY_V3_REVISION_ID
    );
    assert_eq!(manifest.language_version(), LANGUAGE_VERSION_IDENTITY);
    assert_eq!(manifest.source_bundle(), STANDARD_SOURCE_V3_BUNDLE_ID);
    assert_eq!(manifest.source_revision(), STANDARD_SOURCE_V3_REVISION_ID);
    assert_eq!(manifest.types_source_unit(), STD_TYPES_SOURCE_UNIT_ID);
    assert_eq!(manifest.invoke_source_unit(), STD_INVOKE_SOURCE_UNIT_ID);
    assert_eq!(manifest.output_source_unit(), STD_OUTPUT_SOURCE_UNIT_ID);
    assert_eq!(manifest.types_source_logical_path(), SOURCE_LOGICAL_PATH);
    assert_eq!(
        manifest.invoke_source_logical_path(),
        STD_INVOKE_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        manifest.output_source_logical_path(),
        STD_OUTPUT_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        manifest.catalogue().revision(),
        STANDARD_CATALOGUE_V3_REVISION_ID
    );
    assert_eq!(manifest.catalogue().schemas().len(), 5);
    assert_eq!(
        manifest.catalogue().schemas()[3].id(),
        STD_TERMINAL_SCHEMA_ID
    );
    assert_eq!(manifest.catalogue().schemas()[4].id(), STD_IO_SCHEMA_ID);
    assert_eq!(manifest.catalogue().value_types().len(), 16);
    assert_eq!(manifest.catalogue().type_bindings().len(), 33);
    assert_eq!(manifest.catalogue().functions().len(), 1);
    assert_eq!(
        manifest.catalogue().functions()[0].id(),
        STD_INVOKE_ECHO_FUNCTION_ID
    );
    assert_eq!(
        cloned.catalogue().revision(),
        STANDARD_CATALOGUE_V3_REVISION_ID
    );

    let document = manifest
        .catalogue()
        .type_definition_by_id(STD_TERMINAL_DOCUMENT_TYPE_ID)
        .expect("the document type is retained")
        .as_opaque_value()
        .expect("the document type is opaque");
    assert_eq!(document.name().to_string(), "std.terminal.document");
    assert_eq!(
        document.representation_contract(),
        STD_TERMINAL_DOCUMENT_CONTRACT
    );
    let byte_stream = manifest
        .catalogue()
        .type_definition_by_id(STD_IO_BYTE_STREAM_TYPE_ID)
        .expect("the byte-stream type is retained")
        .as_opaque_value()
        .expect("the byte-stream type is opaque");
    assert_eq!(byte_stream.name().to_string(), "std.io.bytestream");
    assert_eq!(
        byte_stream.representation_contract(),
        STD_IO_BYTE_STREAM_CONTRACT
    );

    for (actual, expected) in [
        (
            STANDARD_LIBRARY_V3_REVISION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
        ),
        (
            STANDARD_CATALOGUE_V3_REVISION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
        ),
        (
            STANDARD_SOURCE_V3_BUNDLE_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
        ),
        (
            STANDARD_SOURCE_V3_REVISION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
        ),
        (
            STD_OUTPUT_SOURCE_UNIT_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
        ),
        (
            STD_TERMINAL_SCHEMA_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
        ),
        (
            STD_IO_SCHEMA_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5],
        ),
        (
            STD_TERMINAL_DOCUMENT_TYPE_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0f],
        ),
        (
            STD_IO_BYTE_STREAM_TYPE_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10],
        ),
    ] {
        assert_eq!(actual, expected);
    }
    assert_eq!(
        STD_TERMINAL_DOCUMENT_CONTRACT,
        "orna.std.value.terminal-document@1"
    );
    assert_eq!(STD_IO_BYTE_STREAM_CONTRACT, "orna.std.value.byte-stream@1");
    assert_eq!(TERMINAL_DOCUMENT_MAGIC, "ORNA-TERMINAL-DOCUMENT/1 ");
    assert_eq!(BYTE_STREAM_MAGIC, "ORNA-BYTE-STREAM/1 ");
    assert_eq!(STD_OUTPUT_SOURCE_LOGICAL_PATH, "std/output.orna");
}

#[test]
fn manifest_v4_exposes_the_reserved_ui_standard_facts() {
    let manifest = standard_library_v4_manifest().expect("the accepted V4 manifest is valid");
    let cloned = manifest.clone();

    assert_eq!(STANDARD_LIBRARY_V4_VERSION_IDENTITY, "orna.std/4");
    assert_eq!(
        manifest.standard_library_version(),
        STANDARD_LIBRARY_V4_VERSION_IDENTITY
    );
    assert_eq!(
        manifest.standard_library_revision(),
        STANDARD_LIBRARY_V4_REVISION_ID
    );
    assert_eq!(manifest.language_version(), LANGUAGE_VERSION_IDENTITY);
    assert_eq!(manifest.source_bundle(), STANDARD_SOURCE_V4_BUNDLE_ID);
    assert_eq!(manifest.source_revision(), STANDARD_SOURCE_V4_REVISION_ID);
    assert_eq!(manifest.types_source_unit(), STD_TYPES_SOURCE_UNIT_ID);
    assert_eq!(manifest.invoke_source_unit(), STD_INVOKE_SOURCE_UNIT_ID);
    assert_eq!(manifest.output_source_unit(), STD_OUTPUT_SOURCE_UNIT_ID);
    assert_eq!(manifest.ui_source_unit(), STD_UI_SOURCE_UNIT_ID);
    assert_eq!(manifest.types_source_logical_path(), SOURCE_LOGICAL_PATH);
    assert_eq!(
        manifest.invoke_source_logical_path(),
        STD_INVOKE_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        manifest.output_source_logical_path(),
        STD_OUTPUT_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        manifest.ui_source_logical_path(),
        STD_UI_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        manifest.catalogue().revision(),
        STANDARD_CATALOGUE_V4_REVISION_ID
    );
    assert_eq!(manifest.catalogue().schemas().len(), 6);
    assert_eq!(manifest.catalogue().schemas()[5].id(), STD_UI_SCHEMA_ID);
    assert_eq!(manifest.catalogue().value_types().len(), 17);
    assert_eq!(manifest.catalogue().type_bindings().len(), 34);
    assert_eq!(manifest.catalogue().functions().len(), 1);
    assert_eq!(
        manifest.catalogue().functions()[0].id(),
        STD_INVOKE_ECHO_FUNCTION_ID
    );
    assert_eq!(
        cloned.catalogue().revision(),
        STANDARD_CATALOGUE_V4_REVISION_ID
    );

    let ui = manifest
        .catalogue()
        .type_definition_by_id(STD_UI_TYPE_ID)
        .expect("the ui type is retained")
        .as_opaque_value()
        .expect("the ui type is opaque");
    assert_eq!(ui.name().to_string(), "std.ui.ui");
    assert_eq!(ui.representation_contract(), STD_UI_CONTRACT);
    let ui_binding = manifest
        .catalogue()
        .type_bindings()
        .get(33)
        .expect("the ui binding is retained");
    assert_eq!(ui_binding.target(), STD_UI_TYPE_ID);
    assert_eq!(ui_binding.name().to_string(), "std.ui");

    for (actual, expected) in [
        (
            STANDARD_LIBRARY_V4_REVISION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
        ),
        (
            STANDARD_CATALOGUE_V4_REVISION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
        ),
        (
            STANDARD_SOURCE_V4_BUNDLE_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
        ),
        (
            STANDARD_SOURCE_V4_REVISION_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4],
        ),
        (
            STD_UI_SOURCE_UNIT_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5],
        ),
        (
            STD_UI_SCHEMA_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8],
        ),
        (
            STD_UI_TYPE_ID.to_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x13],
        ),
    ] {
        assert_eq!(actual, expected);
    }
    assert_eq!(STD_UI_CONTRACT, "orna.std.value.ui@1");
    assert_eq!(UI_MAGIC, "ORNA-UI/1 ");
    assert_eq!(STD_UI_SOURCE_LOGICAL_PATH, "std/ui.orna");
}

#[test]
fn retains_the_v3_output_standard_snapshot() {
    let snapshot =
        retained_standard_library_v3_snapshot().expect("the retained V3 standard source is valid");

    assert_eq!(snapshot.revision(), STANDARD_LIBRARY_V3_REVISION_ID);
    assert_eq!(
        snapshot.digest_version(),
        StandardLibraryDigestVersion::Version2,
        "orna.std/3 reuses the V2 digest contract (work ADR 0058)"
    );
    assert_eq!(snapshot.language_version(), LANGUAGE_VERSION_IDENTITY);
    assert_eq!(
        snapshot.catalogue().revision(),
        STANDARD_CATALOGUE_V3_REVISION_ID
    );
    assert_eq!(snapshot.source().id(), STANDARD_SOURCE_V3_REVISION_ID);
    assert_eq!(snapshot.source().bundle(), STANDARD_SOURCE_V3_BUNDLE_ID);
    assert_eq!(
        snapshot.source().parent(),
        Some(STANDARD_SOURCE_V2_REVISION_ID),
        "orna.std/3 must be the append-only child of orna.std/2"
    );
    assert_eq!(snapshot.source().units().len(), 3);
    assert_eq!(snapshot.source().units()[0].id(), STD_TYPES_SOURCE_UNIT_ID);
    assert_eq!(snapshot.source().units()[0].ordinal(), 0);
    assert_eq!(
        snapshot.source().units()[0].logical_path(),
        SOURCE_LOGICAL_PATH
    );
    assert_eq!(snapshot.source().units()[1].id(), STD_INVOKE_SOURCE_UNIT_ID);
    assert_eq!(snapshot.source().units()[1].ordinal(), 1);
    assert_eq!(
        snapshot.source().units()[1].logical_path(),
        STD_INVOKE_SOURCE_LOGICAL_PATH
    );
    assert_eq!(snapshot.source().units()[2].id(), STD_OUTPUT_SOURCE_UNIT_ID);
    assert_eq!(snapshot.source().units()[2].ordinal(), 2);
    assert_eq!(
        snapshot.source().units()[2].logical_path(),
        STD_OUTPUT_SOURCE_LOGICAL_PATH
    );
    assert_eq!(snapshot.catalogue().schemas().len(), 5);
    assert_eq!(snapshot.catalogue().value_types().len(), 16);
    assert_eq!(snapshot.catalogue().type_bindings().len(), 33);
    assert_eq!(snapshot.catalogue().functions().len(), 1);
    assert_eq!(snapshot.origins().len(), 56);

    let [executable] = snapshot.executables() else {
        panic!("the V3 snapshot must retain exactly one executable");
    };
    assert_eq!(executable.function(), STD_INVOKE_ECHO_FUNCTION_ID);
    assert_eq!(
        executable.revision().id(),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID
    );
    assert_eq!(
        executable.revision().revision_number(),
        STD_INVOKE_ECHO_REVISION_NUMBER
    );
}

#[test]
fn v3_output_source_has_the_exact_literal_bytes_and_parse() {
    let snapshot =
        retained_standard_library_v3_snapshot().expect("the retained V3 standard source is valid");
    let types = snapshot.source().units()[0].content();
    let invoke = snapshot.source().units()[1].content();
    let output = snapshot.source().units()[2].content();

    assert_eq!(output, EXPECTED_RETAINED_OUTPUT_SOURCE);
    assert!(output.is_ascii());
    assert!(!output.as_bytes().starts_with(&[0xef, 0xbb, 0xbf]));
    assert!(!output.contains('\r'));
    assert!(output.ends_with('\n'));
    assert!(!output[..output.len() - 1].ends_with('\n'));
    assert_eq!(output.matches(';').count(), 6);
    assert_eq!(types, super::super::RETAINED_STANDARD_SOURCE);
    assert_eq!(invoke, super::super::RETAINED_STANDARD_INVOKE_SOURCE);

    let parsed = orna_syntax::parse(output);
    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), output);
    assert_eq!(parsed.schemas().len(), 2);
    assert_eq!(parsed.opaque_value_types().len(), 2);
    assert_eq!(parsed.type_exports().len(), 2);
    assert!(parsed.object_types().is_empty());
    assert!(parsed.field_renames().is_empty());
    assert!(parsed.primitive_value_types().is_empty());
    assert!(parsed.record_value_types().is_empty());
    assert!(parsed.enum_types().is_empty());
    assert!(parsed.server_functions().is_empty());
    assert!(parsed.client_functions().is_empty());
}

#[test]
fn v3_output_origins_cover_the_exact_declaration_ranges() {
    let snapshot =
        retained_standard_library_v3_snapshot().expect("the retained V3 standard source is valid");
    let output = snapshot.source().units()[2].content();
    let output_origins = snapshot
        .origins()
        .iter()
        .filter(|origin| origin.source().source_unit() == STD_OUTPUT_SOURCE_UNIT_ID)
        .collect::<Vec<_>>();
    assert_eq!(output_origins.len(), 6);

    let terminal_schema_end = "CREATE SCHEMA std.terminal;".len();
    let io_schema_start = output
        .find("CREATE SCHEMA std.io;")
        .expect("the io schema is retained");
    let document_start = output
        .find("CREATE TYPE std.terminal.Document")
        .expect("the document type is retained");
    let document_end = output
        .find("TRANSIENT;")
        .expect("the document type is retained")
        + "TRANSIENT;".len();
    let document_binding_start = output
        .find("EXPORT TYPE std.terminal.Document AS std.Document;")
        .expect("the document binding is retained");
    let bytestream_start = output
        .find("CREATE TYPE std.io.ByteStream")
        .expect("the byte-stream type is retained");
    let bytestream_end = output
        .rfind("TRANSIENT;")
        .expect("the byte-stream type is retained")
        + "TRANSIENT;".len();

    let schema_origin = |id: orna_core::SchemaId| {
        output_origins
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::Schema(id))
            .expect("the schema origin is retained")
            .source()
    };
    let type_origin = |id: orna_core::TypeId| {
        output_origins
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::ValueType(id))
            .expect("the type origin is retained")
            .source()
    };
    let binding_origin = |id: orna_core::TypeBindingId| {
        output_origins
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::TypeBinding(id))
            .expect("the binding origin is retained")
            .source()
    };

    let terminal_schema = schema_origin(STD_TERMINAL_SCHEMA_ID);
    assert_eq!(terminal_schema.byte_start(), 0);
    assert_eq!(terminal_schema.byte_end(), terminal_schema_end as u32);
    let io_schema = schema_origin(STD_IO_SCHEMA_ID);
    assert_eq!(io_schema.byte_start(), io_schema_start as u32);
    assert_eq!(
        io_schema.byte_end(),
        (io_schema_start + "CREATE SCHEMA std.io;".len()) as u32
    );

    let document = type_origin(STD_TERMINAL_DOCUMENT_TYPE_ID);
    assert_eq!(document.byte_start(), document_start as u32);
    assert_eq!(document.byte_end(), document_end as u32);
    let document_binding = binding_origin(
        snapshot
            .catalogue()
            .type_bindings()
            .get(31)
            .expect("the document binding is retained")
            .id(),
    );
    assert_eq!(document_binding.byte_start(), document_binding_start as u32);
    assert_eq!(
        document_binding.byte_end(),
        (document_binding_start + "EXPORT TYPE std.terminal.Document AS std.Document;".len())
            as u32
    );

    let bytestream = type_origin(STD_IO_BYTE_STREAM_TYPE_ID);
    assert_eq!(bytestream.byte_start(), bytestream_start as u32);
    assert_eq!(bytestream.byte_end(), bytestream_end as u32);
    let bytestream_binding = binding_origin(
        snapshot
            .catalogue()
            .type_bindings()
            .get(32)
            .expect("the byte-stream binding is retained")
            .id(),
    );
    let bytestream_binding_start = output
        .find("EXPORT TYPE std.io.ByteStream AS std.ByteStream;")
        .expect("the byte-stream binding is retained");
    assert_eq!(
        bytestream_binding.byte_start(),
        bytestream_binding_start as u32
    );
    assert_eq!(
        bytestream_binding.byte_end(),
        (bytestream_binding_start + "EXPORT TYPE std.io.ByteStream AS std.ByteStream;".len())
            as u32
    );
}

#[test]
fn v3_digest_goldens_are_computed_from_the_retained_source() {
    let snapshot =
        retained_standard_library_v3_snapshot().expect("the retained V3 standard source is valid");
    let types = snapshot.source().units()[0].content();
    let invoke = snapshot.source().units()[1].content();
    let output = snapshot.source().units()[2].content();
    let units = snapshot.source().units().to_vec();
    let executable = &snapshot.executables()[0];
    let function = snapshot
        .catalogue()
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .expect("the echo function is retained");
    let artifact = executable.revision().artifact();
    let references = executable.references();

    assert_eq!(
        source_unit_content_digest(types).expect("the types content digest is valid"),
        super::super::ACCEPTED_V3_TYPES_CONTENT_DIGEST
    );
    assert_eq!(
        source_unit_content_digest(invoke).expect("the invoke content digest is valid"),
        super::super::ACCEPTED_V3_INVOKE_CONTENT_DIGEST
    );
    assert_eq!(
        source_unit_content_digest(output).expect("the output content digest is valid"),
        super::super::ACCEPTED_V3_OUTPUT_CONTENT_DIGEST
    );
    assert_eq!(
        source_bundle_digest(&units).expect("the bundle digest is valid"),
        super::super::ACCEPTED_V3_SOURCE_BUNDLE_DIGEST
    );
    assert_eq!(
        source_revision_record_digest(
            STANDARD_SOURCE_V3_BUNDLE_ID,
            Some(STANDARD_SOURCE_V2_REVISION_ID),
            snapshot.source().bundle_hash(),
        )
        .expect("the source revision digest is valid"),
        super::super::ACCEPTED_V3_SOURCE_REVISION_DIGEST
    );
    assert_eq!(
        artifact_payload_digest(artifact.payload()).expect("the artifact digest is valid"),
        super::super::ACCEPTED_V3_ARTIFACT_DIGEST,
        "orna.std/3 retains the exact V2 parameter-echo artifact"
    );
    assert_eq!(
        function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            function,
            LANGUAGE_VERSION_IDENTITY,
            artifact,
            &[],
            references,
        )
        .expect("the semantic digest is valid"),
        super::super::ACCEPTED_V3_SEMANTIC_DIGEST,
        "orna.std/3 retains the exact V2 semantic digest"
    );
    assert_eq!(
        snapshot.digest(),
        super::super::ACCEPTED_V3_STANDARD_LIBRARY_DIGEST
    );
    assert_eq!(
        standard_library_digest(&snapshot).expect("the retained digest recomputes"),
        super::super::ACCEPTED_V3_STANDARD_LIBRARY_DIGEST
    );
}

#[test]
fn v3_standard_digest_binds_every_retained_byte() {
    let snapshot =
        retained_standard_library_v3_snapshot().expect("the retained V3 standard source is valid");
    let types = snapshot.source().units()[0].content();
    let invoke = snapshot.source().units()[1].content();
    let output = snapshot.source().units()[2].content();

    let tampered_types = format!("{types} ");
    let tampered_invoke = format!("{invoke} ");
    let tampered_output = format!("{output} ");
    for tampered in [
        tampered_v3_snapshot(&tampered_types, invoke, output),
        tampered_v3_snapshot(types, &tampered_invoke, output),
        tampered_v3_snapshot(types, invoke, &tampered_output),
    ] {
        assert_eq!(
            tampered.digest(),
            super::super::ACCEPTED_V3_STANDARD_LIBRARY_DIGEST
        );
        assert!(matches!(
            standard_library_digest(&tampered),
            Err(
                orna_core::canonical_hash::CanonicalHashError::StandardLibraryDigestMismatch { .. }
            )
        ));
        assert!(verify_standard_library_v3_snapshot(tampered).is_err());
    }
}

#[test]
fn v3_snapshot_verifies_and_rejects_and_is_rejected_by_the_other_verifiers() {
    let version_one =
        retained_standard_library_snapshot().expect("the retained V1 source is valid");
    let version_two =
        retained_standard_library_v2_snapshot().expect("the retained V2 source is valid");
    let version_three =
        retained_standard_library_v3_snapshot().expect("the retained V3 source is valid");

    assert!(verify_standard_library_v3_snapshot(version_three.clone()).is_ok());
    assert!(matches!(
        verify_standard_library_v3_snapshot(version_one.clone()),
        Err(super::super::StandardLibraryError::CatalogueIdentityMismatch {
            expected,
            actual
        }) if expected == STANDARD_CATALOGUE_V3_REVISION_ID
            && actual == STANDARD_CATALOGUE_REVISION_ID
    ));
    assert!(matches!(
        verify_standard_library_v3_snapshot(version_two.clone()),
        Err(super::super::StandardLibraryError::CatalogueIdentityMismatch {
            expected,
            actual
        }) if expected == STANDARD_CATALOGUE_V3_REVISION_ID
            && actual == STANDARD_CATALOGUE_V2_REVISION_ID
    ));
    assert!(matches!(
        verify_standard_library_snapshot(version_three.clone()),
        Err(super::super::StandardLibraryError::CatalogueIdentityMismatch {
            expected,
            actual
        }) if expected == STANDARD_CATALOGUE_REVISION_ID
            && actual == STANDARD_CATALOGUE_V3_REVISION_ID
    ));
    assert!(matches!(
        verify_standard_library_v2_snapshot(version_three.clone()),
        Err(super::super::StandardLibraryError::CatalogueIdentityMismatch {
            expected,
            actual
        }) if expected == STANDARD_CATALOGUE_V2_REVISION_ID
            && actual == STANDARD_CATALOGUE_V3_REVISION_ID
    ));
}

#[test]
fn retained_v4_ui_reconciliation_rejects_mutable_ui_declaration() {
    let mutable_ui = RETAINED_STANDARD_UI_SOURCE.replace("\n    IMMUTABLE\n", "\n    MUTABLE\n");
    let manifest = standard_library_v4_manifest().expect("the V4 manifest is valid");

    assert!(matches!(
        super::super::reconcile_retained_ui_source(&mutable_ui, manifest.catalogue()),
        Err(StandardLibraryError::RetainedSourceMismatch)
    ));
}

#[test]
fn retained_v4_ui_reconciliation_accepts_canonical_immutable_catalogue() {
    let manifest = standard_library_v4_manifest().expect("the V4 manifest is valid");
    let ui_definition = manifest
        .catalogue()
        .type_definition_by_id(STD_UI_TYPE_ID)
        .expect("the canonical UI definition is retained")
        .as_opaque_value()
        .expect("the canonical UI definition is opaque");
    assert!(matches!(
        ui_definition.mutability(),
        ValueTypeMutability::Immutable
    ));

    let origins = super::super::reconcile_retained_ui_source(
        RETAINED_STANDARD_UI_SOURCE,
        manifest.catalogue(),
    )
    .expect("the canonical immutable UI source reconciles");
    assert_eq!(origins.len(), 3);
    assert_eq!(
        origins[0].identity(),
        DefinitionIdentity::Schema(STD_UI_SCHEMA_ID)
    );
    assert_eq!(
        origins[1].identity(),
        DefinitionIdentity::ValueType(STD_UI_TYPE_ID)
    );
    assert_eq!(
        origins[2].identity(),
        DefinitionIdentity::TypeBinding(
            manifest
                .catalogue()
                .type_bindings()
                .get(33)
                .expect("the canonical UI binding is retained")
                .id(),
        )
    );
}

#[test]
fn retains_the_v4_ui_standard_snapshot() {
    let snapshot =
        retained_standard_library_v4_snapshot().expect("the retained V4 standard source is valid");

    assert_eq!(snapshot.revision(), STANDARD_LIBRARY_V4_REVISION_ID);
    assert_eq!(
        snapshot.digest_version(),
        StandardLibraryDigestVersion::Version2,
        "orna.std/4 reuses the V2 digest contract (work ADR 0062)"
    );
    assert_eq!(snapshot.language_version(), LANGUAGE_VERSION_IDENTITY);
    assert_eq!(
        snapshot.catalogue().revision(),
        STANDARD_CATALOGUE_V4_REVISION_ID
    );
    assert_eq!(snapshot.source().id(), STANDARD_SOURCE_V4_REVISION_ID);
    assert_eq!(snapshot.source().bundle(), STANDARD_SOURCE_V4_BUNDLE_ID);
    assert_eq!(
        snapshot.source().parent(),
        Some(STANDARD_SOURCE_V3_REVISION_ID),
        "orna.std/4 must be the append-only child of orna.std/3"
    );
    assert_eq!(snapshot.source().units().len(), 4);
    assert_eq!(snapshot.source().units()[0].id(), STD_TYPES_SOURCE_UNIT_ID);
    assert_eq!(snapshot.source().units()[0].ordinal(), 0);
    assert_eq!(
        snapshot.source().units()[0].logical_path(),
        SOURCE_LOGICAL_PATH
    );
    assert_eq!(snapshot.source().units()[1].id(), STD_INVOKE_SOURCE_UNIT_ID);
    assert_eq!(snapshot.source().units()[1].ordinal(), 1);
    assert_eq!(
        snapshot.source().units()[1].logical_path(),
        STD_INVOKE_SOURCE_LOGICAL_PATH
    );
    assert_eq!(snapshot.source().units()[2].id(), STD_OUTPUT_SOURCE_UNIT_ID);
    assert_eq!(snapshot.source().units()[2].ordinal(), 2);
    assert_eq!(
        snapshot.source().units()[2].logical_path(),
        STD_OUTPUT_SOURCE_LOGICAL_PATH
    );
    assert_eq!(snapshot.source().units()[3].id(), STD_UI_SOURCE_UNIT_ID);
    assert_eq!(snapshot.source().units()[3].ordinal(), 3);
    assert_eq!(
        snapshot.source().units()[3].logical_path(),
        STD_UI_SOURCE_LOGICAL_PATH
    );
    assert_eq!(snapshot.catalogue().schemas().len(), 6);
    assert_eq!(snapshot.catalogue().value_types().len(), 17);
    assert_eq!(snapshot.catalogue().type_bindings().len(), 34);
    assert_eq!(snapshot.catalogue().functions().len(), 1);
    assert_eq!(snapshot.origins().len(), 59);

    let [executable] = snapshot.executables() else {
        panic!("the V4 snapshot must retain exactly one executable");
    };
    assert_eq!(executable.function(), STD_INVOKE_ECHO_FUNCTION_ID);
    assert_eq!(
        executable.revision().id(),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID
    );
    assert_eq!(
        executable.revision().revision_number(),
        STD_INVOKE_ECHO_REVISION_NUMBER
    );
}

#[test]
fn v4_ui_source_has_the_exact_literal_bytes_and_parse() {
    let snapshot =
        retained_standard_library_v4_snapshot().expect("the retained V4 standard source is valid");
    let types = snapshot.source().units()[0].content();
    let invoke = snapshot.source().units()[1].content();
    let output = snapshot.source().units()[2].content();
    let ui = snapshot.source().units()[3].content();

    assert_eq!(ui, EXPECTED_RETAINED_UI_SOURCE);
    assert!(ui.is_ascii());
    assert!(!ui.as_bytes().starts_with(&[0xef, 0xbb, 0xbf]));
    assert!(!ui.contains('\r'));
    assert!(ui.ends_with('\n'));
    assert!(!ui[..ui.len() - 1].ends_with('\n'));
    assert_eq!(ui.matches(';').count(), 3);
    assert_eq!(types, super::super::RETAINED_STANDARD_SOURCE);
    assert_eq!(invoke, super::super::RETAINED_STANDARD_INVOKE_SOURCE);
    assert_eq!(output, super::super::RETAINED_STANDARD_OUTPUT_SOURCE);

    let parsed = orna_syntax::parse(ui);
    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), ui);
    assert_eq!(parsed.schemas().len(), 1);
    assert!(super::super::matches_qualified_name(
        &parsed.schemas()[0].name,
        &QualifiedSemanticName::new(["std", "ui"]).expect("the fixed schema name is valid")
    ));
    assert_eq!(parsed.opaque_value_types().len(), 1);
    assert_eq!(
        super::super::decode_sql_string_literal(
            &parsed.opaque_value_types()[0].kernel_contract.text
        )
        .as_deref(),
        Some(STD_UI_CONTRACT)
    );
    assert_eq!(parsed.type_exports().len(), 1);
    assert!(parsed.object_types().is_empty());
    assert!(parsed.field_renames().is_empty());
    assert!(parsed.primitive_value_types().is_empty());
    assert!(parsed.record_value_types().is_empty());
    assert!(parsed.enum_types().is_empty());
    assert!(parsed.server_functions().is_empty());
    assert!(parsed.client_functions().is_empty());
}

#[test]
fn v4_ui_origins_cover_the_exact_declaration_ranges() {
    let snapshot =
        retained_standard_library_v4_snapshot().expect("the retained V4 standard source is valid");
    let ui = snapshot.source().units()[3].content();
    let ui_origins = snapshot
        .origins()
        .iter()
        .filter(|origin| origin.source().source_unit() == STD_UI_SOURCE_UNIT_ID)
        .collect::<Vec<_>>();
    assert_eq!(ui_origins.len(), 3);

    let schema = ui_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_UI_SCHEMA_ID))
        .expect("the schema origin is retained")
        .source();
    assert_eq!(schema.byte_start(), 0);
    assert_eq!(schema.byte_end(), "CREATE SCHEMA std.ui;".len() as u32);

    let type_origin = ui_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::ValueType(STD_UI_TYPE_ID))
        .expect("the type origin is retained")
        .source();
    let type_start = ui
        .find("CREATE TYPE std.ui.UI")
        .expect("the ui type is retained");
    let type_end = ui.find("TRANSIENT;").expect("the ui type is retained") + "TRANSIENT;".len();
    assert_eq!(type_origin.byte_start(), type_start as u32);
    assert_eq!(type_origin.byte_end(), type_end as u32);

    let binding_origin = ui_origins
        .iter()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::TypeBinding(
                    snapshot
                        .catalogue()
                        .type_bindings()
                        .get(33)
                        .expect("the ui binding is retained")
                        .id(),
                )
        })
        .expect("the binding origin is retained")
        .source();
    let binding_start = ui
        .find("EXPORT TYPE std.ui.UI AS std.UI;")
        .expect("the ui binding is retained");
    assert_eq!(binding_origin.byte_start(), binding_start as u32);
    assert_eq!(
        binding_origin.byte_end(),
        (binding_start + "EXPORT TYPE std.ui.UI AS std.UI;".len()) as u32
    );
}

#[test]
fn v4_digest_goldens_are_computed_from_the_retained_source() {
    let snapshot =
        retained_standard_library_v4_snapshot().expect("the retained V4 standard source is valid");
    let types = snapshot.source().units()[0].content();
    let invoke = snapshot.source().units()[1].content();
    let output = snapshot.source().units()[2].content();
    let ui = snapshot.source().units()[3].content();
    let units = snapshot.source().units().to_vec();
    let executable = &snapshot.executables()[0];
    let function = snapshot
        .catalogue()
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .expect("the echo function is retained");
    let artifact = executable.revision().artifact();
    let references = executable.references();

    assert_eq!(
        source_unit_content_digest(types).expect("the types content digest is valid"),
        super::super::ACCEPTED_V4_TYPES_CONTENT_DIGEST
    );
    assert_eq!(
        source_unit_content_digest(invoke).expect("the invoke content digest is valid"),
        super::super::ACCEPTED_V4_INVOKE_CONTENT_DIGEST
    );
    assert_eq!(
        source_unit_content_digest(output).expect("the output content digest is valid"),
        super::super::ACCEPTED_V4_OUTPUT_CONTENT_DIGEST
    );
    assert_eq!(
        source_unit_content_digest(ui).expect("the ui content digest is valid"),
        super::super::ACCEPTED_V4_UI_CONTENT_DIGEST
    );
    assert_eq!(
        source_bundle_digest(&units).expect("the bundle digest is valid"),
        super::super::ACCEPTED_V4_SOURCE_BUNDLE_DIGEST
    );
    assert_eq!(
        source_revision_record_digest(
            STANDARD_SOURCE_V4_BUNDLE_ID,
            Some(STANDARD_SOURCE_V3_REVISION_ID),
            snapshot.source().bundle_hash(),
        )
        .expect("the source revision digest is valid"),
        super::super::ACCEPTED_V4_SOURCE_REVISION_DIGEST
    );
    assert_eq!(
        artifact_payload_digest(artifact.payload()).expect("the artifact digest is valid"),
        super::super::ACCEPTED_V4_ARTIFACT_DIGEST,
        "orna.std/4 retains the exact V2 parameter-echo artifact"
    );
    assert_eq!(
        function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            function,
            LANGUAGE_VERSION_IDENTITY,
            artifact,
            &[],
            references,
        )
        .expect("the semantic digest is valid"),
        super::super::ACCEPTED_V4_SEMANTIC_DIGEST,
        "orna.std/4 retains the exact V3 semantic digest"
    );
    assert_eq!(
        snapshot.digest(),
        super::super::ACCEPTED_V4_STANDARD_LIBRARY_DIGEST
    );
    assert_eq!(
        standard_library_digest(&snapshot).expect("the retained digest recomputes"),
        super::super::ACCEPTED_V4_STANDARD_LIBRARY_DIGEST
    );
}

#[test]
fn v4_standard_digest_binds_every_retained_byte() {
    let snapshot =
        retained_standard_library_v4_snapshot().expect("the retained V4 standard source is valid");
    let types = snapshot.source().units()[0].content();
    let invoke = snapshot.source().units()[1].content();
    let output = snapshot.source().units()[2].content();
    let ui = snapshot.source().units()[3].content();

    let tampered_types = format!("{types} ");
    let tampered_invoke = format!("{invoke} ");
    let tampered_output = format!("{output} ");
    let tampered_ui = format!("{ui} ");
    for tampered in [
        tampered_v4_snapshot(&tampered_types, invoke, output, ui),
        tampered_v4_snapshot(types, &tampered_invoke, output, ui),
        tampered_v4_snapshot(types, invoke, &tampered_output, ui),
        tampered_v4_snapshot(types, invoke, output, &tampered_ui),
    ] {
        assert_eq!(
            tampered.digest(),
            super::super::ACCEPTED_V4_STANDARD_LIBRARY_DIGEST
        );
        assert!(matches!(
            standard_library_digest(&tampered),
            Err(
                orna_core::canonical_hash::CanonicalHashError::StandardLibraryDigestMismatch { .. }
            )
        ));
        assert!(verify_standard_library_v4_snapshot(tampered).is_err());
    }
}

#[test]
fn v4_snapshot_verifies_and_rejects_and_is_rejected_by_the_other_verifiers() {
    let version_one =
        retained_standard_library_snapshot().expect("the retained V1 source is valid");
    let version_two =
        retained_standard_library_v2_snapshot().expect("the retained V2 source is valid");
    let version_three =
        retained_standard_library_v3_snapshot().expect("the retained V3 source is valid");
    let version_four =
        retained_standard_library_v4_snapshot().expect("the retained V4 source is valid");

    assert!(verify_standard_library_v4_snapshot(version_four.clone()).is_ok());
    assert!(matches!(
        verify_standard_library_v4_snapshot(version_one.clone()),
        Err(super::super::StandardLibraryError::CatalogueIdentityMismatch {
            expected,
            actual
        }) if expected == STANDARD_CATALOGUE_V4_REVISION_ID
            && actual == STANDARD_CATALOGUE_REVISION_ID
    ));
    assert!(matches!(
        verify_standard_library_v4_snapshot(version_two.clone()),
        Err(super::super::StandardLibraryError::CatalogueIdentityMismatch {
            expected,
            actual
        }) if expected == STANDARD_CATALOGUE_V4_REVISION_ID
            && actual == STANDARD_CATALOGUE_V2_REVISION_ID
    ));
    assert!(matches!(
        verify_standard_library_v4_snapshot(version_three.clone()),
        Err(super::super::StandardLibraryError::CatalogueIdentityMismatch {
            expected,
            actual
        }) if expected == STANDARD_CATALOGUE_V4_REVISION_ID
            && actual == STANDARD_CATALOGUE_V3_REVISION_ID
    ));
    assert!(matches!(
        verify_standard_library_snapshot(version_four.clone()),
        Err(super::super::StandardLibraryError::CatalogueIdentityMismatch {
            expected,
            actual
        }) if expected == STANDARD_CATALOGUE_REVISION_ID
            && actual == STANDARD_CATALOGUE_V4_REVISION_ID
    ));
    assert!(matches!(
        verify_standard_library_v2_snapshot(version_four.clone()),
        Err(super::super::StandardLibraryError::CatalogueIdentityMismatch {
            expected,
            actual
        }) if expected == STANDARD_CATALOGUE_V2_REVISION_ID
            && actual == STANDARD_CATALOGUE_V4_REVISION_ID
    ));
    assert!(matches!(
        verify_standard_library_v3_snapshot(version_four.clone()),
        Err(super::super::StandardLibraryError::CatalogueIdentityMismatch {
            expected,
            actual
        }) if expected == STANDARD_CATALOGUE_V3_REVISION_ID
            && actual == STANDARD_CATALOGUE_V4_REVISION_ID
    ));
}

#[test]
fn v3_registered_opaque_codecs_construct_the_output_payloads() {
    let verified = verify_standard_library_v3_snapshot(
        retained_standard_library_v3_snapshot().expect("the retained V3 standard source is valid"),
    )
    .expect("the retained V3 standard source verifies");
    let registry = registered_opaque_codecs(&verified).expect("the V3 opaque codecs register");
    let active = empty_version_two_active_revision(&verified);

    let mut document_payload = Vec::from(TERMINAL_DOCUMENT_MAGIC.as_bytes());
    document_payload.extend_from_slice(&6_u32.to_be_bytes());
    document_payload.extend_from_slice(b"hello\n");
    let document = OpaqueValue::new(
        &active,
        &registry,
        STD_TERMINAL_DOCUMENT_TYPE_ID,
        &document_payload,
    )
    .expect("the terminal document payload constructs");
    assert_eq!(document.opaque_type(), STD_TERMINAL_DOCUMENT_TYPE_ID);
    assert_eq!(document.canonical_payload(), document_payload);

    let mut byte_stream_payload = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
    byte_stream_payload.extend_from_slice(&16_u32.to_be_bytes());
    byte_stream_payload.extend_from_slice(b"application/json");
    byte_stream_payload.extend_from_slice(&2_u32.to_be_bytes());
    byte_stream_payload.extend_from_slice(b"{}");
    let byte_stream = OpaqueValue::new(
        &active,
        &registry,
        STD_IO_BYTE_STREAM_TYPE_ID,
        &byte_stream_payload,
    )
    .expect("the byte-stream payload constructs");
    assert_eq!(byte_stream.opaque_type(), STD_IO_BYTE_STREAM_TYPE_ID);
    assert_eq!(byte_stream.canonical_payload(), byte_stream_payload);

    assert_eq!(
        OpaqueValue::new(
            &active,
            &registry,
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            b"WRONG-DOCUMENT/1 \0\0\0\0",
        ),
        Err(OpaqueValueError::InvalidMagic {
            opaque_type: STD_TERMINAL_DOCUMENT_TYPE_ID,
        })
    );
    let mut empty_media_type = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
    empty_media_type.extend_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        OpaqueValue::new(
            &active,
            &registry,
            STD_IO_BYTE_STREAM_TYPE_ID,
            &empty_media_type,
        ),
        Err(OpaqueValueError::InvalidMediaType {
            opaque_type: STD_IO_BYTE_STREAM_TYPE_ID,
        })
    );

    // The V3 registry also retains the fixed-length opaque-token codec.
    let token = OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0xab; 16])
        .expect("the opaque-token payload constructs");
    assert_eq!(token.opaque_type(), OPAQUE_TOKEN_TYPE_ID);

    // The V1 and V2 registries stay unchanged: only the opaque-token codec.
    let version_one = verify_standard_library_snapshot(
        retained_standard_library_snapshot().expect("the retained V1 source is valid"),
    )
    .expect("the retained V1 standard source verifies");
    let version_one_registry =
        registered_opaque_codecs(&version_one).expect("the V1 opaque codecs register");
    let version_two = verify_standard_library_v2_snapshot(
        retained_standard_library_v2_snapshot().expect("the retained V2 source is valid"),
    )
    .expect("the retained V2 standard source verifies");
    let version_two_registry =
        registered_opaque_codecs(&version_two).expect("the V2 opaque codecs register");
    let active_one = empty_version_two_active_revision(&version_one);
    let active_two = empty_version_two_active_revision(&version_two);
    for (active, registry) in [
        (&active_one, &version_one_registry),
        (&active_two, &version_two_registry),
    ] {
        assert_eq!(
            OpaqueValue::new(active, registry, OPAQUE_TOKEN_TYPE_ID, [0xab; 16])
                .expect("the opaque-token payload constructs"),
            OpaqueValue::new(active, registry, OPAQUE_TOKEN_TYPE_ID, [0xab; 16])
                .expect("the opaque-token payload constructs"),
        );
        assert_eq!(
            OpaqueValue::new(active, registry, STD_TERMINAL_DOCUMENT_TYPE_ID, [0; 16]),
            Err(OpaqueValueError::UnregisteredType {
                opaque_type: STD_TERMINAL_DOCUMENT_TYPE_ID,
            })
        );
    }
}

#[test]
fn v4_registered_opaque_codecs_construct_the_ui_payloads() {
    let verified = verify_standard_library_v4_snapshot(
        retained_standard_library_v4_snapshot().expect("the retained V4 standard source is valid"),
    )
    .expect("the retained V4 standard source verifies");
    let registry = registered_opaque_codecs(&verified).expect("the V4 opaque codecs register");
    let active = empty_version_two_active_revision(&verified);

    let body = br#"{"kind":"empty"}"#;
    let mut ui_payload = Vec::from(UI_MAGIC.as_bytes());
    ui_payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
    ui_payload.extend_from_slice(body);
    let ui = OpaqueValue::new(&active, &registry, STD_UI_TYPE_ID, &ui_payload)
        .expect("the ui payload constructs");
    assert_eq!(ui.opaque_type(), STD_UI_TYPE_ID);
    assert_eq!(ui.canonical_payload(), ui_payload);

    let noncanonical_body = br#"{ "kind":"empty"}"#;
    let mut noncanonical_payload = Vec::from(UI_MAGIC.as_bytes());
    noncanonical_payload.extend_from_slice(&(noncanonical_body.len() as u32).to_be_bytes());
    noncanonical_payload.extend_from_slice(noncanonical_body);
    assert_eq!(
        OpaqueValue::new(&active, &registry, STD_UI_TYPE_ID, &noncanonical_payload,),
        Err(OpaqueValueError::InvalidJsonBody {
            opaque_type: STD_UI_TYPE_ID,
        })
    );

    let malformed_body = br#"{"kind":}"#;
    let mut malformed_payload = Vec::from(UI_MAGIC.as_bytes());
    malformed_payload.extend_from_slice(&(malformed_body.len() as u32).to_be_bytes());
    malformed_payload.extend_from_slice(malformed_body);
    assert_eq!(
        OpaqueValue::new(&active, &registry, STD_UI_TYPE_ID, &malformed_payload),
        Err(OpaqueValueError::InvalidJsonBody {
            opaque_type: STD_UI_TYPE_ID,
        })
    );

    let mut wrong_length_payload = ui_payload.clone();
    wrong_length_payload[UI_MAGIC.len()..UI_MAGIC.len() + 4]
        .copy_from_slice(&((body.len() as u32) - 1).to_be_bytes());
    assert_eq!(
        OpaqueValue::new(&active, &registry, STD_UI_TYPE_ID, &wrong_length_payload,),
        Err(OpaqueValueError::InvalidFrameLength {
            opaque_type: STD_UI_TYPE_ID,
        })
    );

    for body in [
        br#"{"children":[{"kind":"empty"}],"kind":"fragment"}"#.as_slice(),
        br#"{"actions":{},"contract":{"id":"std.ui.window@1","name":"std.ui.window","version":"1.0"},"kind":"node","properties":{},"slots":{}}"#.as_slice(),
    ] {
        let mut payload = Vec::from(UI_MAGIC.as_bytes());
        payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
        payload.extend_from_slice(body);
        let value = OpaqueValue::new(&active, &registry, STD_UI_TYPE_ID, &payload)
            .expect("the closed UI shape constructs");
        assert_eq!(value.canonical_payload(), payload.as_slice());
    }

    for body in [
        br#"{"kind":"not-a-ui-kind"}"#.as_slice(),
        br#"{"actions":{},"contract":{"id":"std.ui.window@1","name":"std.ui.window","version":"1.0"},"kind":"node","properties":{},"slots":{},"unknown":null}"#.as_slice(),
        br#"{"children":[{"kind":"not-a-ui-kind"}],"kind":"fragment"}"#.as_slice(),
    ] {
        let mut payload = Vec::from(UI_MAGIC.as_bytes());
        payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
        payload.extend_from_slice(body);
        assert_eq!(
            OpaqueValue::new(&active, &registry, STD_UI_TYPE_ID, &payload),
            Err(OpaqueValueError::InvalidJsonBody {
                opaque_type: STD_UI_TYPE_ID,
            })
        );
    }

    // The V4 registry also binds the opaque-token, terminal-document, and
    // byte-stream codecs unchanged.
    let token = OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0xab; 16])
        .expect("the opaque-token payload constructs");
    assert_eq!(token.opaque_type(), OPAQUE_TOKEN_TYPE_ID);
    let mut document_payload = Vec::from(TERMINAL_DOCUMENT_MAGIC.as_bytes());
    document_payload.extend_from_slice(&3_u32.to_be_bytes());
    document_payload.extend_from_slice(b"{}\n");
    let document = OpaqueValue::new(
        &active,
        &registry,
        STD_TERMINAL_DOCUMENT_TYPE_ID,
        &document_payload,
    )
    .expect("the terminal document payload constructs");
    assert_eq!(document.opaque_type(), STD_TERMINAL_DOCUMENT_TYPE_ID);
    let mut byte_stream_payload = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
    byte_stream_payload.extend_from_slice(&16_u32.to_be_bytes());
    byte_stream_payload.extend_from_slice(b"application/json");
    byte_stream_payload.extend_from_slice(&0_u32.to_be_bytes());
    let byte_stream = OpaqueValue::new(
        &active,
        &registry,
        STD_IO_BYTE_STREAM_TYPE_ID,
        &byte_stream_payload,
    )
    .expect("the byte-stream payload constructs");
    assert_eq!(byte_stream.opaque_type(), STD_IO_BYTE_STREAM_TYPE_ID);

    assert_eq!(
        OpaqueValue::new(&active, &registry, STD_UI_TYPE_ID, b"WRONG-UI/1 \0\0\0\0"),
        Err(OpaqueValueError::InvalidMagic {
            opaque_type: STD_UI_TYPE_ID,
        })
    );
    // The V3 registry does not yet bind the ui codec.
    let version_three = verify_standard_library_v3_snapshot(
        retained_standard_library_v3_snapshot().expect("the retained V3 standard source is valid"),
    )
    .expect("the retained V3 standard source verifies");
    let version_three_registry =
        registered_opaque_codecs(&version_three).expect("the V3 opaque codecs register");
    let active_three = empty_version_two_active_revision(&version_three);
    assert_eq!(
        OpaqueValue::new(
            &active_three,
            &version_three_registry,
            STD_UI_TYPE_ID,
            &ui_payload
        ),
        Err(OpaqueValueError::UnregisteredType {
            opaque_type: STD_UI_TYPE_ID,
        })
    );
}

#[test]
fn prepares_the_v2_to_v3_standard_upgrade_from_an_empty_v2_active_revision() {
    let version_two = verify_standard_library_v2_snapshot(
        retained_standard_library_v2_snapshot().expect("the retained V2 source is valid"),
    )
    .expect("the retained V2 standard source verifies");
    let active = empty_version_two_active_revision(&version_two);

    let upgrade =
        prepare_standard_upgrade_v2_to_v3(&active).expect("the V2-to-V3 standard upgrade prepares");

    assert_eq!(
        upgrade
            .checked_standard_library()
            .verified_snapshot()
            .revision(),
        STANDARD_LIBRARY_V3_REVISION_ID
    );
    assert_eq!(
        upgrade.verified_standard_snapshot().revision(),
        STANDARD_LIBRARY_V3_REVISION_ID
    );
    assert_eq!(
        upgrade.verified_standard_snapshot().source().parent(),
        Some(STANDARD_SOURCE_V2_REVISION_ID),
        "V3 must be the append-only child of the retained V2 source revision"
    );
    let executable = upgrade
        .checked_standard_library()
        .checked_executable()
        .expect("the V3 upgrade retains the executable");
    assert_eq!(executable.function_id(), STD_INVOKE_ECHO_FUNCTION_ID);
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
        Some(STANDARD_LIBRARY_V3_REVISION_ID)
    );
    assert_eq!(
        upgrade
            .application_revision()
            .catalogue_hash_context()
            .standard()
            .map(|snapshot| snapshot.digest_version()),
        Some(StandardLibraryDigestVersion::Version2)
    );
}

#[test]
fn append_only_standard_upgrades_require_an_installed_parent() {
    let active = empty_active_revision();
    let expected_error = "the standard library is not installed";

    for (label, result) in [
        (
            "V2-to-V3",
            super::super::prepare_standard_upgrade_v2_to_v3(&active),
        ),
        (
            "V3-to-V4",
            super::super::prepare_standard_upgrade_v3_to_v4(&active),
        ),
        (
            "V4-to-V5",
            super::super::prepare_standard_upgrade_v4_to_v5(&active),
        ),
        (
            "V5-to-V6",
            super::super::prepare_standard_upgrade_v5_to_v6(&active),
        ),
    ] {
        let error = result.expect_err("an append-only upgrade must require its parent");
        assert!(
            matches!(
                &error,
                StandardUpgradeError::StandardLibrary {
                    source: StandardLibraryError::Unavailable,
                }
            ),
            "{label} returned the wrong missing-parent error: {error:?}"
        );
        assert_eq!(error.to_string(), expected_error, "{label} display");
        assert_eq!(
            error.source().map(ToString::to_string),
            Some(expected_error.to_owned()),
        );
    }
}

#[test]
fn prepare_standard_upgrade_v2_to_v3_fails_closed() {
    // A base that pins V1 is not the V2 parent and fails closed before the
    // V3 pipeline runs.
    let version_one = verify_standard_library_snapshot(
        retained_standard_library_snapshot().expect("the retained V1 source is valid"),
    )
    .expect("the retained V1 standard source verifies");
    let pinned_one = empty_version_two_active_revision(&version_one);
    let error = prepare_standard_upgrade_v2_to_v3(&pinned_one).expect_err("V1 is not the V2 base");
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

    // A base that already pins V3 cannot upgrade to the installed target.
    let version_three = verify_standard_library_v3_snapshot(
        retained_standard_library_v3_snapshot().expect("the retained V3 source is valid"),
    )
    .expect("the retained V3 standard source verifies");
    let pinned_three = empty_version_two_active_revision(&version_three);
    let error =
        prepare_standard_upgrade_v2_to_v3(&pinned_three).expect_err("V3 is not the V2 base");
    assert!(matches!(
        &error,
        StandardUpgradeError::Prepare {
            source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                revision
            }
        } if *revision == STANDARD_LIBRARY_V3_REVISION_ID
    ));
    assert_eq!(
        error.to_string(),
        format!("standard library {STANDARD_LIBRARY_V3_REVISION_ID} is already installed")
    );

    // A V2-pinned base is the exact append-only parent (work ADR 0059
    // upgrade pipeline): the shared machinery admits the source child and
    // prepares the V3 companion application revision on the V2 pair.
    let version_two = verify_standard_library_v2_snapshot(
        retained_standard_library_v2_snapshot().expect("the retained V2 source is valid"),
    )
    .expect("the retained V2 standard source verifies");
    let pinned_two = empty_version_two_active_revision(&version_two);
    let upgrade = prepare_standard_upgrade_v2_to_v3(&pinned_two)
        .expect("the installed V2 parent prepares the append-only V3 upgrade");
    assert_eq!(
        upgrade.verified_standard_snapshot().revision(),
        STANDARD_LIBRARY_V3_REVISION_ID
    );
    assert_eq!(
        upgrade.application_revision().expected_base(),
        pinned_two.pair()
    );
    assert_eq!(
        upgrade
            .application_revision()
            .catalogue_hash_context()
            .standard()
            .map(|snapshot| snapshot.revision()),
        Some(STANDARD_LIBRARY_V3_REVISION_ID)
    );
}

#[test]
fn prepares_the_v3_to_v4_standard_upgrade_from_an_empty_v3_active_revision() {
    let version_three = super::super::verify_standard_library_v3_snapshot(
        super::super::retained_standard_library_v3_snapshot()
            .expect("the retained V3 standard source is valid"),
    )
    .expect("the retained V3 standard source verifies");
    let version_four = super::super::verify_standard_library_v4_snapshot(
        super::super::retained_standard_library_v4_snapshot()
            .expect("the retained V4 standard source is valid"),
    )
    .expect("the retained V4 standard source verifies");
    orna_compiler::check_standard_library_source(&version_four)
        .unwrap_or_else(|error| panic!("the V4 source must check: {error:?}"));

    let active = empty_version_two_active_revision(&version_three);
    let upgrade = super::super::prepare_standard_upgrade_v3_to_v4(&active)
        .unwrap_or_else(|error| panic!("the V3-to-V4 upgrade must prepare: {error:?}"));

    assert_eq!(
        upgrade.verified_standard_snapshot().revision(),
        super::super::STANDARD_LIBRARY_V4_REVISION_ID
    );
    assert_eq!(
        upgrade.verified_standard_snapshot().catalogue().revision(),
        super::super::STANDARD_CATALOGUE_V4_REVISION_ID,
        "V4 must carry the accepted standard catalogue revision"
    );
    assert_eq!(
        upgrade.verified_standard_snapshot().source().parent(),
        Some(super::super::STANDARD_SOURCE_V3_REVISION_ID),
        "V4 must be the append-only child of the retained V3 source revision"
    );
    assert_eq!(
        upgrade.verified_standard_snapshot().source().bundle(),
        super::super::STANDARD_SOURCE_V4_BUNDLE_ID,
        "V4 must retain its reserved source-bundle identity"
    );
    assert_eq!(
        upgrade.verified_standard_snapshot().source().id(),
        super::super::STANDARD_SOURCE_V4_REVISION_ID,
        "V4 must retain its reserved source-revision identity"
    );
    assert_eq!(
        upgrade.verified_standard_snapshot().digest(),
        super::super::ACCEPTED_V4_STANDARD_LIBRARY_DIGEST,
        "V4 must retain the accepted standard-library digest"
    );
    assert_eq!(
        &upgrade.verified_standard_snapshot().source().units()[..3],
        version_three.source().units(),
        "V4 must retain every V3 source unit byte-for-byte"
    );
    assert_eq!(
        upgrade.verified_standard_snapshot().source().units().len(),
        4
    );
    assert_eq!(upgrade.verified_standard_snapshot().executables().len(), 1);
    assert_eq!(
        upgrade.verified_standard_snapshot().catalogue().functions(),
        version_three.catalogue().functions(),
        "V4 must retain the V3 standard function definitions"
    );
    assert_eq!(
        upgrade.verified_standard_snapshot().executables(),
        version_three.executables(),
        "V4 must retain the V3 executable snapshot"
    );
    let checked_executable = upgrade
        .checked_standard_library()
        .checked_executable()
        .expect("the V4 upgrade retains the checked echo executable");
    assert_eq!(
        checked_executable.function_id(),
        super::super::STD_INVOKE_ECHO_FUNCTION_ID
    );
    assert_eq!(
        checked_executable.parameter_ids(),
        &[super::super::STD_INVOKE_ECHO_PARAMETER_ID]
    );
    assert_eq!(
        checked_executable.revision_id(),
        super::super::STD_INVOKE_ECHO_FUNCTION_REVISION_ID
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
        Some(super::super::STANDARD_LIBRARY_V4_REVISION_ID)
    );
    assert_eq!(
        upgrade
            .application_revision()
            .catalogue_hash_context()
            .standard()
            .map(|snapshot| snapshot.digest()),
        Some(upgrade.verified_standard_snapshot().digest()),
        "the V4 application caller must pin the upgraded standard digest"
    );
    assert_eq!(
        upgrade
            .application_revision()
            .catalogue_hash_context()
            .standard()
            .map(|snapshot| snapshot.digest_version()),
        Some(StandardLibraryDigestVersion::Version2)
    );
}

#[test]
fn v3_to_v4_upgrade_rejects_non_v3_parents_before_child_work() {
    let v2 = super::super::verify_standard_library_v2_snapshot(
        super::super::retained_standard_library_v2_snapshot()
            .expect("the retained V2 standard source is valid"),
    )
    .expect("the retained V2 standard source verifies");
    let v4 = super::super::verify_standard_library_v4_snapshot(
        super::super::retained_standard_library_v4_snapshot()
            .expect("the retained V4 standard source is valid"),
    )
    .expect("the retained V4 standard source verifies");

    for (standard, revision) in [
        (&v2, super::super::STANDARD_LIBRARY_V2_REVISION_ID),
        (&v4, super::super::STANDARD_LIBRARY_V4_REVISION_ID),
    ] {
        let active = empty_version_two_active_revision(standard);
        let error = super::super::prepare_standard_upgrade_v3_to_v4(&active)
            .expect_err("a non-V3 parent must not enter the V3-to-V4 path");
        assert!(matches!(
            error,
            super::super::StandardUpgradeError::Prepare {
                source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled { revision: actual }
            } if actual == revision
        ));
    }
}
