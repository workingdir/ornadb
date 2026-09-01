use super::*;
#[test]
fn resource_request_rejects_zero_generation_at_connection_open() {
    let mut request = resource_request_fixture();
    request.generation = 0;
    let mut connection = ResourceProtocolConnection::new();
    let before = connection.clone();

    assert_eq!(
        connection.open(request),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceMalformedPayload,
        }),
    );
    assert_eq!(connection, before);
}

#[test]
fn resource_request_rejects_zero_generation_at_encode() {
    let active = empty_active_revision();
    let registry = test_registry();
    let mut request = resource_request_fixture();
    request.generation = 0;

    assert_eq!(
        encode_resource_request(&active, &registry, &request),
        Err(FrameCodecError::ResourceMalformedPayload),
    );
}

#[test]
fn resource_request_rejects_zero_generation_at_decode() {
    let active = empty_active_revision();
    let registry = test_registry();
    let mut encoded = encode_resource_request(&active, &registry, &resource_request_fixture())
        .expect("non-zero fixture request encodes");
    let generation_start = RESOURCE_HEADER_LENGTH + 8 + 16 + 16 + 16 + 4 + 4 + 16 + 16 + 16;
    encoded[generation_start..generation_start + 8].copy_from_slice(&0_u64.to_be_bytes());

    assert_eq!(
        decode_resource_request(&active, &registry, &encoded),
        Err(FrameCodecError::ResourceMalformedPayload),
    );
}

#[test]
fn resource_request_generation_one_round_trips() {
    let active = empty_active_revision();
    let registry = test_registry();
    let mut request = resource_request_fixture();
    request.generation = 1;
    let encoded = encode_resource_request(&active, &registry, &request)
        .expect("generation one request encodes");

    let decoded = decode_resource_request(&active, &registry, &encoded)
        .expect("generation one request decodes");
    assert_eq!(decoded, request);
    assert_eq!(
        encode_resource_request(&active, &registry, &decoded),
        Ok(encoded)
    );
}

#[test]
fn resource_request_rejects_zero_request_identity() {
    let active = empty_active_revision();
    let registry = test_registry();
    let mut request = resource_request_fixture();
    request.request_id = InvocationId::from_bytes([0; 16]);

    assert_eq!(
        encode_resource_request(&active, &registry, &request),
        Err(FrameCodecError::ResourceMalformedPayload),
    );

    let mut encoded = encode_resource_request(&active, &registry, &resource_request_fixture())
        .expect("non-zero fixture request encodes");
    let request_id_start = RESOURCE_HEADER_LENGTH + 8;
    encoded[request_id_start..request_id_start + 16].fill(0);
    assert_eq!(
        decode_resource_request(&active, &registry, &encoded),
        Err(FrameCodecError::ResourceMalformedPayload),
    );

    let mut connection = ResourceProtocolConnection::new();
    let before = connection.clone();
    assert_eq!(
        connection.open(request),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceMalformedPayload,
        }),
    );
    assert_eq!(connection, before);
}

#[test]
fn resource_request_rejects_zero_parent_invocation_id_at_encode_decode_and_open() {
    let active = empty_active_revision();
    let registry = test_registry();
    let mut request = resource_request_fixture();
    request.parent_invocation_id = InvocationId::from_bytes([0; 16]);

    assert_eq!(
        encode_resource_request(&active, &registry, &request),
        Err(FrameCodecError::ResourceMalformedPayload),
    );

    let mut encoded = encode_resource_request(&active, &registry, &resource_request_fixture())
        .expect("non-zero fixture request encodes");
    let parent_invocation_id_start = RESOURCE_HEADER_LENGTH + 8 + 16;
    encoded[parent_invocation_id_start..parent_invocation_id_start + 16].fill(0);
    assert_eq!(
        decode_resource_request(&active, &registry, &encoded),
        Err(FrameCodecError::ResourceMalformedPayload),
    );

    let mut connection = ResourceProtocolConnection::new();
    let before = connection.clone();
    assert_eq!(
        connection.open(request),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceMalformedPayload,
        }),
    );
    assert_eq!(connection, before);
}

#[test]
fn resource_request_rejects_zero_call_site_id_at_encode_decode_and_open() {
    let active = empty_active_revision();
    let registry = test_registry();
    let mut request = resource_request_fixture();
    request.call_site_id = CallSiteId::from_bytes([0; 16]);

    assert_eq!(
        encode_resource_request(&active, &registry, &request),
        Err(FrameCodecError::ResourceMalformedPayload),
    );

    let mut encoded = encode_resource_request(&active, &registry, &resource_request_fixture())
        .expect("non-zero fixture request encodes");
    let call_site_id_start = RESOURCE_HEADER_LENGTH + 8 + 16 + 16;
    encoded[call_site_id_start..call_site_id_start + 16].fill(0);
    assert_eq!(
        decode_resource_request(&active, &registry, &encoded),
        Err(FrameCodecError::ResourceMalformedPayload),
    );

    let mut connection = ResourceProtocolConnection::new();
    let before = connection.clone();
    assert_eq!(
        connection.open(request),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceMalformedPayload,
        }),
    );
    assert_eq!(connection, before);
}

#[test]
fn resource_request_has_deterministic_wire_order_and_exact_round_trip() {
    let active = empty_active_revision();
    let registry = test_registry();
    let request = resource_request_fixture();
    let encoded = encode_resource_request(&active, &registry, &request).unwrap();
    let mut expected = Vec::new();
    expected.extend_from_slice(b"ORNA-RESOURCE/1");
    expected.extend_from_slice(&[RESOURCE_REQUEST_TAG, 0]);
    expected.extend_from_slice(&141_u32.to_be_bytes());
    expected.extend_from_slice(&1_u64.to_be_bytes());
    expected.extend_from_slice(&[0x02; 16]);
    expected.extend_from_slice(&[0x03; 16]);
    expected.extend_from_slice(&[0x04; 16]);
    expected.extend_from_slice(&0_u32.to_be_bytes());
    expected.extend_from_slice(&0_u32.to_be_bytes());
    expected.extend_from_slice(&[0x05; 16]);
    expected.extend_from_slice(&[0x06; 16]);
    expected.extend_from_slice(&[0x07; 16]);
    expected.extend_from_slice(&9_u64.to_be_bytes());
    expected.extend_from_slice(&[0x01, 0, 0, 0, 0]);
    expected.extend_from_slice(&10_u64.to_be_bytes());
    expected.extend_from_slice(&11_u64.to_be_bytes());
    assert_eq!(encoded, expected);
    let decoded = decode_resource_request(&active, &registry, &encoded).unwrap();
    assert_eq!(decoded, request);
    assert_eq!(
        encode_resource_request(&active, &registry, &decoded),
        Ok(encoded)
    );
}

#[test]
fn resource_request_with_typed_arguments_has_exact_canonical_golden_bytes() {
    let active = empty_active_revision();
    let registry = test_registry();
    let first_value =
        encode_constructed_value(&active, &registry, &RuntimeValue::Integer(1)).unwrap();
    let second_value =
        encode_constructed_value(&active, &registry, &RuntimeValue::Integer(2)).unwrap();
    assert_eq!(
        first_value,
        resource_hex("4f52563503000000000000000000000000000000020000000400000001")
    );
    assert_eq!(
        second_value,
        resource_hex("4f52563503000000000000000000000000000000020000000400000002")
    );
    let request = ResourceRequest {
        arguments: vec![
            ResourceArgument {
                parameter: ParameterId::from_bytes([0x01; 16]),
                value: RuntimeValue::Integer(1),
            },
            ResourceArgument {
                parameter: ParameterId::from_bytes([0x02; 16]),
                value: RuntimeValue::Integer(2),
            },
        ],
        ..resource_request_fixture()
    };
    let encoded = encode_resource_request(&active, &registry, &request).unwrap();
    let expected = resource_hex(concat!(
        "4f524e412d5245534f555243452f310100000000ef",
        "0000000000000001",
        "02020202020202020202020202020202",
        "03030303030303030303030303030303",
        "04040404040404040404040404040404",
        "00000000",
        "00000000",
        "05050505050505050505050505050505",
        "06060606060606060606060606060606",
        "07070707070707070707070707070707",
        "0000000000000009",
        "0100000002",
        "01010101010101010101010101010101",
        "0000001d4f52563503000000000000000000000000000000020000000400000001",
        "02020202020202020202020202020202",
        "0000001d4f52563503000000000000000000000000000000020000000400000002",
        "000000000000000a",
        "000000000000000b",
    ));
    assert_eq!(encoded, expected);
    let decoded = decode_resource_request(&active, &registry, &encoded).unwrap();
    assert_eq!(decoded, request);
    assert_eq!(
        encode_resource_request(&active, &registry, &decoded),
        Ok(expected)
    );
}

#[test]
fn resource_request_preserves_distinct_state_context_values() {
    let active = empty_active_revision();
    let registry = test_registry();
    let mut first = resource_request_fixture();
    first.state_profile = "profile-a".to_owned();
    first.function_instance_key = "instance-a".to_owned();
    let mut second = first.clone();
    second.state_profile = "profile-b".to_owned();
    second.function_instance_key = "instance-b".to_owned();
    let first_decoded = decode_resource_request(
        &active,
        &registry,
        &encode_resource_request(&active, &registry, &first).unwrap(),
    )
    .unwrap();
    let second_decoded = decode_resource_request(
        &active,
        &registry,
        &encode_resource_request(&active, &registry, &second).unwrap(),
    )
    .unwrap();
    assert_eq!(first_decoded.state_profile, "profile-a");
    assert_eq!(first_decoded.function_instance_key, "instance-a");
    assert_eq!(second_decoded.state_profile, "profile-b");
    assert_eq!(second_decoded.function_instance_key, "instance-b");
    assert_ne!(first_decoded, second_decoded);
}

#[test]
fn resource_request_rejects_nul_state_context_text() {
    let active = empty_active_revision();
    let registry = test_registry();

    let mut profile_request = resource_request_fixture();
    profile_request.state_profile = "bad\0profile".to_owned();
    assert_eq!(
        encode_resource_request(&active, &registry, &profile_request),
        Err(FrameCodecError::ResourceInvalidText),
    );

    let mut instance_request = resource_request_fixture();
    instance_request.function_instance_key = "bad\0instance".to_owned();
    assert_eq!(
        encode_resource_request(&active, &registry, &instance_request),
        Err(FrameCodecError::ResourceInvalidText),
    );

    let mut profile_request = resource_request_fixture();
    profile_request.state_profile = "profile".to_owned();
    let mut profile_encoded =
        encode_resource_request(&active, &registry, &profile_request).unwrap();
    let profile_byte = RESOURCE_HEADER_LENGTH + 8 + 16 + 16 + 16 + 4;
    profile_encoded[profile_byte] = 0;
    assert_eq!(
        decode_resource_request(&active, &registry, &profile_encoded),
        Err(FrameCodecError::ResourceInvalidText),
    );

    let mut instance_request = resource_request_fixture();
    instance_request.state_profile = "profile".to_owned();
    instance_request.function_instance_key = "instance".to_owned();
    let mut instance_encoded =
        encode_resource_request(&active, &registry, &instance_request).unwrap();
    let instance_byte = RESOURCE_HEADER_LENGTH + 8 + 16 + 16 + 16 + 4 + 7 + 4;
    instance_encoded[instance_byte] = 0;
    assert_eq!(
        decode_resource_request(&active, &registry, &instance_encoded),
        Err(FrameCodecError::ResourceInvalidText),
    );
}

#[test]
fn resource_connection_rejects_nul_state_context_text_before_reserving() {
    for (profile, instance) in [("bad\0profile", ""), ("", "bad\0instance")] {
        let mut request = resource_request_fixture();
        request.state_profile = profile.to_owned();
        request.function_instance_key = instance.to_owned();
        let mut connection = ResourceProtocolConnection::new();
        let before = connection.clone();

        assert_eq!(
            connection.open(request),
            Err(ResourceConnectionError::InvalidFrame {
                source: FrameCodecError::ResourceInvalidText,
            }),
        );
        assert_eq!(connection, before);
    }
}

