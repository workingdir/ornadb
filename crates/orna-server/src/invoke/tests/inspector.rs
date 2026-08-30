//! Installed Inspector invocation and carrier tests.

use super::*;
fn inspect_test_context() -> (
    ActiveDatabaseRevision,
    orna_core::value::OpaqueCodecRegistry,
) {
    let source_bundle = SourceBundleId::from_bytes([0x91; 16]);
    let source_revision = SourceRevisionId::from_bytes([0x92; 16]);
    let bundle_hash = source_bundle_digest(&[]).expect("source bundle digest");
    let source = StoredSourceRevision::new(
        source_bundle,
        source_revision,
        None,
        Vec::new(),
        bundle_hash,
        source_revision_record_digest(source_bundle, None, bundle_hash)
            .expect("source revision digest"),
    )
    .expect("stored source revision");
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x93; 16]),
        Vec::new(),
        Vec::new(),
    )
    .expect("empty catalogue");
    let standard = verify_standard_library_snapshot(
        retained_standard_library_snapshot().expect("retained standard snapshot"),
    )
    .expect("verified standard snapshot");
    let catalogue_hash =
        catalogue_digest(&catalogue, &[], &[], &[], &[]).expect("catalogue digest");
    let pair = RevisionPair::new(source.id(), catalogue.revision());
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ),
        CatalogueHashContext::version_two(standard.clone()),
    )
    .expect("active revision");
    let registry = registered_opaque_codecs(&standard).expect("standard codecs");
    (active, registry)
}

#[test]
fn inspector_carrier_errors_map_to_stable_codes() {
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::EnvelopeTooLarge {
            actual: 17,
            maximum: 16,
        }),
        "inspect.limit"
    );
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::RowCountExceeded {
            actual: 2,
            maximum: 1,
        }),
        "inspect.limit"
    );
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::RowTooLarge {
            actual: 17,
            maximum: 16,
        }),
        "inspect.limit"
    );
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::InvalidMagic),
        "inspect.malformed_carrier"
    );
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::UnknownProjectionTag(0xff)),
        "inspect.malformed_carrier"
    );
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::InvalidTargetInvocation),
        "inspect.invalid_target"
    );
    assert_eq!(
        map_inspect_carrier_error(InspectCarrierError::TargetInvocationMismatch {
            expected: InvocationId::from_bytes([0x11; 16]),
            actual: InvocationId::from_bytes([0x22; 16]),
        }),
        "inspect.epoch_mismatch"
    );
    assert_eq!(
        map_inspect_opaque_value_error(OpaqueValueError::UnregisteredType {
            opaque_type: TypeId::from_bytes([0x33; 16]),
        }),
        "inspect.unknown_carrier"
    );
    assert_eq!(
        map_inspect_opaque_value_error(OpaqueValueError::InspectCarrierRevisionMismatch {
            opaque_type: TypeId::from_bytes([0x44; 16]),
        },),
        "inspect.epoch_mismatch"
    );
}

#[test]
fn inspector_snapshot_target_rejects_zero_object_bytes() {
    let target = RuntimeValue::Reference {
        target: SYS_INSPECT_INVOCATION_TYPE_ID,
        object: orna_core::ObjectId::from_bytes([0; 16]),
    };
    assert_eq!(
        inspect_snapshot_request_target(&target),
        Err("inspect.invalid_target".to_owned()),
    );
}

