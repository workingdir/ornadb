use orna_core::{
    CatalogueRevisionId, FieldId, FunctionId, InvocationId, ParameterId, SchemaId, SourceBundleId,
    SourceRevisionId, SourceUnitId, TypeId,
    canonical_hash::{
        catalogue_digest, catalogue_digest_with_context, source_bundle_digest,
        source_revision_record_digest, source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName, RecordValueFieldDefinition,
        RecordValueTypeDefinition, SchemaDefinition,
    },
    invocation::{
        InvocationCallerContext, InvocationCallerKind, InvocationClientOffer, InvocationEventBody,
        InvocationFailure, InvocationFailurePhase, InvocationRetryability, InvocationTarget,
        InvocationTracePolicy, InvokeRequestInput, InvokeValue,
    },
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, RevisionPair, SourceOrigin,
        StoredSourceRevision, StoredSourceUnit,
    },
    types::TypeDescriptor,
    value::{EnumValue, RecordValue},
};
use orna_standard::{
    registered_opaque_codecs, retained_standard_library_v2_snapshot,
    verify_standard_library_v2_snapshot,
};
use proptest::prelude::*;

use super::*;

const ENUM_TYPE: TypeId = TypeId::from_bytes([0x51; 16]);

fn empty_active_revision() -> ActiveDatabaseRevision {
    let source_bundle = SourceBundleId::from_bytes([0x81; 16]);
    let source_revision = SourceRevisionId::from_bytes([0x82; 16]);
    let bundle_hash = source_bundle_digest(&[]).unwrap();
    let source = StoredSourceRevision::new(
        source_bundle,
        source_revision,
        None,
        Vec::new(),
        bundle_hash,
        source_revision_record_digest(source_bundle, None, bundle_hash).unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x83; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    ActiveDatabaseRevision::new(
        RevisionPair::new(source.id(), catalogue.revision()),
        source,
        catalogue.clone(),
        catalogue_digest(&catalogue, &[], &[], &[], &[]).unwrap(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap()
}

fn record_active_revision() -> (ActiveDatabaseRevision, TypeId, TypeId) {
    const RECORD_TYPE: TypeId = TypeId::from_bytes([0x91; 16]);
    const OTHER_RECORD_TYPE: TypeId = TypeId::from_bytes([0x98; 16]);
    const FIELD_ID: FieldId = FieldId::from_bytes([0x92; 16]);
    const OTHER_FIELD_ID: FieldId = FieldId::from_bytes([0x99; 16]);
    let standard =
        verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot().unwrap())
            .unwrap();
    let schema_id = SchemaId::from_bytes([0x93; 16]);
    let catalogue_revision = CatalogueRevisionId::from_bytes([0x94; 16]);
    let source_bundle = SourceBundleId::from_bytes([0x95; 16]);
    let source_revision = SourceRevisionId::from_bytes([0x96; 16]);
    let source_unit = SourceUnitId::from_bytes([0x97; 16]);
    let source_content = "record";
    let unit = StoredSourceUnit::new(
        source_unit,
        0,
        "record.orna",
        source_content,
        source_unit_content_digest(source_content).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        source_bundle,
        source_revision,
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(source_bundle, None, bundle_hash).unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        catalogue_revision,
        vec![SchemaDefinition::new(
            schema_id,
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![
            RecordValueTypeDefinition::new(
                RECORD_TYPE,
                QualifiedSemanticName::new(["crm", "event"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        FIELD_ID,
                        "title",
                        0,
                        TypeDescriptor::named(orna_standard::BOOLEAN_TYPE_ID),
                    )
                    .unwrap(),
                ],
            ),
            RecordValueTypeDefinition::new(
                OTHER_RECORD_TYPE,
                QualifiedSemanticName::new(["crm", "other_event"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        OTHER_FIELD_ID,
                        "title",
                        0,
                        TypeDescriptor::named(orna_standard::BOOLEAN_TYPE_ID),
                    )
                    .unwrap(),
                ],
            ),
        ],
        Vec::new(),
    )
    .unwrap();
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema_id),
            SourceOrigin::new(source_unit, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(RECORD_TYPE),
            SourceOrigin::new(source_unit, 1, 2).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: RECORD_TYPE,
                field: FIELD_ID,
            },
            SourceOrigin::new(source_unit, 2, 3).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(OTHER_RECORD_TYPE),
            SourceOrigin::new(source_unit, 3, 4).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: OTHER_RECORD_TYPE,
                field: OTHER_FIELD_ID,
            },
            SourceOrigin::new(source_unit, 4, 5).unwrap(),
        ),
    ];
    let context = CatalogueHashContext::version_two(standard);
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source_revision, catalogue_revision),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), origins, Vec::new()),
        ),
        context,
    )
    .unwrap();
    (active, RECORD_TYPE, OTHER_RECORD_TYPE)
}

fn test_registry() -> OpaqueCodecRegistry {
    let standard =
        verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot().unwrap())
            .unwrap();
    registered_opaque_codecs(&standard).unwrap()
}
fn resource_hex(input: &str) -> Vec<u8> {
    assert_eq!(input.len() % 2, 0);
    input
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).unwrap();
            let low = (pair[1] as char).to_digit(16).unwrap();
            ((high << 4) | low) as u8
        })
        .collect()
}

fn minimal_request(idempotency_key: Option<Vec<u8>>) -> InvokeRequest {
    InvokeRequest::new(InvokeRequestInput {
        target: InvocationTarget::function_id(FunctionId::from_bytes([0x11; 16])),
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
        idempotency_key,
        parent_invocation_id: None,
        observer_context: None,
    })
    .unwrap()
}

fn enum_catalogue(labels: &[&str]) -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes([0x52; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x53; 16]),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            ENUM_TYPE,
            QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
            labels.iter().copied(),
        )],
        vec![],
    )
    .unwrap()
}

#[test]
fn ping_and_pong_have_exact_golden_bytes_and_round_trip() {
    let token = [1, 2, 3, 4, 5, 6, 7, 8];
    let mut ping = b"ORF1\x06\0".to_vec();
    ping.extend_from_slice(&0_u64.to_be_bytes());
    ping.extend_from_slice(&8_u32.to_be_bytes());
    ping.extend_from_slice(&token);
    assert_eq!(
        encode_client_frame(&ClientFrame::Ping { token }),
        Ok(ping.clone())
    );
    assert_eq!(decode_client_frame(&ping), Ok(ClientFrame::Ping { token }));

    let mut pong = ping.clone();
    pong[4] = 0x86;
    assert_eq!(
        encode_server_frame(&ServerFrame::Pong { token }),
        Ok(pong.clone())
    );
    assert_eq!(decode_server_frame(&pong), Ok(ServerFrame::Pong { token }));
    assert_eq!(
        decode_server_frame(&ping),
        Err(FrameCodecError::WrongDirection { tag: PING_TAG })
    );
    assert_eq!(
        decode_client_frame(&pong),
        Err(FrameCodecError::WrongDirection { tag: PONG_TAG })
    );
}

#[test]
fn retained_invoke_request_validates_only_its_outer_envelope_before_protected_decode() {
    let active = empty_active_revision();
    let registry = test_registry();
    let secret = b"not-visible-in-debug".to_vec();
    let request = minimal_request(Some(secret.clone()));
    let retained = encode_invoke_request(&active, &registry, &request).unwrap();
    assert!(retained.encoded_length() > ORV5_HEADER_LENGTH);
    assert_eq!(retained.decode(&active, &registry), Ok(request.clone()));
    assert_eq!(
        decode_retained_invoke_request(&active, &registry, &retained),
        Ok(request)
    );
    let debug = format!("{retained:?}");
    assert!(debug.contains("encoded_length"));
    assert!(!debug.contains("ORV5"));
    assert!(!debug.contains(std::str::from_utf8(&secret).unwrap()));

    let encoded = encode_constructed_value(
        &active,
        &registry,
        &RuntimeValue::InvokeRequest(minimal_request(Some(secret))),
    )
    .unwrap();
    assert_eq!(decode_invoke_request(&encoded), Ok(retained));

    // ORV5 Request payload byte zero is the fixed carrier-version byte.
    // Retention checks only the complete outer envelope.
    let mut invalid_inner = encoded.clone();
    invalid_inner[ORV5_HEADER_LENGTH] = 2;
    let retained_invalid_inner = decode_invoke_request(&invalid_inner).unwrap();
    assert_eq!(
        retained_invalid_inner.decode(&active, &registry),
        Err(FrameCodecError::Value {
            source: ValueCodecError::InvocationCarrier {
                carrier: SYS_INVOKE_REQUEST_TYPE_ID,
                source: crate::InvocationCarrierCodecError::UnsupportedVersion { actual: 2 },
            },
        })
    );

    let mut wrong_marker = encoded.clone();
    wrong_marker[..4].copy_from_slice(b"ORV4");
    assert_eq!(
        decode_invoke_request(&wrong_marker),
        Err(FrameCodecError::Value {
            source: ValueCodecError::InvalidMarker,
        })
    );

    let mut wrong_tag = encoded.clone();
    wrong_tag[4] = 0x0d;
    assert_eq!(
        decode_invoke_request(&wrong_tag),
        Err(FrameCodecError::InvocationCarrierWrongTag { tag: 0x0d })
    );

    let mut wrong_type = encoded.clone();
    wrong_type[5..21].copy_from_slice(&SYS_INVOKE_EVENT_TYPE_ID.to_bytes());
    assert_eq!(
        decode_invoke_request(&wrong_type),
        Err(FrameCodecError::InvocationCarrierWrongType {
            expected: SYS_INVOKE_REQUEST_TYPE_ID,
            actual: SYS_INVOKE_EVENT_TYPE_ID,
        })
    );

    let mut truncated = encoded.clone();
    let declared = u32::from_be_bytes(truncated[21..25].try_into().unwrap());
    truncated[21..25].copy_from_slice(&(declared + 1).to_be_bytes());
    assert_eq!(
        decode_invoke_request(&truncated),
        Err(FrameCodecError::Value {
            source: ValueCodecError::TruncatedPayload {
                declared: (declared + 1) as usize,
                actual: declared as usize,
            },
        })
    );

    let mut trailing = encoded;
    trailing.push(0);
    assert!(matches!(
        decode_invoke_request(&trailing),
        Err(FrameCodecError::Value {
            source: ValueCodecError::TrailingBytes { .. },
        })
    ));
}

