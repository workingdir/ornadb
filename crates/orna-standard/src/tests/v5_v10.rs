use super::*;

#[test]
fn retains_and_verifies_v5_json_standard_snapshot() {
    let snapshot = super::super::retained_standard_library_v5_snapshot()
        .expect("the retained V5 source is valid");
    assert_eq!(snapshot.source().units().len(), 5);
    let verified = super::super::verify_standard_library_v5_snapshot(snapshot)
        .expect("the retained V5 source verifies");
    assert!(super::super::registered_opaque_codecs(&verified).is_ok());
}

#[test]
fn v5_json_origins_match_declaration_identities_and_exact_source_slices() {
    let snapshot = super::super::retained_standard_library_v5_snapshot()
        .expect("the retained V5 source is valid");
    let source = snapshot.source().units()[4].content();
    let json_origins = snapshot
        .origins()
        .iter()
        .filter(|origin| origin.source().source_unit() == super::super::STD_JSON_SOURCE_UNIT_ID)
        .collect::<Vec<_>>();
    assert_eq!(json_origins.len(), 5);

    let binding_id = snapshot
        .catalogue()
        .type_bindings()
        .iter()
        .find(|binding| binding.target() == super::super::STD_JSON_VALUE_TYPE_ID)
        .expect("the V5 JSON export is retained")
        .id();
    let expected = [
        (
            DefinitionIdentity::Schema(super::super::STD_JSON_SCHEMA_ID),
            0,
            23,
            "CREATE SCHEMA std.json;",
        ),
        (
            DefinitionIdentity::ValueType(super::super::STD_JSON_VALUE_TYPE_ID),
            25,
            144,
            "CREATE TYPE std.json.Value AS VALUE\n    OPAQUE\n    KERNEL CONTRACT 'orna.std.value.json@1'\n    IMMUTABLE\n    TRANSIENT;",
        ),
        (
            DefinitionIdentity::TypeBinding(binding_id),
            146,
            190,
            "EXPORT TYPE std.json.Value AS std.JsonValue;",
        ),
        (
            DefinitionIdentity::Function(super::super::STD_JSON_ENCODE_FUNCTION_ID),
            192,
            366,
            "CREATE SERVER FUNCTION std.json.encode(\n    p_value std.json.Value\n)\nRETURNS std.io.ByteStream\nSECURITY INVOKER\nTRANSACTION READ ONLY\nVOLATILITY STABLE\nAS\n    SELECT p_value;",
        ),
        (
            DefinitionIdentity::Parameter {
                owner: super::super::STD_JSON_ENCODE_FUNCTION_ID,
                parameter: super::super::STD_JSON_ENCODE_PARAMETER_ID,
            },
            236,
            258,
            "p_value std.json.Value",
        ),
    ];

    for (origin, (identity, start, end, slice)) in json_origins.iter().zip(expected) {
        assert_eq!(origin.identity(), identity);
        assert_eq!(origin.source().byte_start(), start);
        assert_eq!(origin.source().byte_end(), end);
        assert_eq!(&source[start as usize..end as usize], slice);
    }
}

#[test]
fn rejects_a_malformed_v5_json_presenter_declaration() {
    let json_source = super::super::RETAINED_STANDARD_JSON_SOURCE.replace("p_value", "wrong");
    let manifest = super::super::standard_library_v5_manifest().expect("the V5 manifest is valid");
    let error = super::super::reconcile_retained_json_source(&json_source, manifest.catalogue())
        .expect_err("the JSON presenter must retain its closed ADR 0057 signature");
    assert!(matches!(
        error,
        super::super::StandardLibraryError::RetainedSourceMismatch
    ));
}

#[test]
fn rejects_a_tampered_v5_json_source_byte_before_verification() {
    let mut json_source = super::super::RETAINED_STANDARD_JSON_SOURCE.to_owned();
    json_source.push('\n');
    let error = super::super::retained_standard_library_v5_snapshot_from_source(
        super::super::RETAINED_STANDARD_SOURCE,
        super::super::RETAINED_STANDARD_INVOKE_SOURCE,
        super::super::RETAINED_STANDARD_OUTPUT_SOURCE,
        super::super::RETAINED_STANDARD_UI_SOURCE,
        &json_source,
    )
    .expect_err("a changed V5 source byte must be rejected");
    assert!(matches!(
        error,
        super::super::StandardLibraryError::RetainedSourceMismatch
    ));
}

#[test]
fn rejects_a_tampered_v5_json_executable_through_compiler_dispatch() {
    let snapshot = super::super::retained_standard_library_v5_snapshot()
        .expect("the retained V5 source is valid");
    let json_index = snapshot
        .executables()
        .iter()
        .position(|executable| executable.function() == super::super::STD_JSON_ENCODE_FUNCTION_ID)
        .expect("the retained V5 snapshot contains the JSON executable");
    let original = &snapshot.executables()[json_index];
    let revision = original.revision();
    let mut payload = revision.artifact().payload().to_vec();
    payload.push(0);
    let content_hash =
        artifact_payload_digest(&payload).expect("the tampered payload can be hashed");
    let artifact = ExecutableArtifact::new(
        revision.artifact().kind(),
        revision.artifact().format(),
        revision.artifact().version(),
        payload,
        content_hash,
    )
    .expect("the tampered artifact remains structurally valid");
    let function = snapshot
        .catalogue()
        .function_by_id(super::super::STD_JSON_ENCODE_FUNCTION_ID)
        .expect("the retained V5 catalogue contains the JSON function");
    let semantic_hash = function_semantic_digest_with_version(
        revision.semantic_hash_version(),
        function,
        revision.language_version(),
        &artifact,
        &[],
        original.references(),
    )
    .expect("the tampered semantic hash can be calculated");
    let tampered_revision = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        semantic_hash,
        revision.language_version(),
        artifact,
    )
    .expect("the tampered revision remains structurally valid")
    .with_semantic_hash_version(revision.semantic_hash_version());
    let tampered_executable = StandardExecutable::new(
        original.function(),
        tampered_revision,
        original.references().to_vec(),
    )
    .expect("the tampered executable remains structurally valid");
    let mut executables = snapshot.executables().to_vec();
    executables[json_index] = tampered_executable;
    let build_snapshot = |digest| {
        StandardLibrarySnapshot::new_with_executables(
            snapshot.revision(),
            snapshot.digest_version(),
            snapshot.source().clone(),
            snapshot.language_version(),
            snapshot.catalogue().clone(),
            executables.clone(),
            snapshot.origins().to_vec(),
            digest,
        )
        .expect("the tampered snapshot remains structurally valid")
    };
    let provisional = build_snapshot(snapshot.digest());
    let digest = orna_core::canonical_hash::calculate_standard_library_digest(&provisional)
        .expect("the tampered snapshot digest can be calculated");
    let tampered_snapshot = build_snapshot(digest);
    let verified = super::super::verify_canonical_standard_library_v2_snapshot(tampered_snapshot)
        .expect("the tampered snapshot verifies with its recalculated digest");
    let error = orna_compiler::check_standard_library_source(&verified)
        .expect_err("the V5 compiler path must reject the tampered executable");
    assert!(matches!(
        error,
        orna_compiler::StandardLibraryCheckError::ExecutableMismatch
    ));
}