#[test]
fn inspector_snapshot_row_rejects_zero_value_batch_count() {
    let target = InvocationId::from_bytes([0x17; 16]);
    let epoch = InspectEpochId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
    let root_target = FunctionId::from_bytes([0x18; 16]);
    let mut row = row(INSPECT_SNAPSHOT_ROW_TAG, 0);
    row.extend_from_slice(&epoch.to_bytes());
    row.extend_from_slice(&target.to_bytes());
    row.extend_from_slice(&[0x18; 16]);
    row.push(1);
    row.extend_from_slice(&0_u64.to_be_bytes());
    row.push(1);
    row.extend_from_slice(&0_u64.to_be_bytes());
    row.push(0);

    assert_eq!(row.len(), 76);
    let mut no_values = row.clone();
    no_values[66] = 0;
    no_values.truncate(68);
    assert_eq!(no_values.len(), 68);
    assert_eq!(
        decode_snapshot_row_payload(&no_values, 7),
        Ok((epoch, target, root_target))
    );
    assert_eq!(
        decode_snapshot_row_payload(&row, 7),
        Err("inspect.malformed_carrier".to_owned())
    );
    row[67..75].copy_from_slice(&1_u64.to_be_bytes());
    assert_eq!(
        decode_snapshot_row_payload(&row, 7),
        Ok((epoch, target, root_target))
    );
    row.push(0x19);
    assert_eq!(
        decode_snapshot_row_payload(&row, 7),
        Err("inspect.malformed_carrier".to_owned())
    );
}

#[test]
fn inspector_snapshot_row_rejects_forged_root_provenance() {
    let target = InvocationId::from_bytes([0x17; 16]);
    let epoch = InspectEpochId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
    let expected_root = FunctionId::from_bytes([0x18; 16]);
    let forged_root = FunctionId::from_bytes([0x19; 16]);
    let mut row = row(INSPECT_SNAPSHOT_ROW_TAG, 0);
    row.extend_from_slice(&epoch.to_bytes());
    row.extend_from_slice(&target.to_bytes());
    row.extend_from_slice(&expected_root.to_bytes());
    row.push(1);
    row.extend_from_slice(&0_u64.to_be_bytes());
    row.push(0);
    row.push(0);

    let (_, _, decoded_root) = decode_snapshot_row_payload(&row, 7).expect("valid snapshot row");
    assert_eq!(
        require_inspect_root_provenance(expected_root, decoded_root),
        Ok(())
    );

    row[41..57].copy_from_slice(&forged_root.to_bytes());
    let (_, _, decoded_root) =
        decode_snapshot_row_payload(&row, 7).expect("forged root remains well-formed");
    assert_eq!(
        require_inspect_root_provenance(expected_root, decoded_root),
        Err("inspect.epoch_mismatch".to_owned())
    );
}

#[test]
fn inspector_enriched_row_rejects_forged_root_provenance() {
    let (active, registry) = inspect_test_context();
    let epoch = InspectEpochId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
    let target = InvocationId::from_bytes([0x10; 16]);
    let expected_root = FunctionId::from_bytes([0x11; 16]);
    let forged_root = FunctionId::from_bytes([0x12; 16]);
    let mut payload = row(InspectCarrierKind::SecurityDecisions.tag(), 0);
    id(&mut payload, &epoch.to_bytes());
    id(&mut payload, &target.to_bytes());
    id(&mut payload, &expected_root.to_bytes());
    id(&mut payload, &active.pair().source().to_bytes());
    id(&mut payload, &active.pair().catalogue().to_bytes());
    payload.push(1);
    payload.push(0);
    payload.extend_from_slice(&[4, 1, 0, 2]);

    let encoded = encode_inspect_row(&active, &registry, payload.clone())
        .expect("canonical enriched Inspector row");
    let (_, _, decoded_root) = decode_enriched_inspect_row_target(
        &active,
        &registry,
        &encoded,
        InspectCarrierKind::SecurityDecisions,
        7,
    )
    .expect("valid enriched Inspector row");
    assert_eq!(
        require_inspect_root_provenance(expected_root, decoded_root),
        Ok(())
    );

    let mut forged_payload = payload;
    forged_payload[41..57].copy_from_slice(&forged_root.to_bytes());
    let forged_encoded = encode_inspect_row(&active, &registry, forged_payload)
        .expect("forged root remains well-formed");
    let (_, _, decoded_root) = decode_enriched_inspect_row_target(
        &active,
        &registry,
        &forged_encoded,
        InspectCarrierKind::SecurityDecisions,
        7,
    )
    .expect("forged root remains structurally valid");
    assert_eq!(
        require_inspect_root_provenance(expected_root, decoded_root),
        Err("inspect.epoch_mismatch".to_owned())
    );
}

