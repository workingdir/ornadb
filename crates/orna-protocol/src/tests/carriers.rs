use super::*;
#[test]
fn invocation_carriers_have_independent_exact_orv5_goldens_and_round_trip() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let [value, request, event] = carrier_test_values();

    let inner = orv5_integer(7);
    let mut value_payload = vec![1];
    value_payload.extend_from_slice(&(inner.len() as u32).to_be_bytes());
    value_payload.extend_from_slice(&inner);
    let mut expected_value = b"ORV5".to_vec();
    expected_value.push(0x0c);
    expected_value.extend_from_slice(&SYS_INVOKE_VALUE_TYPE_ID.to_bytes());
    expected_value.extend_from_slice(&(value_payload.len() as u32).to_be_bytes());
    expected_value.extend_from_slice(&value_payload);

    let mut request_payload = vec![1, 0];
    request_payload.extend_from_slice(&[0x11; 16]);
    request_payload.extend_from_slice(&0_u32.to_be_bytes());
    request_payload.extend_from_slice(&[3, 0, 0, 0]);
    request_payload.extend_from_slice(&5_u32.to_be_bytes());
    request_payload.extend_from_slice(b"en-GB");
    request_payload.extend_from_slice(&3_u32.to_be_bytes());
    request_payload.extend_from_slice(b"UTC");
    request_payload.push(0);
    request_payload.extend_from_slice(&5_u16.to_be_bytes());
    request_payload.extend_from_slice(&5_u32.to_be_bytes());
    request_payload.extend_from_slice(b"en-GB");
    request_payload.extend_from_slice(&3_u32.to_be_bytes());
    request_payload.extend_from_slice(b"UTC");
    request_payload.extend_from_slice(&0_u32.to_be_bytes());
    request_payload.extend_from_slice(&0_u32.to_be_bytes());
    request_payload.extend_from_slice(&1_024_u32.to_be_bytes());
    request_payload.extend_from_slice(&0_u64.to_be_bytes());
    request_payload.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut expected_request = b"ORV5".to_vec();
    expected_request.push(0x0c);
    expected_request.extend_from_slice(&SYS_INVOKE_REQUEST_TYPE_ID.to_bytes());
    expected_request.extend_from_slice(&(request_payload.len() as u32).to_be_bytes());
    expected_request.extend_from_slice(&request_payload);

    let mut event_payload = vec![1, 3];
    event_payload.extend_from_slice(&[0x22; 16]);
    event_payload.extend_from_slice(&u64::MAX.to_be_bytes());
    event_payload.extend_from_slice(&99_u64.to_be_bytes());
    let mut expected_event = b"ORV5".to_vec();
    expected_event.push(0x0c);
    expected_event.extend_from_slice(&SYS_INVOKE_EVENT_TYPE_ID.to_bytes());
    expected_event.extend_from_slice(&(event_payload.len() as u32).to_be_bytes());
    expected_event.extend_from_slice(&event_payload);

    for (carrier, expected) in [
        (value, expected_value),
        (request, expected_request),
        (event, expected_event),
    ] {
        assert_eq!(
            encode_constructed_value(&active, &registry, &carrier),
            Ok(expected.clone())
        );
        assert_eq!(
            decode_constructed_value(&active, &registry, &expected),
            Ok(carrier.clone())
        );
        assert_eq!(
            encode_value(&carrier),
            Err(ValueCodecError::UnsupportedValue)
        );
        assert_eq!(
            encode_registered_value(&active, &registry, &carrier),
            Err(ValueCodecError::UnsupportedValue)
        );
    }
}

#[test]
fn request_offer_permutations_encode_identically_and_keep_original_duplicate_indexes() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let descriptor_a = TypeDescriptor::named(BOOLEAN_TYPE_ID);
    let descriptor_b = TypeDescriptor::named(INTEGER_TYPE_ID);
    let sink_a = InvocationSinkOffer::new(
        descriptor_a.clone(),
        ["text/plain", "application/json"],
        true,
        -1,
        None,
    )
    .unwrap();
    let sink_a_permuted = InvocationSinkOffer::new(
        descriptor_a.clone(),
        ["application/json", "text/plain"],
        true,
        -1,
        None,
    )
    .unwrap();
    let sink_b = InvocationSinkOffer::new(
        descriptor_b.clone(),
        ["application/octet-stream"],
        false,
        2,
        None,
    )
    .unwrap();
    let contract = InvocationRuntimeContract::new("render", "1", ["z", "a"]).unwrap();
    let contract_permuted = InvocationRuntimeContract::new("render", "1", ["a", "z"]).unwrap();
    let runtime = InvocationRuntimeOffer::new(
        "wasm",
        "2",
        [descriptor_b.clone(), descriptor_a.clone()],
        [contract],
        0,
        false,
        None,
    )
    .unwrap();
    let runtime_permuted = InvocationRuntimeOffer::new(
        "wasm",
        "2",
        [descriptor_a.clone(), descriptor_b.clone()],
        [contract_permuted],
        0,
        false,
        None,
    )
    .unwrap();
    let left = RuntimeValue::InvokeRequest(minimal_invocation_request(
        vec![sink_a, sink_b.clone()],
        vec![runtime],
    ));
    let right = RuntimeValue::InvokeRequest(minimal_invocation_request(
        vec![sink_b, sink_a_permuted],
        vec![runtime_permuted],
    ));
    let encoded = encode_constructed_value(&active, &registry, &left).unwrap();
    assert_eq!(
        encode_constructed_value(&active, &registry, &right),
        Ok(encoded.clone())
    );
    let decoded = decode_constructed_value(&active, &registry, &encoded).unwrap();
    assert_eq!(
        encode_constructed_value(&active, &registry, &decoded),
        Ok(encoded)
    );

    let duplicate_media =
        InvocationSinkOffer::new(descriptor_a, ["text/plain", "text/plain"], false, 0, None)
            .unwrap();
    let duplicate = RuntimeValue::InvokeRequest(minimal_invocation_request(
        vec![duplicate_media],
        Vec::new(),
    ));
    assert_eq!(
        encode_constructed_value(&active, &registry, &duplicate),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
            source: InvocationCarrierCodecError::DuplicateItem {
                path: InvocationCarrierPath {
                    segments: vec![
                        InvocationCarrierPathSegment::RequestClientOffer,
                        InvocationCarrierPathSegment::ClientSinks,
                        InvocationCarrierPathSegment::Sink(0),
                        InvocationCarrierPathSegment::MediaTypes,
                    ],
                },
                first: 0,
                duplicate: 1,
            },
        })
    );
}