#[test]
fn prepares_the_v4_to_v5_standard_upgrade_from_an_empty_v4_active_revision() {
    let version_four = super::super::verify_standard_library_v4_snapshot(
        super::super::retained_standard_library_v4_snapshot()
            .expect("the retained V4 standard source is valid"),
    )
    .expect("the retained V4 standard source verifies");
    let version_five = super::super::verify_standard_library_v5_snapshot(
        super::super::retained_standard_library_v5_snapshot()
            .expect("the retained V5 standard source is valid"),
    )
    .expect("the retained V5 standard source verifies");
    orna_compiler::check_standard_library_source(&version_five)
        .unwrap_or_else(|error| panic!("the V5 source must check: {error:?}"));
    let active = empty_version_two_active_revision(&version_four);
    let upgrade = super::super::prepare_standard_upgrade_v4_to_v5(&active)
        .unwrap_or_else(|error| panic!("the V4-to-V5 upgrade must prepare: {error:?}"));
    assert_eq!(
        upgrade.verified_standard_snapshot().revision(),
        super::super::STANDARD_LIBRARY_V5_REVISION_ID
    );
    let verified = upgrade.verified_standard_snapshot();
    assert_eq!(
        verified.source().units(),
        version_five.source().units(),
        "the V5 upgrade must retain the expected standard source units"
    );
    assert_eq!(
        verified.origins(),
        version_five.origins(),
        "the V5 upgrade must retain the expected source origins"
    );
    assert_eq!(
        &verified.origins()[..version_four.origins().len()],
        version_four.origins(),
        "V5 must retain every V4 source origin byte-for-byte"
    );
    assert_eq!(
        verified.catalogue().schemas(),
        version_five.catalogue().schemas(),
        "the V5 upgrade must retain the expected standard schemas"
    );
    assert_eq!(
        verified.catalogue().object_types(),
        version_five.catalogue().object_types(),
        "the V5 upgrade must retain the expected object types"
    );
    assert_eq!(
        verified.catalogue().enum_types(),
        version_five.catalogue().enum_types(),
        "the V5 upgrade must retain the expected enum types"
    );
    assert_eq!(
        verified.catalogue().record_value_types(),
        version_five.catalogue().record_value_types(),
        "the V5 upgrade must retain the expected record value types"
    );
    assert_eq!(
        verified.catalogue().value_types(),
        version_five.catalogue().value_types(),
        "the V5 upgrade must retain the expected standard value types"
    );
    assert_eq!(
        verified.catalogue().type_bindings(),
        version_five.catalogue().type_bindings(),
        "the V5 upgrade must retain the expected standard type bindings"
    );
    assert_eq!(
        verified.catalogue().functions(),
        version_five.catalogue().functions(),
        "the V5 upgrade must retain the expected standard functions"
    );
    assert_eq!(
        verified.catalogue().revision(),
        super::super::STANDARD_CATALOGUE_V5_REVISION_ID
    );
    assert_eq!(
        verified.source().bundle(),
        super::super::STANDARD_SOURCE_V5_BUNDLE_ID
    );
    assert_eq!(
        verified.source().id(),
        super::super::STANDARD_SOURCE_V5_REVISION_ID
    );
    assert_eq!(
        verified.source().parent(),
        Some(super::super::STANDARD_SOURCE_V4_REVISION_ID)
    );
    assert_eq!(
        verified.source().bundle_hash(),
        super::super::ACCEPTED_V5_SOURCE_BUNDLE_DIGEST
    );
    assert_eq!(
        verified.source().revision_hash(),
        super::super::ACCEPTED_V5_SOURCE_REVISION_DIGEST
    );
    assert_eq!(verified.source().units().len(), 5);
    assert_eq!(
        &verified.source().units()[..4],
        version_four.source().units()
    );
    assert_eq!(
        verified.source().units()[4].id(),
        super::super::STD_JSON_SOURCE_UNIT_ID
    );
    assert_eq!(verified.source().units()[4].ordinal(), 4);
    assert_eq!(
        verified.source().units()[4].logical_path(),
        super::super::STD_JSON_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        verified.source().units()[4].content(),
        super::super::RETAINED_STANDARD_JSON_SOURCE
    );
    assert_eq!(
        verified.digest(),
        super::super::ACCEPTED_V5_STANDARD_LIBRARY_DIGEST
    );
    assert_eq!(upgrade.verified_standard_snapshot().executables().len(), 2);
    assert_eq!(
        upgrade
            .checked_standard_library()
            .checked_executable()
            .expect("the V5 upgrade retains the echo executable")
            .function_id(),
        super::super::STD_INVOKE_ECHO_FUNCTION_ID
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
        Some(super::super::STANDARD_LIBRARY_V5_REVISION_ID)
    );
    assert_eq!(
        upgrade
            .application_revision()
            .catalogue_hash_context()
            .standard()
            .map(|snapshot| snapshot.digest()),
        Some(verified.digest()),
        "the V5 application caller must pin the expected standard digest"
    );
    let expected_catalogue_hash = catalogue_digest_with_context(
        upgrade.application_revision().catalogue_hash_context(),
        upgrade.application_revision().candidate(),
        upgrade.application_revision().new_function_revisions(),
        upgrade.application_revision().expressions(),
        upgrade.application_revision().origins(),
        upgrade.application_revision().references(),
    )
    .expect("the V5 application catalogue hash recomputes");
    assert_eq!(
        upgrade.application_revision().catalogue_hash(),
        expected_catalogue_hash,
        "the V5 application catalogue hash must cover the retained standard context"
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
fn v4_to_v5_upgrade_rejects_non_v4_parents_before_child_work() {
    let v3 = super::super::verify_standard_library_v3_snapshot(
        super::super::retained_standard_library_v3_snapshot()
            .expect("the retained V3 standard source is valid"),
    )
    .expect("the retained V3 standard source verifies");
    let v5 = super::super::verify_standard_library_v5_snapshot(
        super::super::retained_standard_library_v5_snapshot()
            .expect("the retained V5 standard source is valid"),
    )
    .expect("the retained V5 standard source verifies");

    for (standard, revision) in [
        (&v3, super::super::STANDARD_LIBRARY_V3_REVISION_ID),
        (&v5, super::super::STANDARD_LIBRARY_V5_REVISION_ID),
    ] {
        let active = empty_version_two_active_revision(standard);
        let error = super::super::prepare_standard_upgrade_v4_to_v5(&active)
            .expect_err("a non-V4 parent must not enter the V4-to-V5 path");
        assert!(matches!(
            error,
            super::super::StandardUpgradeError::Prepare {
                source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled { revision: actual }
            } if actual == revision
        ));
    }
}

#[test]
fn prepares_the_v5_to_v6_standard_upgrade_from_an_empty_v5_active_revision() {
    let version_five = super::super::verify_standard_library_v5_snapshot(
        super::super::retained_standard_library_v5_snapshot()
            .expect("the retained V5 standard source is valid"),
    )
    .expect("the retained V5 standard source verifies");
    let version_six = super::super::verify_standard_library_v6_snapshot(
        super::super::retained_standard_library_v6_snapshot()
            .expect("the retained V6 standard source is valid"),
    )
    .expect("the retained V6 standard source verifies");
    orna_compiler::check_standard_library_source(&version_five)
        .unwrap_or_else(|error| panic!("the V5 source must check: {error:?}"));
    let active = empty_version_two_active_revision(&version_five);
    let upgrade = super::super::prepare_standard_upgrade_v5_to_v6(&active)
        .unwrap_or_else(|error| panic!("the V5-to-V6 upgrade must prepare: {error:?}"));

    let verified = upgrade.verified_standard_snapshot();
    assert_eq!(
        verified.source().units(),
        version_six.source().units(),
        "the V6 upgrade must retain the expected standard source units"
    );
    assert_eq!(
        verified.origins(),
        version_six.origins(),
        "the V6 upgrade must retain the expected source origins"
    );
    assert_eq!(
        &verified.origins()[..version_five.origins().len()],
        version_five.origins(),
        "V6 must retain every V5 source origin byte-for-byte"
    );
    assert_eq!(
        verified.catalogue().schemas(),
        version_six.catalogue().schemas(),
        "the V6 upgrade must retain the expected standard schemas"
    );
    assert_eq!(
        verified.catalogue().object_types(),
        version_six.catalogue().object_types(),
        "the V6 upgrade must retain the expected object types"
    );
    assert_eq!(
        verified.catalogue().enum_types(),
        version_six.catalogue().enum_types(),
        "the V6 upgrade must retain the expected enum types"
    );
    assert_eq!(
        verified.catalogue().record_value_types(),
        version_six.catalogue().record_value_types(),
        "the V6 upgrade must retain the expected record value types"
    );
    assert_eq!(
        verified.catalogue().value_types(),
        version_six.catalogue().value_types(),
        "the V6 upgrade must retain the expected standard value types"
    );
    assert_eq!(
        verified.catalogue().type_bindings(),
        version_six.catalogue().type_bindings(),
        "the V6 upgrade must retain the expected standard type bindings"
    );
    assert_eq!(
        verified.catalogue().functions(),
        version_six.catalogue().functions(),
        "the V6 upgrade must retain the expected standard functions"
    );
    assert_eq!(
        verified.revision(),
        super::super::STANDARD_LIBRARY_V6_REVISION_ID
    );
    assert_eq!(
        verified.catalogue().revision(),
        super::super::STANDARD_CATALOGUE_V6_REVISION_ID,
        "V6 must carry the accepted standard catalogue revision"
    );
    assert_eq!(
        verified.source().parent(),
        Some(super::super::STANDARD_SOURCE_V5_REVISION_ID),
        "V6 must be the append-only child of the retained V5 source revision"
    );
    assert_eq!(
        verified.source().bundle(),
        super::super::STANDARD_SOURCE_V6_BUNDLE_ID,
        "V6 must retain its reserved source-bundle identity"
    );
    assert_eq!(
        verified.source().id(),
        super::super::STANDARD_SOURCE_V6_REVISION_ID,
        "V6 must retain its reserved source-revision identity"
    );
    assert_eq!(
        verified.digest(),
        super::super::ACCEPTED_V6_STANDARD_LIBRARY_DIGEST,
        "V6 must retain the accepted standard-library digest"
    );
    assert_eq!(
        &verified.source().units()[..5],
        version_five.source().units(),
        "V6 must retain every V5 source unit byte-for-byte"
    );
    assert_eq!(verified.source().units().len(), 6);
    assert_eq!(
        verified.source().units()[5].id(),
        super::super::STD_ACTION_SOURCE_UNIT_ID
    );
    assert_eq!(
        verified.source().units()[5].logical_path(),
        super::super::STD_ACTION_SOURCE_LOGICAL_PATH
    );
    assert_eq!(verified.executables().len(), 2);
    assert_eq!(
        verified.catalogue().functions(),
        version_five.catalogue().functions(),
        "V6 must retain the V5 standard function definitions"
    );
    assert_eq!(
        verified.executables(),
        version_five.executables(),
        "V6 must retain the V5 executable snapshot"
    );

    assert_eq!(
        upgrade.application_revision().expected_base(),
        active.pair()
    );
    let checked = upgrade.checked_standard_library();
    assert_eq!(
        checked.verified_snapshot().catalogue().revision(),
        super::super::STANDARD_CATALOGUE_V6_REVISION_ID
    );
    let checked_executable = checked
        .checked_executable()
        .expect("the V6 upgrade retains the checked echo executable");
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
        upgrade
            .application_revision()
            .catalogue_hash_context()
            .standard()
            .map(|snapshot| snapshot.digest()),
        Some(verified.digest()),
        "the V6 application caller must pin the upgraded standard digest"
    );
    let expected_catalogue_hash = catalogue_digest_with_context(
        upgrade.application_revision().catalogue_hash_context(),
        upgrade.application_revision().candidate(),
        upgrade.application_revision().new_function_revisions(),
        upgrade.application_revision().expressions(),
        upgrade.application_revision().origins(),
        upgrade.application_revision().references(),
    )
    .expect("the V6 application catalogue hash recomputes");
    assert_eq!(
        upgrade.application_revision().catalogue_hash(),
        expected_catalogue_hash,
        "the V6 application catalogue hash must cover the retained standard context"
    );
}

#[test]
fn v5_to_v6_upgrade_rejects_a_non_v5_parent_before_child_work() {
    let v4 = super::super::verify_standard_library_v4_snapshot(
        super::super::retained_standard_library_v4_snapshot()
            .expect("the retained V4 source is valid"),
    )
    .expect("the retained V4 source verifies");
    let active = empty_version_two_active_revision(&v4);
    let error = super::super::prepare_standard_upgrade_v5_to_v6(&active)
        .expect_err("a V4 parent must not enter the V5-to-V6 path");
    assert!(matches!(
        error,
        super::super::StandardUpgradeError::Prepare {
            source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled { revision }
        } if revision == super::super::STANDARD_LIBRARY_V4_REVISION_ID
    ));
}

#[test]
fn prepares_the_v7_to_v8_standard_upgrade_from_an_empty_v7_active_revision() {
    let version_seven = super::super::verify_standard_library_v7_snapshot(
        super::super::retained_standard_library_v7_snapshot()
            .expect("the retained V7 standard source is valid"),
    )
    .expect("the retained V7 standard source verifies");
    let version_eight = super::super::verify_standard_library_v8_snapshot(
        super::super::retained_standard_library_v8_snapshot()
            .expect("the retained V8 standard source is valid"),
    )
    .expect("the retained V8 standard source verifies");
    orna_compiler::check_standard_library_source(&version_seven)
        .unwrap_or_else(|error| panic!("the V7 source must check: {error:?}"));

    let active = empty_version_two_active_revision(&version_seven);
    let upgrade = super::super::prepare_standard_upgrade_v7_to_v8(&active)
        .unwrap_or_else(|error| panic!("the V7-to-V8 upgrade must prepare: {error:?}"));
    let verified = upgrade.verified_standard_snapshot();

    assert_eq!(
        verified.source().units(),
        version_eight.source().units(),
        "the V8 upgrade must retain the expected standard source units"
    );
    assert_eq!(
        verified.origins(),
        version_eight.origins(),
        "the V8 upgrade must retain the expected source origins"
    );
    assert_eq!(
        &verified.origins()[..version_seven.origins().len()],
        version_seven.origins(),
        "V8 must retain every V7 source origin byte-for-byte"
    );
    assert_eq!(
        verified.catalogue().schemas(),
        version_eight.catalogue().schemas(),
        "the V8 upgrade must retain the expected standard schemas"
    );
    assert_eq!(
        verified.catalogue().object_types(),
        version_eight.catalogue().object_types(),
        "the V8 upgrade must retain the expected object types"
    );
    assert_eq!(
        verified.catalogue().enum_types(),
        version_eight.catalogue().enum_types(),
        "the V8 upgrade must retain the expected enum types"
    );
    assert_eq!(
        verified.catalogue().record_value_types(),
        version_eight.catalogue().record_value_types(),
        "the V8 upgrade must retain the expected record value types"
    );
    assert_eq!(
        verified.catalogue().value_types(),
        version_eight.catalogue().value_types(),
        "the V8 upgrade must retain the expected standard value types"
    );
    assert_eq!(
        verified.catalogue().type_bindings(),
        version_eight.catalogue().type_bindings(),
        "the V8 upgrade must retain the expected standard type bindings"
    );
    assert_eq!(
        verified.catalogue().functions(),
        version_eight.catalogue().functions(),
        "the V8 upgrade must retain the expected standard functions"
    );
    assert_eq!(
        verified.executables(),
        version_eight.executables(),
        "the V8 upgrade must retain the expected executable snapshot"
    );
    assert_eq!(
        verified.revision(),
        super::super::STANDARD_LIBRARY_V8_REVISION_ID,
        "V8 must carry the accepted standard-library revision"
    );
    assert_eq!(
        verified.catalogue().revision(),
        super::super::STANDARD_CATALOGUE_V8_REVISION_ID,
        "V8 must carry the accepted standard catalogue revision"
    );
    assert_eq!(
        verified.source().bundle(),
        super::super::STANDARD_SOURCE_V8_BUNDLE_ID,
        "V8 must retain its reserved source-bundle identity"
    );
    assert_eq!(
        verified.source().id(),
        super::super::STANDARD_SOURCE_V8_REVISION_ID,
        "V8 must retain its reserved source-revision identity"
    );
    assert_eq!(
        verified.source().parent(),
        Some(super::super::STANDARD_SOURCE_V7_REVISION_ID),
        "V8 must be the append-only child of the retained V7 source revision"
    );
    assert_eq!(
        verified.source().bundle_hash(),
        super::super::ACCEPTED_V8_SOURCE_BUNDLE_DIGEST,
        "V8 must retain the accepted source-bundle digest"
    );
    assert_eq!(
        verified.source().revision_hash(),
        super::super::ACCEPTED_V8_SOURCE_REVISION_DIGEST,
        "V8 must retain the accepted source-revision digest"
    );
    assert_eq!(
        verified.digest(),
        super::super::ACCEPTED_V8_STANDARD_LIBRARY_DIGEST,
        "V8 must retain the accepted standard-library digest"
    );
    assert_eq!(verified.source().units().len(), 8);
    assert_eq!(
        &verified.source().units()[..version_seven.source().units().len()],
        version_seven.source().units(),
        "V8 must retain every V7 source unit byte-for-byte"
    );
    assert_eq!(
        verified.source().units()[7].id(),
        super::super::STD_DATA_SOURCE_UNIT_ID
    );
    assert_eq!(verified.source().units()[7].ordinal(), 7);
    assert_eq!(
        verified.source().units()[7].logical_path(),
        super::super::STD_DATA_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        verified.source().units()[7].content(),
        super::super::RETAINED_STANDARD_DATA_SOURCE
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
        Some(super::super::STANDARD_LIBRARY_V8_REVISION_ID)
    );
    assert_eq!(
        upgrade
            .application_revision()
            .catalogue_hash_context()
            .standard()
            .map(|snapshot| snapshot.digest()),
        Some(verified.digest()),
        "the V8 application caller must pin the upgraded standard digest"
    );
    assert_eq!(
        upgrade
            .application_revision()
            .catalogue_hash_context()
            .standard()
            .map(|snapshot| snapshot.digest_version()),
        Some(StandardLibraryDigestVersion::Version2)
    );
    let expected_catalogue_hash = catalogue_digest_with_context(
        upgrade.application_revision().catalogue_hash_context(),
        upgrade.application_revision().candidate(),
        upgrade.application_revision().new_function_revisions(),
        upgrade.application_revision().expressions(),
        upgrade.application_revision().origins(),
        upgrade.application_revision().references(),
    )
    .expect("the V8 application catalogue hash recomputes");
    assert_eq!(
        upgrade.application_revision().catalogue_hash(),
        expected_catalogue_hash,
        "the V8 application catalogue hash must cover the retained standard context"
    );
}

#[test]
fn v7_to_v8_upgrade_rejects_non_v7_parents_before_child_work() {
    let version_six = super::super::verify_standard_library_v6_snapshot(
        super::super::retained_standard_library_v6_snapshot()
            .expect("the retained V6 standard source is valid"),
    )
    .expect("the retained V6 standard source verifies");
    let version_eight = super::super::verify_standard_library_v8_snapshot(
        super::super::retained_standard_library_v8_snapshot()
            .expect("the retained V8 standard source is valid"),
    )
    .expect("the retained V8 standard source verifies");

    for (standard, actual_parent_revision) in [
        (&version_six, super::super::STANDARD_LIBRARY_V6_REVISION_ID),
        (
            &version_eight,
            super::super::STANDARD_LIBRARY_V8_REVISION_ID,
        ),
    ] {
        let active = empty_version_two_active_revision(standard);
        let error = super::super::prepare_standard_upgrade_v7_to_v8(&active)
            .expect_err("a non-V7 parent must not enter the V7-to-V8 path");
        assert!(matches!(
            error,
            super::super::StandardUpgradeError::Prepare {
                source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                    revision
                }
            } if revision == actual_parent_revision
        ));
    }
}