#[test]
fn inspector_enriched_row_rejects_zero_target() {
    let (active, registry) = inspect_test_context();
    let epoch = InspectEpochId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7]);
    let mut payload = row(InspectCarrierKind::SecurityDecisions.tag(), 0);
    id(&mut payload, &epoch.to_bytes());
    id(&mut payload, &[0; 16]);
    id(&mut payload, &[0x11; 16]);
    id(&mut payload, &active.pair().source().to_bytes());
    id(&mut payload, &active.pair().catalogue().to_bytes());
    payload.push(1);
    payload.push(0);
    payload.extend_from_slice(&[4, 1, 0, 2]);

    let encoded =
        encode_inspect_row(&active, &registry, payload).expect("canonical enriched Inspector row");
    assert_eq!(
        decode_enriched_inspect_row_target(
            &active,
            &registry,
            &encoded,
            InspectCarrierKind::SecurityDecisions,
            7,
        ),
        Err("inspect.invalid_target".to_owned())
    );
}

#[test]
fn inspector_projection_requires_target_provenance() {
    let target = InvocationId::from_bytes([0x11; 16]);
    assert_eq!(
        require_inspect_target_provenance(None, target),
        Err("inspect.malformed_carrier".to_owned()),
    );
    assert_eq!(
        require_inspect_target_provenance(Some(InvocationId::from_bytes([0x22; 16])), target,),
        Err("inspect.epoch_mismatch".to_owned()),
    );
    assert_eq!(
        require_inspect_target_provenance(Some(target), target),
        Ok(())
    );
}

#[tokio::test]
async fn inspector_render_recursion_checks_root_parent_and_non_recursive_targets() {
    let root = InvocationId::from_bytes([0x31; 16]);
    let parent = InvocationId::from_bytes([0x32; 16]);
    let descendant = InvocationId::from_bytes([0x33; 16]);
    let mut checked = Vec::new();

    let result = reject_recursive_inspect_target(root, root, parent, |observer, target| {
        checked.push(observer);
        async move { Ok(observer == target) }
    })
    .await;
    assert_eq!(result, Err("inspect.recursion".to_owned()));
    assert_eq!(checked, vec![root]);

    checked.clear();
    let result = reject_recursive_inspect_target(descendant, root, parent, |observer, target| {
        checked.push(observer);
        async move { Ok(observer == parent && target == descendant) }
    })
    .await;
    assert_eq!(result, Err("inspect.recursion".to_owned()));
    assert_eq!(checked, vec![root, parent]);

    checked.clear();
    let result = reject_recursive_inspect_target(
        InvocationId::from_bytes([0x34; 16]),
        root,
        root,
        |observer, _target| {
            checked.push(observer);
            async { Ok(false) }
        },
    )
    .await;
    assert_eq!(result, Ok(()));
    assert_eq!(checked, vec![root]);
}

#[tokio::test]
async fn inspector_denied_recursive_target_does_not_query_lineage() {
    let target = InvocationId::from_bytes([0x35; 16]);
    let observer = InvocationId::from_bytes([0x36; 16]);
    let observer_lineage = [observer];
    let mut checked = Vec::new();

    let result: Result<(), String> = authorize_inspect_target_before_recursion(
        || async { Err("inspect.denied".to_owned()) },
        target,
        &observer_lineage,
        |ancestor, candidate| {
            checked.push((ancestor, candidate));
            async move { Ok(true) }
        },
    )
    .await;

    assert_eq!(result, Err("inspect.denied".to_owned()));
    assert!(
        checked.is_empty(),
        "denied targets must not be classified for recursion"
    );
}