#[test]
fn special_invoke_request_uses_existing_argument_wire_and_state_contract() {
    let active = empty_active_revision();
    let registry = test_registry();
    let request = encode_invoke_request(&active, &registry, &minimal_request(None)).unwrap();
    let frame = ClientFrame::CallInvokeRequest {
        stream: 1,
        request: request.clone(),
    };
    let encoded = encode_constructed_client_frame(&active, &registry, &frame).unwrap();
    let mut expected = b"ORF5\x02\0".to_vec();
    expected.extend_from_slice(&1_u64.to_be_bytes());
    expected.extend_from_slice(&(16_u32 + request.encoded_length() as u32).to_be_bytes());
    expected.extend_from_slice(&SYS_INVOKE_PARAMETER_ID.to_bytes());
    expected.extend_from_slice(&request.encoded);
    assert_eq!(encoded, expected);
    assert_eq!(
        decode_constructed_client_frame(&active, &registry, &encoded),
        Ok(frame.clone())
    );

    let mut malformed = encoded.clone();
    malformed[HEADER_LENGTH + 16] = 0;
    assert!(matches!(
        decode_constructed_client_frame(&active, &registry, &malformed),
        Err(FrameCodecError::Value {
            source: ValueCodecError::InvalidMarker,
        })
    ));

    let mut wrong_parameter = encoded.clone();
    wrong_parameter[HEADER_LENGTH..HEADER_LENGTH + 16].fill(0x44);
    assert!(matches!(
        decode_constructed_client_frame(&active, &registry, &wrong_parameter),
        Err(FrameCodecError::InvocationCarrierNotAccepted {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
        })
    ));

    let mut connection = ProtocolConnection::new();
    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::CallRawStart {
                stream: 1,
                function: SYS_INVOKE_FUNCTION_ID,
            },
        )
        .unwrap();
    assert_eq!(
        connection.receive_constructed(&active, &registry, frame.clone()),
        Ok(None)
    );
    assert_eq!(
        connection.receive_constructed(
            &active,
            &registry,
            ClientFrame::CallInvokeRequest {
                stream: 1,
                request: request.clone(),
            },
        ),
        Err(ConnectionError::WrongState { stream: 1 })
    );
    assert!(matches!(
        connection.receive_constructed(
            &active,
            &registry,
            ClientFrame::CallArgumentsComplete { stream: 1 },
        ),
        Ok(Some(ClientAction::InvokeDispatch { .. }))
    ));

    let mut wrong_function = ProtocolConnection::new();
    wrong_function
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::CallRawStart {
                stream: 1,
                function: FunctionId::from_bytes([0x55; 16]),
            },
        )
        .unwrap();
    assert_eq!(
        wrong_function.receive_constructed(&active, &registry, frame),
        Err(ConnectionError::WrongState { stream: 1 })
    );
}

#[test]
fn sealed_event_batch_uses_event_tag_and_result_credit_lifecycle() {
    let active = empty_active_revision();
    let registry = test_registry();
    let request = encode_invoke_request(&active, &registry, &minimal_request(None)).unwrap();
    let invocation = InvocationId::from_bytes([0x71; 16]);
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .unwrap();
    let value_batch = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::ValueBatch {
            schema: None,
            values: vec![InvokeValue::new(RuntimeValue::Integer(7)).unwrap()],
        },
    )
    .unwrap();
    let completed = InvokeEvent::new(
        invocation,
        2,
        InvocationEventBody::Completed {
            duration_nanoseconds: 11,
        },
    )
    .unwrap();
    let events = InvocationEventBatch::new(vec![
        InvocationEventRecord::new(1, started),
        InvocationEventRecord::new(2, value_batch),
        InvocationEventRecord::new(3, completed),
    ])
    .unwrap();
    let mut connection = ProtocolConnection::new();
    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::CallRawStart {
                stream: 1,
                function: SYS_INVOKE_FUNCTION_ID,
            },
        )
        .unwrap();
    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::CallInvokeRequest { stream: 1, request },
        )
        .unwrap();
    assert!(matches!(
        connection
            .receive_constructed(
                &active,
                &registry,
                ClientFrame::CallArgumentsComplete { stream: 1 }
            )
            .unwrap(),
        Some(ClientAction::InvokeDispatch { .. })
    ));
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::Accepted {
                stream: 1,
                invocation
            }
        ),
        Ok(ServerFrame::CallAccepted {
            stream: 1,
            invocation
        })
    );

    let mut cancellation_connection = connection.clone();
    let started_only = InvocationEventBatch::new(vec![events.records()[0].clone()]).unwrap();
    let mut queued_cancellation_connection = connection.clone();
    queued_cancellation_connection
        .receive(ClientFrame::CallCancel { stream: 1 })
        .unwrap();
    let queued_required = match queued_cancellation_connection.apply_constructed(
        &active,
        &registry,
        ServerAction::InvokeCancelled { stream: 1 },
    ) {
        Err(ConnectionError::InsufficientCredit {
            stream: 1,
            channel: Channel::ResultValues,
            available: 0,
            required,
        }) if required > 0 => required,
        result => panic!("queued cancellation batch should require credit: {result:?}"),
    };
    queued_cancellation_connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: queued_required,
            },
        )
        .unwrap();
    let queued_cancelled = queued_cancellation_connection
        .apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeCancelled { stream: 1 },
        )
        .unwrap();
    assert!(
        matches!(&queued_cancelled, ServerFrame::EventBatch { events, .. } if events.len() == 2 && matches!(&events[0].event, Event::Value(RuntimeValue::InvokeEvent(event)) if event.kind() == InvocationEventKind::InvocationStarted && event.sequence() == 0) && matches!(&events[1].event, Event::Value(RuntimeValue::InvokeEvent(event)) if event.kind() == InvocationEventKind::InvocationCancelled && event.sequence() == 1))
    );
    assert_eq!(
        queued_cancellation_connection.apply(ServerAction::Completed { stream: 1 }),
        Ok(ServerFrame::CallCompleted { stream: 1 })
    );

    let normal_required = match connection.apply_constructed(
        &active,
        &registry,
        ServerAction::InvokeEvents {
            stream: 1,
            events: events.clone(),
        },
    ) {
        Err(ConnectionError::InsufficientCredit {
            stream: 1,
            channel: Channel::ResultValues,
            available: 0,
            required,
        }) if required > 0 => required,
        result => panic!("completed terminal batch should require credit: {result:?}"),
    };
    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: normal_required,
            },
        )
        .unwrap();
    let frame = connection
        .apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents {
                stream: 1,
                events: events.clone(),
            },
        )
        .unwrap();
    assert!(matches!(&frame, ServerFrame::EventBatch { .. }));
    assert_eq!(
        connection.receive_constructed(
            &active,
            &registry,
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultBytes,
                credit: 1
            }
        ),
        Err(ConnectionError::WrongState { stream: 1 })
    );
    let encoded = encode_constructed_server_frame(&active, &registry, &frame).unwrap();
    assert_eq!(encoded[4], EVENT_BATCH_TAG);
    assert_eq!(
        decode_constructed_invocation_event_frame(&active, &registry, &encoded),
        Ok(frame)
    );
    assert_eq!(
        connection.apply(ServerAction::Completed { stream: 1 }),
        Ok(ServerFrame::CallCompleted { stream: 1 })
    );

    let started_required = match cancellation_connection.apply_constructed(
        &active,
        &registry,
        ServerAction::InvokeEvents {
            stream: 1,
            events: started_only.clone(),
        },
    ) {
        Err(ConnectionError::InsufficientCredit {
            stream: 1,
            channel: Channel::ResultValues,
            available: 0,
            required,
        }) if required > 0 => required,
        result => panic!("started event batch should require credit: {result:?}"),
    };
    cancellation_connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: started_required,
            },
        )
        .unwrap();
    cancellation_connection
        .apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents {
                stream: 1,
                events: started_only,
            },
        )
        .unwrap();
    assert_eq!(cancellation_connection.result_credit(1), Ok(0));
    assert!(matches!(
        cancellation_connection.receive(ClientFrame::CallCancel { stream: 1 }),
        Ok(Some(ClientAction::Cancel {
            stream: 1,
            invocation: Some(_)
        }))
    ));
    let cancelled_required = match cancellation_connection.apply_constructed(
        &active,
        &registry,
        ServerAction::InvokeCancelled { stream: 1 },
    ) {
        Err(ConnectionError::InsufficientCredit {
            stream: 1,
            channel: Channel::ResultValues,
            available: 0,
            required,
        }) if required > 0 => required,
        result => panic!("post-start cancellation batch should require credit: {result:?}"),
    };
    cancellation_connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: cancelled_required,
            },
        )
        .unwrap();
    let cancelled = cancellation_connection
        .apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeCancelled { stream: 1 },
        )
        .unwrap();
    assert!(
        matches!(&cancelled, ServerFrame::EventBatch { events, .. } if matches!(&events[0].event, Event::Value(RuntimeValue::InvokeEvent(event)) if event.kind() == InvocationEventKind::InvocationCancelled && event.sequence() == 1))
    );
    assert_eq!(
        cancellation_connection.apply(ServerAction::Completed { stream: 1 }),
        Ok(ServerFrame::CallCompleted { stream: 1 })
    );
}

#[test]
fn running_cancelling_discards_stale_invoke_events_before_cancellation_terminal() {
    let active = empty_active_revision();
    let registry = test_registry();
    let request = encode_invoke_request(&active, &registry, &minimal_request(None)).unwrap();
    let invocation = InvocationId::from_bytes([0x75; 16]);
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .unwrap();
    let value = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::ValueBatch {
            schema: None,
            values: vec![InvokeValue::new(RuntimeValue::Integer(7)).unwrap()],
        },
    )
    .unwrap();
    let completed = InvokeEvent::new(
        invocation,
        2,
        InvocationEventBody::Completed {
            duration_nanoseconds: 11,
        },
    )
    .unwrap();
    let stale = InvocationEventBatch::new(vec![
        InvocationEventRecord::new(2, value),
        InvocationEventRecord::new(3, completed),
    ])
    .unwrap();

    let mut connection = ProtocolConnection::new();
    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::CallRawStart {
                stream: 1,
                function: SYS_INVOKE_FUNCTION_ID,
            },
        )
        .unwrap();
    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::CallInvokeRequest { stream: 1, request },
        )
        .unwrap();
    assert!(matches!(
        connection
            .receive_constructed(
                &active,
                &registry,
                ClientFrame::CallArgumentsComplete { stream: 1 },
            )
            .unwrap(),
        Some(ClientAction::InvokeDispatch { .. })
    ));
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::Accepted {
                stream: 1,
                invocation,
            },
        ),
        Ok(ServerFrame::CallAccepted {
            stream: 1,
            invocation,
        })
    );
    connection
        .receive(ClientFrame::WindowUpdate {
            stream: 1,
            channel: Channel::ResultValues,
            credit: MAX_CHANNEL_WINDOW,
        })
        .unwrap();
    connection
        .apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents {
                stream: 1,
                events: InvocationEventBatch::new(vec![InvocationEventRecord::new(1, started)])
                    .unwrap(),
            },
        )
        .unwrap();
    assert!(matches!(
        connection.receive(ClientFrame::CallCancel { stream: 1 }),
        Ok(Some(ClientAction::Cancel {
            stream: 1,
            invocation: Some(_),
        }))
    ));

    let before_stale = connection.clone();
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents {
                stream: 1,
                events: stale,
            },
        ),
        Err(ConnectionError::WrongState { stream: 1 })
    );
    assert_eq!(connection, before_stale);

    let operational_failure = InvocationFailure::new(
        InvocationFailurePhase::Target,
        "INVOKE_TARGET_FAILED",
        "invocation target failed",
        None,
        InvocationRetryability::Unknown,
    )
    .unwrap();
    let operational_failure = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::Failed(operational_failure),
    )
    .unwrap();
    let before_operational_failure = connection.clone();
    let before_operational_credit = connection.result_credit(1).unwrap();
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents {
                stream: 1,
                events: InvocationEventBatch::new(vec![InvocationEventRecord::new(
                    2,
                    operational_failure,
                )])
                .unwrap(),
            },
        ),
        Err(ConnectionError::WrongState { stream: 1 })
    );
    assert_eq!(connection.result_credit(1), Ok(before_operational_credit));
    let state = connection.streams.get(&1).expect("live stream");
    assert_eq!(state.phase, Phase::RunningCancelling { invocation });
    assert_eq!(state.last_sequence, 1);
    assert_eq!(state.last_invocation_outer_sequence, 1);
    assert_eq!(state.last_invocation_event_sequence, Some(0));
    assert!(!state.invocation_terminal);
    assert_eq!(connection, before_operational_failure);

    let failure = InvocationFailure::new(
        InvocationFailurePhase::Internal,
        "INVOKE_INTERNAL_FAILURE",
        "invocation could not complete",
        None,
        InvocationRetryability::Unknown,
    )
    .unwrap();
    let failed = InvokeEvent::new(invocation, 1, InvocationEventBody::Failed(failure)).unwrap();
    let mut failure_connection = connection.clone();
    let failure_frame = failure_connection
        .apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents {
                stream: 1,
                events: InvocationEventBatch::new(vec![InvocationEventRecord::new(2, failed)])
                    .unwrap(),
            },
        )
        .unwrap();
    assert!(matches!(
        &failure_frame,
        ServerFrame::EventBatch { events, .. }
            if events.len() == 1
                && matches!(&events[0].event, Event::Value(RuntimeValue::InvokeEvent(event)) if event.kind() == InvocationEventKind::InvocationFailed)
    ));
    assert_eq!(
        failure_connection.apply(ServerAction::Completed { stream: 1 }),
        Ok(ServerFrame::CallCompleted { stream: 1 })
    );

    let cancelled = connection
        .apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeCancelled { stream: 1 },
        )
        .unwrap();
    assert!(matches!(
        &cancelled,
        ServerFrame::EventBatch { events, .. }
            if events.len() == 1
                && matches!(&events[0].event, Event::Value(RuntimeValue::InvokeEvent(event)) if event.kind() == InvocationEventKind::InvocationCancelled)
    ));
    assert_eq!(
        connection.apply(ServerAction::Completed { stream: 1 }),
        Ok(ServerFrame::CallCompleted { stream: 1 })
    );
}