#[test]
fn prepares_the_v8_to_v9_standard_upgrade_from_an_empty_v8_active_revision() {
    let version_eight = super::super::verify_standard_library_v8_snapshot(
        super::super::retained_standard_library_v8_snapshot()
            .expect("the retained V8 standard source is valid"),
    )
    .expect("the retained V8 standard source verifies");
    let version_nine = super::super::verify_standard_library_v9_snapshot(
        super::super::retained_standard_library_v9_snapshot()
            .expect("the retained V9 standard source is valid"),
    )
    .expect("the retained V9 standard source verifies");
    orna_compiler::check_standard_library_source(&version_eight)
        .unwrap_or_else(|error| panic!("the V8 source must check: {error:?}"));

    let active = empty_version_two_active_revision(&version_eight);
    let upgrade = super::super::prepare_standard_upgrade_v8_to_v9(&active)
        .unwrap_or_else(|error| panic!("the V8-to-V9 upgrade must prepare: {error:?}"));
    let verified = upgrade.verified_standard_snapshot();

    assert_eq!(
        verified.source().units(),
        version_nine.source().units(),
        "the V9 upgrade must retain the expected standard source units"
    );
    assert_eq!(
        verified.origins(),
        version_nine.origins(),
        "the V9 upgrade must retain the expected source origins"
    );
    assert_eq!(
        &verified.origins()[..version_eight.origins().len()],
        version_eight.origins(),
        "V9 must retain every V8 source origin byte-for-byte"
    );
    assert_eq!(
        verified.catalogue().schemas(),
        version_nine.catalogue().schemas(),
        "the V9 upgrade must retain the expected standard schemas"
    );
    assert_eq!(
        verified.catalogue().object_types(),
        version_nine.catalogue().object_types(),
        "the V9 upgrade must retain the expected object types"
    );
    assert_eq!(
        verified.catalogue().enum_types(),
        version_nine.catalogue().enum_types(),
        "the V9 upgrade must retain the expected enum types"
    );
    assert_eq!(
        verified.catalogue().record_value_types(),
        version_nine.catalogue().record_value_types(),
        "the V9 upgrade must retain the expected record value types"
    );
    assert_eq!(
        verified.catalogue().value_types(),
        version_nine.catalogue().value_types(),
        "the V9 upgrade must retain the expected standard value types"
    );
    assert_eq!(
        verified.catalogue().type_bindings(),
        version_nine.catalogue().type_bindings(),
        "the V9 upgrade must retain the expected standard type bindings"
    );
    assert_eq!(
        verified.catalogue().functions(),
        version_nine.catalogue().functions(),
        "the V9 upgrade must retain the expected standard functions"
    );
    assert_eq!(
        verified.executables(),
        version_nine.executables(),
        "the V9 upgrade must retain the expected executable snapshot"
    );
    assert_eq!(
        verified.revision(),
        super::super::STANDARD_LIBRARY_V9_REVISION_ID,
        "V9 must carry the accepted standard-library revision"
    );
    assert_eq!(
        verified.catalogue().revision(),
        super::super::STANDARD_CATALOGUE_V9_REVISION_ID,
        "V9 must carry the accepted standard catalogue revision"
    );
    assert_eq!(
        verified.source().bundle(),
        super::super::STANDARD_SOURCE_V9_BUNDLE_ID,
        "V9 must retain its reserved source-bundle identity"
    );
    assert_eq!(
        verified.source().id(),
        super::super::STANDARD_SOURCE_V9_REVISION_ID,
        "V9 must retain its reserved source-revision identity"
    );
    assert_eq!(
        verified.source().parent(),
        Some(super::super::STANDARD_SOURCE_V8_REVISION_ID),
        "V9 must be the append-only child of the retained V8 source revision"
    );
    assert_eq!(
        verified.source().bundle_hash(),
        super::super::ACCEPTED_V9_SOURCE_BUNDLE_DIGEST,
        "V9 must retain the accepted source-bundle digest"
    );
    assert_eq!(
        verified.source().revision_hash(),
        super::super::ACCEPTED_V9_SOURCE_REVISION_DIGEST,
        "V9 must retain the accepted source-revision digest"
    );
    assert_eq!(
        verified.digest(),
        super::super::ACCEPTED_V9_STANDARD_LIBRARY_DIGEST,
        "V9 must retain the accepted standard-library digest"
    );
    assert_eq!(verified.source().units().len(), 9);
    assert_eq!(
        &verified.source().units()[..version_eight.source().units().len()],
        version_eight.source().units(),
        "V9 must retain every V8 source unit byte-for-byte"
    );
    assert_eq!(
        verified.source().units()[8].id(),
        super::super::STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID
    );
    assert_eq!(verified.source().units()[8].ordinal(), 8);
    assert_eq!(
        verified.source().units()[8].logical_path(),
        super::super::STD_UI_CONSTRUCTORS_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        verified.source().units()[8].content(),
        super::super::RETAINED_STANDARD_UI_CONSTRUCTORS_SOURCE
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
        Some(super::super::STANDARD_LIBRARY_V9_REVISION_ID)
    );
    assert_eq!(
        upgrade
            .application_revision()
            .catalogue_hash_context()
            .standard()
            .map(|snapshot| snapshot.digest()),
        Some(verified.digest()),
        "the V9 application caller must pin the upgraded standard digest"
    );
    assert_eq!(
        upgrade
            .application_revision()
            .catalogue_hash_context()
            .standard()
            .map(|snapshot| snapshot.digest_version()),
        Some(StandardLibraryDigestVersion::Version2)
    );
    let expected_catalogue_hash = catalogue_digest_with_context(
        upgrade.application_revision().catalogue_hash_context(),
        upgrade.application_revision().candidate(),
        upgrade.application_revision().new_function_revisions(),
        upgrade.application_revision().expressions(),
        upgrade.application_revision().origins(),
        upgrade.application_revision().references(),
    )
    .expect("the V9 application catalogue hash recomputes");
    assert_eq!(
        upgrade.application_revision().catalogue_hash(),
        expected_catalogue_hash,
        "the V9 application catalogue hash must cover the retained standard context"
    );
}