#[test]
fn inspector_projection_requires_matching_observer_context() {
    let root = InvocationId::from_bytes([0x61; 16]);
    let parent = InvocationId::from_bytes([0x62; 16]);
    let other = InvocationId::from_bytes([0x63; 16]);
    let context = InspectObserverContext::new(root, parent).expect("observer context");

    assert_eq!(
        require_inspect_observer_context(Some(context), root, parent),
        Ok(())
    );
    assert_eq!(
        require_inspect_observer_context(Some(context), other, parent),
        Err("inspect.epoch_mismatch".to_owned())
    );
    assert_eq!(
        require_inspect_observer_context(None, root, parent),
        Err("inspect.epoch_mismatch".to_owned())
    );
    assert_eq!(
        require_inspect_observer_context(Some(context), InvocationId::from_bytes([0; 16]), parent,),
        Err("inspect.epoch_mismatch".to_owned())
    );
}

#[test]
fn inspector_rejects_forged_current_observer_root() {
    let root = InvocationId::from_bytes([0x71; 16]);
    let other = InvocationId::from_bytes([0x72; 16]);

    assert_eq!(
        require_current_observer_invocation(Some(root), root),
        Ok(root)
    );
    assert_eq!(
        require_current_observer_invocation(Some(root), other),
        Err("inspect.epoch_mismatch".to_owned())
    );
    assert_eq!(
        require_current_observer_invocation(None, root),
        Err("inspect.epoch_mismatch".to_owned())
    );
}

#[test]
fn inspector_projection_binding_rejects_target_epoch_and_revision_mismatches() {
    let target = InvocationId::from_bytes([0x11; 16]);
    let other_target = InvocationId::from_bytes([0x22; 16]);
    let mut epoch_bytes = [0; 16];
    epoch_bytes[15] = 0x33;
    let epoch = InspectEpochId::from_bytes(epoch_bytes);
    let pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x44; 16]),
        CatalogueRevisionId::from_bytes([0x55; 16]),
    );
    let envelope = InspectCarrierEnvelope::new_with_target(
        InspectCarrierKind::Snapshot,
        target,
        InspectCarrierProvenance::trusted_for_target(0x33, target, pair.source(), pair.catalogue()),
        Vec::new(),
    )
    .expect("snapshot envelope");
    assert_eq!(
        validate_inspect_projection_binding(Some(target), &envelope, epoch, target, pair,),
        Ok(())
    );
    assert_eq!(
        validate_inspect_projection_binding(Some(other_target), &envelope, epoch, target, pair,),
        Err("inspect.epoch_mismatch".to_owned()),
    );
    let mut wrong_epoch_bytes = [0; 16];
    wrong_epoch_bytes[15] = 0x34;
    let wrong_epoch = InspectEpochId::from_bytes(wrong_epoch_bytes);
    assert_eq!(
        validate_inspect_projection_binding(Some(target), &envelope, wrong_epoch, target, pair,),
        Err("inspect.epoch_mismatch".to_owned()),
    );
    let wrong_pair = RevisionPair::new(SourceRevisionId::from_bytes([0x66; 16]), pair.catalogue());
    assert_eq!(
        validate_inspect_projection_binding(Some(target), &envelope, epoch, target, wrong_pair,),
        Err("inspect.epoch_mismatch".to_owned()),
    );
}

#[test]
fn inspector_calls_schema_requires_values_classifier() {
    let invocation = InvocationId::from_bytes([0x31; 16]);
    let schema = InvokeValue::new(RuntimeValue::Boolean(true)).expect("schema value");
    let call = CallRow::new(invocation, Some(schema), 1, 42).expect("call row");

    let redacted = encode_calls(std::slice::from_ref(&call), false).expect("redacted calls");
    let visible = encode_calls(std::slice::from_ref(&call), true).expect("visible calls");

    assert_eq!(redacted[0][25], 0);
    assert_eq!(visible[0][25], 1);
}