#[test]
fn runtime_and_contract_offers_are_canonical_on_encode_and_decode() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let complete_sink = InvocationSinkOffer::new(
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
        ["text/plain"],
        false,
        0,
        None,
    )
    .unwrap();
    let duplicate_sink = RuntimeValue::InvokeRequest(minimal_invocation_request(
        vec![complete_sink.clone(), complete_sink],
        Vec::new(),
    ));
    assert_eq!(
        encode_constructed_value(&active, &registry, &duplicate_sink),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
            source: InvocationCarrierCodecError::DuplicateItem {
                path: InvocationCarrierPath {
                    segments: vec![
                        InvocationCarrierPathSegment::RequestClientOffer,
                        InvocationCarrierPathSegment::ClientSinks,
                    ],
                },
                first: 0,
                duplicate: 1,
            },
        })
    );
    let runtime_a = InvocationRuntimeOffer::new(
        "alpha",
        "1",
        Vec::<TypeDescriptor>::new(),
        Vec::<InvocationRuntimeContract>::new(),
        0,
        false,
        None,
    )
    .unwrap();
    let runtime_b = InvocationRuntimeOffer::new(
        "beta",
        "1",
        Vec::<TypeDescriptor>::new(),
        Vec::<InvocationRuntimeContract>::new(),
        0,
        false,
        None,
    )
    .unwrap();
    let canonical = RuntimeValue::InvokeRequest(minimal_invocation_request(
        Vec::new(),
        vec![runtime_a.clone(), runtime_b.clone()],
    ));
    let permuted = RuntimeValue::InvokeRequest(minimal_invocation_request(
        Vec::new(),
        vec![runtime_b, runtime_a.clone()],
    ));
    assert_eq!(
        encode_constructed_value(&active, &registry, &canonical),
        encode_constructed_value(&active, &registry, &permuted),
    );
    let duplicate_runtime = RuntimeValue::InvokeRequest(minimal_invocation_request(
        Vec::new(),
        vec![runtime_a.clone(), runtime_a],
    ));
    assert_eq!(
        encode_constructed_value(&active, &registry, &duplicate_runtime),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
            source: InvocationCarrierCodecError::DuplicateItem {
                path: InvocationCarrierPath {
                    segments: vec![
                        InvocationCarrierPathSegment::RequestClientOffer,
                        InvocationCarrierPathSegment::ClientRuntimes,
                    ],
                },
                first: 0,
                duplicate: 1,
            },
        })
    );

    let contract_a = InvocationRuntimeContract::new("alpha", "1", Vec::<String>::new()).unwrap();
    let contract_b = InvocationRuntimeContract::new("beta", "1", Vec::<String>::new()).unwrap();
    let with_canonical_contracts = RuntimeValue::InvokeRequest(minimal_invocation_request(
        Vec::new(),
        vec![
            InvocationRuntimeOffer::new(
                "runtime",
                "1",
                Vec::<TypeDescriptor>::new(),
                vec![contract_a.clone(), contract_b.clone()],
                0,
                false,
                None,
            )
            .unwrap(),
        ],
    ));
    let with_permuted_contracts = RuntimeValue::InvokeRequest(minimal_invocation_request(
        Vec::new(),
        vec![
            InvocationRuntimeOffer::new(
                "runtime",
                "1",
                Vec::<TypeDescriptor>::new(),
                vec![contract_b, contract_a.clone()],
                0,
                false,
                None,
            )
            .unwrap(),
        ],
    ));
    assert_eq!(
        encode_constructed_value(&active, &registry, &with_canonical_contracts),
        encode_constructed_value(&active, &registry, &with_permuted_contracts),
    );
    let duplicate_contract = RuntimeValue::InvokeRequest(minimal_invocation_request(
        Vec::new(),
        vec![
            InvocationRuntimeOffer::new(
                "runtime",
                "1",
                Vec::<TypeDescriptor>::new(),
                vec![contract_a.clone(), contract_a],
                0,
                false,
                None,
            )
            .unwrap(),
        ],
    ));
    assert_eq!(
        encode_constructed_value(&active, &registry, &duplicate_contract),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
            source: InvocationCarrierCodecError::DuplicateItem {
                path: InvocationCarrierPath {
                    segments: vec![
                        InvocationCarrierPathSegment::RequestClientOffer,
                        InvocationCarrierPathSegment::ClientRuntimes,
                        InvocationCarrierPathSegment::Runtime(0),
                        InvocationCarrierPathSegment::Contracts,
                    ],
                },
                first: 0,
                duplicate: 1,
            },
        })
    );

    let consumed_left = RuntimeValue::InvokeRequest(minimal_invocation_request(
        Vec::new(),
        vec![
            InvocationRuntimeOffer::new(
                "types",
                "1",
                [
                    TypeDescriptor::named(INTEGER_TYPE_ID),
                    TypeDescriptor::named(BOOLEAN_TYPE_ID),
                ],
                Vec::<InvocationRuntimeContract>::new(),
                0,
                false,
                None,
            )
            .unwrap(),
        ],
    ));
    let consumed_right = RuntimeValue::InvokeRequest(minimal_invocation_request(
        Vec::new(),
        vec![
            InvocationRuntimeOffer::new(
                "types",
                "1",
                [
                    TypeDescriptor::named(BOOLEAN_TYPE_ID),
                    TypeDescriptor::named(INTEGER_TYPE_ID),
                ],
                Vec::<InvocationRuntimeContract>::new(),
                0,
                false,
                None,
            )
            .unwrap(),
        ],
    ));
    assert_eq!(
        encode_constructed_value(&active, &registry, &consumed_left),
        encode_constructed_value(&active, &registry, &consumed_right),
    );
    let duplicate_consumed = RuntimeValue::InvokeRequest(minimal_invocation_request(
        Vec::new(),
        vec![
            InvocationRuntimeOffer::new(
                "types",
                "1",
                [
                    TypeDescriptor::named(BOOLEAN_TYPE_ID),
                    TypeDescriptor::named(BOOLEAN_TYPE_ID),
                ],
                Vec::<InvocationRuntimeContract>::new(),
                0,
                false,
                None,
            )
            .unwrap(),
        ],
    ));
    assert_eq!(
        encode_constructed_value(&active, &registry, &duplicate_consumed),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
            source: InvocationCarrierCodecError::DuplicateItem {
                path: InvocationCarrierPath {
                    segments: vec![
                        InvocationCarrierPathSegment::RequestClientOffer,
                        InvocationCarrierPathSegment::ClientRuntimes,
                        InvocationCarrierPathSegment::Runtime(0),
                        InvocationCarrierPathSegment::ConsumedTypes,
                    ],
                },
                first: 0,
                duplicate: 1,
            },
        })
    );

    let features_left = RuntimeValue::InvokeRequest(minimal_invocation_request(
        Vec::new(),
        vec![
            InvocationRuntimeOffer::new(
                "features",
                "1",
                Vec::<TypeDescriptor>::new(),
                vec![InvocationRuntimeContract::new("contract", "1", ["beta", "alpha"]).unwrap()],
                0,
                false,
                None,
            )
            .unwrap(),
        ],
    ));
    let features_right = RuntimeValue::InvokeRequest(minimal_invocation_request(
        Vec::new(),
        vec![
            InvocationRuntimeOffer::new(
                "features",
                "1",
                Vec::<TypeDescriptor>::new(),
                vec![InvocationRuntimeContract::new("contract", "1", ["alpha", "beta"]).unwrap()],
                0,
                false,
                None,
            )
            .unwrap(),
        ],
    ));
    assert_eq!(
        encode_constructed_value(&active, &registry, &features_left),
        encode_constructed_value(&active, &registry, &features_right),
    );
    let duplicate_features = RuntimeValue::InvokeRequest(minimal_invocation_request(
        Vec::new(),
        vec![
            InvocationRuntimeOffer::new(
                "features",
                "1",
                Vec::<TypeDescriptor>::new(),
                vec![InvocationRuntimeContract::new("contract", "1", ["alpha", "alpha"]).unwrap()],
                0,
                false,
                None,
            )
            .unwrap(),
        ],
    ));
    assert_eq!(
        encode_constructed_value(&active, &registry, &duplicate_features),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
            source: InvocationCarrierCodecError::DuplicateItem {
                path: InvocationCarrierPath {
                    segments: vec![
                        InvocationCarrierPathSegment::RequestClientOffer,
                        InvocationCarrierPathSegment::ClientRuntimes,
                        InvocationCarrierPathSegment::Runtime(0),
                        InvocationCarrierPathSegment::Contracts,
                        InvocationCarrierPathSegment::Contract(0),
                        InvocationCarrierPathSegment::Features,
                    ],
                },
                first: 0,
                duplicate: 1,
            },
        })
    );
}

#[test]
fn public_offer_baselines_reject_noncanonical_and_duplicate_wire_items() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let assert_request_error = |wire: Vec<u8>, source: InvocationCarrierCodecError| {
        assert_eq!(
            decode_constructed_value(&active, &registry, &wire),
            Err(ValueCodecError::InvocationCarrier {
                carrier: SYS_INVOKE_REQUEST_TYPE_ID,
                source,
            })
        );
    };
    let sinks_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
        .with(InvocationCarrierPathSegment::ClientSinks);
    let sink_a = InvocationSinkOffer::new(
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
        ["aaaa"],
        false,
        0,
        None,
    )
    .unwrap();
    let sink_z = InvocationSinkOffer::new(
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
        ["zzzz"],
        false,
        0,
        None,
    )
    .unwrap();
    let sink_baseline = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeRequest(minimal_invocation_request(vec![sink_z, sink_a], Vec::new())),
    )
    .unwrap();
    let mut sink_order = sink_baseline.clone();
    let first_sink = sink_order[90..127].to_vec();
    sink_order.copy_within(127..164, 90);
    sink_order[127..164].copy_from_slice(&first_sink);
    assert_request_error(
        sink_order,
        InvocationCarrierCodecError::NonCanonicalOrder {
            path: sinks_path.clone(),
            index: 1,
        },
    );
    let mut sink_duplicate = sink_baseline;
    let first_sink = sink_duplicate[90..127].to_vec();
    sink_duplicate[127..164].copy_from_slice(&first_sink);
    assert_request_error(
        sink_duplicate,
        InvocationCarrierCodecError::DuplicateItem {
            path: sinks_path.clone(),
            first: 0,
            duplicate: 1,
        },
    );

    let media_path = sinks_path
        .clone()
        .with(InvocationCarrierPathSegment::Sink(0))
        .with(InvocationCarrierPathSegment::MediaTypes);
    let media_baseline = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeRequest(minimal_invocation_request(
            vec![
                InvocationSinkOffer::new(
                    TypeDescriptor::named(BOOLEAN_TYPE_ID),
                    ["zzzz", "aaaa"],
                    false,
                    0,
                    None,
                )
                .unwrap(),
            ],
            Vec::new(),
        )),
    )
    .unwrap();
    let mut media_order = media_baseline.clone();
    let first_media = media_order[113..121].to_vec();
    media_order.copy_within(121..129, 113);
    media_order[121..129].copy_from_slice(&first_media);
    assert_request_error(
        media_order,
        InvocationCarrierCodecError::NonCanonicalOrder {
            path: media_path.clone(),
            index: 1,
        },
    );
    let mut media_duplicate = media_baseline;
    let first_media = media_duplicate[113..121].to_vec();
    media_duplicate[121..129].copy_from_slice(&first_media);
    assert_request_error(
        media_duplicate,
        InvocationCarrierCodecError::DuplicateItem {
            path: media_path,
            first: 0,
            duplicate: 1,
        },
    );

    let runtime_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
        .with(InvocationCarrierPathSegment::ClientRuntimes);
    let runtime_a = InvocationRuntimeOffer::new(
        "aaaa",
        "1",
        Vec::<TypeDescriptor>::new(),
        Vec::<InvocationRuntimeContract>::new(),
        0,
        false,
        None,
    )
    .unwrap();
    let runtime_z = InvocationRuntimeOffer::new(
        "zzzz",
        "1",
        Vec::<TypeDescriptor>::new(),
        Vec::<InvocationRuntimeContract>::new(),
        0,
        false,
        None,
    )
    .unwrap();
    let runtime_baseline = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeRequest(minimal_invocation_request(
            Vec::new(),
            vec![runtime_z, runtime_a],
        )),
    )
    .unwrap();
    let mut runtime_order = runtime_baseline.clone();
    let first_runtime = runtime_order[94..121].to_vec();
    runtime_order.copy_within(121..148, 94);
    runtime_order[121..148].copy_from_slice(&first_runtime);
    assert_request_error(
        runtime_order,
        InvocationCarrierCodecError::NonCanonicalOrder {
            path: runtime_path.clone(),
            index: 1,
        },
    );
    let mut runtime_duplicate = runtime_baseline;
    let first_runtime = runtime_duplicate[94..121].to_vec();
    runtime_duplicate[121..148].copy_from_slice(&first_runtime);
    assert_request_error(
        runtime_duplicate,
        InvocationCarrierCodecError::DuplicateItem {
            path: runtime_path.clone(),
            first: 0,
            duplicate: 1,
        },
    );

    let contracts_path = runtime_path
        .clone()
        .with(InvocationCarrierPathSegment::Runtime(0))
        .with(InvocationCarrierPathSegment::Contracts);
    let contract_baseline = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeRequest(minimal_invocation_request(
            Vec::new(),
            vec![
                InvocationRuntimeOffer::new(
                    "runt",
                    "1",
                    Vec::<TypeDescriptor>::new(),
                    vec![
                        InvocationRuntimeContract::new("zzzz", "1", Vec::<String>::new()).unwrap(),
                        InvocationRuntimeContract::new("aaaa", "1", Vec::<String>::new()).unwrap(),
                    ],
                    0,
                    false,
                    None,
                )
                .unwrap(),
            ],
        )),
    )
    .unwrap();
    let mut contract_order = contract_baseline.clone();
    let first_contract = contract_order[115..132].to_vec();
    contract_order.copy_within(132..149, 115);
    contract_order[132..149].copy_from_slice(&first_contract);
    assert_request_error(
        contract_order,
        InvocationCarrierCodecError::NonCanonicalOrder {
            path: contracts_path.clone(),
            index: 1,
        },
    );
    let mut contract_duplicate = contract_baseline;
    let first_contract = contract_duplicate[115..132].to_vec();
    contract_duplicate[132..149].copy_from_slice(&first_contract);
    assert_request_error(
        contract_duplicate,
        InvocationCarrierCodecError::DuplicateItem {
            path: contracts_path,
            first: 0,
            duplicate: 1,
        },
    );

    let consumed_path = runtime_path
        .clone()
        .with(InvocationCarrierPathSegment::Runtime(0))
        .with(InvocationCarrierPathSegment::ConsumedTypes);
    let consumed_baseline = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeRequest(minimal_invocation_request(
            Vec::new(),
            vec![
                InvocationRuntimeOffer::new(
                    "runt",
                    "1",
                    [
                        TypeDescriptor::named(BOOLEAN_TYPE_ID),
                        TypeDescriptor::named(INTEGER_TYPE_ID),
                    ],
                    Vec::<InvocationRuntimeContract>::new(),
                    0,
                    false,
                    None,
                )
                .unwrap(),
            ],
        )),
    )
    .unwrap();
    let mut consumed_order = consumed_baseline.clone();
    let first_descriptor = consumed_order[111..130].to_vec();
    consumed_order.copy_within(130..149, 111);
    consumed_order[130..149].copy_from_slice(&first_descriptor);
    assert_request_error(
        consumed_order,
        InvocationCarrierCodecError::NonCanonicalOrder {
            path: consumed_path.clone(),
            index: 1,
        },
    );
    let mut consumed_duplicate = consumed_baseline;
    let first_descriptor = consumed_duplicate[111..130].to_vec();
    consumed_duplicate[130..149].copy_from_slice(&first_descriptor);
    assert_request_error(
        consumed_duplicate,
        InvocationCarrierCodecError::DuplicateItem {
            path: consumed_path,
            first: 0,
            duplicate: 1,
        },
    );

    let features_path = runtime_path
        .with(InvocationCarrierPathSegment::Runtime(0))
        .with(InvocationCarrierPathSegment::Contracts)
        .with(InvocationCarrierPathSegment::Contract(0))
        .with(InvocationCarrierPathSegment::Features);
    let features_baseline = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeRequest(minimal_invocation_request(
            Vec::new(),
            vec![
                InvocationRuntimeOffer::new(
                    "runt",
                    "1",
                    Vec::<TypeDescriptor>::new(),
                    vec![InvocationRuntimeContract::new("runt", "1", ["zzzz", "aaaa"]).unwrap()],
                    0,
                    false,
                    None,
                )
                .unwrap(),
            ],
        )),
    )
    .unwrap();
    let mut features_order = features_baseline.clone();
    let first_feature = features_order[132..140].to_vec();
    features_order.copy_within(140..148, 132);
    features_order[140..148].copy_from_slice(&first_feature);
    assert_request_error(
        features_order,
        InvocationCarrierCodecError::NonCanonicalOrder {
            path: features_path.clone(),
            index: 1,
        },
    );
    let mut features_duplicate = features_baseline;
    let first_feature = features_duplicate[132..140].to_vec();
    features_duplicate[140..148].copy_from_slice(&first_feature);
    assert_request_error(
        features_duplicate,
        InvocationCarrierCodecError::DuplicateItem {
            path: features_path,
            first: 0,
            duplicate: 1,
        },
    );
}