#[test]
fn v8_to_v9_upgrade_rejects_non_v8_parents_before_child_work() {
    let version_seven = super::super::verify_standard_library_v7_snapshot(
        super::super::retained_standard_library_v7_snapshot()
            .expect("the retained V7 standard source is valid"),
    )
    .expect("the retained V7 standard source verifies");
    let version_nine = super::super::verify_standard_library_v9_snapshot(
        super::super::retained_standard_library_v9_snapshot()
            .expect("the retained V9 standard source is valid"),
    )
    .expect("the retained V9 standard source verifies");

    for (standard, actual_parent_revision) in [
        (
            &version_seven,
            super::super::STANDARD_LIBRARY_V7_REVISION_ID,
        ),
        (&version_nine, super::super::STANDARD_LIBRARY_V9_REVISION_ID),
    ] {
        let active = empty_version_two_active_revision(standard);
        let error = super::super::prepare_standard_upgrade_v8_to_v9(&active)
            .expect_err("a non-V8 parent must not enter the V8-to-V9 path");
        assert!(matches!(
            error,
            super::super::StandardUpgradeError::Prepare {
                source: orna_compiler::PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                    revision
                }
            } if revision == actual_parent_revision
        ));
    }
}

#[test]
fn retains_and_verifies_v6_action_standard_snapshot() {
    let snapshot = super::super::retained_standard_library_v6_snapshot()
        .expect("the retained V6 action source is valid");
    assert_eq!(snapshot.source().units().len(), 6);
    assert_eq!(
        snapshot.source().parent(),
        Some(super::super::STANDARD_SOURCE_V5_REVISION_ID)
    );
    assert_eq!(
        snapshot.source().units()[5].id(),
        super::super::STD_ACTION_SOURCE_UNIT_ID
    );
    assert_eq!(
        snapshot.source().units()[5].logical_path(),
        super::super::STD_ACTION_SOURCE_LOGICAL_PATH
    );
    assert_eq!(snapshot.executables().len(), 2);
    assert_eq!(
        snapshot
            .origins()
            .iter()
            .filter(
                |origin| origin.source().source_unit() == super::super::STD_ACTION_SOURCE_UNIT_ID
            )
            .count(),
        3
    );
    let verified = super::super::verify_standard_library_v6_snapshot(snapshot)
        .expect("the retained V6 action source verifies");
    assert_eq!(
        verified.revision(),
        super::super::STANDARD_LIBRARY_V6_REVISION_ID
    );
    assert!(super::super::registered_opaque_codecs(&verified).is_ok());
}

#[test]
fn v6_action_source_has_the_exact_literal_bytes_and_parse() {
    let snapshot = super::super::retained_standard_library_v6_snapshot()
        .expect("the retained V6 action source is valid");
    let action = snapshot.source().units()[5].content();

    assert_eq!(action, super::super::RETAINED_STANDARD_ACTION_SOURCE);
    assert_eq!(action, EXPECTED_RETAINED_ACTION_SOURCE);
    assert_eq!(action.len(), 198);
    assert!(action.is_ascii());
    assert!(!action.as_bytes().starts_with(&[0xef, 0xbb, 0xbf]));
    assert!(!action.contains('\r'));
    assert!(action.ends_with('\n'));
    assert!(!action[..action.len() - 1].ends_with('\n'));
    assert_eq!(action.matches(';').count(), 3);

    let parsed = orna_syntax::parse(action);
    assert!(parsed.diagnostics().is_empty());
    assert_eq!(parsed.syntax().text(), action);
    assert_eq!(parsed.schemas().len(), 1);
    assert!(super::super::matches_qualified_name(
        &parsed.schemas()[0].name,
        &QualifiedSemanticName::new(["std", "action"])
            .expect("the fixed action schema name is valid"),
    ));
    assert_eq!(parsed.opaque_value_types().len(), 1);
    let action_type = &parsed.opaque_value_types()[0];
    assert!(super::super::matches_qualified_name(
        &action_type.name,
        &QualifiedSemanticName::new(["std", "action", "Action"])
            .expect("the fixed action type name is valid"),
    ));
    assert_eq!(
        super::super::decode_sql_string_literal(&action_type.kernel_contract.text).as_deref(),
        Some(STD_ACTION_CONTRACT),
    );
    assert_eq!(parsed.type_exports().len(), 1);
    let action_binding = snapshot
        .catalogue()
        .type_bindings()
        .iter()
        .find(|binding| binding.target() == STD_ACTION_TYPE_ID)
        .expect("the V6 action export is retained");
    assert!(super::super::matches_qualified_export(
        &parsed.type_exports()[0],
        &QualifiedSemanticName::new(["std", "action", "Action"])
            .expect("the fixed action type name is valid"),
        STD_ACTION_TYPE_ID,
        action_binding,
    ));
    assert!(parsed.object_types().is_empty());
    assert!(parsed.field_renames().is_empty());
    assert!(parsed.primitive_value_types().is_empty());
    assert!(parsed.record_value_types().is_empty());
    assert!(parsed.enum_types().is_empty());
    assert!(parsed.server_functions().is_empty());
    assert!(parsed.client_functions().is_empty());
}