#[test]
fn accepted_invoke_event_batches_enforce_cross_batch_sequences_credit_and_state() {
    let active = empty_active_revision();
    let registry = test_registry();
    let request = encode_invoke_request(&active, &registry, &minimal_request(None)).unwrap();
    let invocation = InvocationId::from_bytes([0x74; 16]);
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .unwrap();
    let value = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::ValueBatch {
            schema: None,
            values: vec![InvokeValue::new(RuntimeValue::Integer(7)).unwrap()],
        },
    )
    .unwrap();
    let completed = InvokeEvent::new(
        invocation,
        2,
        InvocationEventBody::Completed {
            duration_nanoseconds: 11,
        },
    )
    .unwrap();
    let mut connection = ProtocolConnection::new();
    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::CallRawStart {
                stream: 1,
                function: SYS_INVOKE_FUNCTION_ID,
            },
        )
        .unwrap();
    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::CallInvokeRequest { stream: 1, request },
        )
        .unwrap();
    assert!(matches!(
        connection
            .receive_constructed(
                &active,
                &registry,
                ClientFrame::CallArgumentsComplete { stream: 1 },
            )
            .unwrap(),
        Some(ClientAction::InvokeDispatch { .. })
    ));
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::Accepted {
                stream: 1,
                invocation,
            },
        ),
        Ok(ServerFrame::CallAccepted {
            stream: 1,
            invocation,
        })
    );

    let apply_with_exact_credit = |connection: &mut ProtocolConnection,
                                   events: InvocationEventBatch| {
        let expected_frame = ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: events
                .records()
                .iter()
                .map(|record| EventRecord {
                    sequence: record.outer_sequence(),
                    event: Event::Value(RuntimeValue::InvokeEvent(record.event().clone())),
                })
                .collect(),
        };
        let expected_credit = encode_constructed_server_frame(&active, &registry, &expected_frame)
            .unwrap()
            .len()
            .checked_sub(HEADER_LENGTH)
            .expect("encoded event frame includes its header") as u64;
        let before_insufficient_credit = connection.clone();
        let required = match connection.apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents {
                stream: 1,
                events: events.clone(),
            },
        ) {
            Err(ConnectionError::InsufficientCredit {
                stream: 1,
                channel: Channel::ResultValues,
                available: 0,
                required,
            }) if required > 0 => required,
            result => panic!("event batch should require exact credit: {result:?}"),
        };
        assert_eq!(&*connection, &before_insufficient_credit);
        assert_eq!(required, expected_credit);
        connection
            .receive_constructed(
                &active,
                &registry,
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: required,
                },
            )
            .unwrap();
        let frame = connection
            .apply_constructed(
                &active,
                &registry,
                ServerAction::InvokeEvents { stream: 1, events },
            )
            .unwrap();
        assert_eq!(connection.result_credit(1), Ok(0));
        frame
    };

    let started_frame = apply_with_exact_credit(
        &mut connection,
        InvocationEventBatch::new(vec![InvocationEventRecord::new(1, started)]).unwrap(),
    );
    assert!(matches!(
        &started_frame,
        ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events,
        } if events.len() == 1
            && events[0].sequence == 1
            && matches!(&events[0].event, Event::Value(RuntimeValue::InvokeEvent(event)) if event.kind() == InvocationEventKind::InvocationStarted && event.sequence() == 0)
    ));

    let repeated_started = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .unwrap();
    let before_repeated_started = connection.clone();
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents {
                stream: 1,
                events: InvocationEventBatch::new(vec![InvocationEventRecord::new(
                    2,
                    repeated_started,
                )])
                .unwrap(),
            },
        ),
        Err(ConnectionError::InvalidFrame {
            source: FrameCodecError::InvalidInvocationEventSequence,
        })
    );
    assert_eq!(connection, before_repeated_started);

    let skipped = InvokeEvent::new(
        invocation,
        2,
        InvocationEventBody::ValueBatch {
            schema: None,
            values: vec![InvokeValue::new(RuntimeValue::Integer(8)).unwrap()],
        },
    )
    .unwrap();
    let before_skipped = connection.clone();
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents {
                stream: 1,
                events: InvocationEventBatch::new(vec![InvocationEventRecord::new(2, skipped,)])
                    .unwrap(),
            },
        ),
        Err(ConnectionError::InvalidFrame {
            source: FrameCodecError::InvalidInvocationEventSequence,
        })
    );
    assert_eq!(connection, before_skipped);

    let replayed = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::ValueBatch {
            schema: None,
            values: vec![InvokeValue::new(RuntimeValue::Integer(9)).unwrap()],
        },
    )
    .unwrap();
    let before_replayed = connection.clone();
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents {
                stream: 1,
                events: InvocationEventBatch::new(vec![InvocationEventRecord::new(1, replayed,)])
                    .unwrap(),
            },
        ),
        Err(ConnectionError::InvalidFrame {
            source: FrameCodecError::InvalidInvocationEventSequence,
        })
    );
    assert_eq!(connection, before_replayed);

    let value_frame = apply_with_exact_credit(
        &mut connection,
        InvocationEventBatch::new(vec![InvocationEventRecord::new(2, value)]).unwrap(),
    );
    assert!(matches!(
        &value_frame,
        ServerFrame::EventBatch { events, .. }
            if events.len() == 1
                && events[0].sequence == 2
                && matches!(&events[0].event, Event::Value(RuntimeValue::InvokeEvent(event)) if event.kind() == InvocationEventKind::ValueBatch && event.sequence() == 1)
    ));

    let after_terminal = InvokeEvent::new(
        invocation,
        3,
        InvocationEventBody::ValueBatch {
            schema: None,
            values: vec![InvokeValue::new(RuntimeValue::Integer(10)).unwrap()],
        },
    )
    .unwrap();
    let terminal_before_nonterminal = InvocationEventBatch::new(vec![
        InvocationEventRecord::new(3, completed.clone()),
        InvocationEventRecord::new(4, after_terminal.clone()),
    ])
    .unwrap();
    let before_terminal_before_nonterminal = connection.clone();
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents {
                stream: 1,
                events: terminal_before_nonterminal,
            },
        ),
        Err(ConnectionError::InvalidFrame {
            source: FrameCodecError::InvalidInvocationEventSequence,
        })
    );
    assert_eq!(connection, before_terminal_before_nonterminal);

    let wrong_terminal = InvokeEvent::new(
        invocation,
        2,
        InvocationEventBody::Cancelled { reason: None },
    )
    .unwrap();
    let before_wrong_terminal = connection.clone();
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents {
                stream: 1,
                events: InvocationEventBatch::new(vec![InvocationEventRecord::new(
                    3,
                    wrong_terminal,
                )])
                .unwrap(),
            },
        ),
        Err(ConnectionError::WrongState { stream: 1 })
    );
    assert_eq!(connection, before_wrong_terminal);

    let completed_frame = apply_with_exact_credit(
        &mut connection,
        InvocationEventBatch::new(vec![InvocationEventRecord::new(3, completed)]).unwrap(),
    );
    assert!(matches!(
        &completed_frame,
        ServerFrame::EventBatch { events, .. }
            if events.len() == 1
                && events[0].sequence == 3
                && matches!(&events[0].event, Event::Value(RuntimeValue::InvokeEvent(event)) if event.kind() == InvocationEventKind::InvocationCompleted && event.sequence() == 2)
    ));
    let before_post_terminal = connection.clone();
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents {
                stream: 1,
                events: InvocationEventBatch::new(vec![InvocationEventRecord::new(
                    4,
                    after_terminal,
                )])
                .unwrap(),
            },
        ),
        Err(ConnectionError::WrongState { stream: 1 })
    );
    assert_eq!(connection, before_post_terminal);
    assert_eq!(
        connection.apply(ServerAction::Completed { stream: 1 }),
        Ok(ServerFrame::CallCompleted { stream: 1 })
    );
}

#[test]
fn later_sealed_cancellation_event_batch_uses_constructed_codec() {
    let active = empty_active_revision();
    let registry = test_registry();
    let invocation = InvocationId::from_bytes([0x72; 16]);
    let cancelled = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::Cancelled { reason: None },
    )
    .unwrap();
    let frame = ServerFrame::EventBatch {
        stream: 1,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: 2,
            event: Event::Value(RuntimeValue::InvokeEvent(cancelled)),
        }],
    };

    let encoded = encode_constructed_server_frame(&active, &registry, &frame).unwrap();
    assert_eq!(
        decode_constructed_invocation_event_frame(&active, &registry, &encoded),
        Ok(frame)
    );
}