#[test]
fn orf5_rejects_all_carriers_in_both_ordinary_positions_without_state_or_credit_change() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let function = FunctionId::from_bytes([0x31; 16]);
    let parameter = ParameterId::from_bytes([0x32; 16]);

    for carrier in carrier_test_values() {
        let carrier_id = invocation_carrier_type_id(&carrier).unwrap();
        let rejection = FrameCodecError::InvocationCarrierNotAccepted {
            carrier: carrier_id,
        };
        let frame = ClientFrame::CallArgument {
            stream: 1,
            parameter,
            value: carrier.clone(),
        };
        assert_eq!(
            encode_constructed_client_frame(&active, &registry, &frame),
            Err(rejection.clone())
        );
        let encoded_value = encode_constructed_value(&active, &registry, &carrier).unwrap();
        let mut argument_payload = parameter.to_bytes().to_vec();
        argument_payload.extend_from_slice(&encoded_value);
        assert_eq!(
            decode_constructed_client_frame(
                &active,
                &registry,
                &orf5_frame(0x02, 1, &argument_payload),
            ),
            Err(rejection.clone())
        );

        let event = ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(carrier.clone()),
            }],
        };
        if matches!(&carrier, RuntimeValue::InvokeEvent(_)) {
            assert!(encode_constructed_server_frame(&active, &registry, &event).is_ok());
        } else {
            assert_eq!(
                encode_constructed_server_frame(&active, &registry, &event),
                Err(rejection.clone())
            );
        }
        let mut event_payload = vec![1];
        event_payload.extend_from_slice(&1_u16.to_be_bytes());
        event_payload.extend_from_slice(&1_u64.to_be_bytes());
        event_payload.push(1);
        event_payload.extend_from_slice(&(encoded_value.len() as u32).to_be_bytes());
        event_payload.extend_from_slice(&encoded_value);
        assert_eq!(
            decode_constructed_server_frame(
                &active,
                &registry,
                &orf5_frame(0x82, 1, &event_payload),
            ),
            Err(rejection.clone())
        );

        let mut connection = ProtocolConnection::new();
        connection
            .receive_constructed(
                &active,
                &registry,
                ClientFrame::CallRawStart {
                    stream: 1,
                    function,
                },
            )
            .unwrap();
        let before_argument = connection.clone();
        assert_eq!(
            connection.receive_constructed(&active, &registry, frame),
            Err(ConnectionError::InvalidFrame {
                source: rejection.clone(),
            })
        );
        assert_eq!(connection, before_argument);
        connection
            .receive_constructed(
                &active,
                &registry,
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 4_096,
                },
            )
            .unwrap();
        connection
            .receive_constructed(
                &active,
                &registry,
                ClientFrame::CallArgumentsComplete { stream: 1 },
            )
            .unwrap();
        connection
            .apply_constructed(
                &active,
                &registry,
                ServerAction::Accepted {
                    stream: 1,
                    invocation: InvocationId::from_bytes([0x33; 16]),
                },
            )
            .unwrap();
        let before_event = connection.clone();
        assert_eq!(
            connection.apply_constructed(
                &active,
                &registry,
                ServerAction::Events {
                    stream: 1,
                    events: vec![Event::Value(carrier)],
                },
            ),
            Err(ConnectionError::InvalidFrame { source: rejection })
        );
        assert_eq!(connection, before_event);
    }
}