#[test]
fn inspect_render_signature_covers_all_projection_carriers() {
    assert_eq!(INSPECT_RENDER_CONTRACT, "std.inspect.render@1");
    assert_eq!(INSPECT_RENDER_CARRIER_SIGNATURE.len(), 9);
    for (tag, expected) in [
        (1, InspectCarrierKind::Snapshot),
        (2, InspectCarrierKind::InvocationNodes),
        (3, InspectCarrierKind::Calls),
        (4, InspectCarrierKind::Resources),
        (5, InspectCarrierKind::StateCells),
        (6, InspectCarrierKind::UiNodes),
        (7, InspectCarrierKind::PresentationCandidates),
        (8, InspectCarrierKind::RuntimeBindings),
        (9, InspectCarrierKind::SecurityDecisions),
    ] {
        assert_eq!(InspectCarrierKind::from_tag(tag), Some(expected));
    }
    assert_eq!(
        inspect_classification_tag(
            InspectCarrierKind::RuntimeBindings,
            InspectPrivilege::OwnInvocation
        ),
        0,
    );
    assert_eq!(
        inspect_classification_tag(
            InspectCarrierKind::SecurityDecisions,
            InspectPrivilege::OwnInvocation
        ),
        0,
    );
}

#[test]
fn inspect_rows_use_canonical_orv5_and_preserve_identity_payload() {
    let (active, registry) = inspect_test_context();
    let identity = vec![1, 0, 0, 0, 0, 0, 0, 0, 7, 0xaa, 0xbb];
    let encoded =
        encode_inspect_row(&active, &registry, identity.clone()).expect("Inspector row encodes");
    orna_core::inspect_carrier::validate_inspect_rows(std::slice::from_ref(&encoded))
        .expect("Inspector row is canonical ORV5");
    let decoded =
        decode_constructed_value(&active, &registry, &encoded).expect("Inspector row decodes");
    let RuntimeValue::Constructed(constructed) = decoded else {
        panic!("Inspector row must use the constructed representation");
    };
    let ConstructedValueKind::List(values) = constructed.kind() else {
        panic!("Inspector row must use a deterministic list representation");
    };
    let [RuntimeValue::Bytes(payload)] = values else {
        panic!("Inspector row must carry exactly one identity payload");
    };
    assert_eq!(payload, &identity);
}