#[test]
fn invocation_cancelled_event_is_rejected_before_client_cancel() {
    let active = empty_active_revision();
    let registry = test_registry();
    let request = encode_invoke_request(&active, &registry, &minimal_request(None)).unwrap();
    let invocation = InvocationId::from_bytes([0x73; 16]);
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .unwrap();
    let cancelled = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::Cancelled { reason: None },
    )
    .unwrap();
    let events = InvocationEventBatch::new(vec![
        InvocationEventRecord::new(1, started),
        InvocationEventRecord::new(2, cancelled),
    ])
    .unwrap();
    let mut connection = ProtocolConnection::new();
    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::CallRawStart {
                stream: 1,
                function: SYS_INVOKE_FUNCTION_ID,
            },
        )
        .unwrap();
    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::CallInvokeRequest { stream: 1, request },
        )
        .unwrap();
    assert!(matches!(
        connection
            .receive_constructed(
                &active,
                &registry,
                ClientFrame::CallArgumentsComplete { stream: 1 },
            )
            .unwrap(),
        Some(ClientAction::InvokeDispatch { .. })
    ));
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::Accepted {
                stream: 1,
                invocation,
            },
        ),
        Ok(ServerFrame::CallAccepted {
            stream: 1,
            invocation
        })
    );
    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: MAX_CHANNEL_WINDOW,
            },
        )
        .unwrap();
    let before = connection.clone();
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::InvokeEvents { stream: 1, events },
        ),
        Err(ConnectionError::WrongState { stream: 1 })
    );
    assert_eq!(connection, before);
}

#[test]
fn invocation_event_batch_keeps_outer_and_inner_sequences_independent() {
    let active = empty_active_revision();
    let registry = test_registry();
    let invocation = InvocationId::from_bytes([0x61; 16]);
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .unwrap();
    let completed = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::Completed {
            duration_nanoseconds: 7,
        },
    )
    .unwrap();
    let batch = InvocationEventBatch::new(vec![
        InvocationEventRecord::new(1, started.clone()),
        InvocationEventRecord::new(2, completed.clone()),
    ])
    .unwrap();
    let encoded = encode_invocation_event_batch(&active, &registry, &batch).unwrap();
    assert_eq!(encoded[..3], [Channel::ResultValues.wire(), 0, 2]);
    assert_eq!(encoded[11], CANONICAL_VALUE_EVENT_KIND);
    assert_eq!(
        decode_invocation_event_batch(&active, &registry, &encoded),
        Ok(batch)
    );

    let skipped = InvokeEvent::new(
        invocation,
        2,
        InvocationEventBody::Completed {
            duration_nanoseconds: 7,
        },
    )
    .unwrap();
    assert_eq!(
        InvocationEventBatch::new(vec![
            InvocationEventRecord::new(1, started.clone()),
            InvocationEventRecord::new(2, skipped),
        ]),
        Err(FrameCodecError::InvalidInvocationEventSequence)
    );
    assert_eq!(
        InvocationEventBatch::new(vec![InvocationEventRecord::new(0, started.clone())]),
        Err(FrameCodecError::InvalidInvocationOuterSequence)
    );

    let other = InvokeEvent::new(
        InvocationId::from_bytes([0x62; 16]),
        1,
        InvocationEventBody::Completed {
            duration_nanoseconds: 7,
        },
    )
    .unwrap();
    assert_eq!(
        InvocationEventBatch::new(vec![
            InvocationEventRecord::new(1, started),
            InvocationEventRecord::new(2, other),
        ]),
        Err(FrameCodecError::MismatchedInvocationEvent)
    );

    let mut wrong_outer = encoded.clone();
    wrong_outer[3..11].copy_from_slice(&2_u64.to_be_bytes());
    assert_eq!(
        decode_invocation_event_batch(&active, &registry, &wrong_outer),
        Err(FrameCodecError::InvalidInvocationOuterSequence)
    );
    let mut wrong_kind = encoded;
    wrong_kind[11] = 0x02;
    assert_eq!(
        decode_invocation_event_batch(&active, &registry, &wrong_kind),
        Err(FrameCodecError::InvalidEventChannel {
            channel: Channel::ResultValues,
            kind: 0x02,
        })
    );

    let carrier_request = RuntimeValue::InvokeRequest(minimal_request(None));
    assert_eq!(
        encode_constructed_client_frame(
            &active,
            &registry,
            &ClientFrame::CallArgument {
                stream: 1,
                parameter: ParameterId::from_bytes([0x63; 16]),
                value: carrier_request,
            },
        ),
        Err(FrameCodecError::InvocationCarrierNotAccepted {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
        })
    );
    assert_eq!(
        encode_constructed_server_frame(
            &active,
            &registry,
            &ServerFrame::EventBatch {
                stream: 1,
                channel: Channel::ResultValues,
                events: vec![EventRecord {
                    sequence: 1,
                    event: Event::Value(RuntimeValue::InvokeRequest(minimal_request(None))),
                }],
            },
        ),
        Err(FrameCodecError::InvocationCarrierNotAccepted {
            carrier: SYS_INVOKE_REQUEST_TYPE_ID,
        })
    );
}

#[test]
fn catalogue_frames_have_exact_markers_and_enum_value_bytes() {
    let catalogue = enum_catalogue(&["lead", "qualified"]);
    let parameter = ParameterId::from_bytes([0x54; 16]);
    let value = RuntimeValue::Enum(EnumValue::new(&catalogue, ENUM_TYPE, "qualified").unwrap());
    let frame = ClientFrame::CallArgument {
        stream: 1,
        parameter,
        value: value.clone(),
    };
    let encoded = encode_catalogue_client_frame(&catalogue, &frame).unwrap();

    assert_eq!(&encoded[..4], b"ORF2");
    assert_eq!(&encoded[34..38], b"ORV2");
    assert_eq!(
        decode_catalogue_client_frame(&catalogue, &encoded),
        Ok(frame)
    );
    assert_eq!(
        decode_client_frame(&encoded),
        Err(FrameCodecError::InvalidMarker)
    );

    let server = ServerFrame::EventBatch {
        stream: 1,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: 1,
            event: Event::Value(value),
        }],
    };
    let encoded = encode_catalogue_server_frame(&catalogue, &server).unwrap();
    assert_eq!(&encoded[..4], b"ORF2");
    assert!(encoded.windows(4).any(|bytes| bytes == b"ORV2"));
    assert_eq!(
        decode_catalogue_server_frame(&catalogue, &encoded),
        Ok(server)
    );
    assert_eq!(
        decode_server_frame(&encoded),
        Err(FrameCodecError::InvalidMarker)
    );
}

#[test]
fn catalogue_connection_carries_enum_arguments_and_results_fail_closed() {
    let original = enum_catalogue(&["lead", "qualified"]);
    let active = enum_catalogue(&["lead", "customer"]);
    let function = FunctionId::from_bytes([0x55; 16]);
    let parameter = ParameterId::from_bytes([0x56; 16]);
    let stale = RuntimeValue::Enum(EnumValue::new(&original, ENUM_TYPE, "qualified").unwrap());
    let mut connection = ProtocolConnection::new();
    connection
        .receive_catalogue(
            &active,
            ClientFrame::CallRawStart {
                stream: 1,
                function,
            },
        )
        .unwrap();
    let before = connection.clone();
    assert_eq!(
        connection.receive_catalogue(
            &active,
            ClientFrame::CallArgument {
                stream: 1,
                parameter,
                value: stale,
            }
        ),
        Err(ConnectionError::InvalidFrame {
            source: FrameCodecError::Value {
                source: ValueCodecError::UndeclaredEnumLabel {
                    enum_type: ENUM_TYPE,
                    label: String::from("qualified"),
                },
            },
        })
    );
    assert_eq!(connection, before);

    let value = RuntimeValue::Enum(EnumValue::new(&active, ENUM_TYPE, "customer").unwrap());
    connection
        .receive_catalogue(
            &active,
            ClientFrame::CallArgument {
                stream: 1,
                parameter,
                value: value.clone(),
            },
        )
        .unwrap();
    connection
        .receive_catalogue(
            &active,
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: 1024,
            },
        )
        .unwrap();
    assert_eq!(
        connection
            .receive_catalogue(&active, ClientFrame::CallArgumentsComplete { stream: 1 })
            .unwrap(),
        Some(ClientAction::Dispatch {
            stream: 1,
            call: RawCall {
                function,
                arguments: vec![CallArgument {
                    parameter,
                    value: value.clone(),
                }],
            },
        })
    );
    let invocation = InvocationId::from_bytes([0x57; 16]);
    assert_eq!(
        connection
            .apply_catalogue(
                &active,
                ServerAction::Accepted {
                    stream: 1,
                    invocation,
                },
            )
            .unwrap(),
        ServerFrame::CallAccepted {
            stream: 1,
            invocation,
        }
    );
    let event = Event::Value(value);
    assert_eq!(
        connection
            .apply_catalogue(
                &active,
                ServerAction::Events {
                    stream: 1,
                    events: vec![event.clone()],
                },
            )
            .unwrap(),
        ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord { sequence: 1, event }],
        }
    );
}

#[test]
fn every_client_call_frame_has_exact_golden_bytes_and_round_trips() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let parameter = ParameterId::from_bytes([0x22; 16]);
    let value = RuntimeValue::Boolean(true);

    let cases = [
        (
            ClientFrame::CallRawStart {
                stream: 1,
                function,
            },
            [
                b"ORF1\x01\0".as_slice(),
                &1_u64.to_be_bytes(),
                &16_u32.to_be_bytes(),
                &[0x11; 16],
            ]
            .concat(),
        ),
        (
            ClientFrame::CallArgument {
                stream: 1,
                parameter,
                value: value.clone(),
            },
            [
                b"ORF1\x02\0".as_slice(),
                &1_u64.to_be_bytes(),
                &42_u32.to_be_bytes(),
                &[0x22; 16],
                b"ORV1\x02",
                &orna_standard::BOOLEAN_TYPE_ID.to_bytes(),
                &1_u32.to_be_bytes(),
                &[1],
            ]
            .concat(),
        ),
        (
            ClientFrame::CallArgumentsComplete { stream: 1 },
            [
                b"ORF1\x03\0".as_slice(),
                &1_u64.to_be_bytes(),
                &0_u32.to_be_bytes(),
            ]
            .concat(),
        ),
        (
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::Diagnostic,
                credit: 9,
            },
            [
                b"ORF1\x04\0".as_slice(),
                &1_u64.to_be_bytes(),
                &9_u32.to_be_bytes(),
                &[0x03],
                &9_u64.to_be_bytes(),
            ]
            .concat(),
        ),
        (
            ClientFrame::CallCancel { stream: 1 },
            [
                b"ORF1\x05\0".as_slice(),
                &1_u64.to_be_bytes(),
                &0_u32.to_be_bytes(),
            ]
            .concat(),
        ),
    ];

    for (frame, expected) in cases {
        assert_eq!(encode_client_frame(&frame), Ok(expected.clone()));
        assert_eq!(decode_client_frame(&expected), Ok(frame));
    }
}