#[test]
fn resource_request_and_controls_reject_unframed_payloads() {
    let active = empty_active_revision();
    let registry = test_registry();
    let request = encode_resource_request(&active, &registry, &resource_request_fixture()).unwrap();
    assert!(matches!(
        decode_resource_request(&active, &registry, &request[RESOURCE_HEADER_LENGTH..]),
        Err(FrameCodecError::ResourceInvalidMarker)
    ));

    let window = encode_resource_window_update(&ResourceWindowUpdate {
        stream_id: 1,
        request_id: InvocationId::from_bytes([0x21; 16]),
        add_items: 1,
        add_bytes: 2,
    })
    .unwrap();
    assert!(matches!(
        decode_resource_window_update(&window[RESOURCE_HEADER_LENGTH..]),
        Err(FrameCodecError::ResourceInvalidMarker)
    ));

    let cancel = encode_resource_cancel(&ResourceCancel {
        stream_id: 1,
        request_id: InvocationId::from_bytes([0x22; 16]),
        reason: ResourceCancellationCode::ClientRequested,
    })
    .unwrap();
    assert!(matches!(
        decode_resource_cancel(&cancel[RESOURCE_HEADER_LENGTH..]),
        Err(FrameCodecError::ResourceInvalidMarker)
    ));
}
#[test]
fn resource_envelope_rejects_wrong_major_flags_and_length_errors() {
    let active = empty_active_revision();
    let registry = test_registry();
    let encoded = encode_resource_request(&active, &registry, &resource_request_fixture()).unwrap();

    let mut wrong_major = encoded.clone();
    wrong_major[RESOURCE_MARKER.len() - 1] = b'2';
    assert_eq!(
        decode_resource_request(&active, &registry, &wrong_major),
        Err(FrameCodecError::ResourceInvalidMarker)
    );

    let mut non_zero_flags = encoded.clone();
    non_zero_flags[RESOURCE_MARKER.len() + 1] = 1;
    assert_eq!(
        decode_resource_request(&active, &registry, &non_zero_flags),
        Err(FrameCodecError::NonZeroFlags { flags: 1 })
    );
    assert_eq!(
        decode_resource_request(&active, &registry, &encoded[..RESOURCE_HEADER_LENGTH - 1]),
        Err(FrameCodecError::TruncatedHeader {
            actual: RESOURCE_HEADER_LENGTH - 1,
        })
    );

    let truncated = &encoded[..encoded.len() - 1];
    assert_eq!(
        decode_resource_request(&active, &registry, truncated),
        Err(FrameCodecError::TruncatedPayload {
            declared: 141,
            actual: 140,
        })
    );

    let mut trailing = encoded.clone();
    trailing.push(0);
    assert_eq!(
        decode_resource_request(&active, &registry, &trailing),
        Err(FrameCodecError::TrailingBytes {
            declared: 141,
            actual: 142,
        })
    );

    let mut oversized = encoded;
    oversized[RESOURCE_MARKER.len() + 2..RESOURCE_HEADER_LENGTH].copy_from_slice(
        &u32::try_from(MAX_FRAME_PAYLOAD_LENGTH + 1)
            .unwrap()
            .to_be_bytes(),
    );
    assert_eq!(
        decode_resource_request(&active, &registry, &oversized),
        Err(FrameCodecError::PayloadTooLarge {
            actual: MAX_FRAME_PAYLOAD_LENGTH + 1,
            maximum: MAX_FRAME_PAYLOAD_LENGTH,
        })
    );
}

#[test]
fn resource_frames_have_exact_golden_bytes() {
    let active = empty_active_revision();
    let registry = test_registry();
    let request_id = InvocationId::from_bytes([0x31; 16]);
    let revision = RevisionPair::new(
        SourceRevisionId::from_bytes([0x32; 16]),
        CatalogueRevisionId::from_bytes([0x33; 16]),
    );

    let accepted = ResourceAccepted {
        stream_id: 4,
        request_id,
        nested_invocation_id: InvocationId::from_bytes([0x34; 16]),
        target_revision: revision,
        resource_kind: ResourceKind::Stream,
    };
    let accepted_wire = resource_hex(concat!(
        "4f524e412d5245534f555243452f31810000000049",
        "0000000000000004",
        "31313131313131313131313131313131",
        "34343434343434343434343434343434",
        "32323232323232323232323232323232",
        "33333333333333333333333333333333",
        "02",
    ));
    assert_eq!(encode_resource_accepted(&accepted).unwrap(), accepted_wire);
    assert_eq!(decode_resource_accepted(&accepted_wire), Ok(accepted));

    let value = RuntimeValue::Integer(7);
    let value_bytes = encode_constructed_value(&active, &registry, &value).unwrap();
    assert_eq!(
        value_bytes,
        resource_hex("4f52563503000000000000000000000000000000020000000400000007")
    );
    let values = ResourceValues {
        stream_id: 4,
        request_id,
        target_revision: revision,
        batch_sequence: 0,
        item_count: 1,
        byte_count: value_bytes.len() as u32,
        values: vec![value],
    };
    assert_eq!(
        encode_resource_values(&active, &registry, &values).unwrap(),
        resource_hex(concat!(
            "4f524e412d5245534f555243452f31820000000069",
            "0000000000000004",
            "31313131313131313131313131313131",
            "32323232323232323232323232323232",
            "33333333333333333333333333333333",
            "0000000000000000",
            "00000001",
            "0000001d",
            "0000001d",
            "4f52563503000000000000000000000000000000020000000400000007",
        ))
    );

    let completed = ResourceCompleted {
        stream_id: 4,
        request_id,
        target_revision: revision,
        final_batch_sequence: 0,
        total_items: 1,
    };
    let completed_wire = resource_hex(concat!(
        "4f524e412d5245534f555243452f31830000000048",
        "0000000000000004",
        "31313131313131313131313131313131",
        "32323232323232323232323232323232",
        "33333333333333333333333333333333",
        "0000000000000000",
        "0000000000000001",
    ));
    assert_eq!(
        encode_resource_completed(&completed).unwrap(),
        completed_wire
    );
    assert_eq!(decode_resource_completed(&completed_wire), Ok(completed));

    let failed = ResourceFailed {
        stream_id: 4,
        request_id,
        target_revision: revision,
        failure: CallFailure::TargetUnavailable,
    };
    let failed_wire = resource_hex(concat!(
        "4f524e412d5245534f555243452f3184000000003c",
        "0000000000000004",
        "31313131313131313131313131313131",
        "32323232323232323232323232323232",
        "33333333333333333333333333333333",
        "02000100",
    ));
    assert_eq!(encode_resource_failed(&failed).unwrap(), failed_wire);
    assert_eq!(decode_resource_failed(&failed_wire), Ok(failed));

    let cancelled = ResourceCancelled {
        stream_id: 4,
        request_id,
        target_revision: revision,
        reason: ResourceCancellationCode::ClientRequested,
    };
    let cancelled_wire = resource_hex(concat!(
        "4f524e412d5245534f555243452f31850000000039",
        "0000000000000004",
        "31313131313131313131313131313131",
        "32323232323232323232323232323232",
        "33333333333333333333333333333333",
        "01",
    ));
    assert_eq!(
        encode_resource_cancelled(&cancelled).unwrap(),
        cancelled_wire
    );
    assert_eq!(decode_resource_cancelled(&cancelled_wire), Ok(cancelled));

    let window = ResourceWindowUpdate {
        stream_id: 4,
        request_id,
        add_items: 1,
        add_bytes: 2,
    };
    let window_wire = resource_hex(concat!(
        "4f524e412d5245534f555243452f31020000000028",
        "0000000000000004",
        "31313131313131313131313131313131",
        "0000000000000001",
        "0000000000000002",
    ));
    assert_eq!(encode_resource_window_update(&window).unwrap(), window_wire);
    assert_eq!(decode_resource_window_update(&window_wire), Ok(window));

    let cancel = ResourceCancel {
        stream_id: 4,
        request_id,
        reason: ResourceCancellationCode::ParentInvocationCancelled,
    };
    let cancel_wire = resource_hex(concat!(
        "4f524e412d5245534f555243452f31030000000019",
        "0000000000000004",
        "31313131313131313131313131313131",
        "03",
    ));
    assert_eq!(encode_resource_cancel(&cancel).unwrap(), cancel_wire);
    assert_eq!(decode_resource_cancel(&cancel_wire), Ok(cancel));
}

#[test]
fn resource_result_frames_reject_pre_echo_wire_layout() {
    let revision = resource_revision_fixture();
    let frame = ResourceFailed {
        stream_id: 4,
        request_id: InvocationId::from_bytes([0x31; 16]),
        target_revision: revision,
        failure: CallFailure::TargetUnavailable,
    };
    let mut legacy = encode_resource_failed(&frame).unwrap();
    let revision_start = RESOURCE_HEADER_LENGTH + 8 + 16;
    legacy.drain(revision_start..revision_start + 32);
    let payload_length = (legacy.len() - RESOURCE_HEADER_LENGTH) as u32;
    legacy[RESOURCE_MARKER.len() + 2..RESOURCE_HEADER_LENGTH]
        .copy_from_slice(&payload_length.to_be_bytes());
    assert!(decode_resource_failed(&legacy).is_err());
}

#[test]
fn resource_values_round_trip_preserves_canonical_value_bytes() {
    let active = empty_active_revision();
    let registry = test_registry();
    let value = RuntimeValue::Integer(7);
    let value_bytes = encode_constructed_value(&active, &registry, &value).unwrap();
    let frame = ResourceValues {
        stream_id: 2,
        request_id: InvocationId::from_bytes([0x12; 16]),
        target_revision: resource_revision_fixture(),
        batch_sequence: 0,
        item_count: 1,
        byte_count: value_bytes.len() as u32,
        values: vec![value],
    };
    let encoded = encode_resource_values(&active, &registry, &frame).unwrap();
    let decoded = decode_resource_values(&active, &registry, &encoded).unwrap();
    assert_eq!(decoded, frame);
    assert_eq!(
        encode_resource_values(&active, &registry, &decoded),
        Ok(encoded)
    );
}

#[test]
fn resource_values_record_round_trip_preserves_canonical_orv3_identity() {
    let (active, record_type, other_record_type) = record_active_revision();
    let registry = test_registry();
    let record = RuntimeValue::Record(
        RecordValue::new(
            &active,
            record_type,
            [(String::from("title"), RuntimeValue::Boolean(true))],
        )
        .unwrap(),
    );

    let mut field_value = b"ORV3".to_vec();
    field_value.push(0x02);
    field_value.extend_from_slice(&orna_standard::BOOLEAN_TYPE_ID.to_bytes());
    field_value.extend_from_slice(&1_u32.to_be_bytes());
    field_value.push(1);
    let mut record_payload = 1_u32.to_be_bytes().to_vec();
    record_payload.extend_from_slice(&[0x92; 16]);
    record_payload.extend_from_slice(&(field_value.len() as u32).to_be_bytes());
    record_payload.extend_from_slice(&field_value);
    let mut canonical = b"ORV3".to_vec();
    canonical.push(0x0b);
    canonical.extend_from_slice(&record_type.to_bytes());
    canonical.extend_from_slice(&(record_payload.len() as u32).to_be_bytes());
    canonical.extend_from_slice(&record_payload);
    assert_eq!(encode_active_value(&active, &record), Ok(canonical.clone()));
    let canonical_record = decode_active_value(&active, &canonical).unwrap();
    assert_eq!(canonical_record, record);
    assert_ne!(record_type, other_record_type);
    let other_record = RuntimeValue::Record(
        RecordValue::new(
            &active,
            other_record_type,
            [(String::from("title"), RuntimeValue::Boolean(true))],
        )
        .unwrap(),
    );
    assert_ne!(
        encode_active_value(&active, &other_record),
        Ok(canonical.clone())
    );

    let encoded_value = encode_constructed_value(&active, &registry, &canonical_record).unwrap();
    let frame = ResourceValues {
        stream_id: 2,
        request_id: InvocationId::from_bytes([0x12; 16]),
        target_revision: resource_revision_fixture(),
        batch_sequence: 0,
        item_count: 1,
        byte_count: encoded_value.len() as u32,
        values: vec![canonical_record],
    };
    let encoded = encode_resource_values(&active, &registry, &frame).unwrap();
    let decoded = decode_resource_values(&active, &registry, &encoded).unwrap();
    assert_eq!(decoded, frame);
    assert_eq!(
        encode_resource_values(&active, &registry, &decoded),
        Ok(encoded)
    );
}