#[test]
fn unarmed_security_and_runtime_carriers_redact_classified_bytes() {
    let (active, registry) = inspect_test_context();
    let target = InvocationId::from_bytes([0x32; 16]);
    let principal = PrincipalId::from_bytes([0xa1; 16]);
    let audit_reference = SecurityAuditEventId::from_bytes([0xa3; 16]);
    let denial_reason = "denial-reason-secret";
    let security = SecurityDecisionRow::new(
        InspectSecurityDecisionKind::Inspect,
        InspectSecurityDecisionOutcome::Denied,
        vec![principal],
        Some(FunctionId::from_bytes([0xa4; 16])),
        Some(denial_reason.to_owned()),
        vec![audit_reference],
    )
    .expect("security fixture must validate");
    let runtime = RuntimeBindingRow::new(
        "runtime-secret".to_owned(),
        "platform-secret".to_owned(),
        Vec::new(),
        vec![(
            "runtime-contract-secret".to_owned(),
            "9".to_owned(),
            vec!["platform-detail-secret".to_owned()],
        )],
        true,
        1,
    )
    .expect("runtime fixture must validate");
    let ui = UiNodeRow::new(
        FunctionId::from_bytes([0xa5; 16]),
        "call-site-secret".to_owned(),
        "ui-runtime-contract-secret".to_owned(),
    )
    .expect("UI fixture must validate");
    let selected_sink = TypeDescriptor::map(
        TypeDescriptor::list(TypeDescriptor::named(TypeId::from_bytes([0xa6; 16])))
            .expect("selected sink list must validate"),
        TypeDescriptor::option(TypeDescriptor::reference(TypeId::from_bytes([0xa7; 16])))
            .expect("selected sink option must validate"),
    )
    .expect("selected sink map must validate");
    let presentation = PresentationCandidateRow::new(
        "presenter-secret".to_owned(),
        true,
        "platform-reason-secret".to_owned(),
        Some(selected_sink),
        Some("presentation-runtime-secret".to_owned()),
    )
    .expect("presentation fixture must validate");
    let epoch = InspectSnapshotEpoch::new(
        InspectEpochId::from_bytes([0x31; 16]),
        target,
        active.pair().source(),
        active.pair().catalogue(),
        PrincipalId::from_bytes([0x33; 16]),
        std::time::SystemTime::UNIX_EPOCH,
        FunctionId::from_bytes([0x34; 16]),
        InspectOutcomeKind::Denied,
        InspectSnapshotSummary::new(1, InspectResultSummary::NoValues, None)
            .expect("summary must validate"),
        &InspectSnapshotOptions::structural(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![security.clone()],
    )
    .expect("epoch must validate");

    let unarmed = [InspectPrivilege::OwnInvocation];
    let security_payload = make_inspect_carrier(
        &active,
        &registry,
        InspectCarrierKind::SecurityDecisions,
        &epoch,
        target,
        encode_security_decisions(std::slice::from_ref(&security), false)
            .expect("unarmed security rows encode"),
        0,
    )
    .expect("unarmed security carrier encodes");
    let runtime_payload = make_inspect_carrier(
        &active,
        &registry,
        InspectCarrierKind::RuntimeBindings,
        &epoch,
        target,
        encode_runtime_bindings(std::slice::from_ref(&runtime), false)
            .expect("unarmed runtime rows encode"),
        0,
    )
    .expect("unarmed runtime carrier encodes");
    let ui_payload = make_inspect_carrier(
        &active,
        &registry,
        InspectCarrierKind::UiNodes,
        &epoch,
        target,
        encode_ui_nodes(std::slice::from_ref(&ui), false, false).expect("unarmed UI rows encode"),
        0,
    )
    .expect("unarmed UI carrier encodes");
    let unarmed_presentation_rows =
        encode_presentation_candidates(std::slice::from_ref(&presentation), false)
            .expect("unarmed presentation rows encode");
    let presentation_payload = make_inspect_carrier(
        &active,
        &registry,
        InspectCarrierKind::PresentationCandidates,
        &epoch,
        target,
        unarmed_presentation_rows.clone(),
        0,
    )
    .expect("unarmed presentation carrier encodes");
    for (payload, kind) in [
        (&security_payload, InspectCarrierKind::SecurityDecisions),
        (&runtime_payload, InspectCarrierKind::RuntimeBindings),
        (&ui_payload, InspectCarrierKind::UiNodes),
        (
            &presentation_payload,
            InspectCarrierKind::PresentationCandidates,
        ),
    ] {
        let carrier = InspectCarrierEnvelope::decode(payload).expect("carrier decodes");
        assert_eq!(carrier.carrier_kind(), kind);
        orna_core::inspect_carrier::validate_inspect_rows(carrier.rows())
            .expect("carrier rows remain valid");
        assert_eq!(carrier.rows().len(), 1);
    }
    let contains =
        |payload: &[u8], bytes: &[u8]| payload.windows(bytes.len()).any(|window| window == bytes);
    let contains_row = |rows: &[Vec<u8>], bytes: &[u8]| rows.iter().any(|row| contains(row, bytes));
    assert!(!contains(&security_payload, &principal.to_bytes()));
    assert!(!contains(&security_payload, denial_reason.as_bytes()));
    assert!(!contains(&security_payload, &audit_reference.to_bytes()));
    assert!(!contains(&security_payload, &[0x33; 16]));
    let mut expected_unarmed_presentation_row = row(7, 0);
    expected_unarmed_presentation_row.extend_from_slice(&u32::MAX.to_be_bytes());
    expected_unarmed_presentation_row.push(1);
    expected_unarmed_presentation_row.extend_from_slice(&u32::MAX.to_be_bytes());
    expected_unarmed_presentation_row.push(INSPECT_REDACTED_FIELD_TAG);
    expected_unarmed_presentation_row.push(INSPECT_REDACTED_FIELD_TAG);
    assert_eq!(
        unarmed_presentation_rows,
        vec![expected_unarmed_presentation_row],
        "denied selected sinks encode only redaction markers",
    );
    let mut selected_descriptor_bytes = vec![4, 2, 0];
    selected_descriptor_bytes.extend_from_slice(&[0xa6; 16]);
    selected_descriptor_bytes.extend_from_slice(&[5, 1]);
    selected_descriptor_bytes.extend_from_slice(&[0xa7; 16]);
    assert!(!contains_row(
        &unarmed_presentation_rows,
        &selected_descriptor_bytes,
    ));
    for secret in [
        b"runtime-secret".as_slice(),
        b"platform-secret".as_slice(),
        b"runtime-contract-secret".as_slice(),
        b"platform-detail-secret".as_slice(),
    ] {
        assert!(!contains(&runtime_payload, secret));
    }
    for (payload, secret) in [
        (&ui_payload, b"call-site-secret".as_slice()),
        (&ui_payload, b"ui-runtime-contract-secret".as_slice()),
        (&presentation_payload, b"presenter-secret".as_slice()),
        (&presentation_payload, b"platform-reason-secret".as_slice()),
        (
            &presentation_payload,
            b"presentation-runtime-secret".as_slice(),
        ),
    ] {
        assert!(!contains(payload, secret));
    }

    assert!(contains_row(
        &encode_security_decisions(std::slice::from_ref(&security), true)
            .expect("armed security rows encode"),
        denial_reason.as_bytes(),
    ));
    assert!(contains_row(
        &encode_runtime_bindings(std::slice::from_ref(&runtime), true)
            .expect("armed runtime rows encode"),
        b"runtime-secret",
    ));
    assert!(contains_row(
        &encode_ui_nodes(std::slice::from_ref(&ui), true, true).expect("armed UI rows encode"),
        b"ui-runtime-contract-secret",
    ));
    let armed_presentation_rows =
        encode_presentation_candidates(std::slice::from_ref(&presentation), true)
            .expect("armed presentation rows encode");
    assert!(contains_row(
        &armed_presentation_rows,
        b"presentation-runtime-secret",
    ));
    let armed_presentation_payload = make_inspect_carrier(
        &active,
        &registry,
        InspectCarrierKind::PresentationCandidates,
        &epoch,
        target,
        armed_presentation_rows.clone(),
        0,
    )
    .expect("armed presentation carrier encodes");
    let armed_carrier =
        InspectCarrierEnvelope::decode(&armed_presentation_payload).expect("carrier decodes");
    assert_eq!(
        armed_carrier.carrier_kind(),
        InspectCarrierKind::PresentationCandidates
    );
    orna_core::inspect_carrier::validate_inspect_rows(armed_carrier.rows())
        .expect("armed carrier rows remain valid");
    assert!(contains(
        &armed_presentation_payload,
        &selected_descriptor_bytes
    ));
    let armed_presentation_row = &armed_presentation_rows[0];
    let descriptor_offset = armed_presentation_row
        .windows(selected_descriptor_bytes.len())
        .position(|window| window == selected_descriptor_bytes.as_slice())
        .expect("granted carrier preserves selected sink descriptor");
    assert_eq!(armed_presentation_row[descriptor_offset - 1], 1);
    assert!(!inspect_classifier_granted(
        &unarmed,
        InspectPrivilege::SecurityDetails
    ));
    assert!(!inspect_classifier_granted(
        &unarmed,
        InspectPrivilege::RuntimeInternals
    ));
}
#[test]
fn inspect_denials_do_not_disclose_epoch_existence() {
    let missing_epoch = inspect_kernel_error_code(PostgresKernelError::InspectDenied {
        reason: orna_core::security::InspectDenial::MissingEpoch,
    });
    let missing_privilege = inspect_kernel_error_code(PostgresKernelError::InspectDenied {
        reason: orna_core::security::InspectDenial::MissingPrivilege,
    });

    assert_eq!(missing_epoch, INSPECT_DENIED_CODE);
    assert_eq!(missing_privilege, INSPECT_DENIED_CODE);
}