#[test]
fn every_server_call_frame_has_exact_golden_bytes_and_round_trips() {
    let invocation = InvocationId::from_bytes([0x33; 16]);
    let accepted = ServerFrame::CallAccepted {
        stream: 1,
        invocation,
    };
    let expected_accepted = [
        b"ORF1\x81\0".as_slice(),
        &1_u64.to_be_bytes(),
        &16_u32.to_be_bytes(),
        &[0x33; 16],
    ]
    .concat();

    let events = ServerFrame::EventBatch {
        stream: 1,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: 1,
            event: Event::Value(RuntimeValue::Boolean(true)),
        }],
    };
    let expected_events = [
        b"ORF1\x82\0".as_slice(),
        &1_u64.to_be_bytes(),
        &42_u32.to_be_bytes(),
        &[0x01],
        &1_u16.to_be_bytes(),
        &1_u64.to_be_bytes(),
        &[0x01],
        &26_u32.to_be_bytes(),
        b"ORV1\x02",
        &orna_standard::BOOLEAN_TYPE_ID.to_bytes(),
        &1_u32.to_be_bytes(),
        &[1],
    ]
    .concat();

    let completed = ServerFrame::CallCompleted { stream: 1 };
    let expected_completed = [
        b"ORF1\x83\0".as_slice(),
        &1_u64.to_be_bytes(),
        &0_u32.to_be_bytes(),
    ]
    .concat();
    let failed = ServerFrame::CallFailed {
        stream: 1,
        failure: CallFailure::ExecuteDenied,
    };
    let expected_failed = [
        b"ORF1\x84\0".as_slice(),
        &1_u64.to_be_bytes(),
        &4_u32.to_be_bytes(),
        &[0x01, 0x00, 0x01, 0x00],
    ]
    .concat();
    let cancelled = ServerFrame::CallCancelled { stream: 1 };
    let expected_cancelled = [
        b"ORF1\x85\0".as_slice(),
        &1_u64.to_be_bytes(),
        &0_u32.to_be_bytes(),
    ]
    .concat();

    for (frame, expected) in [
        (accepted, expected_accepted),
        (events, expected_events),
        (completed, expected_completed),
        (failed, expected_failed),
        (cancelled, expected_cancelled),
    ] {
        assert_eq!(encode_server_frame(&frame), Ok(expected.clone()));
        assert_eq!(decode_server_frame(&expected), Ok(frame));
    }
}

#[test]
fn failures_and_event_kinds_use_only_the_closed_version_one_bytes() {
    for (failure, expected) in [
        (CallFailure::ExecuteDenied, [0x01, 0x00, 0x01, 0x00]),
        (CallFailure::TargetUnavailable, [0x02, 0x00, 0x01, 0x00]),
        (
            CallFailure::ClientEvaluationFailed,
            [0x03, 0x00, 0x01, 0x00],
        ),
        (CallFailure::InternalFailure, [0xff, 0x00, 0x01, 0x00]),
    ] {
        let frame = ServerFrame::CallFailed { stream: 1, failure };
        let encoded = encode_server_frame(&frame).unwrap();
        assert_eq!(&encoded[18..], &expected);
        assert_eq!(decode_server_frame(&encoded), Ok(frame));
    }

    for (channel, event) in [
        (Channel::ResultBytes, Event::Bytes(vec![0xaa, 0xbb])),
        (
            Channel::Diagnostic,
            Event::Failure(CallFailure::InternalFailure),
        ),
    ] {
        let frame = ServerFrame::EventBatch {
            stream: 1,
            channel,
            events: vec![EventRecord { sequence: 9, event }],
        };
        let encoded = encode_server_frame(&frame).unwrap();
        assert_eq!(decode_server_frame(&encoded), Ok(frame));
    }

    assert_eq!(
        CallFailure::from_wire([0x01, 0x00, 0x01, 0x01]),
        Err(FrameCodecError::InvalidFailure {
            bytes: [0x01, 0x00, 0x01, 0x01],
        })
    );
}

#[test]
fn codecs_reject_invalid_envelopes_payloads_and_event_shapes() {
    assert_eq!(
        decode_client_frame(b"ORF1"),
        Err(FrameCodecError::TruncatedHeader { actual: 4 })
    );
    let valid = encode_client_frame(&ClientFrame::Ping { token: [7; 8] }).unwrap();

    let mut invalid = valid.clone();
    invalid[0] = b'X';
    assert_eq!(
        decode_client_frame(&invalid),
        Err(FrameCodecError::InvalidMarker)
    );
    invalid = valid.clone();
    invalid[5] = 1;
    assert_eq!(
        decode_client_frame(&invalid),
        Err(FrameCodecError::NonZeroFlags { flags: 1 })
    );
    invalid = valid.clone();
    invalid[4] = CALL_RAW_START_TAG;
    assert_eq!(
        decode_client_frame(&invalid),
        Err(FrameCodecError::InvalidStream {
            tag: CALL_RAW_START_TAG,
            stream: 0,
        })
    );
    invalid = valid.clone();
    invalid[14..18].copy_from_slice(&((MAX_FRAME_PAYLOAD_LENGTH + 1) as u32).to_be_bytes());
    assert_eq!(
        decode_client_frame(&invalid),
        Err(FrameCodecError::PayloadTooLarge {
            actual: MAX_FRAME_PAYLOAD_LENGTH + 1,
            maximum: MAX_FRAME_PAYLOAD_LENGTH,
        })
    );
    invalid = valid[..valid.len() - 1].to_vec();
    assert_eq!(
        decode_client_frame(&invalid),
        Err(FrameCodecError::TruncatedPayload {
            declared: 8,
            actual: 7,
        })
    );
    invalid = valid.clone();
    invalid.push(0);
    assert_eq!(
        decode_client_frame(&invalid),
        Err(FrameCodecError::TrailingBytes {
            declared: 8,
            actual: 9,
        })
    );
    invalid = valid.clone();
    invalid[4] = 0x7f;
    assert_eq!(
        decode_client_frame(&invalid),
        Err(FrameCodecError::UnknownTag { tag: 0x7f })
    );

    let mut window = [
        b"ORF1\x04\0".as_slice(),
        &1_u64.to_be_bytes(),
        &9_u32.to_be_bytes(),
        &[0xff],
        &1_u64.to_be_bytes(),
    ]
    .concat();
    assert_eq!(
        decode_client_frame(&window),
        Err(FrameCodecError::UnknownChannel { value: 0xff })
    );
    window[19..27].copy_from_slice(&0_u64.to_be_bytes());
    window[18] = Channel::ResultValues.wire();
    assert_eq!(
        decode_client_frame(&window),
        Err(FrameCodecError::ZeroWindowCredit)
    );

    let mut argument = encode_client_frame(&ClientFrame::CallArgument {
        stream: 1,
        parameter: ParameterId::from_bytes([1; 16]),
        value: RuntimeValue::Boolean(true),
    })
    .unwrap();
    argument[34] = b'X';
    assert_eq!(
        decode_client_frame(&argument),
        Err(FrameCodecError::Value {
            source: ValueCodecError::InvalidMarker,
        })
    );

    assert_eq!(
        encode_server_frame(&ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultBytes,
            events: vec![],
        }),
        Err(FrameCodecError::EmptyEventBatch)
    );
    assert_eq!(
        encode_server_frame(&ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultBytes,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Bytes(vec![]),
            }],
        }),
        Err(FrameCodecError::EmptyByteChunk)
    );
    assert_eq!(
        encode_server_frame(&ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::Diagnostic,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Bytes(vec![1]),
            }],
        }),
        Err(FrameCodecError::InvalidEventChannel {
            channel: Channel::Diagnostic,
            kind: 0x02,
        })
    );
}

#[test]
fn connection_dispatches_sorted_arguments_and_closes_the_stream() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let low = ParameterId::from_bytes([0x10; 16]);
    let high = ParameterId::from_bytes([0x20; 16]);
    let invocation = InvocationId::from_bytes([0x33; 16]);
    let mut connection = ProtocolConnection::new();

    assert_eq!(
        connection.receive(ClientFrame::CallRawStart {
            stream: 1,
            function,
        }),
        Ok(None)
    );
    assert_eq!(
        connection.receive(ClientFrame::CallArgument {
            stream: 1,
            parameter: high,
            value: RuntimeValue::Boolean(true),
        }),
        Ok(None)
    );
    assert_eq!(
        connection.receive(ClientFrame::CallArgument {
            stream: 1,
            parameter: low,
            value: RuntimeValue::Integer(7),
        }),
        Ok(None)
    );
    assert_eq!(
        connection.receive(ClientFrame::CallArgumentsComplete { stream: 1 }),
        Ok(Some(ClientAction::Dispatch {
            stream: 1,
            call: RawCall {
                function,
                arguments: vec![
                    CallArgument {
                        parameter: low,
                        value: RuntimeValue::Integer(7),
                    },
                    CallArgument {
                        parameter: high,
                        value: RuntimeValue::Boolean(true),
                    },
                ],
            },
        }))
    );
    assert_eq!(
        connection.apply(ServerAction::Accepted {
            stream: 1,
            invocation,
        }),
        Ok(ServerFrame::CallAccepted {
            stream: 1,
            invocation,
        })
    );
    assert_eq!(
        connection.apply(ServerAction::Completed { stream: 1 }),
        Ok(ServerFrame::CallCompleted { stream: 1 })
    );
    assert_eq!(connection.live_streams(), 0);
    assert_eq!(connection.high_water_mark(), Some(1));
    assert_eq!(
        connection.receive(ClientFrame::CallRawStart {
            stream: 1,
            function,
        }),
        Err(ConnectionError::StreamNotIncreasing {
            stream: 1,
            previous: 1,
        })
    );
}

#[test]
fn cancellation_distinguishes_receiving_dispatching_and_running_calls() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let invocation = InvocationId::from_bytes([0x33; 16]);
    let mut connection = ProtocolConnection::new();

    connection
        .receive(ClientFrame::CallRawStart {
            stream: 1,
            function,
        })
        .unwrap();
    assert_eq!(
        connection.receive(ClientFrame::CallCancel { stream: 1 }),
        Ok(Some(ClientAction::Send(ServerFrame::CallCancelled {
            stream: 1,
        })))
    );

    connection
        .receive(ClientFrame::CallRawStart {
            stream: 2,
            function,
        })
        .unwrap();
    connection
        .receive(ClientFrame::CallArgumentsComplete { stream: 2 })
        .unwrap();
    assert_eq!(
        connection.receive(ClientFrame::CallCancel { stream: 2 }),
        Ok(Some(ClientAction::Cancel {
            stream: 2,
            invocation: None,
        }))
    );
    assert_eq!(
        connection.apply(ServerAction::Accepted {
            stream: 2,
            invocation,
        }),
        Err(ConnectionError::WrongState { stream: 2 })
    );
    assert_eq!(
        connection.apply(ServerAction::Cancelled { stream: 2 }),
        Ok(ServerFrame::CallCancelled { stream: 2 })
    );

    connection
        .receive(ClientFrame::CallRawStart {
            stream: 3,
            function,
        })
        .unwrap();
    connection
        .receive(ClientFrame::CallArgumentsComplete { stream: 3 })
        .unwrap();
    connection
        .apply(ServerAction::Accepted {
            stream: 3,
            invocation,
        })
        .unwrap();
    assert_eq!(
        connection.receive(ClientFrame::CallCancel { stream: 3 }),
        Ok(Some(ClientAction::Cancel {
            stream: 3,
            invocation: Some(invocation),
        }))
    );
    assert_eq!(
        connection.receive(ClientFrame::CallCancel { stream: 3 }),
        Err(ConnectionError::WrongState { stream: 3 })
    );
    assert_eq!(
        connection.apply(ServerAction::Cancelled { stream: 3 }),
        Ok(ServerFrame::CallCancelled { stream: 3 })
    );
    assert_eq!(connection.live_streams(), 0);
}

