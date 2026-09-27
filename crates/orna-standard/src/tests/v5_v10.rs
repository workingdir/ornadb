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
fn v6_action_snapshot_selection_and_supplied_snapshots_fail_closed() {
    let revision = super::super::STANDARD_LIBRARY_V6_REVISION_ID;
    assert!(matches!(
        super::super::retained_standard_library_v6_snapshot(),
        Err(super::super::StandardLibraryError::UnsupportedRevision { revision: actual }) if actual == revision
    ));
    assert!(matches!(
        super::super::select_verified_standard_library(revision),
        Err(super::super::StandardLibraryError::UnsupportedRevision { revision: actual }) if actual == revision
    ));
    let supplied = super::super::retained_standard_library_v5_snapshot()
        .expect("the retired verifier rejects caller-supplied snapshots before inspecting them");
    assert!(matches!(
        super::super::verify_standard_library_v6_snapshot(supplied),
        Err(super::super::StandardLibraryError::UnsupportedRevision { revision: actual }) if actual == revision
    ));
}

#[test]
fn v7_source_selection_and_supplied_snapshots_fail_closed() {
    let error = super::super::select_verified_standard_library(
        super::super::STANDARD_LIBRARY_V7_REVISION_ID,
    )
    .expect_err("retired V7 source selection must fail closed");
    assert!(matches!(
        error,
        super::super::StandardLibraryError::UnsupportedRevision { revision }
            if revision == super::super::STANDARD_LIBRARY_V7_REVISION_ID
    ));

    let supplied = super::super::retained_standard_library_v5_snapshot()
        .expect("the V5 fixture is available for a supplied-snapshot rejection check");
    let error = super::super::verify_standard_library_v7_snapshot(supplied)
        .expect_err("caller-supplied historical revisions must not regain V7 authority");
    assert!(matches!(
        error,
        super::super::StandardLibraryError::UnsupportedRevision { revision }
            if revision == super::super::STANDARD_LIBRARY_V7_REVISION_ID
    ));
}

#[test]
fn v9_retired_standard_source_fails_closed() {
    let manifest = super::super::standard_library_v9_manifest()
        .expect("the V9 identity remains reserved as historical metadata");
    assert!(manifest.catalogue().functions().iter().all(|function| {
        ![
            "std.ui.text",
            "std.ui.button",
            "std.ui.panel",
            "std.ui.row",
            "std.ui.column",
            "std.ui.text_input",
            "std.ui.tabs",
        ]
        .contains(&function.name().to_string().as_str())
    }));
    let retained = super::super::retained_standard_library_v9_snapshot()
        .expect_err("retired V9 source must not be reconstructed");
    assert!(matches!(
        retained,
        super::super::StandardLibraryError::UnsupportedRevision { revision }
            if revision == super::super::STANDARD_LIBRARY_V9_REVISION_ID
    ));
    let selected = super::super::select_verified_standard_library(
        super::super::STANDARD_LIBRARY_V9_REVISION_ID,
    )
    .expect_err("retired V9 selection must fail closed");
    assert!(matches!(
        selected,
        super::super::StandardLibraryError::UnsupportedRevision { revision }
            if revision == super::super::STANDARD_LIBRARY_V9_REVISION_ID
    ));
}

#[test]
fn v8_to_v9_upgrade_fails_closed_after_rows_source_retirement() {
    let error = super::super::retained_standard_library_v8_snapshot()
        .expect_err("retired V8 source must not be reconstructed");
    assert!(matches!(
        error,
        super::super::StandardLibraryError::UnsupportedRevision { revision }
            if revision == super::super::STANDARD_LIBRARY_V8_REVISION_ID
    ));
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
fn v8_rows_snapshot_fails_closed_after_source_retirement() {
    let error = super::super::retained_standard_library_v8_snapshot()
        .expect_err("retired V8 Rows source must not be reconstructed");
    assert!(matches!(
        error,
        super::super::StandardLibraryError::UnsupportedRevision { revision }
            if revision == super::super::STANDARD_LIBRARY_V8_REVISION_ID
    ));
    let supplied = super::super::retained_standard_library_snapshot()
        .expect("the pinned reference standard snapshot remains available");
    let supplied = super::super::verify_standard_library_v8_snapshot(supplied)
        .expect_err("a supplied historical snapshot cannot restore retired V8");
    assert!(matches!(
        supplied,
        super::super::StandardLibraryError::UnsupportedRevision { revision }
            if revision == super::super::STANDARD_LIBRARY_V8_REVISION_ID
    ));
    let selected = super::super::select_verified_standard_library(
        super::super::STANDARD_LIBRARY_V8_REVISION_ID,
    )
    .expect_err("retired V8 selection must fail closed");
    assert!(matches!(
        selected,
        super::super::StandardLibraryError::UnsupportedRevision { revision }
            if revision == super::super::STANDARD_LIBRARY_V8_REVISION_ID
    ));
}
#[test]
fn v10_cli_source_is_not_retained_or_selected() {
    let retained = super::super::retained_standard_library_v10_snapshot()
        .expect_err("V10 source must not be reconstructed or substituted");
    assert!(matches!(
        retained,
        super::super::StandardLibraryError::UnsupportedRevision { revision }
            if revision == super::super::STANDARD_LIBRARY_V10_REVISION_ID
    ));
    let selected = super::super::select_verified_standard_library(
        super::super::STANDARD_LIBRARY_V10_REVISION_ID,
    )
    .expect_err("V10 selection must fail closed");
    assert!(matches!(
        selected,
        super::super::StandardLibraryError::UnsupportedRevision { revision }
            if revision == super::super::STANDARD_LIBRARY_V10_REVISION_ID
    ));
}