#[test]
fn v6_action_origins_cover_the_exact_declaration_ranges() {
    let snapshot = super::super::retained_standard_library_v6_snapshot()
        .expect("the retained V6 action source is valid");
    let action = snapshot.source().units()[5].content();
    let action_origins = snapshot
        .origins()
        .iter()
        .filter(|origin| origin.source().source_unit() == STD_ACTION_SOURCE_UNIT_ID)
        .collect::<Vec<_>>();
    assert_eq!(action_origins.len(), 3);

    let schema_origin = |id: orna_core::SchemaId| {
        action_origins
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::Schema(id))
            .expect("the schema origin is retained")
            .source()
    };
    let type_origin = |id: orna_core::TypeId| {
        action_origins
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::ValueType(id))
            .expect("the type origin is retained")
            .source()
    };
    let binding_origin = |id: orna_core::TypeBindingId| {
        action_origins
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::TypeBinding(id))
            .expect("the binding origin is retained")
            .source()
    };

    let schema = schema_origin(STD_ACTION_SCHEMA_ID);
    assert_eq!(schema.byte_start(), 0);
    assert_eq!(schema.byte_end(), "CREATE SCHEMA std.action;".len() as u32);
    assert_eq!(
        &action[schema.byte_start() as usize..schema.byte_end() as usize],
        "CREATE SCHEMA std.action;",
    );

    let type_origin = type_origin(STD_ACTION_TYPE_ID);
    let type_start = action
        .find("CREATE TYPE std.action.Action")
        .expect("the action type is retained");
    let type_end = action
        .find("TRANSIENT;")
        .expect("the action type is retained")
        + "TRANSIENT;".len();
    assert_eq!(type_origin.byte_start(), type_start as u32);
    assert_eq!(type_origin.byte_end(), type_end as u32);
    assert_eq!(
        &action[type_origin.byte_start() as usize..type_origin.byte_end() as usize],
        &action[type_start..type_end],
    );

    let action_binding_id = snapshot
        .catalogue()
        .type_bindings()
        .iter()
        .find(|binding| binding.target() == STD_ACTION_TYPE_ID)
        .expect("the V6 action export is retained")
        .id();
    let binding_origin = binding_origin(action_binding_id);
    let binding_start = action
        .find("EXPORT TYPE std.action.Action AS std.Action;")
        .expect("the action export is retained");
    assert_eq!(binding_origin.byte_start(), binding_start as u32);
    assert_eq!(
        binding_origin.byte_end(),
        (binding_start + "EXPORT TYPE std.action.Action AS std.Action;".len()) as u32,
    );
    assert_eq!(
        &action[binding_origin.byte_start() as usize..binding_origin.byte_end() as usize],
        "EXPORT TYPE std.action.Action AS std.Action;",
    );
}

#[test]
fn v6_action_codec_rejects_malformed_descriptor_structure() {
    let verified = super::super::verify_standard_library_v6_snapshot(
        super::super::retained_standard_library_v6_snapshot()
            .expect("the retained V6 action source is valid"),
    )
    .expect("the retained V6 action source verifies");
    let registry =
        super::super::registered_opaque_codecs(&verified).expect("the V6 opaque codecs register");
    let active = empty_version_two_active_revision(&verified);

    let frame = |tag: u8, value: &[u8]| {
        let mut encoded = b"ORV3".to_vec();
        encoded.push(tag);
        encoded.extend_from_slice(&super::super::INTEGER_TYPE_ID.to_bytes());
        encoded.extend_from_slice(&(value.len() as u32).to_be_bytes());
        encoded.extend_from_slice(value);
        encoded
    };
    let descriptor_payload = |domain: u8, arguments: &[([u8; 16], Vec<u8>)]| {
        let mut body = vec![domain];
        for _ in 0..5 {
            body.extend_from_slice(&[0x11; 16]);
        }
        body.extend_from_slice(&(arguments.len() as u32).to_be_bytes());
        for (parameter, value) in arguments {
            body.extend_from_slice(parameter);
            body.extend_from_slice(&(value.len() as u32).to_be_bytes());
            body.extend_from_slice(value);
        }
        let mut payload = Vec::from(super::super::ACTION_MAGIC.as_bytes());
        payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
        payload.extend_from_slice(&body);
        payload
    };
    let integer = frame(0x03, &7_i32.to_be_bytes());
    let valid = descriptor_payload(1, &[([1; 16], integer.clone())]);
    let accepted = OpaqueValue::new(&active, &registry, super::super::STD_ACTION_TYPE_ID, &valid)
        .expect("a canonical action descriptor is accepted");
    assert_eq!(accepted.canonical_payload(), valid.as_slice());

    let mut arbitrary = Vec::from(super::super::ACTION_MAGIC.as_bytes());
    arbitrary.extend_from_slice(&3_u32.to_be_bytes());
    arbitrary.extend_from_slice(&[0xa5, 0x00, 0xff]);
    assert!(matches!(
        OpaqueValue::new(
            &active,
            &registry,
            super::super::STD_ACTION_TYPE_ID,
            arbitrary
        ),
        Err(OpaqueValueError::InvalidActionFrame { .. })
    ));

    let mut invalid_domain = valid.clone();
    invalid_domain[super::super::ACTION_MAGIC.len() + 4] = 0;
    let mut oversized_count = valid.clone();
    let count_offset = super::super::ACTION_MAGIC.len() + 4 + 1 + (16 * 5);
    oversized_count[count_offset..count_offset + 4].copy_from_slice(
        &u32::try_from(MAX_OPAQUE_CODEC_ACTION_ARGUMENTS + 1)
            .expect("the action argument limit fits in u32")
            .to_be_bytes(),
    );
    let mut bad_marker = valid.clone();
    let frame_offset = count_offset + 4 + 16 + 4;
    bad_marker[frame_offset..frame_offset + 4].copy_from_slice(b"ORV2");
    let bad_boolean = descriptor_payload(1, &[([1; 16], frame(0x02, &[2]))]);
    let mut trailing = valid.clone();
    trailing.push(0xaa);
    let body_length_offset = super::super::ACTION_MAGIC.len();
    let body_length = u32::from_be_bytes(
        trailing[body_length_offset..body_length_offset + 4]
            .try_into()
            .expect("the action body length is four bytes"),
    );
    trailing[body_length_offset..body_length_offset + 4]
        .copy_from_slice(&(body_length + 1).to_be_bytes());

    let unsorted = descriptor_payload(1, &[([2; 16], integer.clone()), ([1; 16], integer.clone())]);
    let repeated = descriptor_payload(1, &[([1; 16], integer.clone()), ([1; 16], integer.clone())]);
    let max_field_count_record_body = u32::try_from(MAX_RUNTIME_VALUE_NODES)
        .expect("the runtime node limit fits in u32")
        .to_be_bytes()
        .to_vec();
    let max_field_count_record =
        descriptor_payload(1, &[([1; 16], frame(0x0b, &max_field_count_record_body))]);

    let first_child = frame(0x07, &[0; 45]);
    let mut truncated_identity_record_body = 2_u32.to_be_bytes().to_vec();
    truncated_identity_record_body.extend_from_slice(&[0x44; 16]);
    truncated_identity_record_body.extend_from_slice(&(first_child.len() as u32).to_be_bytes());
    truncated_identity_record_body.extend_from_slice(&first_child);
    let truncated_identity_record = descriptor_payload(
        1,
        &[([1; 16], frame(0x0b, &truncated_identity_record_body))],
    );

    let mut duplicate_record_body = 2_u32.to_be_bytes().to_vec();
    for _ in 0..2 {
        duplicate_record_body.extend_from_slice(&[0x44; 16]);
        duplicate_record_body.extend_from_slice(&(integer.len() as u32).to_be_bytes());
        duplicate_record_body.extend_from_slice(&integer);
    }
    let duplicate_record = descriptor_payload(1, &[([1; 16], frame(0x0b, &duplicate_record_body))]);
    for malformed in [
        invalid_domain,
        oversized_count,
        bad_marker,
        bad_boolean,
        trailing,
        unsorted,
        repeated,
        duplicate_record,
        max_field_count_record,
        truncated_identity_record,
    ] {
        assert!(matches!(
            OpaqueValue::new(
                &active,
                &registry,
                super::super::STD_ACTION_TYPE_ID,
                malformed
            ),
            Err(OpaqueValueError::InvalidActionFrame { .. })
        ));
    }
}

#[test]
fn v6_manifest_appends_action_without_changing_v5_catalogue_content() {
    let v5 = super::super::standard_library_v5_manifest().expect("the V5 manifest is valid");
    let v6 = super::super::standard_library_v6_manifest().expect("the V6 manifest is valid");
    assert_eq!(
        v6.catalogue().schemas().len(),
        v5.catalogue().schemas().len() + 1
    );
    assert_eq!(
        v6.catalogue().value_types().len(),
        v5.catalogue().value_types().len() + 1
    );
    assert_eq!(
        v6.catalogue().type_bindings().len(),
        v5.catalogue().type_bindings().len() + 1
    );
    assert_eq!(v6.catalogue().functions(), v5.catalogue().functions());
    assert_eq!(
        v6.action_source_unit(),
        super::super::STD_ACTION_SOURCE_UNIT_ID
    );
    assert_eq!(
        v6.action_source_logical_path(),
        super::super::STD_ACTION_SOURCE_LOGICAL_PATH
    );
}