#[test]
fn carrier_aggregate_preflight_accepts_65536_and_precedes_later_inner_materialisation() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let descriptor = TypeDescriptor::list(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
    let at_limit_list = RuntimeValue::list(
        &active,
        descriptor.clone(),
        vec![RuntimeValue::Boolean(true); 65_531],
    )
    .unwrap();
    let at_limit_value = InvokeValue::new(at_limit_list).unwrap();
    let scalar_value = InvokeValue::new(RuntimeValue::Integer(1)).unwrap();
    let mut at_limit_input = minimal_invocation_request(Vec::new(), Vec::new()).into_input();
    at_limit_input.arguments = vec![
        InvocationArgument::new(
            InvocationParameterSelector::parameter_id(ParameterId::from_bytes([0x40; 16])),
            at_limit_value,
        ),
        InvocationArgument::new(
            InvocationParameterSelector::parameter_id(ParameterId::from_bytes([0x41; 16])),
            scalar_value,
        ),
    ];
    let at_limit = RuntimeValue::InvokeRequest(InvokeRequest::new(at_limit_input).unwrap());
    let encoded_at_limit = encode_constructed_value(&active, &registry, &at_limit).unwrap();
    assert_eq!(
        decode_constructed_value(&active, &registry, &encoded_at_limit),
        Ok(at_limit)
    );

    let over_limit_list = RuntimeValue::list(
        &active,
        descriptor,
        vec![RuntimeValue::Boolean(true); 65_532],
    )
    .unwrap();
    let over_limit_value = RuntimeValue::InvokeValue(InvokeValue::new(over_limit_list).unwrap());
    let scalar = RuntimeValue::InvokeValue(InvokeValue::new(RuntimeValue::Integer(1)).unwrap());
    let over_limit_envelope =
        encode_constructed_value(&active, &registry, &over_limit_value).unwrap();
    let scalar_envelope = encode_constructed_value(&active, &registry, &scalar).unwrap();
    let over_limit = raw_request_carrier(&[
        (
            ParameterId::from_bytes([0x40; 16]),
            over_limit_envelope.clone(),
        ),
        (ParameterId::from_bytes([0x41; 16]), scalar_envelope.clone()),
    ]);
    let limit_error = ValueCodecError::InvocationCarrier {
        carrier: SYS_INVOKE_REQUEST_TYPE_ID,
        source: InvocationCarrierCodecError::TooManyNodes {
            maximum: MAX_INVOCATION_CARRIER_NODES,
        },
    };
    assert_eq!(
        decode_constructed_value(&active, &registry, &over_limit),
        Err(limit_error.clone())
    );

    // Structural parsing wins before aggregate preflight, even when the
    // otherwise-later argument would take the request over its node cap.
    let mut syntax_before_limit = raw_request_payload(&[(
        ParameterId::from_bytes([0x40; 16]),
        over_limit_envelope.clone(),
    )]);
    syntax_before_limit[1] = 2;
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &syntax_before_limit,
        InvocationCarrierCodecError::UnknownDiscriminant {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTarget),
            actual: 2,
        },
    );

    let truncated_before_limit = raw_request_carrier(&[
        (ParameterId::from_bytes([0x40; 16]), Vec::new()),
        (
            ParameterId::from_bytes([0x41; 16]),
            over_limit_envelope.clone(),
        ),
    ]);
    assert!(matches!(
        decode_constructed_value(&active, &registry, &truncated_before_limit),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
            source: InvocationCarrierCodecError::Truncated { .. },
        })
    ));

    let arguments_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestArguments);
    let order_before_limit = raw_request_payload(&[
        (ParameterId::from_bytes([0x41; 16]), scalar_envelope.clone()),
        (
            ParameterId::from_bytes([0x40; 16]),
            over_limit_envelope.clone(),
        ),
    ]);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &order_before_limit,
        InvocationCarrierCodecError::NonCanonicalOrder {
            path: arguments_path.clone(),
            index: 1,
        },
    );
    let duplicate_before_limit = raw_request_payload(&[
        (ParameterId::from_bytes([0x40; 16]), scalar_envelope.clone()),
        (
            ParameterId::from_bytes([0x40; 16]),
            over_limit_envelope.clone(),
        ),
    ]);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &duplicate_before_limit,
        InvocationCarrierCodecError::DuplicateItem {
            path: arguments_path,
            first: 0,
            duplicate: 1,
        },
    );

    let mut inactive_inner = orv5_integer(1);
    inactive_inner[5..21].fill(0x7f);
    let inactive_envelope = raw_invoke_value_carrier(&inactive_inner);
    let limit_before_later_inactive = raw_request_carrier(&[
        (
            ParameterId::from_bytes([0x40; 16]),
            over_limit_envelope.clone(),
        ),
        (ParameterId::from_bytes([0x41; 16]), inactive_envelope),
    ]);
    assert_eq!(
        decode_constructed_value(&active, &registry, &limit_before_later_inactive),
        Err(limit_error.clone())
    );

    let mut later_wrong_marker = orv5_integer(1);
    later_wrong_marker[..4].copy_from_slice(b"ORV4");
    let definitely_over_limit_list = RuntimeValue::list(
        &active,
        TypeDescriptor::list(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap(),
        vec![RuntimeValue::Boolean(true); 65_534],
    )
    .unwrap();
    let definitely_over_limit_envelope = raw_invoke_value_carrier(
        &encode_constructed_value(&active, &registry, &definitely_over_limit_list).unwrap(),
    );
    let limit_before_later_malformed = raw_request_carrier(&[
        (
            ParameterId::from_bytes([0x40; 16]),
            definitely_over_limit_envelope,
        ),
        (
            ParameterId::from_bytes([0x41; 16]),
            raw_invoke_value_carrier(&later_wrong_marker),
        ),
    ]);
    assert_eq!(
        decode_constructed_value(&active, &registry, &limit_before_later_malformed),
        Err(limit_error)
    );

    let mut wrong_marker = orv5_integer(1);
    wrong_marker[..4].copy_from_slice(b"ORV4");
    let malformed_envelope = raw_invoke_value_carrier(&wrong_marker);
    let malformed_before_limit = raw_request_carrier(&[
        (ParameterId::from_bytes([0x40; 16]), malformed_envelope),
        (ParameterId::from_bytes([0x41; 16]), over_limit_envelope),
    ]);
    assert_eq!(
        decode_constructed_value(&active, &registry, &malformed_before_limit),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
            source: InvocationCarrierCodecError::InnerValue {
                path: InvocationCarrierPath {
                    segments: vec![
                        InvocationCarrierPathSegment::RequestArguments,
                        InvocationCarrierPathSegment::Argument(0),
                        InvocationCarrierPathSegment::Value,
                    ],
                },
                source: Box::new(ValueCodecError::InvalidMarker),
            },
        })
    );

    let mut structurally_trailing = over_limit;
    structurally_trailing.push(0xff);
    let declared = u32::from_be_bytes(structurally_trailing[21..25].try_into().unwrap());
    structurally_trailing[21..25].copy_from_slice(&(declared + 1).to_be_bytes());
    assert_eq!(
        decode_constructed_value(&active, &registry, &structurally_trailing),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
            source: InvocationCarrierCodecError::Trailing { remaining: 1 },
        })
    );
}

#[test]
fn invocation_event_carrier_round_trips_every_body_and_closed_scalar() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let invocation = InvocationId::from_bytes([0x61; 16]);
    let integer = || InvokeValue::new(RuntimeValue::Integer(7)).unwrap();
    let mut events = vec![
        InvokeEvent::new(
            invocation,
            0,
            InvocationEventBody::Started {
                visible_principal: None,
            },
        )
        .unwrap(),
        InvokeEvent::new(
            invocation,
            1,
            InvocationEventBody::Started {
                visible_principal: Some(PrincipalId::from_bytes([0x62; 16])),
            },
        )
        .unwrap(),
        InvokeEvent::new(
            invocation,
            u64::MAX,
            InvocationEventBody::value_batch(Some(integer()), [integer(), integer()]).unwrap(),
        )
        .unwrap(),
        InvokeEvent::new(
            invocation,
            3,
            InvocationEventBody::Completed {
                duration_nanoseconds: u64::MAX,
            },
        )
        .unwrap(),
        InvokeEvent::new(invocation, 4, InvocationEventBody::cancelled(None).unwrap()).unwrap(),
        InvokeEvent::new(
            invocation,
            5,
            InvocationEventBody::cancelled(Some(String::from("operator stopped call"))).unwrap(),
        )
        .unwrap(),
    ];

    for severity in [
        InvocationDiagnosticSeverity::Info,
        InvocationDiagnosticSeverity::Warning,
        InvocationDiagnosticSeverity::Error,
    ] {
        events.push(
            InvokeEvent::new(
                invocation,
                events.len() as u64,
                InvocationEventBody::Diagnostic(
                    InvocationDiagnostic::new(severity, "NOTICE", "public diagnostic").unwrap(),
                ),
            )
            .unwrap(),
        );
    }
    for phase in [
        InvocationFailurePhase::Resolve,
        InvocationFailurePhase::Bind,
        InvocationFailurePhase::Authorise,
        InvocationFailurePhase::Target,
        InvocationFailurePhase::Present,
        InvocationFailurePhase::Runtime,
        InvocationFailurePhase::Transport,
        InvocationFailurePhase::Internal,
    ] {
        for retryability in [
            InvocationRetryability::Unknown,
            InvocationRetryability::No,
            InvocationRetryability::Yes,
        ] {
            events.push(
                InvokeEvent::new(
                    invocation,
                    events.len() as u64,
                    InvocationEventBody::Failed(
                        InvocationFailure::new(
                            phase,
                            "FAILED",
                            "public failure",
                            Some(integer()),
                            retryability,
                        )
                        .unwrap(),
                    ),
                )
                .unwrap(),
            );
        }
    }

    for event in events {
        let value = RuntimeValue::InvokeEvent(event.clone());
        let encoded = encode_constructed_value(&active, &registry, &value).unwrap();
        assert_eq!(
            decode_constructed_value(&active, &registry, &encoded),
            Ok(value)
        );
    }
}

#[test]
fn invocation_carrier_raw_request_rejects_each_closed_header_choice_causally() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let request = RuntimeValue::InvokeRequest(minimal_invocation_request(Vec::new(), Vec::new()));
    let encoded = encode_constructed_value(&active, &registry, &request).unwrap();
    let payload = encoded[25..].to_vec();
    assert_eq!(payload.len(), 90, "independent minimum request wire length");

    let cases: &[(usize, InvocationCarrierPath)] = &[
        (
            24,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller)
                .with(InvocationCarrierPathSegment::TerminalColumns),
        ),
        (
            25,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller)
                .with(InvocationCarrierPathSegment::TerminalRows),
        ),
        (
            42,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller)
                .with(InvocationCarrierPathSegment::PreferencePolicy),
        ),
        (
            81,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
                .with(InvocationCarrierPathSegment::ClientLimits),
        ),
        (
            82,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
                .with(InvocationCarrierPathSegment::ClientPreferences),
        ),
        (
            83,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestOutputRequirement),
        ),
        (
            84,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestStateProfile),
        ),
        (
            87,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestIdempotencyKey),
        ),
        (
            88,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestParentInvocation),
        ),
        (
            89,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestObserverContext),
        ),
    ];
    for (offset, path) in cases {
        let mut malformed = payload.clone();
        malformed[*offset] = 2;
        let error = decode_constructed_value(
            &active,
            &registry,
            &raw_carrier(SYS_INVOKE_REQUEST_TYPE_ID, &malformed),
        )
        .unwrap_err();
        assert_eq!(
            error,
            ValueCodecError::InvocationCarrier {
                carrier: SYS_INVOKE_REQUEST_TYPE_ID,
                source: InvocationCarrierCodecError::InvalidBoolean {
                    path: path.clone(),
                    actual: 2,
                },
            },
            "offset {offset} must reject its own invalid presence byte"
        );
    }

    let mut target = payload.clone();
    target[1] = 2;
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &target,
        InvocationCarrierCodecError::UnknownDiscriminant {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTarget),
            actual: 2,
        },
    );
    let mut caller = payload.clone();
    caller[22] = 10;
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &caller,
        InvocationCarrierCodecError::UnknownDiscriminant {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller)
                .with(InvocationCarrierPathSegment::CallerKind),
            actual: 10,
        },
    );
    let mut flags = payload.clone();
    flags[23] = 4;
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &flags,
        InvocationCarrierCodecError::InvalidField {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller)
                .with(InvocationCarrierPathSegment::CallerFlags),
        },
    );
    let mut locale = payload.clone();
    locale[30] = 0xff;
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &locale,
        InvocationCarrierCodecError::InvalidText {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller)
                .with(InvocationCarrierPathSegment::Locale),
        },
    );
    let mut protocol = payload.clone();
    protocol[43..45].copy_from_slice(&4_u16.to_be_bytes());
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &protocol,
        InvocationCarrierCodecError::InvalidField {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
                .with(InvocationCarrierPathSegment::ClientProtocol),
        },
    );
    let mut trace = payload.clone();
    trace[85] = 5;
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &trace,
        InvocationCarrierCodecError::UnknownDiscriminant {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTracePolicy),
            actual: 5,
        },
    );
    let mut deadline = payload;
    deadline[86] = 1;
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &deadline,
        InvocationCarrierCodecError::InvalidField {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestDeadline),
        },
    );
}