#[test]
fn resource_values_reject_metadata_limits_before_materialising_values() {
    let active = empty_active_revision();
    let registry = test_registry();
    let value = RuntimeValue::Integer(7);
    let value_bytes = encode_constructed_value(&active, &registry, &value).unwrap();
    let frame = ResourceValues {
        stream_id: 1,
        request_id: InvocationId::from_bytes([0x12; 16]),
        target_revision: resource_revision_fixture(),
        batch_sequence: 0,
        item_count: 1,
        byte_count: value_bytes.len() as u32,
        values: vec![value],
    };
    let encoded = encode_resource_values(&active, &registry, &frame).unwrap();

    let item_count_offset = RESOURCE_HEADER_LENGTH + 8 + 16 + 32 + 8;
    let mut too_many_items = encoded.clone();
    too_many_items[item_count_offset..item_count_offset + 4].copy_from_slice(
        &u32::try_from(MAX_RESOURCE_BATCH_ITEMS + 1)
            .unwrap()
            .to_be_bytes(),
    );
    assert_eq!(
        decode_resource_values(&active, &registry, &too_many_items),
        Err(FrameCodecError::TooManyResourceEntries {
            actual: MAX_RESOURCE_BATCH_ITEMS + 1,
            maximum: MAX_RESOURCE_BATCH_ITEMS,
        })
    );

    let byte_count_offset = item_count_offset + 4;
    let mut oversized_bytes = encoded;
    oversized_bytes[byte_count_offset..byte_count_offset + 4].copy_from_slice(
        &u32::try_from(MAX_FRAME_PAYLOAD_LENGTH + 1)
            .unwrap()
            .to_be_bytes(),
    );
    assert_eq!(
        decode_resource_values(&active, &registry, &oversized_bytes),
        Err(FrameCodecError::PayloadTooLarge {
            actual: MAX_FRAME_PAYLOAD_LENGTH + 1,
            maximum: MAX_FRAME_PAYLOAD_LENGTH,
        })
    );
}

#[test]
fn resource_values_reject_truncated_value_length_without_mutating_connection() {
    let active = empty_active_revision();
    let registry = test_registry();
    let value = RuntimeValue::Integer(7);
    let value_bytes = encode_constructed_value(&active, &registry, &value).unwrap();
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 2;
    request.byte_window = (value_bytes.len() * 2) as u64;
    let mut unrelated = request.clone();
    unrelated.stream_id = 2;
    unrelated.request_id = InvocationId::from_bytes([0x13; 16]);

    let mut connection = ResourceProtocolConnection::new();
    connection.open(request.clone()).unwrap();
    connection.open(unrelated.clone()).unwrap();
    for (stream_id, request_id) in [
        (request.stream_id, request.request_id),
        (unrelated.stream_id, unrelated.request_id),
    ] {
        connection
            .apply(ResourceServerFrame::Accepted(ResourceAccepted {
                stream_id,
                request_id,
                nested_invocation_id: InvocationId::from_bytes([0x55 + stream_id as u8; 16]),
                target_revision: request.target_revision,
                resource_kind: ResourceKind::Stream,
            }))
            .unwrap();
    }

    let values = ResourceValues {
        stream_id: request.stream_id,
        request_id: request.request_id,
        target_revision: request.target_revision,
        batch_sequence: 0,
        item_count: 1,
        byte_count: value_bytes.len() as u32,
        values: vec![value],
    };
    let mut encoded = encode_resource_values(&active, &registry, &values).unwrap();
    let value_length_offset = RESOURCE_HEADER_LENGTH + 8 + 16 + 32 + 8 + 4 + 4;
    encoded[value_length_offset..value_length_offset + 4]
        .copy_from_slice(&(value_bytes.len() as u32 + 1).to_be_bytes());
    let before = connection.clone();

    assert_eq!(
        decode_resource_server_frame(&active, &registry, &encoded),
        Err(FrameCodecError::ResourceMalformedPayload)
    );
    assert_eq!(connection, before);
    assert_eq!(
        connection.resource_credit(unrelated.stream_id, unrelated.request_id),
        Ok(ResourceCredit {
            item_available: unrelated.item_window,
            byte_available: unrelated.byte_window,
        })
    );
}

#[test]
fn resource_values_reject_declared_payload_bound_before_encoding_values() {
    let active = empty_active_revision();
    let registry = test_registry();
    let frame = ResourceValues {
        stream_id: 1,
        request_id: InvocationId::from_bytes([0x12; 16]),
        target_revision: resource_revision_fixture(),
        batch_sequence: 0,
        item_count: 1,
        byte_count: (MAX_FRAME_PAYLOAD_LENGTH + 1) as u32,
        values: vec![RuntimeValue::Integer(7)],
    };

    assert_eq!(
        encode_resource_values(&active, &registry, &frame),
        Err(FrameCodecError::PayloadTooLarge {
            actual: MAX_FRAME_PAYLOAD_LENGTH + 1,
            maximum: MAX_FRAME_PAYLOAD_LENGTH,
        })
    );
}

#[test]
fn resource_request_rejects_duplicate_and_noncanonical_arguments() {
    let active = empty_active_revision();
    let registry = test_registry();
    let duplicate = ResourceRequest {
        arguments: vec![
            ResourceArgument {
                parameter: ParameterId::from_bytes([1; 16]),
                value: RuntimeValue::Integer(1),
            },
            ResourceArgument {
                parameter: ParameterId::from_bytes([1; 16]),
                value: RuntimeValue::Integer(2),
            },
        ],
        ..resource_request_fixture()
    };
    assert!(matches!(
        encode_resource_request(&active, &registry, &duplicate),
        Err(FrameCodecError::DuplicateResourceArgument { .. })
    ));
    let descending = ResourceRequest {
        arguments: vec![
            ResourceArgument {
                parameter: ParameterId::from_bytes([2; 16]),
                value: RuntimeValue::Integer(1),
            },
            ResourceArgument {
                parameter: ParameterId::from_bytes([1; 16]),
                value: RuntimeValue::Integer(2),
            },
        ],
        ..resource_request_fixture()
    };
    assert!(matches!(
        encode_resource_request(&active, &registry, &descending),
        Err(FrameCodecError::NonCanonicalResourceArgumentOrder { .. })
    ));
    let mut canonical = resource_request_fixture();
    canonical.arguments = vec![
        ResourceArgument {
            parameter: ParameterId::from_bytes([1; 16]),
            value: RuntimeValue::Integer(1),
        },
        ResourceArgument {
            parameter: ParameterId::from_bytes([2; 16]),
            value: RuntimeValue::Integer(2),
        },
    ];
    let valid = encode_resource_request(&active, &registry, &canonical).unwrap();
    let first_value =
        encode_constructed_value(&active, &registry, &RuntimeValue::Integer(1)).unwrap();
    let second_parameter_offset = RESOURCE_HEADER_LENGTH + 125 + 16 + 4 + first_value.len();
    let mut duplicate = valid.clone();
    duplicate[second_parameter_offset..second_parameter_offset + 16].copy_from_slice(&[1; 16]);
    assert!(matches!(
        decode_resource_request(&active, &registry, &duplicate),
        Err(FrameCodecError::DuplicateResourceArgument { .. })
    ));
    let mut descending_wire = valid;
    descending_wire[second_parameter_offset..second_parameter_offset + 16]
        .copy_from_slice(&[0; 16]);
    assert!(matches!(
        decode_resource_request(&active, &registry, &descending_wire),
        Err(FrameCodecError::NonCanonicalResourceArgumentOrder { .. })
    ));
    let mut bytes =
        encode_resource_request(&active, &registry, &resource_request_fixture()).unwrap();
    bytes.extend_from_slice(&[0]);
    assert!(matches!(
        decode_resource_request(&active, &registry, &bytes),
        Err(FrameCodecError::TrailingBytes { .. })
    ));
}

#[test]
fn resource_decoder_rejects_malformed_kind_windows_and_overflow() {
    let active = empty_active_revision();
    let registry = test_registry();
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 0;
    assert!(matches!(
        encode_resource_request(&active, &registry, &request),
        Err(FrameCodecError::ResourceWindowOverflow)
    ));

    request.resource_kind = ResourceKind::Single;
    request.item_window = 0;
    request.byte_window = 1;
    assert!(matches!(
        encode_resource_request(&active, &registry, &request),
        Err(FrameCodecError::ResourceWindowOverflow)
    ));
    let mut connection = ResourceProtocolConnection::new();
    assert!(matches!(
        connection.open(request.clone()),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceWindowOverflow
        })
    ));
    request.item_window = 1;
    request.byte_window = 0;
    assert!(matches!(
        connection.open(request.clone()),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceWindowOverflow
        })
    ));
    assert!(matches!(
        encode_resource_request(&active, &registry, &request),
        Err(FrameCodecError::ResourceWindowOverflow)
    ));
    request.item_window = 1;
    request.byte_window = 1;
    assert!(encode_resource_request(&active, &registry, &request).is_ok());
    request.item_window = MAX_RESOURCE_WINDOW;
    request.byte_window = MAX_RESOURCE_WINDOW;
    assert!(encode_resource_request(&active, &registry, &request).is_ok());
    request.item_window = MAX_RESOURCE_WINDOW + 1;
    assert!(matches!(
        encode_resource_request(&active, &registry, &request),
        Err(FrameCodecError::ResourceWindowExceeded { .. })
    ));
    let mut encoded =
        encode_resource_request(&active, &registry, &resource_request_fixture()).unwrap();
    let kind_offset = RESOURCE_HEADER_LENGTH + 8 + 16 + 16 + 16 + 4 + 4 + 16 + 16 + 16 + 8;
    encoded[kind_offset] = 0xff;
    assert!(matches!(
        decode_resource_request(&active, &registry, &encoded),
        Err(FrameCodecError::InvalidResourceKind { value: 0xff })
    ));
    let update = ResourceWindowUpdate {
        stream_id: 1,
        request_id: InvocationId::from_bytes([1; 16]),
        add_items: MAX_RESOURCE_WINDOW + 1,
        add_bytes: 1,
    };
    assert!(matches!(
        encode_resource_window_update(&update),
        Err(FrameCodecError::ResourceWindowExceeded { .. })
    ));
    let lower_update = ResourceWindowUpdate {
        stream_id: 1,
        request_id: InvocationId::from_bytes([1; 16]),
        add_items: 1,
        add_bytes: 1,
    };
    assert!(encode_resource_window_update(&lower_update).is_ok());
    let upper_update = ResourceWindowUpdate {
        add_items: MAX_RESOURCE_WINDOW,
        add_bytes: MAX_RESOURCE_WINDOW,
        ..lower_update
    };
    assert!(encode_resource_window_update(&upper_update).is_ok());
    let completed = ResourceCompleted {
        stream_id: 1,
        request_id: InvocationId::from_bytes([1; 16]),
        target_revision: resource_revision_fixture(),
        final_batch_sequence: 0,
        total_items: u64::MAX,
    };
    assert!(matches!(
        encode_resource_completed(&completed),
        Err(FrameCodecError::ResourceTotalItemsExceeded { .. })
    ));
}

#[test]
fn resource_frames_reject_sealed_invocation_carriers_as_ordinary_values() {
    let active = empty_active_revision();
    let registry = test_registry();
    let carrier = RuntimeValue::InvokeRequest(minimal_request(None));
    let request = ResourceRequest {
        arguments: vec![ResourceArgument {
            parameter: ParameterId::from_bytes([0x08; 16]),
            value: carrier.clone(),
        }],
        ..resource_request_fixture()
    };
    assert_eq!(
        encode_resource_request(&active, &registry, &request),
        Err(FrameCodecError::InvocationCarrierNotAccepted {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
        })
    );

    let value_bytes = encode_constructed_value(&active, &registry, &carrier).unwrap();
    let values = ResourceValues {
        stream_id: 1,
        request_id: InvocationId::from_bytes([0x12; 16]),
        target_revision: resource_revision_fixture(),
        batch_sequence: 0,
        item_count: 1,
        byte_count: value_bytes.len() as u32,
        values: vec![carrier],
    };
    assert_eq!(
        encode_resource_values(&active, &registry, &values),
        Err(FrameCodecError::InvocationCarrierNotAccepted {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
        })
    );
}