#[test]
fn v5_and_v6_retain_the_locked_sources_and_catalogue_identities() {
    let v4 = super::super::retained_standard_library_v4_snapshot()
        .expect("the retained V4 source is valid");
    let v5 = super::super::retained_standard_library_v5_snapshot()
        .expect("the retained V5 source is valid");
    let v6 = super::super::retained_standard_library_v6_snapshot()
        .expect("the retained V6 source is valid");

    assert_eq!(
        super::super::standard_library_v5_manifest()
            .expect("the V5 manifest is valid")
            .standard_library_version(),
        STANDARD_LIBRARY_V5_VERSION_IDENTITY
    );
    assert_eq!(v5.revision(), STANDARD_LIBRARY_V5_REVISION_ID);
    assert_eq!(v5.catalogue().revision(), STANDARD_CATALOGUE_V5_REVISION_ID);
    assert_eq!(v5.source().bundle(), STANDARD_SOURCE_V5_BUNDLE_ID);
    assert_eq!(v5.source().id(), STANDARD_SOURCE_V5_REVISION_ID);
    assert_eq!(v5.source().parent(), Some(STANDARD_SOURCE_V4_REVISION_ID));
    assert_eq!(v5.source().units().len(), 5);
    assert_eq!(&v5.source().units()[..4], v4.source().units());
    assert_eq!(v5.source().units()[4].id(), STD_JSON_SOURCE_UNIT_ID);
    assert_eq!(v5.source().units()[4].ordinal(), 4);
    assert_eq!(
        v5.source().units()[4].logical_path(),
        STD_JSON_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        v5.source().units()[4].content(),
        super::super::RETAINED_STANDARD_JSON_SOURCE
    );
    assert_eq!(v5.catalogue().schemas().len(), 7);
    assert_eq!(v5.catalogue().value_types().len(), 18);
    assert_eq!(v5.catalogue().type_bindings().len(), 35);
    assert_eq!(v5.catalogue().functions().len(), 2);
    assert_eq!(v5.origins().len(), 64);

    let json_schema = v5
        .catalogue()
        .schema_by_id(STD_JSON_SCHEMA_ID)
        .expect("the V5 JSON schema is retained");
    assert_eq!(json_schema.name().to_string(), "std.json");
    let json_type = v5
        .catalogue()
        .type_definition_by_id(STD_JSON_VALUE_TYPE_ID)
        .expect("the V5 JSON value type is retained")
        .as_opaque_value()
        .expect("the V5 JSON value type is opaque");
    assert_eq!(json_type.name().to_string(), "std.json.value");
    assert_eq!(json_type.representation_contract(), STD_JSON_CONTRACT);
    assert_eq!(
        v5.catalogue()
            .type_bindings()
            .iter()
            .find(|binding| binding.target() == STD_JSON_VALUE_TYPE_ID)
            .expect("the V5 JSON export is retained")
            .name()
            .to_string(),
        "std.jsonvalue"
    );
    assert!(v5
        .catalogue()
        .function_by_id(STD_JSON_ENCODE_FUNCTION_ID)
        .is_some());

    assert_eq!(
        super::super::standard_library_v6_manifest()
            .expect("the V6 manifest is valid")
            .standard_library_version(),
        STANDARD_LIBRARY_V6_VERSION_IDENTITY
    );
    assert_eq!(v6.revision(), STANDARD_LIBRARY_V6_REVISION_ID);
    assert_eq!(v6.catalogue().revision(), STANDARD_CATALOGUE_V6_REVISION_ID);
    assert_eq!(v6.source().bundle(), STANDARD_SOURCE_V6_BUNDLE_ID);
    assert_eq!(v6.source().id(), STANDARD_SOURCE_V6_REVISION_ID);
    assert_eq!(v6.source().parent(), Some(STANDARD_SOURCE_V5_REVISION_ID));
    assert_eq!(v6.source().units().len(), 6);
    assert_eq!(&v6.source().units()[..5], v5.source().units());
    assert_eq!(v6.source().units()[5].id(), STD_ACTION_SOURCE_UNIT_ID);
    assert_eq!(v6.source().units()[5].ordinal(), 5);
    assert_eq!(
        v6.source().units()[5].logical_path(),
        STD_ACTION_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        v6.source().units()[5].content(),
        super::super::RETAINED_STANDARD_ACTION_SOURCE
    );
    assert_eq!(v6.catalogue().schemas().len(), 8);
    assert_eq!(v6.catalogue().value_types().len(), 19);
    assert_eq!(v6.catalogue().type_bindings().len(), 36);
    assert_eq!(v6.catalogue().functions(), v5.catalogue().functions());
    assert_eq!(v6.executables(), v5.executables());
    assert_eq!(v6.origins().len(), 67);

    let action_schema = v6
        .catalogue()
        .schema_by_id(STD_ACTION_SCHEMA_ID)
        .expect("the V6 action schema is retained");
    assert_eq!(action_schema.name().to_string(), "std.action");
    let action_type = v6
        .catalogue()
        .type_definition_by_id(STD_ACTION_TYPE_ID)
        .expect("the V6 action value type is retained")
        .as_opaque_value()
        .expect("the V6 action value type is opaque");
    assert_eq!(action_type.name().to_string(), "std.action.action");
    assert_eq!(action_type.representation_contract(), STD_ACTION_CONTRACT);
    assert_eq!(
        v6.catalogue()
            .type_bindings()
            .iter()
            .find(|binding| binding.target() == STD_ACTION_TYPE_ID)
            .expect("the V6 action export is retained")
            .name()
            .to_string(),
        "std.action"
    );
}

#[test]
fn v5_and_v6_digest_goldens_cover_retained_units_and_catalogues() {
    let v5 = super::super::retained_standard_library_v5_snapshot()
        .expect("the retained V5 source is valid");
    let v6 = super::super::retained_standard_library_v6_snapshot()
        .expect("the retained V6 source is valid");

    let v5_expected = [
        super::super::ACCEPTED_V5_TYPES_CONTENT_DIGEST,
        super::super::ACCEPTED_V5_INVOKE_CONTENT_DIGEST,
        super::super::ACCEPTED_V5_OUTPUT_CONTENT_DIGEST,
        super::super::ACCEPTED_V5_UI_CONTENT_DIGEST,
        super::super::ACCEPTED_V5_JSON_CONTENT_DIGEST,
    ];
    for (unit, expected) in v5.source().units().iter().zip(v5_expected) {
        assert_eq!(
            source_unit_content_digest(unit.content()).expect("the V5 unit digest is valid"),
            expected
        );
    }
    assert_eq!(
        source_bundle_digest(v5.source().units()).expect("the V5 bundle digest is valid"),
        super::super::ACCEPTED_V5_SOURCE_BUNDLE_DIGEST
    );
    assert_eq!(
        source_revision_record_digest(
            STANDARD_SOURCE_V5_BUNDLE_ID,
            Some(STANDARD_SOURCE_V4_REVISION_ID),
            v5.source().bundle_hash(),
        )
        .expect("the V5 source revision digest is valid"),
        super::super::ACCEPTED_V5_SOURCE_REVISION_DIGEST
    );
    assert_eq!(
        standard_library_digest(&v5).expect("the V5 standard digest recomputes"),
        super::super::ACCEPTED_V5_STANDARD_LIBRARY_DIGEST
    );
    assert_eq!(
        v5.digest(),
        super::super::ACCEPTED_V5_STANDARD_LIBRARY_DIGEST
    );

    let v6_expected = [
        super::super::ACCEPTED_V6_TYPES_CONTENT_DIGEST,
        super::super::ACCEPTED_V6_INVOKE_CONTENT_DIGEST,
        super::super::ACCEPTED_V6_OUTPUT_CONTENT_DIGEST,
        super::super::ACCEPTED_V6_UI_CONTENT_DIGEST,
        super::super::ACCEPTED_V6_JSON_CONTENT_DIGEST,
        super::super::ACCEPTED_V6_ACTION_CONTENT_DIGEST,
    ];
    for (unit, expected) in v6.source().units().iter().zip(v6_expected) {
        assert_eq!(
            source_unit_content_digest(unit.content()).expect("the V6 unit digest is valid"),
            expected
        );
    }
    assert_eq!(
        source_bundle_digest(v6.source().units()).expect("the V6 bundle digest is valid"),
        super::super::ACCEPTED_V6_SOURCE_BUNDLE_DIGEST
    );
    assert_eq!(
        source_revision_record_digest(
            STANDARD_SOURCE_V6_BUNDLE_ID,
            Some(STANDARD_SOURCE_V5_REVISION_ID),
            v6.source().bundle_hash(),
        )
        .expect("the V6 source revision digest is valid"),
        super::super::ACCEPTED_V6_SOURCE_REVISION_DIGEST
    );
    assert_eq!(
        standard_library_digest(&v6).expect("the V6 standard digest recomputes"),
        super::super::ACCEPTED_V6_STANDARD_LIBRARY_DIGEST
    );
    assert_eq!(
        v6.digest(),
        super::super::ACCEPTED_V6_STANDARD_LIBRARY_DIGEST
    );
}