#[test]
fn invocation_request_round_trips_each_public_selector_and_discriminant() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let caller_kinds = [
        InvocationCallerKind::CliTty,
        InvocationCallerKind::CliPipe,
        InvocationCallerKind::DesktopLauncher,
        InvocationCallerKind::Browser,
        InvocationCallerKind::ClientFunction,
        InvocationCallerKind::JsonRpcGateway,
        InvocationCallerKind::McpGateway,
        InvocationCallerKind::Scheduler,
        InvocationCallerKind::TestRunner,
        InvocationCallerKind::Recovery,
    ];
    let streaming = [
        InvocationStreamingRequirement::Unspecified,
        InvocationStreamingRequirement::Required,
        InvocationStreamingRequirement::Preferred,
        InvocationStreamingRequirement::Forbidden,
    ];
    let trace = [
        InvocationTracePolicy::Off,
        InvocationTracePolicy::Basic,
        InvocationTracePolicy::Normal,
        InvocationTracePolicy::Verbose,
        InvocationTracePolicy::Profile,
    ];
    for (index, kind) in caller_kinds.into_iter().enumerate() {
        let (interactive, stdout_is_tty, columns, rows) = match kind {
            InvocationCallerKind::CliTty => (true, true, Some(80), Some(24)),
            InvocationCallerKind::CliPipe => (false, false, None, None),
            _ => (false, false, None, None),
        };
        let mut input = minimal_invocation_request(Vec::new(), Vec::new()).into_input();
        input.target = if index % 2 == 0 {
            InvocationTarget::function_id(FunctionId::from_bytes([index as u8; 16]))
        } else {
            InvocationTarget::qualified_name(QualifiedSemanticName::new(["sys", "invoke"]).unwrap())
                .unwrap()
        };
        input.arguments = vec![
            InvocationArgument::new(
                InvocationParameterSelector::parameter_id(ParameterId::from_bytes(
                    [index as u8; 16],
                )),
                InvokeValue::new(RuntimeValue::Integer(index as i32)).unwrap(),
            ),
            InvocationArgument::new(
                InvocationParameterSelector::name("named").unwrap(),
                InvokeValue::new(RuntimeValue::Text(String::from("selector"))).unwrap(),
            ),
        ];
        input.caller_context = InvocationCallerContext::new(
            kind,
            interactive,
            stdout_is_tty,
            columns,
            rows,
            "en-GB",
            "UTC",
            Some(InvokeValue::new(RuntimeValue::Boolean(true)).unwrap()),
        )
        .unwrap();
        input.output_requirement = Some(
            InvocationOutputRequirement::new(
                Some(String::from("result")),
                None,
                Some(if index % 2 == 0 {
                    InvocationOutputTypeSelector::type_id(INTEGER_TYPE_ID)
                } else {
                    InvocationOutputTypeSelector::qualified_name(
                        QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
                    )
                    .unwrap()
                }),
                streaming[index % streaming.len()],
            )
            .unwrap(),
        );
        input.state_profile = Some(String::from("state"));
        input.trace_policy = trace[index % trace.len()];
        input.idempotency_key = Some(vec![index as u8]);
        input.parent_invocation_id = Some(InvocationId::from_bytes([0x68; 16]));
        input.observer_context = Some(InvokeValue::new(RuntimeValue::Boolean(false)).unwrap());
        let request = RuntimeValue::InvokeRequest(InvokeRequest::new(input).unwrap());
        let encoded = encode_constructed_value(&active, &registry, &request).unwrap();
        assert_eq!(
            decode_constructed_value(&active, &registry, &encoded),
            Ok(request)
        );
    }
}

#[test]
fn invocation_carrier_raw_event_rejects_each_body_discriminant_and_text_failure() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let common = |kind| {
        let mut payload = vec![1, kind];
        payload.extend_from_slice(&[0x63; 16]);
        payload.extend_from_slice(&u64::MAX.to_be_bytes());
        payload
    };
    let event_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody);

    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_EVENT_TYPE_ID,
        &common(6),
        InvocationCarrierCodecError::UnknownDiscriminant {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventKind),
            actual: 6,
        },
    );
    let mut started = common(0);
    started.push(2);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_EVENT_TYPE_ID,
        &started,
        InvocationCarrierCodecError::InvalidBoolean {
            path: event_path
                .clone()
                .with(InvocationCarrierPathSegment::VisiblePrincipal),
            actual: 2,
        },
    );
    let mut batch = common(1);
    batch.push(1);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_EVENT_TYPE_ID,
        &batch,
        InvocationCarrierCodecError::UnknownDiscriminant {
            path: event_path
                .clone()
                .with(InvocationCarrierPathSegment::Channel),
            actual: 1,
        },
    );
    let mut malformed_schema_presence = common(1);
    malformed_schema_presence.extend_from_slice(&[0, 2]);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_EVENT_TYPE_ID,
        &malformed_schema_presence,
        InvocationCarrierCodecError::InvalidBoolean {
            path: event_path
                .clone()
                .with(InvocationCarrierPathSegment::Schema),
            actual: 2,
        },
    );
    let mut empty_batch = common(1);
    empty_batch.extend_from_slice(&[0, 0]);
    empty_batch.extend_from_slice(&0_u32.to_be_bytes());
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_EVENT_TYPE_ID,
        &empty_batch,
        InvocationCarrierCodecError::InvalidField {
            path: event_path
                .clone()
                .with(InvocationCarrierPathSegment::BatchValues),
        },
    );
    let mut diagnostic = common(2);
    diagnostic.push(3);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_EVENT_TYPE_ID,
        &diagnostic,
        InvocationCarrierCodecError::UnknownDiscriminant {
            path: event_path
                .clone()
                .with(InvocationCarrierPathSegment::Severity),
            actual: 3,
        },
    );
    let mut malformed_message = common(2);
    malformed_message.push(0);
    malformed_message.extend_from_slice(&1_u32.to_be_bytes());
    malformed_message.push(b'E');
    malformed_message.extend_from_slice(&1_u32.to_be_bytes());
    malformed_message.push(0xff);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_EVENT_TYPE_ID,
        &malformed_message,
        InvocationCarrierCodecError::InvalidText {
            path: event_path
                .clone()
                .with(InvocationCarrierPathSegment::Message),
        },
    );
    let mut failed = common(4);
    failed.push(8);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_EVENT_TYPE_ID,
        &failed,
        InvocationCarrierCodecError::UnknownDiscriminant {
            path: event_path.clone().with(InvocationCarrierPathSegment::Phase),
            actual: 8,
        },
    );
    let mut retryability = common(4);
    retryability.push(0);
    retryability.extend_from_slice(&1_u32.to_be_bytes());
    retryability.push(b'E');
    retryability.extend_from_slice(&0_u32.to_be_bytes());
    retryability.push(0);
    retryability.push(3);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_EVENT_TYPE_ID,
        &retryability,
        InvocationCarrierCodecError::UnknownDiscriminant {
            path: event_path
                .clone()
                .with(InvocationCarrierPathSegment::Retryability),
            actual: 3,
        },
    );
    let mut cancelled = common(5);
    cancelled.push(2);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_EVENT_TYPE_ID,
        &cancelled,
        InvocationCarrierCodecError::InvalidBoolean {
            path: event_path.with(InvocationCarrierPathSegment::Reason),
            actual: 2,
        },
    );
}