#[test]
fn event_windows_start_at_zero_and_consume_the_exact_payload() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let invocation = InvocationId::from_bytes([0x33; 16]);
    let mut connection = ProtocolConnection::new();
    connection
        .receive(ClientFrame::CallRawStart {
            stream: 1,
            function,
        })
        .unwrap();
    connection
        .receive(ClientFrame::CallArgumentsComplete { stream: 1 })
        .unwrap();
    connection
        .apply(ServerAction::Accepted {
            stream: 1,
            invocation,
        })
        .unwrap();

    let event = Event::Value(RuntimeValue::Boolean(true));
    assert_eq!(
        connection.apply(ServerAction::Events {
            stream: 1,
            events: vec![event.clone()],
        }),
        Err(ConnectionError::InsufficientCredit {
            stream: 1,
            channel: Channel::ResultValues,
            available: 0,
            required: 42,
        })
    );
    connection
        .receive(ClientFrame::WindowUpdate {
            stream: 1,
            channel: Channel::ResultValues,
            credit: 42,
        })
        .unwrap();
    assert_eq!(
        connection.apply(ServerAction::Events {
            stream: 1,
            events: vec![event.clone()],
        }),
        Ok(ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: event.clone(),
            }],
        })
    );
    assert_eq!(
        connection.apply(ServerAction::Events {
            stream: 1,
            events: vec![event.clone()],
        }),
        Err(ConnectionError::InsufficientCredit {
            stream: 1,
            channel: Channel::ResultValues,
            available: 0,
            required: 42,
        })
    );
    connection
        .receive(ClientFrame::WindowUpdate {
            stream: 1,
            channel: Channel::ResultValues,
            credit: 42,
        })
        .unwrap();
    assert_eq!(
        connection.apply(ServerAction::Events {
            stream: 1,
            events: vec![event.clone()],
        }),
        Ok(ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord { sequence: 2, event }],
        })
    );
}

#[test]
fn result_credit_reports_live_window_without_mutating_state() {
    let mut connection = ProtocolConnection::new();
    connection
        .receive(ClientFrame::CallRawStart {
            stream: 1,
            function: FunctionId::from_bytes([0x11; 16]),
        })
        .unwrap();

    assert_eq!(connection.result_credit(1), Ok(0));
    connection
        .receive(ClientFrame::WindowUpdate {
            stream: 1,
            channel: Channel::ResultValues,
            credit: 42,
        })
        .unwrap();
    assert_eq!(connection.result_credit(1), Ok(42));
    let after_update = connection.clone();
    assert_eq!(connection.result_credit(1), Ok(42));
    assert_eq!(connection, after_update);
    assert_eq!(
        connection.result_credit(99),
        Err(ConnectionError::UnknownStream { stream: 99 })
    );
}

#[test]
fn raw_call_client_starts_exactly_and_preserves_ordered_values() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let invocation = InvocationId::from_bytes([0x22; 16]);
    let (mut client, frames) = RawCallClient::start(function);
    assert_eq!(
        frames,
        [
            ClientFrame::CallRawStart {
                stream: 1,
                function,
            },
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: MAX_CHANNEL_WINDOW,
            },
            ClientFrame::CallArgumentsComplete { stream: 1 },
        ]
    );
    assert_eq!(
        client
            .receive_encoded(
                &encode_server_frame(&ServerFrame::CallAccepted {
                    stream: 1,
                    invocation,
                })
                .unwrap(),
            )
            .unwrap(),
        RawCallClientResponse::Accepted { invocation }
    );
    assert_eq!(
        client
            .receive_encoded(
                &encode_server_frame(&ServerFrame::EventBatch {
                    stream: 1,
                    channel: Channel::ResultValues,
                    events: vec![
                        EventRecord {
                            sequence: 1,
                            event: Event::Value(RuntimeValue::Boolean(true)),
                        },
                        EventRecord {
                            sequence: 2,
                            event: Event::Value(RuntimeValue::Integer(7)),
                        },
                    ],
                })
                .unwrap(),
            )
            .unwrap(),
        RawCallClientResponse::Values(vec![RuntimeValue::Boolean(true), RuntimeValue::Integer(7),])
    );
    assert_eq!(
        client
            .receive_encoded(
                &encode_server_frame(&ServerFrame::CallCompleted { stream: 1 }).unwrap(),
            )
            .unwrap(),
        RawCallClientResponse::Completed
    );
    assert_eq!(
        client.receive_encoded(
            &encode_server_frame(&ServerFrame::CallCompleted { stream: 1 }).unwrap()
        ),
        Err(RawCallClientError::WrongState)
    );
}

#[test]
fn raw_call_client_closes_failures_and_cancellation() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let invocation = InvocationId::from_bytes([0x22; 16]);
    let accepted = encode_server_frame(&ServerFrame::CallAccepted {
        stream: 1,
        invocation,
    })
    .unwrap();

    let (mut failed, _) = RawCallClient::start(function);
    failed.receive_encoded(&accepted).unwrap();
    assert_eq!(
        failed
            .receive_encoded(
                &encode_server_frame(&ServerFrame::CallFailed {
                    stream: 1,
                    failure: CallFailure::ExecuteDenied,
                })
                .unwrap(),
            )
            .unwrap(),
        RawCallClientResponse::Failed(CallFailure::ExecuteDenied)
    );

    let (mut cancelled, _) = RawCallClient::start(function);
    cancelled.receive_encoded(&accepted).unwrap();
    assert_eq!(
        cancelled.request_cancellation().unwrap(),
        ClientFrame::CallCancel { stream: 1 }
    );

    let (mut cancelled_before_acceptance, _) = RawCallClient::start(function);
    cancelled_before_acceptance.request_cancellation().unwrap();
    assert_eq!(
        cancelled_before_acceptance
            .receive_encoded(
                &encode_server_frame(&ServerFrame::CallCancelled { stream: 1 }).unwrap(),
            )
            .unwrap(),
        RawCallClientResponse::Cancelled
    );
    assert_eq!(
        cancelled.request_cancellation(),
        Err(RawCallClientError::WrongState)
    );
    assert_eq!(
        cancelled
            .receive_encoded(
                &encode_server_frame(&ServerFrame::CallCancelled { stream: 1 }).unwrap(),
            )
            .unwrap(),
        RawCallClientResponse::Cancelled
    );
}

#[test]
fn raw_call_client_accepts_pre_acceptance_failure_as_terminal_without_state_change() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let failure = encode_server_frame(&ServerFrame::CallFailed {
        stream: 1,
        failure: CallFailure::TargetUnavailable,
    })
    .unwrap();
    let (mut client, _) = RawCallClient::start(function);

    assert_eq!(
        client.receive_encoded(&failure).unwrap(),
        RawCallClientResponse::Failed(CallFailure::TargetUnavailable)
    );
    assert_eq!(
        client.request_cancellation(),
        Err(RawCallClientError::WrongState)
    );

    let terminal = client.clone();
    assert_eq!(
        client.receive_encoded(&failure),
        Err(RawCallClientError::WrongState)
    );
    assert_eq!(client, terminal);

    let (mut cancelled, _) = RawCallClient::start(function);
    cancelled.request_cancellation().unwrap();
    let before_cancelled_failure = cancelled.clone();
    assert_eq!(
        cancelled.receive_encoded(&failure),
        Err(RawCallClientError::WrongState)
    );
    assert_eq!(cancelled, before_cancelled_failure);
}

#[test]
fn raw_call_client_rejects_late_acceptance_after_cancellation_without_state_change() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let invocation = InvocationId::from_bytes([0x22; 16]);
    let accepted = encode_server_frame(&ServerFrame::CallAccepted {
        stream: 1,
        invocation,
    })
    .unwrap();
    let (mut client, _) = RawCallClient::start(function);
    client.request_cancellation().unwrap();
    let before = client.clone();

    assert_eq!(
        client.receive_encoded(&accepted),
        Err(RawCallClientError::WrongState)
    );
    assert_eq!(client, before);
}

#[test]
fn raw_call_client_rejects_every_response_boundary() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let invocation = InvocationId::from_bytes([0x22; 16]);
    let accepted =
        |stream| encode_server_frame(&ServerFrame::CallAccepted { stream, invocation }).unwrap();
    let event = |stream, channel, sequence, event| {
        encode_server_frame(&ServerFrame::EventBatch {
            stream,
            channel,
            events: vec![EventRecord { sequence, event }],
        })
        .unwrap()
    };

    let (mut client, _) = RawCallClient::start(function);
    assert_eq!(
        client.receive_encoded(&accepted(2)),
        Err(RawCallClientError::WrongStream {
            expected: 1,
            actual: 2,
        })
    );
    assert_eq!(
        client.receive_encoded(&event(
            1,
            Channel::ResultValues,
            1,
            Event::Value(RuntimeValue::Boolean(true)),
        )),
        Err(RawCallClientError::WrongState)
    );
    client.receive_encoded(&accepted(1)).unwrap();
    assert_eq!(
        client.receive_encoded(&event(1, Channel::ResultBytes, 1, Event::Bytes(vec![1]),)),
        Err(RawCallClientError::WrongChannel {
            actual: Channel::ResultBytes,
        })
    );
    assert_eq!(
        client.receive_encoded(&event(
            1,
            Channel::ResultValues,
            2,
            Event::Value(RuntimeValue::Boolean(true)),
        )),
        Err(RawCallClientError::WrongSequence {
            expected: 1,
            actual: 2,
        })
    );
    assert_eq!(
        client.receive_encoded(&event(
            1,
            Channel::Diagnostic,
            1,
            Event::Failure(CallFailure::InternalFailure),
        )),
        Err(RawCallClientError::WrongChannel {
            actual: Channel::Diagnostic,
        })
    );
    assert_eq!(
        client.receive_encoded(
            &encode_server_frame(&ServerFrame::CallCancelled { stream: 1 }).unwrap()
        ),
        Err(RawCallClientError::WrongState)
    );

    let mut wrong_marker = accepted(1);
    wrong_marker[..4].copy_from_slice(b"ORF2");
    let (mut marker_client, _) = RawCallClient::start(function);
    assert!(matches!(
        marker_client.receive_encoded(&wrong_marker),
        Err(RawCallClientError::Frame {
            source: FrameCodecError::InvalidMarker,
        })
    ));
}

#[test]
fn raw_call_client_accepts_the_terminal_sequence_once() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let invocation = InvocationId::from_bytes([0x22; 16]);
    let (mut client, _) = RawCallClient::start(function);
    client
        .receive_encoded(
            &encode_server_frame(&ServerFrame::CallAccepted {
                stream: 1,
                invocation,
            })
            .unwrap(),
        )
        .unwrap();
    client.next_sequence = Some(u64::MAX);
    let terminal = encode_server_frame(&ServerFrame::EventBatch {
        stream: 1,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: u64::MAX,
            event: Event::Value(RuntimeValue::Boolean(true)),
        }],
    })
    .unwrap();
    assert_eq!(
        client.receive_encoded(&terminal).unwrap(),
        RawCallClientResponse::Values(vec![RuntimeValue::Boolean(true)])
    );
    assert_eq!(
        client.receive_encoded(&terminal),
        Err(RawCallClientError::SequenceExhausted)
    );
}