#[test]
fn v5_and_v6_opaque_codecs_match_the_append_only_registration_surface() {
    let v4 = super::super::verify_standard_library_v4_snapshot(
        super::super::retained_standard_library_v4_snapshot()
            .expect("the retained V4 source is valid"),
    )
    .expect("the retained V4 source verifies");
    let v5 = super::super::verify_standard_library_v5_snapshot(
        super::super::retained_standard_library_v5_snapshot()
            .expect("the retained V5 source is valid"),
    )
    .expect("the retained V5 source verifies");
    let v6 = super::super::verify_standard_library_v6_snapshot(
        super::super::retained_standard_library_v6_snapshot()
            .expect("the retained V6 source is valid"),
    )
    .expect("the retained V6 source verifies");
    let v4_registry = super::super::registered_opaque_codecs(&v4).expect("the V4 codecs register");
    let v5_registry = super::super::registered_opaque_codecs(&v5).expect("the V5 codecs register");
    let v6_registry = super::super::registered_opaque_codecs(&v6).expect("the V6 codecs register");
    let v4_active = empty_version_two_active_revision(&v4);
    let v5_active = empty_version_two_active_revision(&v5);
    let v6_active = empty_version_two_active_revision(&v6);

    let mut json_payload = Vec::from(JSON_MAGIC.as_bytes());
    json_payload.extend_from_slice(&7_u32.to_be_bytes());
    json_payload.extend_from_slice(br#"{"a":1}"#);
    assert_eq!(
        OpaqueValue::new(
            &v4_active,
            &v4_registry,
            STD_JSON_VALUE_TYPE_ID,
            &json_payload
        ),
        Err(OpaqueValueError::UnregisteredType {
            opaque_type: STD_JSON_VALUE_TYPE_ID
        })
    );
    assert_eq!(
        OpaqueValue::new(
            &v5_active,
            &v5_registry,
            STD_JSON_VALUE_TYPE_ID,
            &json_payload
        )
        .expect("the V5 JSON codec is registered")
        .canonical_payload(),
        json_payload.as_slice()
    );
    assert_eq!(
        OpaqueValue::new(
            &v6_active,
            &v6_registry,
            STD_JSON_VALUE_TYPE_ID,
            &json_payload
        )
        .expect("the V6 retained JSON codec is registered")
        .canonical_payload(),
        json_payload.as_slice()
    );

    let mut action_payload = Vec::from(ACTION_MAGIC.as_bytes());
    action_payload.extend_from_slice(&3_u32.to_be_bytes());
    action_payload.extend_from_slice(&[0xa5, 0x00, 0xff]);
    assert_eq!(
        OpaqueValue::new(
            &v5_active,
            &v5_registry,
            STD_ACTION_TYPE_ID,
            &action_payload
        ),
        Err(OpaqueValueError::UnregisteredType {
            opaque_type: STD_ACTION_TYPE_ID
        })
    );
    assert_eq!(
        OpaqueValue::new(
            &v6_active,
            &v6_registry,
            STD_ACTION_TYPE_ID,
            &action_payload
        ),
        Err(OpaqueValueError::InvalidActionFrame {
            opaque_type: STD_ACTION_TYPE_ID,
        })
    );
}

#[test]
fn v5_json_codec_rejects_non_canonical_body_bytes() {
    let verified = super::super::verify_standard_library_v5_snapshot(
        super::super::retained_standard_library_v5_snapshot()
            .expect("the retained V5 standard source is valid"),
    )
    .expect("the retained V5 standard source verifies");
    let registry =
        super::super::registered_opaque_codecs(&verified).expect("the V5 opaque codecs register");
    let active = empty_version_two_active_revision(&verified);

    let body = br#"{"a": 1}"#;
    let mut payload = Vec::from(JSON_MAGIC.as_bytes());
    payload.extend_from_slice(
        &u32::try_from(body.len())
            .expect("the canonical JSON body length fits in the frame")
            .to_be_bytes(),
    );
    payload.extend_from_slice(body);

    assert_eq!(
        OpaqueValue::new(&active, &registry, STD_JSON_VALUE_TYPE_ID, payload),
        Err(OpaqueValueError::InvalidJsonBody {
            opaque_type: STD_JSON_VALUE_TYPE_ID,
        })
    );
}

#[test]
fn v5_json_codec_rejects_malformed_and_trailing_frame_bytes() {
    let verified = super::super::verify_standard_library_v5_snapshot(
        super::super::retained_standard_library_v5_snapshot()
            .expect("the retained V5 standard source is valid"),
    )
    .expect("the retained V5 standard source verifies");
    let registry =
        super::super::registered_opaque_codecs(&verified).expect("the V5 opaque codecs register");
    let active = empty_version_two_active_revision(&verified);

    let mut truncated_body = Vec::from(JSON_MAGIC.as_bytes());
    truncated_body.extend_from_slice(&2_u32.to_be_bytes());
    truncated_body.extend_from_slice(b"{");
    assert_eq!(
        OpaqueValue::new(&active, &registry, STD_JSON_VALUE_TYPE_ID, &truncated_body,),
        Err(OpaqueValueError::InvalidFrameLength {
            opaque_type: STD_JSON_VALUE_TYPE_ID,
        })
    );

    let malformed_json = br#"{"a":}"#;
    let mut malformed_body = Vec::from(JSON_MAGIC.as_bytes());
    malformed_body.extend_from_slice(&(malformed_json.len() as u32).to_be_bytes());
    malformed_body.extend_from_slice(malformed_json);
    assert_eq!(
        OpaqueValue::new(&active, &registry, STD_JSON_VALUE_TYPE_ID, &malformed_body,),
        Err(OpaqueValueError::InvalidJsonBody {
            opaque_type: STD_JSON_VALUE_TYPE_ID,
        })
    );

    let body = br#"{"a":1}"#;
    let mut trailing = Vec::from(JSON_MAGIC.as_bytes());
    trailing.extend_from_slice(&(body.len() as u32).to_be_bytes());
    trailing.extend_from_slice(body);
    trailing.push(0);
    assert_eq!(
        OpaqueValue::new(&active, &registry, STD_JSON_VALUE_TYPE_ID, &trailing),
        Err(OpaqueValueError::InvalidFrameLength {
            opaque_type: STD_JSON_VALUE_TYPE_ID,
        })
    );
}

#[test]
fn v5_json_registry_accepts_canonical_values_and_rejects_wrong_magic_and_noncanonical_frames() {
    let verified = super::super::verify_standard_library_v5_snapshot(
        super::super::retained_standard_library_v5_snapshot()
            .expect("the retained V5 standard source is valid"),
    )
    .expect("the retained V5 standard source verifies");
    assert_eq!(verified.revision(), STANDARD_LIBRARY_V5_REVISION_ID);
    assert_eq!(
        verified.catalogue().revision(),
        STANDARD_CATALOGUE_V5_REVISION_ID
    );
    assert_eq!(JSON_MAGIC, "ORNA-JSON-VALUE/1 ");

    let registry = super::super::registered_opaque_codecs(&verified)
        .expect("the V5 opaque codecs register the JSON codec");
    let active = empty_version_two_active_revision(&verified);
    let frame = |magic: &[u8], body: &[u8]| {
        let mut payload = Vec::from(magic);
        payload.extend_from_slice(
            &u32::try_from(body.len())
                .expect("the JSON body length fits in the frame")
                .to_be_bytes(),
        );
        payload.extend_from_slice(body);
        payload
    };

    let canonical_body = br#"{"a":1,"nested":[true,null]}"#;
    let canonical_payload = frame(JSON_MAGIC.as_bytes(), canonical_body);
    let value = OpaqueValue::new(
        &active,
        &registry,
        STD_JSON_VALUE_TYPE_ID,
        &canonical_payload,
    )
    .expect("the V5 JSON registry accepts canonical JSON");
    assert_eq!(value.opaque_type(), STD_JSON_VALUE_TYPE_ID);
    assert_eq!(value.canonical_payload(), canonical_payload.as_slice());

    let wrong_magic = frame(b"WRONG-JSON/1 ", canonical_body);
    assert_eq!(
        OpaqueValue::new(&active, &registry, STD_JSON_VALUE_TYPE_ID, &wrong_magic),
        Err(OpaqueValueError::InvalidMagic {
            opaque_type: STD_JSON_VALUE_TYPE_ID,
        })
    );

    let noncanonical_body = br#"{ "a":1,"nested":[true,null]}"#;
    let noncanonical_payload = frame(JSON_MAGIC.as_bytes(), noncanonical_body);
    assert_eq!(
        OpaqueValue::new(
            &active,
            &registry,
            STD_JSON_VALUE_TYPE_ID,
            &noncanonical_payload,
        ),
        Err(OpaqueValueError::InvalidJsonBody {
            opaque_type: STD_JSON_VALUE_TYPE_ID,
        })
    );
}

#[test]
fn v6_json_registry_accepts_canonical_values_and_rejects_wrong_magic_and_noncanonical_frames() {
    let verified = super::super::verify_standard_library_v6_snapshot(
        super::super::retained_standard_library_v6_snapshot()
            .expect("the retained V6 standard source is valid"),
    )
    .expect("the retained V6 standard source verifies");
    assert_eq!(verified.revision(), STANDARD_LIBRARY_V6_REVISION_ID);
    assert_eq!(
        verified.catalogue().revision(),
        STANDARD_CATALOGUE_V6_REVISION_ID
    );
    assert_eq!(JSON_MAGIC, "ORNA-JSON-VALUE/1 ");

    let registry = super::super::registered_opaque_codecs(&verified)
        .expect("the V6 opaque codecs register the JSON codec");
    let active = empty_version_two_active_revision(&verified);
    let frame = |magic: &[u8], body: &[u8]| {
        let mut payload = Vec::from(magic);
        payload.extend_from_slice(
            &u32::try_from(body.len())
                .expect("the JSON body length fits in the frame")
                .to_be_bytes(),
        );
        payload.extend_from_slice(body);
        payload
    };

    let canonical_body = br#"{"a":1,"nested":[true,null]}"#;
    let canonical_payload = frame(JSON_MAGIC.as_bytes(), canonical_body);
    let value = OpaqueValue::new(
        &active,
        &registry,
        STD_JSON_VALUE_TYPE_ID,
        &canonical_payload,
    )
    .expect("the V6 JSON registry accepts canonical JSON");
    assert_eq!(value.opaque_type(), STD_JSON_VALUE_TYPE_ID);
    assert_eq!(value.canonical_payload(), canonical_payload.as_slice());

    let wrong_magic = frame(b"WRONG-JSON/1 ", canonical_body);
    assert_eq!(
        OpaqueValue::new(&active, &registry, STD_JSON_VALUE_TYPE_ID, &wrong_magic),
        Err(OpaqueValueError::InvalidMagic {
            opaque_type: STD_JSON_VALUE_TYPE_ID,
        })
    );

    let noncanonical_body = br#"{ "a":1,"nested":[true,null]}"#;
    let noncanonical_payload = frame(JSON_MAGIC.as_bytes(), noncanonical_body);
    assert_eq!(
        OpaqueValue::new(
            &active,
            &registry,
            STD_JSON_VALUE_TYPE_ID,
            &noncanonical_payload,
        ),
        Err(OpaqueValueError::InvalidJsonBody {
            opaque_type: STD_JSON_VALUE_TYPE_ID,
        })
    );
}

#[test]
fn v5_append_only_retains_v4_source_units_byte_for_byte() {
    let v4 = super::super::retained_standard_library_v4_snapshot()
        .expect("the retained V4 standard source is valid");
    let v5 = super::super::retained_standard_library_v5_snapshot()
        .expect("the retained V5 standard source is valid");

    assert_eq!(&v5.source().units()[..4], v4.source().units());
    assert_eq!(v4.source().units()[0].content(), RETAINED_STANDARD_SOURCE);
    assert_eq!(
        v4.source().units()[1].content(),
        RETAINED_STANDARD_INVOKE_SOURCE
    );
    assert_eq!(
        v4.source().units()[2].content(),
        RETAINED_STANDARD_OUTPUT_SOURCE
    );
    assert_eq!(
        v4.source().units()[3].content(),
        RETAINED_STANDARD_UI_SOURCE
    );
    assert_eq!(
        v4.digest(),
        super::super::ACCEPTED_V4_STANDARD_LIBRARY_DIGEST
    );
    assert_eq!(
        super::super::standard_library_digest(&v4).expect("the V4 digest recomputes"),
        super::super::ACCEPTED_V4_STANDARD_LIBRARY_DIGEST
    );
}

#[test]
fn inspect_carrier_registry_is_fixed_and_deterministic() {
    let expected = [
        (
            SYS_INSPECT_SNAPSHOT_TYPE_ID,
            SYS_INSPECT_SNAPSHOT_REPRESENTATION_CONTRACT,
        ),
        (
            SYS_INSPECT_INVOCATION_NODES_TYPE_ID,
            SYS_INSPECT_INVOCATION_NODES_REPRESENTATION_CONTRACT,
        ),
        (
            SYS_INSPECT_CALLS_TYPE_ID,
            SYS_INSPECT_CALLS_REPRESENTATION_CONTRACT,
        ),
        (
            SYS_INSPECT_RESOURCES_TYPE_ID,
            SYS_INSPECT_RESOURCES_REPRESENTATION_CONTRACT,
        ),
        (
            SYS_INSPECT_STATE_CELLS_TYPE_ID,
            SYS_INSPECT_STATE_CELLS_REPRESENTATION_CONTRACT,
        ),
        (
            SYS_INSPECT_UI_NODES_TYPE_ID,
            SYS_INSPECT_UI_NODES_REPRESENTATION_CONTRACT,
        ),
        (
            SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID,
            SYS_INSPECT_PRESENTATION_CANDIDATES_REPRESENTATION_CONTRACT,
        ),
        (
            SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID,
            SYS_INSPECT_RUNTIME_BINDINGS_REPRESENTATION_CONTRACT,
        ),
        (
            SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID,
            SYS_INSPECT_SECURITY_DECISIONS_REPRESENTATION_CONTRACT,
        ),
    ];
    let registrations = registered_inspect_carrier_codecs();
    assert_eq!(registrations.len(), expected.len());
    for (registration, (opaque_type, contract)) in registrations.iter().zip(expected) {
        assert_eq!(registration.opaque_type(), opaque_type);
        assert_eq!(registration.representation_contract(), contract);
        assert!(is_registered_inspect_carrier_type(opaque_type));
    }
    assert!(!is_registered_inspect_carrier_type(TypeId::from_bytes(
        [0xaa; 16]
    )));
}

#[test]
fn v8_rows_snapshot_retains_canonical_source_and_digests() {
    let snapshot = super::super::retained_standard_library_v8_snapshot()
        .expect("the retained V8 source is valid");
    let units = snapshot.source().units();
    assert_eq!(units.len(), 8);
    assert_eq!(
        snapshot.source().parent(),
        Some(super::super::STANDARD_SOURCE_V7_REVISION_ID)
    );
    assert_eq!(units[7].id(), super::super::STD_DATA_SOURCE_UNIT_ID);
    assert_eq!(units[7].ordinal(), 7);
    assert_eq!(
        units[7].logical_path(),
        super::super::STD_DATA_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        units[7].content(),
        super::super::RETAINED_STANDARD_DATA_SOURCE
    );
    assert_eq!(
        source_unit_content_digest(super::super::RETAINED_STANDARD_DATA_SOURCE)
            .expect("the data source digest is valid"),
        units[7].content_hash()
    );
    assert_eq!(
        source_bundle_digest(units).expect("the V8 bundle digest is valid"),
        snapshot.source().bundle_hash()
    );
    assert_eq!(
        source_revision_record_digest(
            super::super::STANDARD_SOURCE_V8_BUNDLE_ID,
            Some(super::super::STANDARD_SOURCE_V7_REVISION_ID),
            snapshot.source().bundle_hash(),
        )
        .expect("the V8 source revision digest is valid"),
        snapshot.source().revision_hash()
    );
    assert_eq!(
        calculate_standard_library_digest(&snapshot).expect("the V8 standard digest recomputes"),
        snapshot.digest()
    );
    assert_eq!(
        snapshot
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>(),
        vec![
            super::super::STD_INVOKE_ECHO_FUNCTION_ID,
            super::super::STD_JSON_ENCODE_FUNCTION_ID,
            super::super::STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            super::super::STD_UI_WINDOW_FUNCTION_ID,
        ]
    );
    assert_eq!(
        snapshot
            .executables()
            .iter()
            .map(StandardExecutable::function)
            .collect::<Vec<_>>(),
        snapshot
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>()
    );

    let verified = super::super::verify_standard_library_v8_snapshot(snapshot)
        .expect("the V8 snapshot verifies");
    let registry =
        super::super::registered_opaque_codecs(&verified).expect("the V8 codecs register");
    let active = empty_version_two_active_revision(&verified);
    let mut payload = b"ORNA-ROWS/1 ".to_vec();
    payload.extend_from_slice(&1_u16.to_be_bytes());
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.push(b'x');
    payload.push(0x01);
    payload.extend_from_slice(&[0; 15]);
    payload.push(0x02);
    payload.push(0);
    payload.extend_from_slice(&0_u32.to_be_bytes());
    let value = OpaqueValue::new(
        &active,
        &registry,
        super::super::STD_DATA_ROWS_TYPE_ID,
        &payload,
    )
    .expect("the V8 registry admits the canonical zero-row Rows frame");
    assert_eq!(value.canonical_payload(), payload);
}
#[test]
fn v9_ui_constructors_snapshot_retains_source_digests_and_codecs() {
    let snapshot = super::super::retained_standard_library_v9_snapshot()
        .expect("the retained V9 source is valid");
    let units = snapshot.source().units();
    assert_eq!(units.len(), 9);
    assert_eq!(
        snapshot.source().parent(),
        Some(super::super::STANDARD_SOURCE_V8_REVISION_ID)
    );
    assert_eq!(
        units[8].id(),
        super::super::STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID
    );
    assert_eq!(units[8].ordinal(), 8);
    assert_eq!(
        units[8].logical_path(),
        super::super::STD_UI_CONSTRUCTORS_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        units[8].content(),
        super::super::RETAINED_STANDARD_UI_CONSTRUCTORS_SOURCE
    );
    assert_eq!(
        source_unit_content_digest(super::super::RETAINED_STANDARD_UI_CONSTRUCTORS_SOURCE)
            .expect("constructor source digest"),
        units[8].content_hash()
    );
    assert_eq!(
        source_bundle_digest(units).expect("the V9 bundle digest is valid"),
        snapshot.source().bundle_hash()
    );
    assert_eq!(
        source_revision_record_digest(
            super::super::STANDARD_SOURCE_V9_BUNDLE_ID,
            Some(super::super::STANDARD_SOURCE_V8_REVISION_ID),
            snapshot.source().bundle_hash(),
        )
        .expect("the V9 source revision digest is valid"),
        snapshot.source().revision_hash()
    );
    assert_eq!(
        calculate_standard_library_digest(&snapshot).expect("the V9 standard digest recomputes"),
        snapshot.digest()
    );
    let expected_functions = vec![
        super::super::STD_INVOKE_ECHO_FUNCTION_ID,
        super::super::STD_JSON_ENCODE_FUNCTION_ID,
        super::super::STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        super::super::STD_UI_WINDOW_FUNCTION_ID,
        super::super::STD_UI_TEXT_FUNCTION_ID,
        super::super::STD_UI_BUTTON_FUNCTION_ID,
        super::super::STD_UI_PANEL_FUNCTION_ID,
        super::super::STD_UI_ROW_FUNCTION_ID,
        super::super::STD_UI_COLUMN_FUNCTION_ID,
        super::super::STD_UI_TEXT_INPUT_FUNCTION_ID,
        super::super::STD_UI_TABS_FUNCTION_ID,
    ];
    assert_eq!(
        snapshot
            .catalogue()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>(),
        expected_functions
    );
    assert_eq!(
        snapshot
            .executables()
            .iter()
            .map(StandardExecutable::function)
            .collect::<Vec<_>>(),
        expected_functions
    );

    let verified = super::super::verify_standard_library_v9_snapshot(snapshot)
        .expect("the V9 snapshot verifies");
    let registry =
        super::super::registered_opaque_codecs(&verified).expect("the V9 codecs register");
    let active = empty_version_two_active_revision(&verified);
    let checked =
        orna_compiler::check_standard_library_source(&verified).expect("the V9 source checks");
    assert_eq!(
        checked.checked_executables().len(),
        expected_functions.len()
    );

    let mut rows_payload = b"ORNA-ROWS/1 ".to_vec();
    rows_payload.extend_from_slice(&1_u16.to_be_bytes());
    rows_payload.extend_from_slice(&1_u32.to_be_bytes());
    rows_payload.extend_from_slice(&1_u32.to_be_bytes());
    rows_payload.push(b'x');
    rows_payload.push(0x01);
    rows_payload.extend_from_slice(&[0; 15]);
    rows_payload.push(0x02);
    rows_payload.push(0);
    rows_payload.extend_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        OpaqueValue::new(
            &active,
            &registry,
            super::super::STD_DATA_ROWS_TYPE_ID,
            &rows_payload,
        )
        .expect("the V9 registry retains the Rows codec")
        .canonical_payload(),
        rows_payload
    );
}