#[test]
fn resource_frame_family_round_trips_through_directional_dispatch() {
    let active = empty_active_revision();
    let registry = test_registry();
    let request_id = InvocationId::from_bytes([0x31; 16]);
    let revision = RevisionPair::new(
        SourceRevisionId::from_bytes([0x32; 16]),
        CatalogueRevisionId::from_bytes([0x33; 16]),
    );
    let value = RuntimeValue::Integer(7);
    let value_bytes = encode_constructed_value(&active, &registry, &value).unwrap();
    let server_frames = [
        ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: 4,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x34; 16]),
            target_revision: revision,
            resource_kind: ResourceKind::Stream,
        }),
        ResourceServerFrame::Values(ResourceValues {
            stream_id: 4,
            request_id,
            target_revision: resource_revision_fixture(),
            batch_sequence: 0,
            item_count: 1,
            byte_count: value_bytes.len() as u32,
            values: vec![value.clone()],
        }),
        ResourceServerFrame::Completed(ResourceCompleted {
            stream_id: 4,
            request_id,
            target_revision: resource_revision_fixture(),
            final_batch_sequence: 0,
            total_items: 1,
        }),
        ResourceServerFrame::Failed(ResourceFailed {
            stream_id: 4,
            request_id,
            target_revision: resource_revision_fixture(),
            failure: CallFailure::TargetUnavailable,
        }),
        ResourceServerFrame::Cancelled(ResourceCancelled {
            stream_id: 4,
            request_id,
            target_revision: resource_revision_fixture(),
            reason: ResourceCancellationCode::ClientRequested,
        }),
    ];
    for frame in server_frames {
        let encoded = encode_resource_server_frame(&active, &registry, &frame).unwrap();
        assert!(encoded.starts_with(RESOURCE_MARKER));
        assert_eq!(
            decode_resource_server_frame(&active, &registry, &encoded),
            Ok(frame)
        );
    }

    let controls = [
        ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
            stream_id: 4,
            request_id,
            add_items: 1,
            add_bytes: 2,
        }),
        ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: 4,
            request_id,
            reason: ResourceCancellationCode::ParentInvocationCancelled,
        }),
    ];
    for frame in controls {
        let encoded = encode_resource_client_frame(&active, &registry, &frame).unwrap();
        assert_eq!(
            decode_resource_client_frame(&active, &registry, &encoded),
            Ok(frame)
        );
        let mut trailing = encoded;
        trailing.push(0);
        assert!(matches!(
            decode_resource_client_frame(&active, &registry, &trailing),
            Err(FrameCodecError::TrailingBytes { .. })
        ));
    }
}
#[test]
fn resource_frame_variants_reject_zero_request_ids_at_codec_and_connection_boundaries() {
    let active = empty_active_revision();
    let registry = test_registry();
    let zero = InvocationId::from_bytes([0; 16]);
    let request_id = InvocationId::from_bytes([0x31; 16]);
    let revision = RevisionPair::new(
        SourceRevisionId::from_bytes([0x32; 16]),
        CatalogueRevisionId::from_bytes([0x33; 16]),
    );
    let value = RuntimeValue::Integer(7);
    let value_bytes = encode_constructed_value(&active, &registry, &value).unwrap();
    let accepted = ResourceAccepted {
        stream_id: 4,
        request_id,
        nested_invocation_id: InvocationId::from_bytes([0x34; 16]),
        target_revision: revision,
        resource_kind: ResourceKind::Stream,
    };
    let values = ResourceValues {
        stream_id: 4,
        request_id,
        target_revision: resource_revision_fixture(),
        batch_sequence: 0,
        item_count: 1,
        byte_count: value_bytes.len() as u32,
        values: vec![value.clone()],
    };
    let completed = ResourceCompleted {
        stream_id: 4,
        request_id,
        target_revision: resource_revision_fixture(),
        final_batch_sequence: 0,
        total_items: 0,
    };
    let failed = ResourceFailed {
        stream_id: 4,
        request_id,
        target_revision: resource_revision_fixture(),
        failure: CallFailure::InternalFailure,
    };
    let cancelled = ResourceCancelled {
        stream_id: 4,
        request_id,
        target_revision: resource_revision_fixture(),
        reason: ResourceCancellationCode::ClientRequested,
    };
    let window = ResourceWindowUpdate {
        stream_id: 4,
        request_id,
        add_items: 1,
        add_bytes: 2,
    };
    let cancel = ResourceCancel {
        stream_id: 4,
        request_id,
        reason: ResourceCancellationCode::ClientRequested,
    };

    let zero_request_id = |mut encoded: Vec<u8>| {
        encoded[RESOURCE_HEADER_LENGTH + 8..RESOURCE_HEADER_LENGTH + 8 + 16].fill(0);
        encoded
    };

    let mut zero_accepted = accepted.clone();
    zero_accepted.request_id = zero;
    assert_eq!(
        encode_resource_accepted(&zero_accepted),
        Err(FrameCodecError::ResourceMalformedPayload),
    );
    assert_eq!(
        decode_resource_accepted(&zero_request_id(
            encode_resource_accepted(&accepted).unwrap()
        )),
        Err(FrameCodecError::ResourceMalformedPayload),
    );

    let mut zero_values = values.clone();
    zero_values.request_id = zero;
    assert_eq!(
        encode_resource_values(&active, &registry, &zero_values),
        Err(FrameCodecError::ResourceMalformedPayload),
    );
    assert_eq!(
        decode_resource_values(
            &active,
            &registry,
            &zero_request_id(encode_resource_values(&active, &registry, &values).unwrap()),
        ),
        Err(FrameCodecError::ResourceMalformedPayload),
    );

    let mut zero_completed = completed;
    zero_completed.request_id = zero;
    assert_eq!(
        encode_resource_completed(&zero_completed),
        Err(FrameCodecError::ResourceMalformedPayload),
    );
    assert_eq!(
        decode_resource_completed(&zero_request_id(
            encode_resource_completed(&ResourceCompleted {
                request_id,
                ..zero_completed
            })
            .unwrap(),
        )),
        Err(FrameCodecError::ResourceMalformedPayload),
    );

    let mut zero_failed = failed;
    zero_failed.request_id = zero;
    assert_eq!(
        encode_resource_failed(&zero_failed),
        Err(FrameCodecError::ResourceMalformedPayload),
    );
    assert_eq!(
        decode_resource_failed(&zero_request_id(
            encode_resource_failed(&ResourceFailed {
                request_id,
                ..zero_failed
            })
            .unwrap(),
        )),
        Err(FrameCodecError::ResourceMalformedPayload),
    );

    let mut zero_cancelled = cancelled;
    zero_cancelled.request_id = zero;
    assert_eq!(
        encode_resource_cancelled(&zero_cancelled),
        Err(FrameCodecError::ResourceMalformedPayload),
    );
    assert_eq!(
        decode_resource_cancelled(&zero_request_id(
            encode_resource_cancelled(&ResourceCancelled {
                request_id,
                ..zero_cancelled
            })
            .unwrap(),
        )),
        Err(FrameCodecError::ResourceMalformedPayload),
    );

    let mut zero_window = window;
    zero_window.request_id = zero;
    assert_eq!(
        encode_resource_window_update(&zero_window),
        Err(FrameCodecError::ResourceMalformedPayload),
    );
    assert_eq!(
        decode_resource_window_update(&zero_request_id(
            encode_resource_window_update(&ResourceWindowUpdate {
                request_id,
                ..zero_window
            })
            .unwrap(),
        )),
        Err(FrameCodecError::ResourceMalformedPayload),
    );

    let mut zero_cancel = cancel;
    zero_cancel.request_id = zero;
    assert_eq!(
        encode_resource_cancel(&zero_cancel),
        Err(FrameCodecError::ResourceMalformedPayload),
    );
    assert_eq!(
        decode_resource_cancel(&zero_request_id(
            encode_resource_cancel(&ResourceCancel {
                request_id,
                ..zero_cancel
            })
            .unwrap(),
        )),
        Err(FrameCodecError::ResourceMalformedPayload),
    );

    let mut request = resource_request_fixture();
    request.stream_id = 4;
    request.request_id = request_id;
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 1;
    request.byte_window = value_bytes.len() as u64;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request.clone()).unwrap();
    let mut zero_accepted = accepted;
    zero_accepted.request_id = zero;
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(zero_accepted)),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceMalformedPayload,
        }),
    );
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x34; 16]),
            target_revision: request.target_revision,
            resource_kind: request.resource_kind,
        }))
        .unwrap();

    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            request_id: zero,
            target_revision: resource_revision_fixture(),
            values: vec![value.clone()],
            item_count: 1,
            byte_count: value_bytes.len() as u32,
            ..values
        })),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceMalformedPayload,
        }),
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Completed(ResourceCompleted {
            stream_id: request.stream_id,
            request_id: zero,
            target_revision: resource_revision_fixture(),
            final_batch_sequence: 0,
            total_items: 0,
        })),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceMalformedPayload,
        }),
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Failed(ResourceFailed {
            stream_id: request.stream_id,
            request_id: zero,
            target_revision: resource_revision_fixture(),
            failure: CallFailure::InternalFailure,
        })),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceMalformedPayload,
        }),
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Cancelled(ResourceCancelled {
            stream_id: request.stream_id,
            request_id: zero,
            target_revision: resource_revision_fixture(),
            reason: ResourceCancellationCode::ClientRequested,
        })),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceMalformedPayload,
        }),
    );
    assert_eq!(
        connection.receive(ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
            stream_id: request.stream_id,
            request_id: zero,
            add_items: 1,
            add_bytes: 1,
        })),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceMalformedPayload,
        }),
    );
    assert_eq!(
        connection.receive(ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: request.stream_id,
            request_id: zero,
            reason: ResourceCancellationCode::ClientRequested,
        })),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceMalformedPayload,
        }),
    );
    assert_eq!(
        connection.apply_cancelled_after_client_cancel(ResourceCancelled {
            stream_id: request.stream_id,
            request_id: zero,
            target_revision: resource_revision_fixture(),
            reason: ResourceCancellationCode::ClientRequested,
        }),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceMalformedPayload,
        }),
    );
    assert_eq!(connection.live_resources(), 1);
}

#[test]
fn resource_acceptance_rejects_zero_nested_identity_at_decode_and_apply() {
    let request = resource_request_fixture();
    let request_id = request.request_id;
    let accepted = ResourceAccepted {
        stream_id: request.stream_id,
        request_id,
        nested_invocation_id: InvocationId::from_bytes([0x44; 16]),
        target_revision: request.target_revision,
        resource_kind: request.resource_kind,
    };
    let mut encoded = encode_resource_accepted(&accepted).unwrap();
    let nested_start = RESOURCE_HEADER_LENGTH + 8 + 16;
    encoded[nested_start..nested_start + 16].fill(0);
    assert_eq!(
        decode_resource_accepted(&encoded),
        Err(FrameCodecError::ResourceMalformedPayload)
    );

    let mut connection = ResourceProtocolConnection::new();
    connection.open(request.clone()).unwrap();
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(ResourceAccepted {
            nested_invocation_id: InvocationId::from_bytes([0; 16]),
            ..accepted
        })),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceMalformedPayload,
        })
    );
    assert_eq!(
        connection.resource_nested_invocation_id(request.stream_id, request_id),
        Ok(None)
    );
    assert_eq!(connection.live_resources(), 1);
}

#[test]
fn resource_connection_apply_rejects_unbounded_direct_batch_metadata() {
    let request_id = InvocationId::from_bytes([0x45; 16]);
    let mut request = resource_request_fixture();
    request.request_id = request_id;
    request.resource_kind = ResourceKind::Stream;
    request.item_window = MAX_RESOURCE_WINDOW;
    request.byte_window = MAX_RESOURCE_WINDOW;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request.clone()).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x46; 16]),
            target_revision: request.target_revision,
            resource_kind: ResourceKind::Stream,
        }))
        .unwrap();

    let before = connection.clone();
    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id: request.stream_id,
            request_id,
            target_revision: resource_revision_fixture(),
            batch_sequence: 0,
            item_count: (MAX_RESOURCE_BATCH_ITEMS + 1) as u32,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(7); MAX_RESOURCE_BATCH_ITEMS + 1],
        })),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::TooManyResourceEntries {
                actual: MAX_RESOURCE_BATCH_ITEMS + 1,
                maximum: MAX_RESOURCE_BATCH_ITEMS,
            },
        })
    );
    assert_eq!(connection, before);

    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id: request.stream_id,
            request_id,
            target_revision: resource_revision_fixture(),
            batch_sequence: 0,
            item_count: 1,
            byte_count: (MAX_FRAME_PAYLOAD_LENGTH + 1) as u32,
            values: vec![RuntimeValue::Integer(7)],
        })),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::PayloadTooLarge {
                actual: MAX_FRAME_PAYLOAD_LENGTH + 1,
                maximum: MAX_FRAME_PAYLOAD_LENGTH,
            },
        })
    );
    assert_eq!(connection, before);
}

#[test]
fn constructed_resource_application_rejects_forged_byte_count_before_credit() {
    let active = empty_active_revision();
    let registry = test_registry();
    let value = RuntimeValue::Integer(7);
    let value_bytes = encode_constructed_value(&active, &registry, &value).unwrap();
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 1;
    request.byte_window = value_bytes.len() as u64;
    let request_id = request.request_id;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request.clone()).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x55; 16]),
            target_revision: request.target_revision,
            resource_kind: ResourceKind::Stream,
        }))
        .unwrap();
    let before = connection.clone();

    let forged = ResourceValues {
        stream_id: request.stream_id,
        request_id,
        target_revision: resource_revision_fixture(),
        batch_sequence: 0,
        item_count: 1,
        byte_count: 0,
        values: vec![value.clone()],
    };
    assert!(matches!(
        connection.apply_constructed(
            &active,
            &registry,
            ResourceServerFrame::Values(forged),
        ),
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceByteCountMismatch {
                declared: 0,
                actual,
            },
        }) if actual == value_bytes.len()
    ));
    assert_eq!(connection, before);
    assert_eq!(connection.live_resources(), 1);
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ResourceServerFrame::Values(ResourceValues {
                stream_id: request.stream_id,
                request_id,
                target_revision: resource_revision_fixture(),
                batch_sequence: 0,
                item_count: 1,
                byte_count: value_bytes.len() as u32,
                values: vec![value],
            }),
        ),
        Ok(ResourceFrameDisposition::Applied)
    );
}

