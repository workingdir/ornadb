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
mod resource;

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
fn invocation_client_rejects_inner_sequence_exhaustion_without_restarting() {
    let active = empty_active_revision();
    let registry = test_registry();
    let retained =
        encode_invoke_request(&active, &registry, &minimal_request(None)).expect("request");
    let (mut client, _) = InvocationClient::start(retained);
    let invocation = InvocationId::from_bytes([0x76; 16]);
    let accepted = encode_constructed_server_frame(
        &active,
        &registry,
        &ServerFrame::CallAccepted {
            stream: 1,
            invocation,
        },
    )
    .expect("accepted frame");
    client
        .receive_encoded(&active, &registry, &accepted)
        .expect("accepted response");

    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .expect("started event");
    let started_frame = encode_constructed_server_frame(
        &active,
        &registry,
        &ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(RuntimeValue::InvokeEvent(started)),
            }],
        },
    )
    .expect("started frame");
    client
        .receive_encoded(&active, &registry, &started_frame)
        .expect("started response");

    // Simulate the last representable inner sequence without constructing
    // u64::MAX intermediate events.
    client.next_inner_sequence = Some(u64::MAX);
    let value = InvokeEvent::new(
        invocation,
        u64::MAX,
        InvocationEventBody::ValueBatch {
            schema: None,
            values: vec![InvokeValue::new(RuntimeValue::Integer(7)).expect("value")],
        },
    )
    .expect("last value event");
    let value_frame = encode_constructed_server_frame(
        &active,
        &registry,
        &ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 2,
                event: Event::Value(RuntimeValue::InvokeEvent(value)),
            }],
        },
    )
    .expect("last value frame");
    client
        .receive_encoded(&active, &registry, &value_frame)
        .expect("last value response");

    let repeated_started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .expect("repeated started event");
    let repeated_started_frame = encode_constructed_server_frame(
        &active,
        &registry,
        &ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 3,
                event: Event::Value(RuntimeValue::InvokeEvent(repeated_started)),
            }],
        },
    )
    .expect("repeated started frame");
    let before = client.clone();
    assert_eq!(
        client.receive_encoded(&active, &registry, &repeated_started_frame),
        Err(InvocationClientError::SequenceExhausted)
    );
    assert_eq!(client, before);
}

#[test]
fn invocation_client_debits_exact_encoded_event_batch_payload() {
    let active = empty_active_revision();
    let registry = test_registry();
    let retained =
        encode_invoke_request(&active, &registry, &minimal_request(None)).expect("request");
    let invocation = InvocationId::from_bytes([0x77; 16]);
    let accepted = encode_constructed_server_frame(
        &active,
        &registry,
        &ServerFrame::CallAccepted {
            stream: 1,
            invocation,
        },
    )
    .expect("accepted frame");
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .expect("started event");
    let event = encode_constructed_server_frame(
        &active,
        &registry,
        &ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(RuntimeValue::InvokeEvent(started)),
            }],
        },
    )
    .expect("event frame");
    let required = u64::try_from(event.len() - HEADER_LENGTH).expect("bounded payload");
    assert!(required > 0);

    let (mut exact, _) = InvocationClient::start(retained.clone());
    exact
        .receive_encoded(&active, &registry, &accepted)
        .expect("accepted response");
    exact.remaining_result_credit = required;
    assert!(matches!(
        exact.receive_encoded(&active, &registry, &event),
        Ok(InvocationClientResponse::EventBatch(_))
    ));
    assert_eq!(exact.remaining_result_credit, 0);

    let (mut short, _) = InvocationClient::start(retained);
    short
        .receive_encoded(&active, &registry, &accepted)
        .expect("accepted response");
    short.remaining_result_credit = required - 1;
    let before = short.clone();
    assert_eq!(
        short.receive_encoded(&active, &registry, &event),
        Err(InvocationClientError::InsufficientCredit {
            available: required - 1,
            required,
        })
    );
    assert_eq!(short, before);
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