#[test]
fn invocation_carrier_raw_parser_rejects_version_selectors_names_and_nested_carriers() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_VALUE_TYPE_ID,
        &[2],
        InvocationCarrierCodecError::UnsupportedVersion { actual: 2 },
    );

    let base = raw_request_payload(&[]);
    let mut invalid_name = vec![1, 1];
    invalid_name.extend_from_slice(&1_u32.to_be_bytes());
    invalid_name.extend_from_slice(&3_u32.to_be_bytes());
    invalid_name.extend_from_slice(b"sys");
    invalid_name.extend_from_slice(&0_u32.to_be_bytes());
    invalid_name.extend_from_slice(&base[22..]);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &invalid_name,
        InvocationCarrierCodecError::InvalidSemanticName {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTarget),
        },
    );

    let mut invalid_selector = base[..18].to_vec();
    invalid_selector.extend_from_slice(&1_u32.to_be_bytes());
    invalid_selector.push(2);
    invalid_selector.extend_from_slice(&base[22..]);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &invalid_selector,
        InvocationCarrierCodecError::UnknownDiscriminant {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestArguments)
                .with(InvocationCarrierPathSegment::Argument(0))
                .with(InvocationCarrierPathSegment::Selector),
            actual: 2,
        },
    );

    let mut invalid_output_selector = base[..83].to_vec();
    invalid_output_selector.extend_from_slice(&[1, 0, 0, 1, 2]);
    invalid_output_selector.extend_from_slice(&base[84..]);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &invalid_output_selector,
        InvocationCarrierCodecError::UnknownDiscriminant {
            path: InvocationCarrierPath::one(
                InvocationCarrierPathSegment::RequestOutputRequirement,
            )
            .with(InvocationCarrierPathSegment::OutputType),
            actual: 2,
        },
    );

    let mut invalid_streaming = base[..83].to_vec();
    invalid_streaming.extend_from_slice(&[1, 1]);
    invalid_streaming.extend_from_slice(&6_u32.to_be_bytes());
    invalid_streaming.extend_from_slice(b"result");
    invalid_streaming.extend_from_slice(&[0, 0, 4]);
    invalid_streaming.extend_from_slice(&base[84..]);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &invalid_streaming,
        InvocationCarrierCodecError::UnknownDiscriminant {
            path: InvocationCarrierPath::one(
                InvocationCarrierPathSegment::RequestOutputRequirement,
            )
            .with(InvocationCarrierPathSegment::OutputStreaming),
            actual: 4,
        },
    );

    let mut invalid_deadline = base.clone();
    invalid_deadline[86] = 2;
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &invalid_deadline,
        InvocationCarrierCodecError::InvalidBoolean {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestDeadline),
            actual: 2,
        },
    );

    let sink_request = RuntimeValue::InvokeRequest(minimal_invocation_request(
        vec![
            InvocationSinkOffer::new(
                TypeDescriptor::named(BOOLEAN_TYPE_ID),
                ["text/plain"],
                false,
                0,
                None,
            )
            .unwrap(),
        ],
        Vec::new(),
    ));
    let sink_baseline = encode_constructed_value(&active, &registry, &sink_request).unwrap();
    let mut invalid_descriptor = sink_baseline.clone();
    invalid_descriptor[25 + 67] = 0xff;
    assert_eq!(
        decode_constructed_value(&active, &registry, &invalid_descriptor),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
            source: InvocationCarrierCodecError::UnknownDiscriminant {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
                    .with(InvocationCarrierPathSegment::ClientSinks)
                    .with(InvocationCarrierPathSegment::Sink(0))
                    .with(InvocationCarrierPathSegment::Descriptor),
                actual: 0xff,
            },
        })
    );
    let mut invalid_sink_streaming = sink_baseline;
    invalid_sink_streaming[25 + 102] = 2;
    assert_eq!(
        decode_constructed_value(&active, &registry, &invalid_sink_streaming),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
            source: InvocationCarrierCodecError::InvalidBoolean {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
                    .with(InvocationCarrierPathSegment::ClientSinks)
                    .with(InvocationCarrierPathSegment::Sink(0))
                    .with(InvocationCarrierPathSegment::Streaming),
                actual: 2,
            },
        })
    );

    let runtime_request = RuntimeValue::InvokeRequest(minimal_invocation_request(
        Vec::new(),
        vec![
            InvocationRuntimeOffer::new(
                "runt",
                "1",
                Vec::<TypeDescriptor>::new(),
                Vec::<InvocationRuntimeContract>::new(),
                0,
                false,
                None,
            )
            .unwrap(),
        ],
    ));
    let mut invalid_trust = encode_constructed_value(&active, &registry, &runtime_request).unwrap();
    invalid_trust[25 + 94] = 2;
    assert_eq!(
        decode_constructed_value(&active, &registry, &invalid_trust),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
            source: InvocationCarrierCodecError::InvalidBoolean {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
                    .with(InvocationCarrierPathSegment::ClientRuntimes)
                    .with(InvocationCarrierPathSegment::Runtime(0))
                    .with(InvocationCarrierPathSegment::Trusted),
                actual: 2,
            },
        })
    );

    for carrier in [
        SYS_INVOKE_VALUE_TYPE_ID,
        SYS_INVOKE_REQUEST_TYPE_ID,
        SYS_INVOKE_EVENT_TYPE_ID,
    ] {
        assert_carrier_source(
            &active,
            &registry,
            SYS_INVOKE_VALUE_TYPE_ID,
            &raw_invoke_value_payload(&raw_carrier(carrier, &[1])),
            InvocationCarrierCodecError::NestedCarrier {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::ValueInner),
                carrier,
            },
        );
    }
    let mut wrong_marker = orv5_integer(1);
    wrong_marker[..4].copy_from_slice(b"ORV4");
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_VALUE_TYPE_ID,
        &raw_invoke_value_payload(&wrong_marker),
        InvocationCarrierCodecError::InnerValue {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::ValueInner),
            source: Box::new(ValueCodecError::InvalidMarker),
        },
    );
}

#[test]
fn invocation_carrier_raw_semantic_boundaries_map_to_closed_errors() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let base = raw_request_payload(&[]);

    let mut empty_idempotency = base[..87].to_vec();
    empty_idempotency.push(1);
    empty_idempotency.extend_from_slice(&0_u32.to_be_bytes());
    empty_idempotency.extend_from_slice(&base[88..]);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &empty_idempotency,
        InvocationCarrierCodecError::InvalidField {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestIdempotencyKey),
        },
    );

    let mut zero_columns = base[..24].to_vec();
    zero_columns.push(1);
    zero_columns.extend_from_slice(&0_u32.to_be_bytes());
    zero_columns.extend_from_slice(&base[25..]);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &zero_columns,
        InvocationCarrierCodecError::InvalidField {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller)
                .with(InvocationCarrierPathSegment::TerminalColumns),
        },
    );

    let mut cli_tty = base.clone();
    cli_tty[22] = 0;
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &cli_tty,
        InvocationCarrierCodecError::InvalidField {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller),
        },
    );
    let mut cli_pipe = base.clone();
    cli_pipe[22] = 1;
    cli_pipe[23] = 1;
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &cli_pipe,
        InvocationCarrierCodecError::InvalidField {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller),
        },
    );

    let mut small_frame = base.clone();
    small_frame[69..73].copy_from_slice(&1_023_u32.to_be_bytes());
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &small_frame,
        InvocationCarrierCodecError::InvalidField {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
                .with(InvocationCarrierPathSegment::ClientMaximumFrameSize),
        },
    );

    let mut empty_output = base[..83].to_vec();
    empty_output.extend_from_slice(&[1, 0, 0, 0]);
    empty_output.extend_from_slice(&base[84..]);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &empty_output,
        InvocationCarrierCodecError::InvalidField {
            path: InvocationCarrierPath::one(
                InvocationCarrierPathSegment::RequestOutputRequirement,
            ),
        },
    );

    let mut embedded_too_large = b"ORV5".to_vec();
    embedded_too_large.push(0x0c);
    embedded_too_large.extend_from_slice(&SYS_INVOKE_VALUE_TYPE_ID.to_bytes());
    embedded_too_large.extend_from_slice(&(16_u32 * 1024 * 1024 + 1).to_be_bytes());
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &raw_request_payload(&[(ParameterId::from_bytes([0x6a; 16]), embedded_too_large)]),
        InvocationCarrierCodecError::PayloadTooLarge {
            actual: 16 * 1024 * 1024 + 1,
            maximum: 16 * 1024 * 1024,
        },
    );

    let unsupported_embedded_version = raw_request_payload(&[(
        ParameterId::from_bytes([0x6b; 16]),
        raw_carrier(SYS_INVOKE_VALUE_TYPE_ID, &[2]),
    )]);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &unsupported_embedded_version,
        InvocationCarrierCodecError::UnsupportedVersion { actual: 2 },
    );

    let mut diagnostic = vec![1, 2];
    diagnostic.extend_from_slice(&[0x6c; 16]);
    diagnostic.extend_from_slice(&0_u64.to_be_bytes());
    diagnostic.push(0);
    diagnostic.extend_from_slice(&1_u32.to_be_bytes());
    diagnostic.push(b'\n');
    diagnostic.extend_from_slice(&0_u32.to_be_bytes());
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_EVENT_TYPE_ID,
        &diagnostic,
        InvocationCarrierCodecError::InvalidField {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                .with(InvocationCarrierPathSegment::Code),
        },
    );

    let mut failed = vec![1, 4];
    failed.extend_from_slice(&[0x6d; 16]);
    failed.extend_from_slice(&0_u64.to_be_bytes());
    failed.push(0);
    failed.extend_from_slice(&1_u32.to_be_bytes());
    failed.push(b'\n');
    failed.extend_from_slice(&0_u32.to_be_bytes());
    failed.extend_from_slice(&[0, 0]);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_EVENT_TYPE_ID,
        &failed,
        InvocationCarrierCodecError::InvalidField {
            path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                .with(InvocationCarrierPathSegment::Code),
        },
    );
}

proptest! {
    #[test]
    fn arbitrary_event_sequences_round_trip_each_event_body(
        sequence in any::<u64>(),
        body_index in 0_usize..6,
    ) {
        let active = active_record_revision();
        let registry = registered_opaque_codecs(
            active.catalogue_hash_context().standard().unwrap(),
        ).unwrap();
        let value = || InvokeValue::new(RuntimeValue::Integer(7)).unwrap();
        let body = match body_index {
            0 => InvocationEventBody::Started { visible_principal: None },
            1 => InvocationEventBody::value_batch(None, [value()]).unwrap(),
            2 => InvocationEventBody::Diagnostic(
                InvocationDiagnostic::new(
                    InvocationDiagnosticSeverity::Warning,
                    "NOTICE",
                    "event",
                ).unwrap(),
            ),
            3 => InvocationEventBody::Completed { duration_nanoseconds: sequence },
            4 => InvocationEventBody::Failed(
                InvocationFailure::new(
                    InvocationFailurePhase::Runtime,
                    "FAILED",
                    "event",
                    None,
                    InvocationRetryability::No,
                ).unwrap(),
            ),
            5 => InvocationEventBody::cancelled(Some(String::from("event"))).unwrap(),
            _ => unreachable!("bounded body index"),
        };
        let event = RuntimeValue::InvokeEvent(
            InvokeEvent::new(InvocationId::from_bytes([0x67; 16]), sequence, body).unwrap(),
        );
        let encoded = encode_constructed_value(&active, &registry, &event).unwrap();
        prop_assert_eq!(decode_constructed_value(&active, &registry, &encoded), Ok(event));
    }
}

#[test]
fn invocation_request_tuple_order_and_duplicates_are_checked_before_materialisation() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let first = raw_invoke_value_carrier(&orv5_integer(1));
    let second = raw_invoke_value_carrier(&orv5_integer(2));
    let arguments_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestArguments);

    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &raw_request_payload(&[
            (ParameterId::from_bytes([0x72; 16]), first.clone()),
            (ParameterId::from_bytes([0x71; 16]), second.clone()),
        ]),
        InvocationCarrierCodecError::NonCanonicalOrder {
            path: arguments_path.clone(),
            index: 1,
        },
    );
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &raw_request_payload(&[
            (ParameterId::from_bytes([0x71; 16]), first),
            (ParameterId::from_bytes([0x71; 16]), second),
        ]),
        InvocationCarrierCodecError::DuplicateItem {
            path: arguments_path,
            first: 0,
            duplicate: 1,
        },
    );
}