#[test]
fn constructed_resource_application_drops_malformed_or_unsupported_late_values() {
    let active = empty_active_revision();
    let registry = test_registry();
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    let stream_id = request.stream_id;
    let request_id = request.request_id;
    let target_revision = request.target_revision;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request.clone()).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x56; 16]),
            target_revision,
            resource_kind: request.resource_kind,
        }))
        .unwrap();
    assert_eq!(
        connection.apply(ResourceServerFrame::Completed(ResourceCompleted {
            stream_id,
            request_id,
            target_revision,
            final_batch_sequence: 0,
            total_items: 0,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    let before = connection.clone();
    let before_terminal = connection.terminal.clone();

    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ResourceServerFrame::Values(ResourceValues {
                stream_id,
                request_id,
                target_revision,
                batch_sequence: u64::MAX,
                item_count: 0,
                byte_count: u32::MAX,
                values: Vec::new(),
            }),
        ),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
    assert_eq!(connection, before);
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ResourceServerFrame::Values(ResourceValues {
                stream_id,
                request_id,
                target_revision,
                batch_sequence: 1,
                item_count: 1,
                byte_count: 0,
                values: vec![RuntimeValue::InvokeRequest(minimal_request(None))],
            }),
        ),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
    assert_eq!(connection, before);
    assert_eq!(connection.terminal, before_terminal);
    assert_eq!(connection.live_resources(), 0);
    assert_eq!(
        connection.resource_credit(stream_id, request_id),
        Err(ResourceConnectionError::UnknownStream { stream_id })
    );
}

#[test]
fn resource_connection_rejects_window_updates_for_scalar_resources() {
    let request = resource_request_fixture();
    let request_id = request.request_id;
    let mut connection = ResourceProtocolConnection::new();
    assert_eq!(
        connection.open(request.clone()),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x57; 16]),
            target_revision: request.target_revision,
            resource_kind: ResourceKind::Single,
        })),
        Ok(ResourceFrameDisposition::Applied),
    );

    assert_eq!(
        connection.receive(ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
            stream_id: request.stream_id,
            request_id,
            add_items: 1,
            add_bytes: 1,
        })),
        Err(ResourceConnectionError::WrongState {
            stream_id: request.stream_id,
        }),
    );
    assert_eq!(connection.live_resources(), 1);
}

#[test]
fn resource_connection_rejects_acceptance_identity_mismatches_without_mutating_state() {
    let request = resource_request_fixture();
    let request_id = request.request_id;
    let mut connection = ResourceProtocolConnection::new();
    assert_eq!(
        connection.open(request.clone()),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.resource_nested_invocation_id(request.stream_id, request_id),
        Ok(None),
    );

    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x59; 16]),
            target_revision: request.target_revision,
            resource_kind: ResourceKind::Stream,
        })),
        Err(ResourceConnectionError::ResourceAcceptanceMismatch {
            stream_id: request.stream_id,
        }),
    );
    assert_eq!(connection.live_resources(), 1);

    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x5a; 16]),
            target_revision: RevisionPair::new(
                SourceRevisionId::from_bytes([0x60; 16]),
                CatalogueRevisionId::from_bytes([0x61; 16]),
            ),
            resource_kind: request.resource_kind,
        })),
        Err(ResourceConnectionError::ResourceAcceptanceMismatch {
            stream_id: request.stream_id,
        }),
    );
    assert_eq!(connection.live_resources(), 1);

    let accepted = ResourceAccepted {
        stream_id: request.stream_id,
        request_id,
        nested_invocation_id: InvocationId::from_bytes([0x62; 16]),
        target_revision: request.target_revision,
        resource_kind: request.resource_kind,
    };
    let decoded = decode_resource_accepted(&encode_resource_accepted(&accepted).unwrap()).unwrap();
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(decoded)),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.resource_nested_invocation_id(request.stream_id, request_id),
        Ok(Some(InvocationId::from_bytes([0x62; 16])))
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x63; 16]),
            target_revision: request.target_revision,
            resource_kind: request.resource_kind,
        })),
        Err(ResourceConnectionError::WrongState {
            stream_id: request.stream_id
        }),
    );
    assert_eq!(
        connection.resource_nested_invocation_id(request.stream_id, request_id),
        Ok(Some(InvocationId::from_bytes([0x62; 16])))
    );
    assert_eq!(connection.live_resources(), 1);
}

#[test]
fn resource_result_revision_mismatch_precedes_value_credit_and_terminal_mutation() {
    let request = resource_request_fixture();
    let mut values_connection = ResourceProtocolConnection::new();
    values_connection.open(request.clone()).unwrap();
    values_connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x70; 16]),
            target_revision: request.target_revision,
            resource_kind: request.resource_kind,
        }))
        .unwrap();
    let credit_before = values_connection
        .resource_credit(request.stream_id, request.request_id)
        .unwrap();
    assert_eq!(
        values_connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: resource_other_revision_fixture(),
            batch_sequence: 99,
            item_count: u32::MAX,
            byte_count: u32::MAX,
            values: Vec::new(),
        })),
        Err(ResourceConnectionError::ResourceRevisionMismatch {
            stream_id: request.stream_id,
        })
    );
    assert_eq!(
        values_connection
            .resource_credit(request.stream_id, request.request_id)
            .unwrap(),
        credit_before
    );
    assert_eq!(values_connection.live_resources(), 1);

    let mut completed_request = request.clone();
    completed_request.stream_id = 2;
    completed_request.resource_kind = ResourceKind::Stream;
    let mut completed_connection = ResourceProtocolConnection::new();
    completed_connection
        .open(completed_request.clone())
        .unwrap();
    completed_connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: completed_request.stream_id,
            request_id: completed_request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x71; 16]),
            target_revision: completed_request.target_revision,
            resource_kind: completed_request.resource_kind,
        }))
        .unwrap();
    assert_eq!(
        completed_connection.apply(ResourceServerFrame::Completed(ResourceCompleted {
            stream_id: completed_request.stream_id,
            request_id: completed_request.request_id,
            target_revision: resource_other_revision_fixture(),
            final_batch_sequence: u64::MAX,
            total_items: u64::MAX,
        })),
        Err(ResourceConnectionError::ResourceRevisionMismatch {
            stream_id: completed_request.stream_id,
        })
    );
    assert_eq!(completed_connection.live_resources(), 1);

    for (stream_id, frame) in [
        (
            3,
            ResourceServerFrame::Failed(ResourceFailed {
                stream_id: 3,
                request_id: request.request_id,
                target_revision: resource_other_revision_fixture(),
                failure: CallFailure::InternalFailure,
            }),
        ),
        (
            4,
            ResourceServerFrame::Cancelled(ResourceCancelled {
                stream_id: 4,
                request_id: request.request_id,
                target_revision: resource_other_revision_fixture(),
                reason: ResourceCancellationCode::ServerRequested,
            }),
        ),
    ] {
        let mut terminal_request = request.clone();
        terminal_request.stream_id = stream_id;
        let mut connection = ResourceProtocolConnection::new();
        connection.open(terminal_request.clone()).unwrap();
        connection
            .apply(ResourceServerFrame::Accepted(ResourceAccepted {
                stream_id,
                request_id: terminal_request.request_id,
                nested_invocation_id: InvocationId::from_bytes([0x72 + stream_id as u8; 16]),
                target_revision: terminal_request.target_revision,
                resource_kind: terminal_request.resource_kind,
            }))
            .unwrap();
        assert_eq!(
            connection.apply(frame),
            Err(ResourceConnectionError::ResourceRevisionMismatch { stream_id })
        );
        assert_eq!(connection.live_resources(), 1);
    }
}

#[test]
fn resource_connection_rejects_multi_value_scalar_batches() {
    let request = resource_request_fixture();
    let request_id = request.request_id;
    let mut connection = ResourceProtocolConnection::new();
    assert_eq!(
        connection.open(request.clone()),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x55; 16]),
            target_revision: request.target_revision,
            resource_kind: ResourceKind::Single,
        })),
        Ok(ResourceFrameDisposition::Applied),
    );

    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id: request.stream_id,
            request_id,
            target_revision: resource_revision_fixture(),
            batch_sequence: 0,
            item_count: 2,
            byte_count: 2,
            values: vec![RuntimeValue::Integer(1), RuntimeValue::Integer(2)],
        })),
        Err(ResourceConnectionError::ResourceBatchMismatch {
            stream_id: request.stream_id,
        }),
    );
    assert_eq!(connection.live_resources(), 1);
}

#[test]
fn resource_connection_rejects_scalar_terminal_after_values_but_completes() {
    let mut request = resource_request_fixture();
    request.item_window = 1;
    request.byte_window = 1;
    let request_id = request.request_id;
    let mut connection = ResourceProtocolConnection::new();
    assert_eq!(
        connection.open(request.clone()),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x58; 16]),
            target_revision: request.target_revision,
            resource_kind: ResourceKind::Single,
        })),
        Ok(ResourceFrameDisposition::Applied),
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id: request.stream_id,
            request_id,
            target_revision: resource_revision_fixture(),
            batch_sequence: 0,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(7)],
        })),
        Ok(ResourceFrameDisposition::Applied),
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Failed(ResourceFailed {
            stream_id: request.stream_id,
            request_id,
            target_revision: resource_revision_fixture(),
            failure: CallFailure::InternalFailure,
        })),
        Err(ResourceConnectionError::WrongState {
            stream_id: request.stream_id,
        }),
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Cancelled(ResourceCancelled {
            stream_id: request.stream_id,
            request_id,
            target_revision: resource_revision_fixture(),
            reason: ResourceCancellationCode::ServerRequested,
        })),
        Err(ResourceConnectionError::WrongState {
            stream_id: request.stream_id,
        }),
    );
    assert_eq!(connection.live_resources(), 1);
    assert_eq!(
        connection.apply(ResourceServerFrame::Completed(ResourceCompleted {
            stream_id: request.stream_id,
            request_id,
            target_revision: resource_revision_fixture(),
            final_batch_sequence: 0,
            total_items: 1,
        })),
        Ok(ResourceFrameDisposition::Applied),
    );
    assert_eq!(connection.live_resources(), 0);
}