#[test]
fn raw_call_client_charges_the_exact_event_payload_credit() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let invocation = InvocationId::from_bytes([0x22; 16]);
    let accepted = encode_server_frame(&ServerFrame::CallAccepted {
        stream: 1,
        invocation,
    })
    .unwrap();
    let event = encode_server_frame(&ServerFrame::EventBatch {
        stream: 1,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: 1,
            event: Event::Value(RuntimeValue::Boolean(true)),
        }],
    })
    .unwrap();
    let required = u64::try_from(event.len() - HEADER_LENGTH).unwrap();

    let (mut exact, _) = RawCallClient::start(function);
    exact.receive_encoded(&accepted).unwrap();
    exact.remaining_result_credit = required;
    assert_eq!(
        exact.receive_encoded(&event).unwrap(),
        RawCallClientResponse::Values(vec![RuntimeValue::Boolean(true)])
    );
    assert_eq!(exact.remaining_result_credit, 0);

    let (mut short, _) = RawCallClient::start(function);
    short.receive_encoded(&accepted).unwrap();
    short.remaining_result_credit = required - 1;
    let before = short.clone();
    assert_eq!(
        short.receive_encoded(&event),
        Err(RawCallClientError::InsufficientCredit {
            available: required - 1,
            required,
        })
    );
    assert_eq!(short, before);
}

#[test]
fn sixty_four_interleaved_streams_keep_all_call_state_independent() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let mut connection = ProtocolConnection::new();
    for stream in 1_u64..=64 {
        let parameter = ParameterId::from_bytes([stream as u8; 16]);
        connection
            .receive(ClientFrame::CallRawStart { stream, function })
            .unwrap();
        connection
            .receive(ClientFrame::CallArgument {
                stream,
                parameter,
                value: RuntimeValue::Integer(stream as i32),
            })
            .unwrap();
        connection
            .receive(ClientFrame::WindowUpdate {
                stream,
                channel: Channel::ResultBytes,
                credit: 18 + stream,
            })
            .unwrap();
        assert_eq!(
            connection
                .receive(ClientFrame::CallArgumentsComplete { stream })
                .unwrap(),
            Some(ClientAction::Dispatch {
                stream,
                call: RawCall {
                    function,
                    arguments: vec![CallArgument {
                        parameter,
                        value: RuntimeValue::Integer(stream as i32),
                    }],
                },
            })
        );
    }
    for stream in (1_u64..=64).rev() {
        let invocation = InvocationId::from_bytes([stream as u8; 16]);
        connection
            .apply(ServerAction::Accepted { stream, invocation })
            .unwrap();
        assert_eq!(
            connection.apply(ServerAction::Events {
                stream,
                events: vec![Event::Bytes(vec![0xaa, 0xbb])],
            }),
            Ok(ServerFrame::EventBatch {
                stream,
                channel: Channel::ResultBytes,
                events: vec![EventRecord {
                    sequence: 1,
                    event: Event::Bytes(vec![0xaa, 0xbb]),
                }],
            })
        );
    }
    for stream in 1_u64..=64 {
        let state = connection.streams.get(&stream).expect("stream is live");
        assert!(matches!(state.phase, Phase::Running { .. }));
        assert_eq!(state.last_sequence, 1);
        assert_eq!(state.windows[channel_index(Channel::ResultBytes)], stream);
    }
}

#[test]
fn event_sequence_exhaustion_fails_without_consuming_credit() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let invocation = InvocationId::from_bytes([0x33; 16]);
    let mut connection = ProtocolConnection::new();
    connection
        .receive(ClientFrame::CallRawStart {
            stream: 1,
            function,
        })
        .unwrap();
    connection
        .receive(ClientFrame::WindowUpdate {
            stream: 1,
            channel: Channel::ResultBytes,
            credit: 36,
        })
        .unwrap();
    connection
        .receive(ClientFrame::CallArgumentsComplete { stream: 1 })
        .unwrap();
    connection
        .apply(ServerAction::Accepted {
            stream: 1,
            invocation,
        })
        .unwrap();
    connection
        .streams
        .get_mut(&1)
        .expect("stream is live")
        .last_sequence = u64::MAX - 1;

    assert_eq!(
        connection.apply(ServerAction::Events {
            stream: 1,
            events: vec![Event::Bytes(vec![0xaa, 0xbb])],
        }),
        Ok(ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultBytes,
            events: vec![EventRecord {
                sequence: u64::MAX,
                event: Event::Bytes(vec![0xaa, 0xbb]),
            }],
        })
    );
    assert_eq!(
        connection.apply(ServerAction::Events {
            stream: 1,
            events: vec![Event::Bytes(vec![0xaa, 0xbb])],
        }),
        Err(ConnectionError::EventSequenceExhausted { stream: 1 })
    );
    assert_eq!(
        connection
            .streams
            .get(&1)
            .expect("failed event retains stream")
            .windows[channel_index(Channel::ResultBytes)],
        18
    );
}

#[test]
fn window_and_transition_errors_leave_the_complete_state_unchanged() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let parameter = ParameterId::from_bytes([0x22; 16]);
    let mut connection = ProtocolConnection::new();
    connection
        .receive(ClientFrame::CallRawStart {
            stream: 1,
            function,
        })
        .unwrap();
    connection
        .receive(ClientFrame::CallArgument {
            stream: 1,
            parameter,
            value: RuntimeValue::Boolean(true),
        })
        .unwrap();
    let before_duplicate = connection.clone();
    assert_eq!(
        connection.receive(ClientFrame::CallArgument {
            stream: 1,
            parameter,
            value: RuntimeValue::Boolean(false),
        }),
        Err(ConnectionError::DuplicateArgument {
            stream: 1,
            parameter,
        })
    );
    assert_eq!(connection, before_duplicate);

    connection
        .receive(ClientFrame::WindowUpdate {
            stream: 1,
            channel: Channel::Diagnostic,
            credit: MAX_CHANNEL_WINDOW,
        })
        .unwrap();
    let before_overflow = connection.clone();
    assert_eq!(
        connection.receive(ClientFrame::WindowUpdate {
            stream: 1,
            channel: Channel::Diagnostic,
            credit: 1,
        }),
        Err(ConnectionError::WindowOverflow {
            stream: 1,
            channel: Channel::Diagnostic,
        })
    );
    assert_eq!(connection, before_overflow);

    connection
        .receive(ClientFrame::CallArgumentsComplete { stream: 1 })
        .unwrap();
    let before_wrong_state = connection.clone();
    assert_eq!(
        connection.receive(ClientFrame::CallArgument {
            stream: 1,
            parameter: ParameterId::from_bytes([0x23; 16]),
            value: RuntimeValue::Boolean(true),
        }),
        Err(ConnectionError::WrongState { stream: 1 })
    );
    assert_eq!(connection, before_wrong_state);
    assert_eq!(
        connection.apply(ServerAction::Completed { stream: 1 }),
        Err(ConnectionError::WrongState { stream: 1 })
    );
    assert_eq!(connection, before_wrong_state);
}

#[test]
fn connection_and_argument_limits_fail_without_changing_prior_state() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let mut streams = ProtocolConnection::new();
    for stream in 1..=64 {
        assert_eq!(
            streams.receive(ClientFrame::CallRawStart { stream, function }),
            Ok(None)
        );
    }
    assert_eq!(
        streams.receive(ClientFrame::CallRawStart {
            stream: 65,
            function,
        }),
        Err(ConnectionError::TooManyLiveStreams)
    );
    assert_eq!(streams.high_water_mark(), Some(64));
    streams
        .receive(ClientFrame::CallCancel { stream: 1 })
        .unwrap();
    assert_eq!(
        streams.receive(ClientFrame::CallRawStart {
            stream: 65,
            function,
        }),
        Ok(None)
    );

    let mut exhausted = ProtocolConnection::new();
    exhausted
        .receive(ClientFrame::CallRawStart {
            stream: u64::MAX,
            function,
        })
        .unwrap();
    exhausted
        .receive(ClientFrame::CallCancel { stream: u64::MAX })
        .unwrap();
    assert_eq!(
        exhausted.receive(ClientFrame::CallRawStart {
            stream: u64::MAX,
            function,
        }),
        Err(ConnectionError::StreamNumberExhausted)
    );

    let null = RuntimeValue::null(orna_core::types::ResolvedType::scalar(
        orna_core::types::StandardScalar::Boolean,
    ))
    .unwrap();
    let mut count = ProtocolConnection::new();
    count
        .receive(ClientFrame::CallRawStart {
            stream: 1,
            function,
        })
        .unwrap();
    for index in 0_u16..256 {
        let mut bytes = [0; 16];
        bytes[14..].copy_from_slice(&index.to_be_bytes());
        assert_eq!(
            count.receive(ClientFrame::CallArgument {
                stream: 1,
                parameter: ParameterId::from_bytes(bytes),
                value: null.clone(),
            }),
            Ok(None)
        );
    }
    assert_eq!(
        count.receive(ClientFrame::CallArgument {
            stream: 1,
            parameter: ParameterId::from_bytes([0xff; 16]),
            value: null.clone(),
        }),
        Err(ConnectionError::TooManyArguments { stream: 1 })
    );

    let mut bytes = ProtocolConnection::new();
    bytes
        .receive(ClientFrame::CallRawStart {
            stream: 1,
            function,
        })
        .unwrap();
    bytes
        .receive(ClientFrame::CallArgument {
            stream: 1,
            parameter: ParameterId::from_bytes([1; 16]),
            value: RuntimeValue::Bytes(vec![0; MAX_FRAME_PAYLOAD_LENGTH - 82]),
        })
        .unwrap();
    bytes
        .receive(ClientFrame::CallArgument {
            stream: 1,
            parameter: ParameterId::from_bytes([2; 16]),
            value: null.clone(),
        })
        .unwrap();
    assert_eq!(
        bytes.receive(ClientFrame::CallArgument {
            stream: 1,
            parameter: ParameterId::from_bytes([3; 16]),
            value: null,
        }),
        Err(ConnectionError::ArgumentsTooLarge { stream: 1 })
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn arbitrary_frame_bytes_never_panic(encoded in prop::collection::vec(any::<u8>(), 0..8192)) {
        let _ = decode_client_frame(&encoded);
        let _ = decode_server_frame(&encoded);
    }

    #[test]
    fn marker_valid_arbitrary_frames_never_panic(
        tag in any::<u8>(),
        flags in any::<u8>(),
        stream in any::<u64>(),
        declared in 0_u32..8192,
        payload in prop::collection::vec(any::<u8>(), 0..8192),
    ) {
        let mut encoded = b"ORF1".to_vec();
        encoded.push(tag);
        encoded.push(flags);
        encoded.extend_from_slice(&stream.to_be_bytes());
        encoded.extend_from_slice(&declared.to_be_bytes());
        encoded.extend_from_slice(&payload);
        let _ = decode_client_frame(&encoded);
        let _ = decode_server_frame(&encoded);
    }

    #[test]
    fn arbitrary_typed_actions_preserve_all_connection_bounds(
        operations in prop::collection::vec((any::<u8>(), any::<u16>(), any::<u8>()), 0..512),
    ) {
        let mut connection = ProtocolConnection::new();
        let mut observed_high_water = None;
        for (operation, number, value) in operations {
            let stream = u64::from(number) + 1;
            let function = FunctionId::from_bytes([value; 16]);
            let invocation = InvocationId::from_bytes([value; 16]);
            let parameter = ParameterId::from_bytes([value; 16]);
            let before = connection.clone();
            let failed = match operation % 11 {
                0 => connection
                    .receive(ClientFrame::CallRawStart { stream, function })
                    .is_err(),
                1 => connection
                    .receive(ClientFrame::CallArgument {
                        stream,
                        parameter,
                        value: RuntimeValue::Boolean(value & 1 == 1),
                    })
                    .is_err(),
                2 => connection
                    .receive(ClientFrame::CallArgumentsComplete { stream })
                    .is_err(),
                3 => connection
                    .receive(ClientFrame::WindowUpdate {
                        stream,
                        channel: Channel::ResultValues,
                        credit: u64::from(value) + 1,
                    })
                    .is_err(),
                4 => connection
                    .receive(ClientFrame::CallCancel { stream })
                    .is_err(),
                5 => connection
                    .apply(ServerAction::Accepted { stream, invocation })
                    .is_err(),
                6 => connection
                    .apply(ServerAction::Events {
                        stream,
                        events: vec![Event::Value(RuntimeValue::Boolean(value & 1 == 1))],
                    })
                    .is_err(),
                7 => connection
                    .apply(ServerAction::Completed { stream })
                    .is_err(),
                8 => connection
                    .apply(ServerAction::Failed {
                        stream,
                        failure: CallFailure::InternalFailure,
                    })
                    .is_err(),
                9 => connection
                    .apply(ServerAction::Cancelled { stream })
                    .is_err(),
                _ => connection
                    .receive(ClientFrame::Ping { token: [value; 8] })
                    .is_err(),
            };
            if failed {
                prop_assert_eq!(&connection, &before);
            }

            prop_assert!(connection.live_streams() <= MAX_LIVE_STREAMS);
            prop_assert!(connection.high_water_mark() >= observed_high_water);
            observed_high_water = connection.high_water_mark();
            for state in connection.streams.values() {
                prop_assert!(state
                    .windows
                    .iter()
                    .all(|window| *window <= MAX_CHANNEL_WINDOW));
                if let Phase::Receiving {
                    arguments,
                    argument_bytes,
                    ..
                } = &state.phase
                {
                    prop_assert!(arguments.len() <= MAX_ARGUMENTS);
                    prop_assert!(*argument_bytes <= MAX_ARGUMENT_BYTES);
                }
            }
        }
    }
}