#[test]
fn invocation_carrier_counts_lengths_and_order_precede_later_inner_values() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let accepted = raw_invoke_value_carrier(&orv5_integer(1));
    let malformed = raw_invoke_value_carrier(b"not an ORV5 value");
    let arguments_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestArguments);
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &raw_request_payload(&[
            (ParameterId::from_bytes([0x72; 16]), accepted.clone()),
            (ParameterId::from_bytes([0x71; 16]), malformed.clone()),
        ]),
        InvocationCarrierCodecError::NonCanonicalOrder {
            path: arguments_path.clone(),
            index: 1,
        },
    );
    assert_carrier_source(
        &active,
        &registry,
        SYS_INVOKE_REQUEST_TYPE_ID,
        &raw_request_payload(&[
            (ParameterId::from_bytes([0x71; 16]), accepted),
            (ParameterId::from_bytes([0x71; 16]), malformed),
        ]),
        InvocationCarrierCodecError::DuplicateItem {
            path: arguments_path,
            first: 0,
            duplicate: 1,
        },
    );

    let mut count = raw_request_payload(&[])[..22].to_vec();
    count[18..22].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(matches!(
        decode_constructed_value(
            &active,
            &registry,
            &raw_carrier(SYS_INVOKE_REQUEST_TYPE_ID, &count),
        ),
        Err(ValueCodecError::InvocationCarrier {
            source: InvocationCarrierCodecError::Truncated { .. },
            ..
        })
    ));

    let mut length = vec![1];
    length.extend_from_slice(&u32::MAX.to_be_bytes());
    assert!(matches!(
        decode_constructed_value(
            &active,
            &registry,
            &raw_carrier(SYS_INVOKE_VALUE_TYPE_ID, &length),
        ),
        Err(ValueCodecError::InvocationCarrier {
            source: InvocationCarrierCodecError::Truncated { .. },
            ..
        })
    ));
}

#[test]
fn invocation_carrier_current_authority_and_debug_redaction_are_causal() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let stale_inner = RuntimeValue::InvokeValue(
        InvokeValue::new(RuntimeValue::Enum(
            EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
        ))
        .unwrap(),
    );
    let stale_bytes = encode_constructed_value(&active, &registry, &stale_inner).unwrap();
    assert_eq!(
        decode_constructed_value(&active_revision_without_standard(), &registry, &stale_bytes),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_VALUE_TYPE_ID,
            source: InvocationCarrierCodecError::InnerValue {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::ValueInner),
                source: Box::new(ValueCodecError::InactiveEnumType {
                    enum_type: ENUM_TYPE
                }),
            },
        })
    );

    let secret = "do-not-log-this-secret";
    let request = RuntimeValue::InvokeRequest(
        InvokeRequest::new(InvokeRequestInput {
            target: InvocationTarget::function_id(FunctionId::from_bytes([0x64; 16])),
            arguments: Vec::new(),
            caller_context: InvocationCallerContext::new(
                InvocationCallerKind::Browser,
                false,
                false,
                None,
                None,
                "en-GB",
                "UTC",
                None,
            )
            .unwrap(),
            client_offer: InvocationClientOffer::new(
                5,
                "en-GB",
                "UTC",
                Vec::new(),
                Vec::new(),
                1_024,
                0,
                None,
                None,
            )
            .unwrap(),
            output_requirement: None,
            state_profile: None,
            trace_policy: InvocationTracePolicy::Off,
            idempotency_key: Some(secret.as_bytes().to_vec()),
            parent_invocation_id: None,
            observer_context: None,
        })
        .unwrap(),
    );
    let failed = RuntimeValue::InvokeEvent(
        InvokeEvent::new(
            InvocationId::from_bytes([0x65; 16]),
            0,
            InvocationEventBody::Failed(
                InvocationFailure::new(
                    InvocationFailurePhase::Internal,
                    "INTERNAL",
                    secret,
                    None,
                    InvocationRetryability::No,
                )
                .unwrap(),
            ),
        )
        .unwrap(),
    );
    assert!(!format!("{request:?}").contains(secret));
    assert!(!format!("{failed:?}").contains(secret));
}

#[test]
fn invoke_value_carrier_rechecks_every_admitted_orv5_family_and_authority() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let reference_target = TypeId::from_bytes([0x69; 16]);
    let opaque = RuntimeValue::Opaque(
        OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0x71; 16]).unwrap(),
    );
    let mut values = vec![
        RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap(),
        RuntimeValue::Boolean(true),
        RuntimeValue::Integer(-7),
        RuntimeValue::BigInt(-9),
        RuntimeValue::Float(RuntimeFloat::new(1.5).unwrap()),
        RuntimeValue::Text(String::from("text")),
        RuntimeValue::Bytes(vec![0, 0xff]),
        RuntimeValue::null(ResolvedType::reference(reference_target)).unwrap(),
        RuntimeValue::Reference {
            target: reference_target,
            object: ObjectId::from_bytes([0x6a; 16]),
        },
        RuntimeValue::null(ResolvedType::named(ENUM_TYPE)).unwrap(),
        RuntimeValue::Enum(EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap()),
        opaque.clone(),
    ];
    values.extend(constructed_collection_values(&active));
    for inner in values {
        let carrier = RuntimeValue::InvokeValue(InvokeValue::new(inner).unwrap());
        let encoded = encode_constructed_value(&active, &registry, &carrier).unwrap();
        assert_eq!(
            decode_constructed_value(&active, &registry, &encoded),
            Ok(carrier)
        );
    }

    let nested_active = active_nested_record_revision();
    let nested_registry =
        registered_opaque_codecs(nested_active.catalogue_hash_context().standard().unwrap())
            .unwrap();
    let nested =
        RuntimeValue::InvokeValue(InvokeValue::new(nested_record_value(&nested_active)).unwrap());
    let nested_encoded =
        encode_constructed_value(&nested_active, &nested_registry, &nested).unwrap();
    assert_eq!(
        decode_constructed_value(&nested_active, &nested_registry, &nested_encoded),
        Ok(nested)
    );

    let opaque_carrier = RuntimeValue::InvokeValue(InvokeValue::new(opaque).unwrap());
    let opaque_encoded = encode_constructed_value(&active, &registry, &opaque_carrier).unwrap();
    let alternate_active = active_record_revision_with_types_and_standard(
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
        TypeDescriptor::named(ENUM_TYPE),
        alternate_verified_standard(),
    );
    assert_eq!(
        decode_constructed_value(&alternate_active, &registry, &opaque_encoded),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_VALUE_TYPE_ID,
            source: InvocationCarrierCodecError::InnerValue {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::ValueInner),
                source: Box::new(ValueCodecError::OpaqueValue {
                    source: OpaqueValueError::ActiveStandardMismatch,
                }),
            },
        })
    );
}

#[test]
fn invoke_value_carriers_revalidate_stale_definition_and_opaque_authority() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let inner_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::ValueInner);
    let assert_inner = |wire: Vec<u8>, source: ValueCodecError| {
        assert_eq!(
            decode_constructed_value(&active, &registry, &wire),
            Err(ValueCodecError::InvocationCarrier {
                carrier: SYS_INVOKE_VALUE_TYPE_ID,
                source: InvocationCarrierCodecError::InnerValue {
                    path: inner_path.clone(),
                    source: Box::new(source),
                },
            })
        );
    };

    let enum_carrier = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeValue(
            InvokeValue::new(RuntimeValue::Enum(
                EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
            ))
            .unwrap(),
        ),
    )
    .unwrap();
    let mut stale_label = enum_carrier;
    // ORV5 InvokeValue -> ORV5 enum: the four-byte `lead` label is 55..59.
    stale_label[55..59].copy_from_slice(b"lost");
    assert_inner(
        stale_label,
        ValueCodecError::UndeclaredEnumLabel {
            enum_type: ENUM_TYPE,
            label: String::from("lost"),
        },
    );

    let record_type = active.catalogue().record_value_types()[0].id();
    let record_carrier = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeValue(
            InvokeValue::new(RuntimeValue::Record(
                RecordValue::new(
                    &active,
                    record_type,
                    [
                        (String::from("enabled"), RuntimeValue::Boolean(true)),
                        (
                            String::from("verified"),
                            RuntimeValue::Enum(
                                EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                            ),
                        ),
                    ],
                )
                .unwrap(),
            ))
            .unwrap(),
        ),
    )
    .unwrap();
    assert_eq!(
        decode_constructed_value(
            &active_revision_without_standard(),
            &registry,
            &record_carrier
        ),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_VALUE_TYPE_ID,
            source: InvocationCarrierCodecError::InnerValue {
                path: inner_path.clone(),
                source: Box::new(ValueCodecError::InactiveRecordType { record_type }),
            },
        })
    );
    let stale_record_active =
        active_record_revision_with_second_type(TypeDescriptor::named(BIGINT_TYPE_ID));
    let stale_record_registry = registered_opaque_codecs(
        stale_record_active
            .catalogue_hash_context()
            .standard()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        decode_constructed_value(
            &stale_record_active,
            &stale_record_registry,
            &record_carrier
        ),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_VALUE_TYPE_ID,
            source: InvocationCarrierCodecError::InnerValue {
                path: inner_path.clone(),
                source: Box::new(ValueCodecError::WrongRecordFieldType {
                    ordinal: 1,
                    expected: TypeDescriptor::named(BIGINT_TYPE_ID),
                    tag: 0x0a,
                    actual: ENUM_TYPE,
                }),
            },
        })
    );

    let opaque_carrier = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeValue(
            InvokeValue::new(RuntimeValue::Opaque(
                OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0x71; 16]).unwrap(),
            ))
            .unwrap(),
        ),
    )
    .unwrap();
    assert_eq!(
        decode_constructed_value(
            &active_revision_without_standard(),
            &registry,
            &opaque_carrier
        ),
        Err(ValueCodecError::InvocationCarrier {
            carrier: SYS_INVOKE_VALUE_TYPE_ID,
            source: InvocationCarrierCodecError::InnerValue {
                path: inner_path.clone(),
                source: Box::new(ValueCodecError::OpaqueValue {
                    source: OpaqueValueError::ActiveStandardRequired,
                }),
            },
        })
    );

    let mut unregistered_opaque = opaque_carrier.clone();
    // ORV5 InvokeValue -> ORV5 opaque: inner type identity is 35..51.
    unregistered_opaque[35..51].fill(0x72);
    assert_inner(
        unregistered_opaque,
        ValueCodecError::OpaqueValue {
            source: OpaqueValueError::UnregisteredType {
                opaque_type: TypeId::from_bytes([0x72; 16]),
            },
        },
    );

    let mut wrong_opaque_contract = opaque_carrier;
    // The embedded normal opaque payload is 16 bytes at 55..71.  Keep all
    // three enclosing declared lengths coherent while making it 15 bytes.
    wrong_opaque_contract[21..25].copy_from_slice(&45_u32.to_be_bytes());
    wrong_opaque_contract[26..30].copy_from_slice(&40_u32.to_be_bytes());
    wrong_opaque_contract[51..55].copy_from_slice(&15_u32.to_be_bytes());
    wrong_opaque_contract.pop();
    assert_inner(
        wrong_opaque_contract,
        ValueCodecError::OpaqueValue {
            source: OpaqueValueError::WrongPayloadLength {
                opaque_type: OPAQUE_TOKEN_TYPE_ID,
                expected: 16,
                actual: 15,
            },
        },
    );
}