#[test]
fn resource_connection_accepts_terminal_frames_before_acceptance() {
    let request = resource_request_fixture();
    let request_id = request.request_id;
    let stream_id = request.stream_id;

    let mut failed_connection = ResourceProtocolConnection::new();
    failed_connection.open(request.clone()).unwrap();
    assert_eq!(
        failed_connection.apply(ResourceServerFrame::Failed(ResourceFailed {
            stream_id,
            request_id,
            target_revision: resource_revision_fixture(),
            failure: CallFailure::InternalFailure,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(failed_connection.live_resources(), 0);

    let mut cancelled_connection = ResourceProtocolConnection::new();
    cancelled_connection.open(request).unwrap();
    assert_eq!(
        cancelled_connection.apply(ResourceServerFrame::Cancelled(ResourceCancelled {
            stream_id,
            request_id,
            target_revision: resource_revision_fixture(),
            reason: ResourceCancellationCode::ServerRequested,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(cancelled_connection.live_resources(), 0);
}

#[test]
fn resource_connection_accepts_empty_stream_completion() {
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 1;
    request.byte_window = 1;
    let request_id = request.request_id;
    let mut connection = ResourceProtocolConnection::new();

    assert_eq!(
        connection.open(request.clone()),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x64; 16]),
            target_revision: request.target_revision,
            resource_kind: ResourceKind::Stream,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Completed(ResourceCompleted {
            stream_id: request.stream_id,
            request_id,
            target_revision: resource_revision_fixture(),
            final_batch_sequence: 0,
            total_items: 0,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(connection.live_resources(), 0);
    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id: request.stream_id,
            request_id,
            target_revision: resource_revision_fixture(),
            batch_sequence: 0,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(1)],
        })),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
}

#[test]
fn resource_connection_isolates_pre_acceptance_terminal_outcomes_from_live_resources() {
    let request_id = InvocationId::from_bytes([0x12; 16]);
    let live_request_id = InvocationId::from_bytes([0x13; 16]);
    let outcomes = [
        ResourceServerFrame::Failed(ResourceFailed {
            stream_id: 1,
            request_id,
            target_revision: resource_revision_fixture(),
            failure: CallFailure::InternalFailure,
        }),
        ResourceServerFrame::Cancelled(ResourceCancelled {
            stream_id: 1,
            request_id,
            target_revision: resource_revision_fixture(),
            reason: ResourceCancellationCode::ServerRequested,
        }),
    ];

    for outcome in outcomes {
        let mut requested = resource_request_fixture();
        requested.request_id = request_id;
        let mut live = resource_request_fixture();
        live.stream_id = 2;
        live.request_id = live_request_id;
        live.resource_kind = ResourceKind::Stream;
        live.item_window = 17;
        live.byte_window = 19;

        let mut connection = ResourceProtocolConnection::new();
        connection.open(requested.clone()).unwrap();
        connection.open(live.clone()).unwrap();
        let nested_invocation_id = InvocationId::from_bytes([0x14; 16]);
        connection
            .apply(ResourceServerFrame::Accepted(ResourceAccepted {
                stream_id: live.stream_id,
                request_id: live.request_id,
                nested_invocation_id,
                target_revision: live.target_revision,
                resource_kind: live.resource_kind,
            }))
            .unwrap();

        assert_eq!(connection.live_resources(), 2);
        assert_eq!(
            connection.apply(outcome),
            Ok(ResourceFrameDisposition::Applied)
        );
        assert_eq!(connection.live_resources(), 1);
        assert_eq!(
            connection.resource_credit(requested.stream_id, requested.request_id),
            Err(ResourceConnectionError::UnknownStream {
                stream_id: requested.stream_id,
            }),
        );
        assert_eq!(
            connection.resource_credit(live.stream_id, live.request_id),
            Ok(ResourceCredit {
                item_available: 17,
                byte_available: 19,
            }),
        );
        assert_eq!(
            connection.resource_nested_invocation_id(live.stream_id, live.request_id),
            Ok(Some(nested_invocation_id)),
        );
    }
}

#[test]
fn resource_connection_reports_available_credit_for_live_stream() {
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 7;
    request.byte_window = 11;
    let request_id = request.request_id;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request.clone()).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x54; 16]),
            target_revision: request.target_revision,
            resource_kind: ResourceKind::Stream,
        }))
        .unwrap();
    let before = connection.clone();

    assert_eq!(
        connection.resource_credit(request.stream_id, request_id),
        Ok(ResourceCredit {
            item_available: 7,
            byte_available: 11,
        }),
    );
    assert_eq!(connection, before);
}

#[test]
fn resource_connection_reports_zero_credit_after_values_exhaust_window() {
    let active = empty_active_revision();
    let registry = test_registry();
    let value = RuntimeValue::Integer(7);
    let value_bytes = encode_constructed_value(&active, &registry, &value).unwrap();
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 1;
    request.byte_window = value_bytes.len() as u64;
    let request_id = request.request_id;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request.clone()).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x55; 16]),
            target_revision: request.target_revision,
            resource_kind: ResourceKind::Stream,
        }))
        .unwrap();
    connection
        .apply(ResourceServerFrame::Values(ResourceValues {
            stream_id: request.stream_id,
            request_id,
            target_revision: resource_revision_fixture(),
            batch_sequence: 0,
            item_count: 1,
            byte_count: value_bytes.len() as u32,
            values: vec![value],
        }))
        .unwrap();

    assert_eq!(
        connection.resource_credit(request.stream_id, request_id),
        Ok(ResourceCredit {
            item_available: 0,
            byte_available: 0,
        }),
    );
}

#[test]
fn resource_connection_reports_credit_after_checked_window_update() {
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 2;
    request.byte_window = 3;
    let request_id = request.request_id;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request.clone()).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: request.stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x56; 16]),
            target_revision: request.target_revision,
            resource_kind: ResourceKind::Stream,
        }))
        .unwrap();

    assert_eq!(
        connection.receive(ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
            stream_id: request.stream_id,
            request_id,
            add_items: 4,
            add_bytes: 5,
        })),
        Ok(ResourceFrameDisposition::Applied),
    );
    assert_eq!(
        connection.resource_credit(request.stream_id, request_id),
        Ok(ResourceCredit {
            item_available: 6,
            byte_available: 8,
        }),
    );
}

#[test]
fn resource_connection_rejects_credit_lookup_for_unknown_or_mismatched_request() {
    let request = resource_request_fixture();
    let request_id = request.request_id;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request.clone()).unwrap();

    assert_eq!(
        connection.resource_credit(request.stream_id, InvocationId::from_bytes([0x99; 16])),
        Err(ResourceConnectionError::MismatchedRequest {
            stream_id: request.stream_id,
        }),
    );
    assert_eq!(
        connection.resource_credit(99, request_id),
        Err(ResourceConnectionError::UnknownStream { stream_id: 99 }),
    );
}

#[test]
fn resource_connection_tracks_acceptance_credit_sequence_and_terminal_late_frames() {
    let active = empty_active_revision();
    let registry = test_registry();
    let value = RuntimeValue::Integer(7);
    let value_bytes = encode_constructed_value(&active, &registry, &value).unwrap();
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 1;
    request.byte_window = value_bytes.len() as u64;
    let request_id = request.request_id;
    let mut connection = ResourceProtocolConnection::new();
    assert_eq!(
        connection.open(request.clone()),
        Ok(ResourceFrameDisposition::Applied)
    );
    let accepted = ResourceAccepted {
        stream_id: request.stream_id,
        request_id,
        nested_invocation_id: InvocationId::from_bytes([0x55; 16]),
        target_revision: request.target_revision,
        resource_kind: ResourceKind::Stream,
    };
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(accepted)),
        Ok(ResourceFrameDisposition::Applied)
    );
    let values = ResourceValues {
        stream_id: request.stream_id,
        request_id,
        target_revision: resource_revision_fixture(),
        batch_sequence: 0,
        item_count: 1,
        byte_count: value_bytes.len() as u32,
        values: vec![value.clone()],
    };
    assert_eq!(
        connection.apply(ResourceServerFrame::Values(values.clone())),
        Ok(ResourceFrameDisposition::Applied)
    );
    let mut exhausted = values.clone();
    exhausted.batch_sequence = 1;
    assert_eq!(
        connection.apply(ResourceServerFrame::Values(exhausted)),
        Err(ResourceConnectionError::InsufficientCredit {
            stream_id: 1,
            item_available: 0,
            item_required: 1,
            byte_available: 0,
            byte_required: value_bytes.len() as u64,
        })
    );
    assert_eq!(
        connection.receive(ResourceClientFrame::WindowUpdate(ResourceWindowUpdate {
            stream_id: 1,
            request_id,
            add_items: 1,
            add_bytes: value_bytes.len() as u64,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id: 1,
            request_id,
            target_revision: resource_revision_fixture(),
            batch_sequence: 1,
            item_count: 1,
            byte_count: value_bytes.len() as u32,
            values: vec![value.clone()],
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Completed(ResourceCompleted {
            stream_id: 1,
            request_id,
            target_revision: resource_revision_fixture(),
            final_batch_sequence: 1,
            total_items: 2,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Values(values)),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
    assert_eq!(connection.live_resources(), 0);
    assert_eq!(
        connection.resource_nested_invocation_id(1, request_id),
        Err(ResourceConnectionError::UnknownStream { stream_id: 1 }),
    );
}

#[test]
fn resource_connection_reports_item_and_byte_credit_exhaustion_independently() {
    let active = empty_active_revision();
    let registry = test_registry();
    let value = RuntimeValue::Integer(7);
    let value_bytes = encode_constructed_value(&active, &registry, &value).unwrap();
    let mut connection = ResourceProtocolConnection::new();

    let mut item_request = resource_request_fixture();
    item_request.stream_id = 10;
    item_request.request_id = InvocationId::from_bytes([0x70; 16]);
    item_request.resource_kind = ResourceKind::Stream;
    item_request.item_window = 1;
    item_request.byte_window = (value_bytes.len() * 2) as u64;
    connection.open(item_request.clone()).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: 10,
            request_id: item_request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x71; 16]),
            target_revision: item_request.target_revision,
            resource_kind: ResourceKind::Stream,
        }))
        .unwrap();
    connection
        .apply(ResourceServerFrame::Values(ResourceValues {
            stream_id: 10,
            request_id: item_request.request_id,
            target_revision: resource_revision_fixture(),
            batch_sequence: 0,
            item_count: 1,
            byte_count: value_bytes.len() as u32,
            values: vec![value.clone()],
        }))
        .unwrap();
    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id: 10,
            request_id: item_request.request_id,
            target_revision: resource_revision_fixture(),
            batch_sequence: 1,
            item_count: 1,
            byte_count: value_bytes.len() as u32,
            values: vec![value.clone()],
        })),
        Err(ResourceConnectionError::InsufficientCredit {
            stream_id: 10,
            item_available: 0,
            item_required: 1,
            byte_available: value_bytes.len() as u64,
            byte_required: value_bytes.len() as u64,
        })
    );

    let mut byte_request = resource_request_fixture();
    byte_request.stream_id = 11;
    byte_request.request_id = InvocationId::from_bytes([0x72; 16]);
    byte_request.resource_kind = ResourceKind::Stream;
    byte_request.item_window = 2;
    byte_request.byte_window = value_bytes.len() as u64;
    connection.open(byte_request.clone()).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: 11,
            request_id: byte_request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x73; 16]),
            target_revision: byte_request.target_revision,
            resource_kind: ResourceKind::Stream,
        }))
        .unwrap();
    connection
        .apply(ResourceServerFrame::Values(ResourceValues {
            stream_id: 11,
            request_id: byte_request.request_id,
            target_revision: resource_revision_fixture(),
            batch_sequence: 0,
            item_count: 1,
            byte_count: value_bytes.len() as u32,
            values: vec![value.clone()],
        }))
        .unwrap();
    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id: 11,
            request_id: byte_request.request_id,
            target_revision: resource_revision_fixture(),
            batch_sequence: 1,
            item_count: 1,
            byte_count: value_bytes.len() as u32,
            values: vec![value],
        })),
        Err(ResourceConnectionError::InsufficientCredit {
            stream_id: 11,
            item_available: 1,
            item_required: 1,
            byte_available: 0,
            byte_required: value_bytes.len() as u64,
        })
    );
}