#[test]
fn v10_cli_snapshot_retains_source_and_recomputes_digests() {
    let snapshot = super::super::retained_standard_library_v10_snapshot()
        .expect("the retained V10 source is valid");
    assert_eq!(snapshot.source().units().len(), 10);
    assert_eq!(
        snapshot.source().parent(),
        Some(super::super::STANDARD_SOURCE_V9_REVISION_ID)
    );
    assert_eq!(
        snapshot.source().units()[9].logical_path(),
        super::super::STD_CLI_SOURCE_LOGICAL_PATH
    );
    assert_eq!(
        source_unit_content_digest(super::super::RETAINED_STANDARD_CLI_SOURCE)
            .expect("the CLI source digest is valid"),
        snapshot.source().units()[9].content_hash()
    );
    assert_eq!(
        source_bundle_digest(snapshot.source().units()).expect("the V10 bundle digest is valid"),
        snapshot.source().bundle_hash()
    );
    assert_eq!(
        calculate_standard_library_digest(&snapshot).expect("the V10 digest recomputes"),
        snapshot.digest()
    );
    let verified = super::super::verify_standard_library_v10_snapshot(snapshot)
        .expect("the V10 snapshot verifies");
    let checked =
        orna_compiler::check_standard_library_source(&verified).expect("the V10 source checks");
    assert_eq!(checked.checked_executables().len(), 12);
}