#[test]
fn invocation_carrier_public_offer_text_and_optional_values_fail_at_exact_paths() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let assert_request = |wire: Vec<u8>, source: InvocationCarrierCodecError| {
        assert_eq!(
            decode_constructed_value(&active, &registry, &wire),
            Err(ValueCodecError::InvocationCarrier {
                carrier: SYS_INVOKE_REQUEST_TYPE_ID,
                source,
            })
        );
    };
    let offer_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer);
    let caller_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller);

    let basic = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeRequest(minimal_invocation_request(Vec::new(), Vec::new())),
    )
    .unwrap();
    for (offset, path) in [
        (
            25 + 30,
            caller_path
                .clone()
                .with(InvocationCarrierPathSegment::Locale),
        ),
        (
            25 + 39,
            caller_path
                .clone()
                .with(InvocationCarrierPathSegment::Timezone),
        ),
        (
            25 + 49,
            offer_path
                .clone()
                .with(InvocationCarrierPathSegment::ClientLocale),
        ),
        (
            25 + 58,
            offer_path
                .clone()
                .with(InvocationCarrierPathSegment::ClientTimezone),
        ),
    ] {
        let mut hostile = basic.clone();
        hostile[offset] = 0xff;
        assert_request(hostile, InvocationCarrierCodecError::InvalidText { path });
    }

    let sink_text = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeRequest(minimal_invocation_request(
            vec![
                InvocationSinkOffer::new(
                    TypeDescriptor::named(BOOLEAN_TYPE_ID),
                    ["media"],
                    false,
                    0,
                    None,
                )
                .unwrap(),
            ],
            Vec::new(),
        )),
    )
    .unwrap();
    let mut hostile_media = sink_text;
    hostile_media[25 + 92] = 0xff;
    assert_request(
        hostile_media,
        InvocationCarrierCodecError::InvalidText {
            path: offer_path
                .clone()
                .with(InvocationCarrierPathSegment::ClientSinks)
                .with(InvocationCarrierPathSegment::Sink(0))
                .with(InvocationCarrierPathSegment::MediaTypes)
                .with(InvocationCarrierPathSegment::MediaType(0)),
        },
    );

    let runtime_text = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeRequest(minimal_invocation_request(
            Vec::new(),
            vec![
                InvocationRuntimeOffer::new(
                    "runtime",
                    "version",
                    Vec::<TypeDescriptor>::new(),
                    vec![
                        InvocationRuntimeContract::new("contract", "release", ["feature"]).unwrap(),
                    ],
                    0,
                    false,
                    None,
                )
                .unwrap(),
            ],
        )),
    )
    .unwrap();
    let runtime_path = offer_path
        .clone()
        .with(InvocationCarrierPathSegment::ClientRuntimes)
        .with(InvocationCarrierPathSegment::Runtime(0));
    for (offset, path) in [
        (
            25 + 73,
            runtime_path
                .clone()
                .with(InvocationCarrierPathSegment::RuntimeName),
        ),
        (
            25 + 84,
            runtime_path
                .clone()
                .with(InvocationCarrierPathSegment::RuntimeVersion),
        ),
        (
            25 + 103,
            runtime_path
                .clone()
                .with(InvocationCarrierPathSegment::Contracts)
                .with(InvocationCarrierPathSegment::Contract(0))
                .with(InvocationCarrierPathSegment::ContractName),
        ),
        (
            25 + 115,
            runtime_path
                .clone()
                .with(InvocationCarrierPathSegment::Contracts)
                .with(InvocationCarrierPathSegment::Contract(0))
                .with(InvocationCarrierPathSegment::ContractVersion),
        ),
        (
            25 + 130,
            runtime_path
                .clone()
                .with(InvocationCarrierPathSegment::Contracts)
                .with(InvocationCarrierPathSegment::Contract(0))
                .with(InvocationCarrierPathSegment::Features)
                .with(InvocationCarrierPathSegment::Feature(0)),
        ),
    ] {
        let mut hostile = runtime_text.clone();
        hostile[offset] = 0xff;
        assert_request(hostile, InvocationCarrierCodecError::InvalidText { path });
    }

    let value = InvokeValue::new(RuntimeValue::Integer(1)).unwrap();
    let sink_limits = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeRequest(minimal_invocation_request(
            vec![
                InvocationSinkOffer::new(
                    TypeDescriptor::named(BOOLEAN_TYPE_ID),
                    ["media"],
                    false,
                    0,
                    Some(value.clone()),
                )
                .unwrap(),
            ],
            Vec::new(),
        )),
    )
    .unwrap();
    let mut hostile_sink_limits = sink_limits;
    hostile_sink_limits[25 + 137] = b'X';
    assert_request(
        hostile_sink_limits,
        InvocationCarrierCodecError::InnerValue {
            path: offer_path
                .clone()
                .with(InvocationCarrierPathSegment::ClientSinks)
                .with(InvocationCarrierPathSegment::Sink(0))
                .with(InvocationCarrierPathSegment::Limits),
            source: Box::new(ValueCodecError::InvalidMarker),
        },
    );

    let runtime_limits = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeRequest(minimal_invocation_request(
            Vec::new(),
            vec![
                InvocationRuntimeOffer::new(
                    "runtime",
                    "version",
                    Vec::<TypeDescriptor>::new(),
                    Vec::<InvocationRuntimeContract>::new(),
                    0,
                    false,
                    Some(value.clone()),
                )
                .unwrap(),
            ],
        )),
    )
    .unwrap();
    let mut hostile_runtime_limits = runtime_limits;
    hostile_runtime_limits[25 + 139] = b'X';
    assert_request(
        hostile_runtime_limits,
        InvocationCarrierCodecError::InnerValue {
            path: runtime_path.with(InvocationCarrierPathSegment::Limits),
            source: Box::new(ValueCodecError::InvalidMarker),
        },
    );

    let mut client_input = minimal_invocation_request(Vec::new(), Vec::new()).into_input();
    client_input.client_offer = InvocationClientOffer::new(
        5,
        "en-GB",
        "UTC",
        Vec::new(),
        Vec::new(),
        1_024,
        0,
        Some(value.clone()),
        Some(value),
    )
    .unwrap();
    let client_values = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeRequest(InvokeRequest::new(client_input).unwrap()),
    )
    .unwrap();
    for (offset, path) in [
        (
            25 + 116,
            offer_path
                .clone()
                .with(InvocationCarrierPathSegment::ClientLimits),
        ),
        (
            25 + 180,
            offer_path.with(InvocationCarrierPathSegment::ClientPreferences),
        ),
    ] {
        let mut hostile = client_values.clone();
        hostile[offset] = b'X';
        assert_request(
            hostile,
            InvocationCarrierCodecError::InnerValue {
                path,
                source: Box::new(ValueCodecError::InvalidMarker),
            },
        );
    }
}

#[test]
fn invocation_carrier_prefixes_and_outer_lengths_fail_without_materialisation() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    for value in carrier_test_values() {
        let carrier = invocation_carrier_type_id(&value).unwrap();
        let encoded = encode_constructed_value(&active, &registry, &value).unwrap();
        let payload = &encoded[25..];
        for prefix in 0..payload.len() {
            let error = decode_constructed_value(
                &active,
                &registry,
                &raw_carrier(carrier, &payload[..prefix]),
            )
            .unwrap_err();
            assert!(
                matches!(
                    error,
                    ValueCodecError::InvocationCarrier {
                        source: InvocationCarrierCodecError::Truncated { .. },
                        ..
                    }
                ),
                "carrier {carrier:?}, payload prefix {prefix} must truncate"
            );
        }
        let mut trailing = payload.to_vec();
        trailing.push(0);
        assert_carrier_source(
            &active,
            &registry,
            carrier,
            &trailing,
            InvocationCarrierCodecError::Trailing { remaining: 1 },
        );
    }

    let mut oversized = b"ORV5".to_vec();
    oversized.push(0x0c);
    oversized.extend_from_slice(&SYS_INVOKE_VALUE_TYPE_ID.to_bytes());
    oversized.extend_from_slice(&((16 * 1024 * 1024 + 1) as u32).to_be_bytes());
    assert_eq!(
        decode_constructed_value(&active, &registry, &oversized),
        Err(ValueCodecError::PayloadTooLarge {
            actual: 16 * 1024 * 1024 + 1,
            maximum: 16 * 1024 * 1024,
        })
    );
}