#[test]
fn resource_connection_enforces_max_live_streams_with_state_preservation() {
    let mut connection = ResourceProtocolConnection::new();
    for stream_id in 1..=MAX_LIVE_STREAMS as u64 {
        let mut request = resource_request_fixture();
        request.stream_id = stream_id;
        request.request_id = InvocationId::from_bytes([stream_id as u8; 16]);
        assert_eq!(
            connection.open(request),
            Ok(ResourceFrameDisposition::Applied)
        );
    }

    let before_rejected_open = connection.clone();
    let mut rejected_request = resource_request_fixture();
    rejected_request.stream_id = MAX_LIVE_STREAMS as u64 + 1;
    rejected_request.request_id = InvocationId::from_bytes([(MAX_LIVE_STREAMS as u8) + 1; 16]);
    assert_eq!(
        connection.open(rejected_request.clone()),
        Err(ResourceConnectionError::TooManyLiveResources)
    );
    assert_eq!(connection, before_rejected_open);

    assert_eq!(
        connection.apply(ResourceServerFrame::Failed(ResourceFailed {
            stream_id: 1,
            request_id: InvocationId::from_bytes([1; 16]),
            target_revision: resource_revision_fixture(),
            failure: CallFailure::InternalFailure,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(connection.live_resources(), MAX_LIVE_STREAMS - 1);
    assert_eq!(
        connection.open(rejected_request),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(connection.live_resources(), MAX_LIVE_STREAMS);
}

#[test]
fn resource_terminal_tombstones_evict_oldest_late_frames() {
    let mut connection = ResourceProtocolConnection::new();
    for stream_id in 1..=(MAX_LIVE_STREAMS + 1) as u64 {
        let mut request = resource_request_fixture();
        request.stream_id = stream_id;
        request.request_id = InvocationId::from_bytes([stream_id as u8; 16]);
        assert_eq!(
            connection.open(request.clone()),
            Ok(ResourceFrameDisposition::Applied)
        );
        assert_eq!(
            connection.apply(ResourceServerFrame::Accepted(ResourceAccepted {
                stream_id,
                request_id: request.request_id,
                nested_invocation_id: InvocationId::from_bytes([0x80; 16]),
                target_revision: request.target_revision,
                resource_kind: request.resource_kind,
            })),
            Ok(ResourceFrameDisposition::Applied)
        );
        assert_eq!(
            connection.apply(ResourceServerFrame::Failed(ResourceFailed {
                stream_id,
                request_id: request.request_id,
                target_revision: resource_revision_fixture(),
                failure: CallFailure::InternalFailure,
            })),
            Ok(ResourceFrameDisposition::Applied)
        );
    }
    assert_eq!(connection.live_resources(), 0);
    assert_eq!(
        connection.apply(ResourceServerFrame::Failed(ResourceFailed {
            stream_id: 1,
            request_id: InvocationId::from_bytes([1; 16]),
            target_revision: resource_revision_fixture(),
            failure: CallFailure::InternalFailure,
        })),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Failed(ResourceFailed {
            stream_id: 2,
            request_id: InvocationId::from_bytes([2; 16]),
            target_revision: resource_revision_fixture(),
            failure: CallFailure::InternalFailure,
        })),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
}

#[test]
fn resource_terminal_tombstones_retain_oldest_cancelled_stream() {
    let mut connection = ResourceProtocolConnection::new();
    let oldest_request_id = InvocationId::from_bytes([1; 16]);

    for stream_id in 1..=(MAX_LIVE_STREAMS + 1) as u64 {
        let mut request = resource_request_fixture();
        request.stream_id = stream_id;
        request.request_id = InvocationId::from_bytes([stream_id as u8; 16]);
        assert_eq!(
            connection.open(request.clone()),
            Ok(ResourceFrameDisposition::Applied)
        );
        assert_eq!(
            connection.receive(ResourceClientFrame::Cancel(ResourceCancel {
                stream_id,
                request_id: request.request_id,
                reason: ResourceCancellationCode::ClientRequested,
            })),
            Ok(ResourceFrameDisposition::Applied)
        );
    }

    assert_eq!(connection.live_resources(), 0);
    assert_eq!(connection.terminal.len(), MAX_LIVE_STREAMS + 1);
    assert_eq!(
        connection
            .terminal
            .get(&1)
            .map(|(request_id, _, _)| request_id),
        Some(&oldest_request_id)
    );

    let oldest_cancel = ResourceClientFrame::Cancel(ResourceCancel {
        stream_id: 1,
        request_id: oldest_request_id,
        reason: ResourceCancellationCode::ClientRequested,
    });
    assert_eq!(
        connection.receive(oldest_cancel.clone()),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
    assert_eq!(
        connection.receive(oldest_cancel),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
}

#[test]
fn resource_connection_cancellation_and_shutdown_drop_late_frames() {
    let mut request = resource_request_fixture();
    request.stream_id = 2;
    request.request_id = InvocationId::from_bytes([0x44; 16]);
    request.resource_kind = ResourceKind::Stream;
    let request_id = request.request_id;
    let mut connection = ResourceProtocolConnection::new();
    assert_eq!(
        connection.receive(ResourceClientFrame::Request(request.clone())),
        Ok(ResourceFrameDisposition::Applied)
    );
    let accepted = ResourceAccepted {
        stream_id: 2,
        request_id,
        nested_invocation_id: InvocationId::from_bytes([0x56; 16]),
        target_revision: request.target_revision,
        resource_kind: ResourceKind::Stream,
    };
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(accepted.clone())),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.receive(ResourceClientFrame::Cancel(ResourceCancel {
            stream_id: 2,
            request_id,
            reason: ResourceCancellationCode::ClientRequested
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply_cancelled_after_client_cancel(ResourceCancelled {
            stream_id: 2,
            request_id,
            target_revision: resource_other_revision_fixture(),
            reason: ResourceCancellationCode::ClientRequested,
        }),
        Err(ResourceConnectionError::ResourceRevisionMismatch { stream_id: 2 })
    );
    assert_eq!(
        connection.apply_cancelled_after_client_cancel(ResourceCancelled {
            stream_id: 2,
            request_id,
            target_revision: request.target_revision,
            reason: ResourceCancellationCode::ClientRequested,
        }),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(accepted)),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id: 2,
            request_id,
            target_revision: resource_revision_fixture(),
            batch_sequence: 0,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(1)]
        })),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Completed(ResourceCompleted {
            stream_id: 2,
            request_id,
            target_revision: resource_revision_fixture(),
            final_batch_sequence: 0,
            total_items: 0
        })),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Failed(ResourceFailed {
            stream_id: 2,
            request_id,
            target_revision: resource_revision_fixture(),
            failure: CallFailure::InternalFailure
        })),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Cancelled(ResourceCancelled {
            stream_id: 2,
            request_id,
            target_revision: resource_revision_fixture(),
            reason: ResourceCancellationCode::ClientRequested
        })),
        Ok(ResourceFrameDisposition::DroppedLate)
    );

    let mut second = resource_request_fixture();
    second.stream_id = 3;
    second.request_id = InvocationId::from_bytes([0x45; 16]);
    assert_eq!(
        connection.open(second.clone()),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(connection.shutdown(), 1);
    let mut after_shutdown = second.clone();
    after_shutdown.stream_id = 4;
    assert_eq!(
        connection.open(after_shutdown),
        Err(ResourceConnectionError::WrongState { stream_id: 4 }),
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Failed(ResourceFailed {
            stream_id: 3,
            request_id: second.request_id,
            target_revision: resource_revision_fixture(),
            failure: CallFailure::InternalFailure
        })),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
}

#[test]
fn resource_connection_completion_wins_over_late_client_cancellation() {
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    let request_id = request.request_id;
    let stream_id = request.stream_id;
    let target_revision = request.target_revision;
    let mut connection = ResourceProtocolConnection::new();

    assert_eq!(
        connection.open(request.clone()),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x66; 16]),
            target_revision,
            resource_kind: ResourceKind::Stream,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Completed(ResourceCompleted {
            stream_id,
            request_id,
            target_revision,
            final_batch_sequence: 0,
            total_items: 0,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(connection.live_resources(), 0);
    let tombstone_after_completion = connection.terminal.clone();

    assert_eq!(
        connection.receive(ResourceClientFrame::Cancel(ResourceCancel {
            stream_id,
            request_id,
            reason: ResourceCancellationCode::ClientRequested,
        })),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
    assert_eq!(connection.live_resources(), 0);
    assert_eq!(connection.terminal, tombstone_after_completion);

    for frame in [
        ResourceServerFrame::Values(ResourceValues {
            stream_id,
            request_id,
            target_revision,
            batch_sequence: 0,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(1)],
        }),
        ResourceServerFrame::Completed(ResourceCompleted {
            stream_id,
            request_id,
            target_revision,
            final_batch_sequence: 0,
            total_items: 0,
        }),
        ResourceServerFrame::Failed(ResourceFailed {
            stream_id,
            request_id,
            target_revision,
            failure: CallFailure::InternalFailure,
        }),
        ResourceServerFrame::Cancelled(ResourceCancelled {
            stream_id,
            request_id,
            target_revision,
            reason: ResourceCancellationCode::ClientRequested,
        }),
    ] {
        assert_eq!(
            connection.apply(frame),
            Ok(ResourceFrameDisposition::DroppedLate)
        );
        assert_eq!(connection.live_resources(), 0);
        assert_eq!(connection.terminal, tombstone_after_completion);
    }
}

#[test]
fn resource_connection_rejects_stale_terminal_identity_without_mutating_state() {
    let request = resource_request_fixture();
    let stream_id = request.stream_id;
    let request_id = request.request_id;
    let target_revision = request.target_revision;
    let stale_request_id = InvocationId::from_bytes([0xa7; 16]);
    let mut connection = ResourceProtocolConnection::new();

    connection.open(request).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0xa8; 16]),
            target_revision,
            resource_kind: ResourceKind::Single,
        }))
        .unwrap();
    let before = connection.clone();

    for frame in [
        ResourceServerFrame::Completed(ResourceCompleted {
            stream_id,
            request_id: stale_request_id,
            target_revision,
            final_batch_sequence: 0,
            total_items: 0,
        }),
        ResourceServerFrame::Failed(ResourceFailed {
            stream_id,
            request_id: stale_request_id,
            target_revision,
            failure: CallFailure::InternalFailure,
        }),
        ResourceServerFrame::Cancelled(ResourceCancelled {
            stream_id,
            request_id: stale_request_id,
            target_revision,
            reason: ResourceCancellationCode::ServerRequested,
        }),
    ] {
        assert_eq!(
            connection.apply(frame),
            Err(ResourceConnectionError::MismatchedRequest { stream_id })
        );
        assert_eq!(connection, before);
    }
    assert_eq!(connection.live_resources(), 1);
    assert_eq!(
        connection.resource_credit(stream_id, request_id),
        Ok(ResourceCredit {
            item_available: 10,
            byte_available: 11,
        })
    );
}

#[test]
fn resource_connection_rejects_stale_cancel_confirmation_identity_without_mutating_state() {
    let request = resource_request_fixture();
    let stream_id = request.stream_id;
    let request_id = request.request_id;
    let target_revision = request.target_revision;
    let stale_request_id = InvocationId::from_bytes([0xa9; 16]);
    let mut connection = ResourceProtocolConnection::new();

    assert_eq!(
        connection.open(request.clone()),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0xaa; 16]),
            target_revision,
            resource_kind: request.resource_kind,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.receive(ResourceClientFrame::Cancel(ResourceCancel {
            stream_id,
            request_id,
            reason: ResourceCancellationCode::ClientRequested,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );

    let before = connection.clone();
    let credit_before = connection.resource_credit(stream_id, request_id);
    let live_before = connection.live_resources();
    assert_eq!(
        credit_before,
        Err(ResourceConnectionError::UnknownStream { stream_id })
    );
    assert_eq!(live_before, 0);

    assert_eq!(
        connection.apply_cancelled_after_client_cancel(ResourceCancelled {
            stream_id,
            request_id: stale_request_id,
            target_revision,
            reason: ResourceCancellationCode::ClientRequested,
        }),
        Err(ResourceConnectionError::MismatchedRequest { stream_id })
    );
    assert_eq!(connection, before);
    assert_eq!(connection.resource_credit(stream_id, request_id), credit_before);
    assert_eq!(connection.live_resources(), live_before);

    assert_eq!(
        connection.apply_cancelled_after_client_cancel(ResourceCancelled {
            stream_id,
            request_id,
            target_revision,
            reason: ResourceCancellationCode::ClientRequested,
        }),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.terminal.get(&stream_id),
        Some(&(request_id, target_revision, ResourceTerminalKind::Cancelled))
    );
}

#[test]
fn resource_connection_drops_cancel_confirmation_after_committed_completion() {
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    let request_id = request.request_id;
    let stream_id = request.stream_id;
    let target_revision = request.target_revision;
    let mut connection = ResourceProtocolConnection::new();

    assert_eq!(
        connection.open(request.clone()),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x77; 16]),
            target_revision,
            resource_kind: ResourceKind::Stream,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Completed(ResourceCompleted {
            stream_id,
            request_id,
            target_revision,
            final_batch_sequence: 0,
            total_items: 0,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );

    let before_late_cancel = connection.clone();
    let before_credit = connection.resource_credit(stream_id, request_id);
    assert_eq!(
        before_credit,
        Err(ResourceConnectionError::UnknownStream { stream_id })
    );
    assert_eq!(
        connection.apply_cancelled_after_client_cancel(ResourceCancelled {
            stream_id,
            request_id,
            target_revision,
            reason: ResourceCancellationCode::ClientRequested,
        }),
        Ok(ResourceFrameDisposition::DroppedLate)
    );
    assert_eq!(connection, before_late_cancel);
    assert_eq!(connection.live_resources(), 0);
    assert_eq!(
        connection.resource_credit(stream_id, request_id),
        before_credit
    );
}

#[test]
fn resource_connection_rejects_request_id_reuse_across_streams_and_after_cleanup() {
    let mut first = resource_request_fixture();
    first.stream_id = 1;
    first.request_id = InvocationId::from_bytes([0x71; 16]);
    let mut duplicate = first.clone();
    duplicate.stream_id = 2;
    let mut connection = ResourceProtocolConnection::new();

    assert_eq!(
        connection.open(first.clone()),
        Ok(ResourceFrameDisposition::Applied)
    );
    let before_duplicate = connection.clone();
    assert_eq!(
        connection.open(duplicate),
        Err(ResourceConnectionError::DuplicateRequestId {
            request_id: first.request_id,
        }),
    );
    assert_eq!(connection, before_duplicate);

    assert_eq!(
        connection.apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id: first.stream_id,
            request_id: first.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x81; 16]),
            target_revision: first.target_revision,
            resource_kind: first.resource_kind,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Failed(ResourceFailed {
            stream_id: first.stream_id,
            request_id: first.request_id,
            target_revision: resource_revision_fixture(),
            failure: CallFailure::InternalFailure,
        })),
        Ok(ResourceFrameDisposition::Applied),
    );

    let request_id = first.request_id;
    let mut after_cleanup = first;
    after_cleanup.stream_id = 3;
    assert_eq!(
        connection.open(after_cleanup),
        Err(ResourceConnectionError::DuplicateRequestId { request_id }),
    );
}