fn resource_revision_fixture() -> RevisionPair {
    RevisionPair::new(
        SourceRevisionId::from_bytes([0x06; 16]),
        CatalogueRevisionId::from_bytes([0x07; 16]),
    )
}

fn resource_other_revision_fixture() -> RevisionPair {
    RevisionPair::new(
        SourceRevisionId::from_bytes([0x16; 16]),
        CatalogueRevisionId::from_bytes([0x17; 16]),
    )
}

fn resource_request_fixture() -> ResourceRequest {
    ResourceRequest {
        stream_id: 1,
        request_id: InvocationId::from_bytes([0x02; 16]),
        parent_invocation_id: InvocationId::from_bytes([0x03; 16]),
        call_site_id: orna_core::CallSiteId::from_bytes([0x04; 16]),
        state_profile: String::new(),
        function_instance_key: String::new(),
        target_function_id: FunctionId::from_bytes([0x05; 16]),
        target_revision: RevisionPair::new(
            SourceRevisionId::from_bytes([0x06; 16]),
            CatalogueRevisionId::from_bytes([0x07; 16]),
        ),
        generation: 9,
        resource_kind: ResourceKind::Single,
        arguments: Vec::new(),
        item_window: 10,
        byte_window: 11,
    }
}

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

#[test]
fn call_accepted_rejects_zero_invocation_id_without_mutating_state() {
    let function = FunctionId::from_bytes([0x11; 16]);
    let zero = InvocationId::from_bytes([0; 16]);
    let valid = InvocationId::from_bytes([0x22; 16]);
    let zero_frame = ServerFrame::CallAccepted {
        stream: 1,
        invocation: zero,
    };
    assert_eq!(
        encode_server_frame(&zero_frame),
        Err(FrameCodecError::ZeroInvocationId)
    );

    let zero_encoded = [
        b"ORF1\x81\0".as_slice(),
        &1_u64.to_be_bytes(),
        &16_u32.to_be_bytes(),
        &[0; 16],
    ]
    .concat();
    assert_eq!(
        decode_server_frame(&zero_encoded),
        Err(FrameCodecError::ZeroInvocationId)
    );

    let mut connection = ProtocolConnection::new();
    connection
        .receive(ClientFrame::CallRawStart {
            stream: 1,
            function,
        })
        .unwrap();
    connection
        .receive(ClientFrame::CallArgumentsComplete { stream: 1 })
        .unwrap();
    let before = connection.clone();
    assert_eq!(
        connection.apply(ServerAction::Accepted {
            stream: 1,
            invocation: zero,
        }),
        Err(ConnectionError::InvalidFrame {
            source: FrameCodecError::ZeroInvocationId,
        })
    );
    assert_eq!(connection, before);
    assert_eq!(
        connection.apply(ServerAction::Accepted {
            stream: 1,
            invocation: valid,
        }),
        Ok(ServerFrame::CallAccepted {
            stream: 1,
            invocation: valid,
        })
    );

    let (mut client, _) = RawCallClient::start(function);
    let before = client.clone();
    assert_eq!(
        client.receive_encoded(&zero_encoded),
        Err(RawCallClientError::Frame {
            source: FrameCodecError::ZeroInvocationId,
        })
    );
    assert_eq!(client, before);
    let valid_encoded = encode_server_frame(&ServerFrame::CallAccepted {
        stream: 1,
        invocation: valid,
    })
    .unwrap();
    assert_eq!(
        client.receive_encoded(&valid_encoded).unwrap(),
        RawCallClientResponse::Accepted { invocation: valid }
    );
}

#[test]
fn invocation_client_starts_on_a_constructed_stream() {
    let active = empty_active_revision();
    let registry = test_registry();
    let retained =
        encode_invoke_request(&active, &registry, &minimal_request(None)).expect("request");
    let (client, frames) = InvocationClient::start(retained.clone());

    assert_eq!(frames.len(), 4);
    assert_eq!(
        frames[0],
        ClientFrame::CallRawStart {
            stream: 1,
            function: SYS_INVOKE_FUNCTION_ID,
        }
    );
    assert_eq!(
        frames[1],
        ClientFrame::WindowUpdate {
            stream: 1,
            channel: Channel::ResultValues,
            credit: MAX_CHANNEL_WINDOW,
        }
    );
    assert!(matches!(
        &frames[2],
        ClientFrame::CallInvokeRequest { stream: 1, request } if request == &retained
    ));
    assert_eq!(frames[3], ClientFrame::CallArgumentsComplete { stream: 1 });
    let mut server = ProtocolConnection::new();
    for (index, frame) in frames.iter().cloned().enumerate() {
        let action = server
            .receive_constructed(&active, &registry, frame)
            .expect("server accepts invocation startup frame");
        if index == 3 {
            assert!(matches!(
                action,
                Some(ClientAction::InvokeDispatch { stream: 1, .. }),
            ));
        } else {
            assert!(action.is_none());
        }
    }
    assert_eq!(
        InvocationClient::start_on_stream(0, retained),
        Err(InvocationClientError::InvalidStream),
    );
    assert_eq!(
        client,
        InvocationClient {
            stream: 1,
            phase: InvocationClientPhase::AwaitingAcceptance,
            cancellation_requested: false,
            invocation: None,
            next_outer_sequence: Some(1),
            next_inner_sequence: None,
            remaining_result_credit: MAX_CHANNEL_WINDOW,
        },
    );
}

#[test]
fn invocation_client_validates_split_event_batches_and_terminal_completion() {
    let active = empty_active_revision();
    let registry = test_registry();
    let retained =
        encode_invoke_request(&active, &registry, &minimal_request(None)).expect("request");
    let (mut client, _) = InvocationClient::start(retained);
    let invocation = InvocationId::from_bytes([0x72; 16]);
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .expect("started event");
    let value = InvokeEvent::new(
        invocation,
        1,
        InvocationEventBody::ValueBatch {
            schema: None,
            values: vec![InvokeValue::new(RuntimeValue::Integer(7)).expect("value")],
        },
    )
    .expect("value event");
    let completed = InvokeEvent::new(
        invocation,
        2,
        InvocationEventBody::Completed {
            duration_nanoseconds: 11,
        },
    )
    .expect("completed event");

    let accepted = encode_constructed_server_frame(
        &active,
        &registry,
        &ServerFrame::CallAccepted {
            stream: 1,
            invocation,
        },
    )
    .expect("accepted frame");
    assert_eq!(
        client.receive_encoded(&active, &registry, &accepted),
        Ok(InvocationClientResponse::Accepted { invocation }),
    );

    for (outer_sequence, event) in [(1, started), (2, value), (3, completed)] {
        let frame = encode_constructed_server_frame(
            &active,
            &registry,
            &ServerFrame::EventBatch {
                stream: 1,
                channel: Channel::ResultValues,
                events: vec![EventRecord {
                    sequence: outer_sequence,
                    event: Event::Value(RuntimeValue::InvokeEvent(event)),
                }],
            },
        )
        .expect("event frame");
        let response = client
            .receive_encoded(&active, &registry, &frame)
            .expect("event response");
        let InvocationClientResponse::EventBatch(batch) = response else {
            panic!("expected one event batch");
        };
        assert_eq!(batch.records().len(), 1);
    }

    let completed = encode_constructed_server_frame(
        &active,
        &registry,
        &ServerFrame::CallCompleted { stream: 1 },
    )
    .expect("completion frame");
    assert_eq!(
        client.receive_encoded(&active, &registry, &completed),
        Ok(InvocationClientResponse::Completed),
    );
    assert_eq!(
        client.request_cancellation(),
        Err(InvocationClientError::WrongState),
    );
}

#[test]
fn invocation_client_cancellation_is_explicit_and_one_shot() {
    let active = empty_active_revision();
    let registry = test_registry();
    let retained =
        encode_invoke_request(&active, &registry, &minimal_request(None)).expect("request");
    let (mut client, _) = InvocationClient::start(retained);
    assert_eq!(
        client.request_cancellation(),
        Ok(ClientFrame::CallCancel { stream: 1 }),
    );
    assert_eq!(
        client.request_cancellation(),
        Err(InvocationClientError::WrongState),
    );
    let cancelled = encode_constructed_server_frame(
        &active,
        &registry,
        &ServerFrame::CallCancelled { stream: 1 },
    )
    .expect("cancelled frame");
    assert_eq!(
        client.receive_encoded(&active, &registry, &cancelled),
        Ok(InvocationClientResponse::Cancelled),
    );
}
#[test]
fn invocation_client_tracks_additional_result_credit() {
    let active = empty_active_revision();
    let registry = test_registry();
    let retained =
        encode_invoke_request(&active, &registry, &minimal_request(None)).expect("request");
    let (mut client, _) = InvocationClient::start(retained);

    client
        .grant_result_credit(7)
        .expect("additional result credit fits");
    assert_eq!(
        client.grant_result_credit(0),
        Err(InvocationClientError::WrongState),
    );
    assert_eq!(
        client.grant_result_credit(u64::MAX),
        Err(InvocationClientError::CreditOverflow),
    );
}