#[test]
fn resource_connection_accepts_distinct_request_ids_on_distinct_streams() {
    let mut first = resource_request_fixture();
    first.stream_id = 1;
    first.request_id = InvocationId::from_bytes([0x72; 16]);
    let mut second = first.clone();
    second.stream_id = 2;
    second.request_id = InvocationId::from_bytes([0x73; 16]);
    let mut connection = ResourceProtocolConnection::new();

    assert_eq!(
        connection.open(first),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(
        connection.open(second),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(connection.live_resources(), 2);
}
#[test]
fn resource_connection_rejects_unknown_lower_stream_without_tombstone() {
    let mut request = resource_request_fixture();
    request.stream_id = 2;
    let mut connection = ResourceProtocolConnection::new();
    assert_eq!(
        connection.open(request),
        Ok(ResourceFrameDisposition::Applied)
    );

    assert_eq!(
        connection.apply(ResourceServerFrame::Failed(ResourceFailed {
            stream_id: 1,
            request_id: InvocationId::from_bytes([0x99; 16]),
            target_revision: resource_revision_fixture(),
            failure: CallFailure::InternalFailure,
        })),
        Err(ResourceConnectionError::UnknownStream { stream_id: 1 }),
    );
    assert_eq!(connection.live_resources(), 1);
}

#[test]
fn resource_connection_bounds_request_id_history_at_terminal_eviction_boundary() {
    let mut connection = ResourceProtocolConnection::new();
    for stream_id in 1..=MAX_REQUEST_ID_HISTORY as u64 {
        let mut request = resource_request_fixture();
        request.stream_id = stream_id;
        request.request_id = InvocationId::from_bytes([stream_id as u8; 16]);
        assert_eq!(
            connection.open(request.clone()),
            Ok(ResourceFrameDisposition::Applied)
        );
        assert_eq!(
            connection.apply(ResourceServerFrame::Failed(ResourceFailed {
                stream_id,
                request_id: request.request_id,
                target_revision: resource_revision_fixture(),
                failure: CallFailure::InternalFailure,
            })),
            Ok(ResourceFrameDisposition::Applied),
        );
    }

    let mut next = resource_request_fixture();
    next.stream_id = MAX_REQUEST_ID_HISTORY as u64 + 1;
    next.request_id = InvocationId::from_bytes([0xff; 16]);
    assert_eq!(
        connection.open(next.clone()),
        Ok(ResourceFrameDisposition::Applied),
    );
    assert_eq!(
        connection.apply(ResourceServerFrame::Failed(ResourceFailed {
            stream_id: next.stream_id,
            request_id: next.request_id,
            target_revision: resource_revision_fixture(),
            failure: CallFailure::InternalFailure,
        })),
        Ok(ResourceFrameDisposition::Applied),
    );
    assert_eq!(connection.terminal.len(), MAX_REQUEST_ID_HISTORY);
    assert!(!connection.terminal.contains_key(&1));
    assert_eq!(
        connection
            .terminal
            .get(&2)
            .map(|(request_id, _, _)| request_id),
        Some(&InvocationId::from_bytes([2; 16])),
    );

    let mut retained_duplicate = resource_request_fixture();
    retained_duplicate.stream_id = MAX_REQUEST_ID_HISTORY as u64 + 2;
    retained_duplicate.request_id = InvocationId::from_bytes([2; 16]);
    assert_eq!(
        connection.open(retained_duplicate),
        Err(ResourceConnectionError::DuplicateRequestId {
            request_id: InvocationId::from_bytes([2; 16]),
        }),
    );

    assert_eq!(
        connection.apply(ResourceServerFrame::Failed(ResourceFailed {
            stream_id: 1,
            request_id: InvocationId::from_bytes([1; 16]),
            target_revision: resource_revision_fixture(),
            failure: CallFailure::InternalFailure,
        })),
        Err(ResourceConnectionError::UnknownStream { stream_id: 1 }),
    );

    let mut evicted = resource_request_fixture();
    evicted.stream_id = MAX_REQUEST_ID_HISTORY as u64 + 3;
    evicted.request_id = InvocationId::from_bytes([1; 16]);
    assert_eq!(
        connection.open(evicted.clone()),
        Ok(ResourceFrameDisposition::Applied),
    );
    assert_eq!(connection.terminal.len(), MAX_REQUEST_ID_HISTORY);

    let mut active_duplicate = evicted;
    active_duplicate.stream_id += 1;
    let before_active_duplicate = connection.clone();
    assert_eq!(
        connection.open(active_duplicate),
        Err(ResourceConnectionError::DuplicateRequestId {
            request_id: InvocationId::from_bytes([1; 16]),
        }),
    );
    assert_eq!(connection, before_active_duplicate);
}

#[test]
fn resource_connection_rejects_duplicate_batch_sequence_without_mutating_state() {
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 2;
    request.byte_window = 2;
    let request_id = request.request_id;
    let stream_id = request.stream_id;
    let target_revision = request.target_revision;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x91; 16]),
            target_revision,
            resource_kind: ResourceKind::Stream,
        }))
        .unwrap();
    connection
        .apply(ResourceServerFrame::Values(ResourceValues {
            stream_id,
            request_id,
            target_revision,
            batch_sequence: 0,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(7)],
        }))
        .unwrap();
    let before = connection.clone();
    let credit_before = connection.resource_credit(stream_id, request_id).unwrap();

    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id,
            request_id,
            target_revision,
            batch_sequence: 0,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(8)],
        })),
        Err(ResourceConnectionError::BatchSequenceMismatch {
            stream_id,
            expected: 1,
            actual: 0,
        })
    );
    assert_eq!(connection, before);
    assert_eq!(
        connection.resource_credit(stream_id, request_id),
        Ok(credit_before)
    );
}

#[test]
fn resource_connection_rejects_skipped_batch_sequence_without_mutating_state() {
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 2;
    request.byte_window = 2;
    let request_id = request.request_id;
    let stream_id = request.stream_id;
    let target_revision = request.target_revision;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x92; 16]),
            target_revision,
            resource_kind: ResourceKind::Stream,
        }))
        .unwrap();
    connection
        .apply(ResourceServerFrame::Values(ResourceValues {
            stream_id,
            request_id,
            target_revision,
            batch_sequence: 0,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(7)],
        }))
        .unwrap();
    let before = connection.clone();
    let credit_before = connection.resource_credit(stream_id, request_id).unwrap();

    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id,
            request_id,
            target_revision,
            batch_sequence: 2,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(8)],
        })),
        Err(ResourceConnectionError::BatchSequenceMismatch {
            stream_id,
            expected: 1,
            actual: 2,
        })
    );
    assert_eq!(connection, before);
    assert_eq!(
        connection.resource_credit(stream_id, request_id),
        Ok(credit_before)
    );
}

#[test]
fn resource_connection_accepts_max_batch_sequence_once_and_completes() {
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 2;
    request.byte_window = 2;
    let request_id = request.request_id;
    let stream_id = request.stream_id;
    let target_revision = request.target_revision;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x95; 16]),
            target_revision,
            resource_kind: ResourceKind::Stream,
        }))
        .unwrap();
    connection
        .streams
        .get_mut(&stream_id)
        .expect("accepted resource state")
        .next_batch_sequence = u64::MAX;

    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id,
            request_id,
            target_revision,
            batch_sequence: u64::MAX,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(7)],
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    let state = connection
        .streams
        .get(&stream_id)
        .expect("max-sequence resource remains live");
    assert_eq!(state.next_batch_sequence, u64::MAX);
    assert_eq!(state.last_batch_sequence, Some(u64::MAX));
    assert_eq!(state.total_items, 1);
    assert_eq!(
        connection.resource_credit(stream_id, request_id),
        Ok(ResourceCredit {
            item_available: 1,
            byte_available: 1,
        })
    );

    assert_eq!(
        connection.apply(ResourceServerFrame::Completed(ResourceCompleted {
            stream_id,
            request_id,
            target_revision,
            final_batch_sequence: u64::MAX,
            total_items: 1,
        })),
        Ok(ResourceFrameDisposition::Applied)
    );
    assert_eq!(connection.live_resources(), 0);
}

#[test]
fn resource_connection_rejects_batch_after_max_and_terminal_mismatch_without_mutating_state() {
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 2;
    request.byte_window = 2;
    let request_id = request.request_id;
    let stream_id = request.stream_id;
    let target_revision = request.target_revision;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x96; 16]),
            target_revision,
            resource_kind: ResourceKind::Stream,
        }))
        .unwrap();
    connection
        .streams
        .get_mut(&stream_id)
        .expect("accepted resource state")
        .next_batch_sequence = u64::MAX;
    connection
        .apply(ResourceServerFrame::Values(ResourceValues {
            stream_id,
            request_id,
            target_revision,
            batch_sequence: u64::MAX,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(7)],
        }))
        .unwrap();

    let before = connection.clone();
    let credit_before = connection.resource_credit(stream_id, request_id).unwrap();
    assert_eq!(
        connection.apply(ResourceServerFrame::Values(ResourceValues {
            stream_id,
            request_id,
            target_revision,
            batch_sequence: u64::MAX,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(8)],
        })),
        Err(ResourceConnectionError::SequenceExhausted { stream_id })
    );
    assert_eq!(connection, before);
    assert_eq!(
        connection.resource_credit(stream_id, request_id),
        Ok(credit_before)
    );

    assert_eq!(
        connection.apply(ResourceServerFrame::Completed(ResourceCompleted {
            stream_id,
            request_id,
            target_revision,
            final_batch_sequence: u64::MAX - 1,
            total_items: 1,
        })),
        Err(ResourceConnectionError::BatchSequenceMismatch {
            stream_id,
            expected: u64::MAX,
            actual: u64::MAX - 1,
        })
    );
    assert_eq!(connection, before);
    assert_eq!(
        connection.resource_credit(stream_id, request_id),
        Ok(credit_before)
    );
}

#[test]
fn resource_connection_rejects_terminal_sequence_mismatch_without_mutating_state() {
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 2;
    request.byte_window = 2;
    let request_id = request.request_id;
    let stream_id = request.stream_id;
    let target_revision = request.target_revision;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x93; 16]),
            target_revision,
            resource_kind: ResourceKind::Stream,
        }))
        .unwrap();
    connection
        .apply(ResourceServerFrame::Values(ResourceValues {
            stream_id,
            request_id,
            target_revision,
            batch_sequence: 0,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(7)],
        }))
        .unwrap();
    let before = connection.clone();
    let credit_before = connection.resource_credit(stream_id, request_id).unwrap();

    assert_eq!(
        connection.apply(ResourceServerFrame::Completed(ResourceCompleted {
            stream_id,
            request_id,
            target_revision,
            final_batch_sequence: 1,
            total_items: 1,
        })),
        Err(ResourceConnectionError::BatchSequenceMismatch {
            stream_id,
            expected: 0,
            actual: 1,
        })
    );
    assert_eq!(connection, before);
    assert_eq!(
        connection.resource_credit(stream_id, request_id),
        Ok(credit_before)
    );
}

#[test]
fn resource_connection_rejects_terminal_total_mismatch_without_mutating_state() {
    let mut request = resource_request_fixture();
    request.resource_kind = ResourceKind::Stream;
    request.item_window = 2;
    request.byte_window = 2;
    let request_id = request.request_id;
    let stream_id = request.stream_id;
    let target_revision = request.target_revision;
    let mut connection = ResourceProtocolConnection::new();
    connection.open(request).unwrap();
    connection
        .apply(ResourceServerFrame::Accepted(ResourceAccepted {
            stream_id,
            request_id,
            nested_invocation_id: InvocationId::from_bytes([0x94; 16]),
            target_revision,
            resource_kind: ResourceKind::Stream,
        }))
        .unwrap();
    connection
        .apply(ResourceServerFrame::Values(ResourceValues {
            stream_id,
            request_id,
            target_revision,
            batch_sequence: 0,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(7)],
        }))
        .unwrap();
    let before = connection.clone();
    let credit_before = connection.resource_credit(stream_id, request_id).unwrap();

    assert_eq!(
        connection.apply(ResourceServerFrame::Completed(ResourceCompleted {
            stream_id,
            request_id,
            target_revision,
            final_batch_sequence: 0,
            total_items: 2,
        })),
        Err(ResourceConnectionError::ResourceTotalMismatch {
            stream_id,
            expected: 1,
            actual: 2,
        })
    );
    assert_eq!(connection, before);
    assert_eq!(
        connection.resource_credit(stream_id, request_id),
        Ok(credit_before)
    );
}
