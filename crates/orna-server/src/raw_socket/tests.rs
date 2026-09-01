use std::{
    fs,
    future::poll_fn,
    os::unix::net::UnixStream as BlockingUnixStream,
    path::PathBuf,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
};

use orna_core::system::SYS_INVOKE_FUNCTION_ID;
use orna_core::{
    CatalogueRevisionId, FunctionId, InvocationId, PrincipalId, SchemaId, SourceBundleId,
    SourceRevisionId, TypeId,
    canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
    catalogue::{CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName, SchemaDefinition},
    invocation::{
        InvocationCallerContext, InvocationCallerKind, InvocationClientOffer, InvocationTarget,
        InvocationTracePolicy, InvokeRequest, InvokeRequestInput,
    },
    revision::{ActiveDatabaseRevision, RevisionPair, StoredSourceRevision},
    security::{AuthenticatedSession, Principal, PrincipalKind, PrincipalStatus, SecuritySnapshot},
    value::{EnumValue, RuntimeValue},
};
use orna_protocol::{
    Channel, ClientFrame, Event, MAX_RESOURCE_WINDOW, ResourceCancel, ResourceCancellationCode,
    ResourceClientFrame, ResourceKind, ResourceRequest, ResourceServerFrame, ResourceWindowUpdate,
    ServerFrame, SessionClientFrame, SessionServerFrame, decode_catalogue_server_frame,
    decode_constructed_server_frame, decode_resource_server_frame, decode_server_frame,
    decode_session_server_frame, encode_catalogue_client_frame, encode_client_frame,
    encode_constructed_client_frame, encode_invoke_request, encode_resource_client_frame,
    encode_session_client_frame,
};
use orna_standard::{
    registered_opaque_codecs, retained_standard_library_snapshot, verify_standard_library_snapshot,
};
use tokio::sync::{Notify, oneshot};
use tokio::time::timeout;

use super::*;
use crate::invoke::SessionBridge;

const FUNCTION: FunctionId = FunctionId::from_bytes([1; 16]);
const ENUM_TYPE: TypeId = TypeId::from_bytes([0x31; 16]);

fn enum_catalogue() -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes([0x32; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x33; 16]),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            ENUM_TYPE,
            QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
            ["lead", "qualified"],
        )],
        vec![],
    )
    .unwrap()
}

fn constructed_test_version() -> (RawProtocolVersion, RevisionPair) {
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
    let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[]).unwrap();
    let active = ActiveDatabaseRevision::new(
        RevisionPair::new(source.id(), catalogue.revision()),
        source,
        catalogue,
        catalogue_hash,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let revision = active.pair();
    let standard =
        verify_standard_library_snapshot(retained_standard_library_snapshot().unwrap()).unwrap();
    let registry = registered_opaque_codecs(&standard).unwrap();
    (
        RawProtocolVersion::Constructed(Arc::new(active), Arc::new(registry)),
        revision,
    )
}

fn resource_request(revision: RevisionPair) -> ResourceRequest {
    ResourceRequest {
        stream_id: 1,
        request_id: InvocationId::from_bytes([0x11; 16]),
        parent_invocation_id: InvocationId::from_bytes([0x12; 16]),
        call_site_id: orna_core::CallSiteId::from_bytes([0x13; 16]),
        state_profile: String::new(),
        function_instance_key: String::new(),
        target_function_id: FUNCTION,
        target_revision: revision,
        generation: 1,
        resource_kind: ResourceKind::Single,
        arguments: Vec::new(),
        item_window: 1,
        byte_window: MAX_RESOURCE_WINDOW,
    }
}
fn apply_resource_frame(
    version: &RawProtocolVersion,
    connection: &mut ResourceProtocolConnection,
    frame: ResourceServerFrame,
) -> Result<ResourceFrameDisposition, ResourceConnectionError> {
    version.apply_resource(connection, frame)
}

#[test]
fn redacted_prepare_failure_contains_terminal_failure_event() {
    let invocation = InvocationId::from_bytes([0x41; 16]);
    let events = redacted_invoke_failure(
        invocation,
        InvocationFailurePhase::Internal,
        "INVOKE_INTERNAL_FAILURE",
        "invocation could not complete",
        InvocationRetryability::Unknown,
    );
    assert_eq!(events.records().len(), 1);
    assert_eq!(events.records()[0].event().invocation_id(), invocation);
    assert!(matches!(
        events.records()[0].event().body(),
        InvocationEventBody::Failed(_)
    ));
}

#[test]
fn presentation_failure_uses_canonical_redacted_terminal_mapping() {
    let invocation = InvocationId::from_bytes([0x42; 16]);
    let mut actions = sealed_presentation_failure_actions(7, invocation);
    let Some(ServerAction::InvokeEvents { stream, events }) = actions.pop_front() else {
        panic!("presentation failure emits an invocation event");
    };
    assert_eq!(stream, 7);
    assert_eq!(events.records().len(), 1);
    let record = &events.records()[0];
    assert_eq!(record.outer_sequence(), 2);
    assert_eq!(record.event().sequence(), 1);
    assert_eq!(record.event().kind(), InvocationEventKind::InvocationFailed);
    assert_eq!(record.event().invocation_id(), invocation);
    let InvocationEventBody::Failed(failure) = record.event().body() else {
        panic!("presentation failure emits InvocationFailed");
    };
    assert_eq!(failure.phase(), InvocationFailurePhase::Internal);
    assert_eq!(failure.code(), "INVOKE_INTERNAL_FAILURE");
    assert_eq!(failure.message(), "invocation could not complete");
    assert_eq!(failure.retryability(), InvocationRetryability::Unknown);
    assert!(failure.details().is_none());
    assert!(matches!(
        actions.pop_front(),
        Some(ServerAction::Completed { stream: 7 })
    ));
    assert!(actions.is_empty());
}

#[test]
fn exhausted_resource_credit_schedules_terminal_probes_without_value_credit() {
    let (version, revision) = constructed_test_version();
    let request = resource_request(revision);
    let scalar = AuthenticatedServerResourceAccepted {
        stream_id: request.stream_id,
        request_id: request.request_id,
        nested_invocation_id: InvocationId::from_bytes([0x21; 16]),
        target_revision: request.target_revision,
        resource_kind: AuthenticatedServerResourceKind::Single,
    };
    let mut connection = ResourceProtocolConnection::new();
    connection
        .receive(ResourceClientFrame::Request(request.clone()))
        .expect("scalar request opens");
    apply_resource_frame(
        &version,
        &mut connection,
        ResourceServerFrame::Accepted(orna_protocol::ResourceAccepted {
            stream_id: scalar.stream_id,
            request_id: scalar.request_id,
            nested_invocation_id: scalar.nested_invocation_id,
            target_revision: scalar.target_revision,
            resource_kind: ResourceKind::Single,
        }),
    )
    .expect("scalar request accepts");
    apply_resource_frame(
        &version,
        &mut connection,
        ResourceServerFrame::Values(orna_protocol::ResourceValues {
            stream_id: scalar.stream_id,
            request_id: scalar.request_id,
            target_revision: request.target_revision,
            batch_sequence: 0,
            item_count: 1,
            byte_count: resource_value_byte_count(&version, &RuntimeValue::Integer(7))
                .expect("scalar value encodes"),
            values: vec![RuntimeValue::Integer(7)],
        }),
    )
    .expect("scalar value consumes its item credit");
    let available = connection
        .resource_credit(scalar.stream_id, scalar.request_id)
        .expect("scalar credit remains identity-bound");
    assert_eq!(available.item_available, 0);
    assert_eq!(
        resource_producer_credit(scalar, available.item_available, available.byte_available),
        Some(ResourceCredit {
            item_count: 0,
            byte_count: available.byte_available,
        }),
    );
    let exhausted = (available.item_available, available.byte_available);

    let stream = AuthenticatedServerResourceAccepted {
        resource_kind: AuthenticatedServerResourceKind::Stream,
        ..scalar
    };
    assert_eq!(
        resource_producer_credit(stream, exhausted.0, exhausted.1),
        Some(ResourceCredit {
            item_count: 0,
            byte_count: exhausted.1,
        })
    );
    assert_eq!(
        resource_producer_credit(stream, 1, 0),
        Some(ResourceCredit {
            item_count: 1,
            byte_count: 0,
        })
    );
    assert_eq!(
        resource_producer_credit(scalar, 0, 0),
        Some(ResourceCredit {
            item_count: 0,
            byte_count: 0,
        })
    );
    assert_eq!(
        resource_producer_credit(scalar, 1, 0),
        Some(ResourceCredit {
            item_count: 1,
            byte_count: 0,
        })
    );
}

#[test]
fn resource_completion_values_declare_exact_encoded_byte_count() {
    let (version, revision) = constructed_test_version();
    let request = resource_request(revision);
    let value = RuntimeValue::Integer(7);
    let expected = match &version {
        RawProtocolVersion::Constructed(active, registry) => {
            orna_protocol::encode_constructed_value(active, registry, &value)
                .expect("resource value encodes")
                .len() as u32
        }
        _ => unreachable!("constructed test version"),
    };
    let actions = resource_completion_actions(
        &version,
        &request,
        Ok(
            orna_postgres::AuthenticatedServerResourceResult::Completed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                nested_invocation_id: InvocationId::from_bytes([0x21; 16]),
                target_revision: request.target_revision,
                resource_kind: request.resource_kind,
                values: vec![value],
            },
        ),
    );

    let Some(ResourceServerFrame::Values(frame)) = actions.get(1) else {
        panic!("resource completion contains a values frame");
    };
    assert_eq!(frame.byte_count, expected);
}

#[test]
fn constructed_resource_values_validate_before_credit_mutation() {
    let (version, revision) = constructed_test_version();
    let request = resource_request(revision);
    let mut connection = ResourceProtocolConnection::new();
    connection
        .receive(ResourceClientFrame::Request(request.clone()))
        .expect("resource request opens");
    apply_resource_frame(
        &version,
        &mut connection,
        ResourceServerFrame::Accepted(orna_protocol::ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x21; 16]),
            target_revision: request.target_revision,
            resource_kind: ResourceKind::Single,
        }),
    )
    .expect("resource request accepts");
    let before = connection
        .resource_credit(request.stream_id, request.request_id)
        .expect("resource credit is available");
    let result = version.apply_resource(
        &mut connection,
        ResourceServerFrame::Values(orna_protocol::ResourceValues {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: request.target_revision,
            batch_sequence: 0,
            item_count: 1,
            byte_count: 0,
            values: vec![RuntimeValue::Integer(7)],
        }),
    );
    assert!(matches!(
        result,
        Err(ResourceConnectionError::InvalidFrame { .. })
    ));
    assert_eq!(
        connection
            .resource_credit(request.stream_id, request.request_id)
            .expect("resource credit remains available"),
        before
    );
}

#[test]
fn non_constructed_resource_values_reject_before_credit_or_state_mutation() {
    let (constructed, revision) = constructed_test_version();
    let version = RawProtocolVersion::One;
    let request = resource_request(revision);
    let mut connection = ResourceProtocolConnection::new();
    connection
        .receive(ResourceClientFrame::Request(request.clone()))
        .expect("resource request opens");
    apply_resource_frame(
        &constructed,
        &mut connection,
        ResourceServerFrame::Accepted(orna_protocol::ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x21; 16]),
            target_revision: request.target_revision,
            resource_kind: ResourceKind::Single,
        }),
    )
    .expect("resource request accepts");
    let before_state = connection.clone();
    let before = connection
        .resource_credit(request.stream_id, request.request_id)
        .expect("resource credit is available");

    let result = version.apply_resource(
        &mut connection,
        ResourceServerFrame::Values(orna_protocol::ResourceValues {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: request.target_revision,
            batch_sequence: 0,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(7)],
        }),
    );

    assert_eq!(
        result,
        Err(ResourceConnectionError::InvalidFrame {
            source: FrameCodecError::ResourceRequiresConstructed,
        })
    );
    assert_eq!(connection, before_state);
    assert_eq!(
        connection
            .resource_credit(request.stream_id, request.request_id)
            .expect("resource credit remains available"),
        before
    );
}

#[test]
fn resource_terminal_provenance_controls_cancellation_synthesis_and_late_frame_precedence() {
    let (version, revision) = constructed_test_version();
    let request = resource_request(revision);
    let cancel = ResourceCancel {
        stream_id: request.stream_id,
        request_id: request.request_id,
        reason: ResourceCancellationCode::ClientRequested,
    };
    let mut pending = BTreeMap::new();
    let mut cancelled = BTreeMap::new();
    cancelled.insert(request.stream_id, (cancel, request.target_revision));

    let late_values = ResourceDispatchCompletion {
        actions: VecDeque::from([
            ResourceServerFrame::Accepted(orna_protocol::ResourceAccepted {
                stream_id: request.stream_id,
                request_id: request.request_id,
                nested_invocation_id: InvocationId::from_bytes([0x21; 16]),
                target_revision: request.target_revision,
                resource_kind: request.resource_kind,
            }),
            ResourceServerFrame::Values(orna_protocol::ResourceValues {
                stream_id: request.stream_id,
                request_id: request.request_id,
                target_revision: request.target_revision,
                batch_sequence: 0,
                item_count: 1,
                byte_count: 1,
                values: vec![RuntimeValue::Integer(7)],
            }),
        ]),
        producer: None,
        producer_waiting_bytes: None,
        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
    };
    assert!(store_resource_completion(
        request.stream_id,
        late_values,
        &mut pending,
        &mut cancelled,
    ));
    assert!(matches!(
        pending
            .get(&request.stream_id)
            .and_then(|completion| completion.actions.front()),
        Some(ResourceServerFrame::Cancelled(frame))
            if frame.request_id == request.request_id
                && frame.target_revision == request.target_revision
                && frame.reason == ResourceCancellationCode::ClientRequested
    ));
    assert!(cancelled.is_empty());

    let mut committed_request = request.clone();
    committed_request.stream_id = 2;
    committed_request.request_id = InvocationId::from_bytes([0x22; 16]);
    let committed_cancel = ResourceCancel {
        stream_id: committed_request.stream_id,
        request_id: committed_request.request_id,
        reason: ResourceCancellationCode::ClientRequested,
    };
    cancelled.insert(
        committed_request.stream_id,
        (committed_cancel, committed_request.target_revision),
    );
    let committed = ResourceDispatchCompletion {
        actions: resource_actions(&version, &committed_request, vec![RuntimeValue::Integer(8)]),
        producer: None,
        producer_waiting_bytes: None,
        terminal_provenance: ResourceTerminalProvenance::Authenticated,
    };
    assert!(store_resource_completion(
        committed_request.stream_id,
        committed,
        &mut pending,
        &mut cancelled,
    ));
    assert_eq!(
        pending
            .get(&committed_request.stream_id)
            .expect("authenticated terminal is retained")
            .terminal_provenance,
        ResourceTerminalProvenance::Authenticated,
    );

    let late_non_terminal = ResourceDispatchCompletion {
        actions: VecDeque::from([ResourceServerFrame::Values(orna_protocol::ResourceValues {
            stream_id: committed_request.stream_id,
            request_id: committed_request.request_id,
            target_revision: committed_request.target_revision,
            batch_sequence: 1,
            item_count: 1,
            byte_count: 1,
            values: vec![RuntimeValue::Integer(9)],
        })]),
        producer: None,
        producer_waiting_bytes: None,
        terminal_provenance: ResourceTerminalProvenance::Uncommitted,
    };
    assert!(!store_resource_completion(
        committed_request.stream_id,
        late_non_terminal,
        &mut pending,
        &mut cancelled,
    ));
    assert!(matches!(
        pending
            .get(&committed_request.stream_id)
            .and_then(|completion| completion.actions.back()),
        Some(ResourceServerFrame::Completed(frame))
            if frame.request_id == committed_request.request_id
                && frame.target_revision == committed_request.target_revision
    ));
    assert!(cancelled.is_empty());
}

async fn read_resource_server_frame(
    stream: &mut UnixStream,
    active: &orna_core::revision::ActiveDatabaseRevision,
    registry: &orna_core::value::OpaqueCodecRegistry,
) -> ResourceServerFrame {
    let mut header = [0_u8; RESOURCE_HEADER_LENGTH];
    stream.read_exact(&mut header).await.unwrap();
    let payload_length = u32::from_be_bytes(header[17..21].try_into().unwrap()) as usize;
    let mut encoded = header.to_vec();
    encoded.resize(RESOURCE_HEADER_LENGTH + payload_length, 0);
    stream
        .read_exact(&mut encoded[RESOURCE_HEADER_LENGTH..])
        .await
        .unwrap();
    decode_resource_server_frame(active, registry, &encoded).unwrap()
}

fn resource_actions(
    version: &RawProtocolVersion,
    request: &ResourceRequest,
    values: Vec<RuntimeValue>,
) -> VecDeque<ResourceServerFrame> {
    let total_items = values.len() as u64;
    let final_batch_sequence = total_items.saturating_sub(1);
    let mut actions = VecDeque::with_capacity(values.len() + 2);
    actions.push_back(ResourceServerFrame::Accepted(
        orna_protocol::ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x21; 16]),
            target_revision: request.target_revision,
            resource_kind: request.resource_kind,
        },
    ));
    for (batch_sequence, value) in values.into_iter().enumerate() {
        actions.push_back(ResourceServerFrame::Values(orna_protocol::ResourceValues {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: request.target_revision,
            batch_sequence: batch_sequence as u64,
            item_count: 1,
            byte_count: resource_value_byte_count(version, &value).expect("resource value encodes"),
            values: vec![value],
        }));
    }
    actions.push_back(ResourceServerFrame::Completed(
        orna_protocol::ResourceCompleted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            target_revision: request.target_revision,
            final_batch_sequence,
            total_items,
        },
    ));
    actions
}

#[derive(Clone)]
struct ResourceDispatch;

impl DispatchService for ResourceDispatch {
    fn start(
        &self,
        _session: AuthenticatedSession,
        _stream: u64,
        _call: RawCall,
    ) -> StartedDispatch {
        panic!("resource transport test does not issue a raw call")
    }

    fn start_resource(
        &self,
        _session: AuthenticatedSession,
        request: ResourceRequest,
        _resources: LocalRawSocketResources,
        version: RawProtocolVersion,
    ) -> Option<StartedResourceDispatch> {
        let actions = resource_actions(&version, &request, vec![RuntimeValue::Integer(7)]);
        Some(StartedResourceDispatch {
            future: Box::pin(async move {
                ResourceDispatchCompletion {
                    actions,
                    producer: None,
                    producer_waiting_bytes: None,
                    terminal_provenance: ResourceTerminalProvenance::Authenticated,
                }
            }),
            cancellation: ResourceCancellation::new(),
        })
    }
}

#[derive(Clone)]
struct PreAcceptResourceDispatch {
    authorized: bool,
    resource_start_calls: Arc<AtomicUsize>,
}

impl DispatchService for PreAcceptResourceDispatch {
    fn start(
        &self,
        _session: AuthenticatedSession,
        _stream: u64,
        _call: RawCall,
    ) -> StartedDispatch {
        panic!("pre-accept resource test does not issue a raw call")
    }

    fn authorize_resource_request(&self, _request: &ResourceRequest) -> bool {
        self.authorized
    }

    fn start_resource(
        &self,
        _session: AuthenticatedSession,
        _request: ResourceRequest,
        _resources: LocalRawSocketResources,
        _version: RawProtocolVersion,
    ) -> Option<StartedResourceDispatch> {
        self.resource_start_calls.fetch_add(1, Ordering::SeqCst);
        None
    }
}

#[derive(Clone, Copy)]
enum DirectResourceFailureKind {
    SecurityDenied,
    TargetUnavailable,
    ProducerFailure,
}

#[derive(Clone)]
struct DirectResourceFailureDispatch {
    kind: DirectResourceFailureKind,
    authenticated_terminal: Arc<AtomicUsize>,
}

impl DispatchService for DirectResourceFailureDispatch {
    fn start(
        &self,
        _session: AuthenticatedSession,
        _stream: u64,
        _call: RawCall,
    ) -> StartedDispatch {
        panic!("resource transport test does not issue a raw call")
    }

    // Admission succeeds so each matrix case exercises the post-reservation path.
    fn authorize_resource_request(&self, _request: &ResourceRequest) -> bool {
        true
    }

    fn record_resource_terminal_provenance(
        &self,
        _stream_id: u64,
        _request_id: InvocationId,
        provenance: ResourceTerminalProvenance,
    ) {
        if provenance.is_committed() {
            self.authenticated_terminal.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn start_resource(
        &self,
        _session: AuthenticatedSession,
        request: ResourceRequest,
        _resources: LocalRawSocketResources,
        _version: RawProtocolVersion,
    ) -> Option<StartedResourceDispatch> {
        let failure = match self.kind {
            DirectResourceFailureKind::SecurityDenied => CallFailure::ExecuteDenied,
            DirectResourceFailureKind::TargetUnavailable => CallFailure::TargetUnavailable,
            DirectResourceFailureKind::ProducerFailure => CallFailure::InternalFailure,
        };
        let terminal_provenance = match self.kind {
            DirectResourceFailureKind::TargetUnavailable
            | DirectResourceFailureKind::SecurityDenied
            | DirectResourceFailureKind::ProducerFailure => {
                ResourceTerminalProvenance::Authenticated
            }
        };
        let completion = ResourceDispatchCompletion {
            actions: VecDeque::from([ResourceServerFrame::Failed(orna_protocol::ResourceFailed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                target_revision: request.target_revision,
                failure,
            })]),
            producer: None,
            producer_waiting_bytes: None,
            terminal_provenance,
        };
        Some(StartedResourceDispatch {
            future: Box::pin(async move { completion }),
            cancellation: ResourceCancellation::new(),
        })
    }
}

#[derive(Clone)]
struct MalformedPendingResourceDispatch;

impl DispatchService for MalformedPendingResourceDispatch {
    fn start(
        &self,
        _session: AuthenticatedSession,
        stream: u64,
        _call: RawCall,
    ) -> StartedDispatch {
        let future = Box::pin(async move {
            DispatchCompletion {
                sealed_producer: None,
                sealed_invocation: None,
                sealed_next_event_sequence: 1,
                sealed_next_outer_sequence: 2,
                actions: VecDeque::from([ServerAction::Completed { stream }]),
                cancellation: ServerAction::Cancelled { stream },
                cancellation_token: None,
                start_gate: None,
                start_delivered: false,
                terminal_delivered: false,
                terminal_claimed: false,
                worker_completed: false,
                _guards: None,
            }
        });
        StartedDispatch {
            accepted: ServerAction::Accepted {
                stream,
                invocation: InvocationId::from_bytes([0x31; 16]),
            },
            started: None,
            start_gate: None,
            future,
        }
    }

    fn start_resource(
        &self,
        _session: AuthenticatedSession,
        request: ResourceRequest,
        _resources: LocalRawSocketResources,
        version: RawProtocolVersion,
    ) -> Option<StartedResourceDispatch> {
        let mut actions = resource_actions(&version, &request, vec![RuntimeValue::Integer(7)]);
        if request.stream_id == 1 {
            let Some(ResourceServerFrame::Values(frame)) = actions.get_mut(1) else {
                panic!("resource actions contain values");
            };
            frame.item_count = 0;
        }
        Some(StartedResourceDispatch {
            future: Box::pin(async move {
                ResourceDispatchCompletion {
                    actions,
                    producer: None,
                    producer_waiting_bytes: None,
                    terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                }
            }),
            cancellation: ResourceCancellation::new(),
        })
    }
}

#[derive(Clone)]
struct MultiValueResourceDispatch;

impl DispatchService for MultiValueResourceDispatch {
    fn start(
        &self,
        _session: AuthenticatedSession,
        _stream: u64,
        _call: RawCall,
    ) -> StartedDispatch {
        panic!("resource transport test does not issue a raw call")
    }

    fn start_resource(
        &self,
        _session: AuthenticatedSession,
        request: ResourceRequest,
        _resources: LocalRawSocketResources,
        version: RawProtocolVersion,
    ) -> Option<StartedResourceDispatch> {
        let actions = resource_actions(
            &version,
            &request,
            vec![RuntimeValue::Integer(7), RuntimeValue::Integer(8)],
        );
        Some(StartedResourceDispatch {
            future: Box::pin(async move {
                ResourceDispatchCompletion {
                    actions,
                    producer: None,
                    producer_waiting_bytes: None,
                    terminal_provenance: ResourceTerminalProvenance::Authenticated,
                }
            }),
            cancellation: ResourceCancellation::new(),
        })
    }
}

#[derive(Clone)]
struct BlockingResourceDispatch {
    started: Arc<Notify>,
    cancelled: Arc<AtomicBool>,
}

impl DispatchService for BlockingResourceDispatch {
    fn start(
        &self,
        _session: AuthenticatedSession,
        _stream: u64,
        _call: RawCall,
    ) -> StartedDispatch {
        panic!("resource transport test does not issue a raw call")
    }

    fn start_resource(
        &self,
        _session: AuthenticatedSession,
        _request: ResourceRequest,
        _resources: LocalRawSocketResources,
        _version: RawProtocolVersion,
    ) -> Option<StartedResourceDispatch> {
        let started = Arc::clone(&self.started);
        let cancelled = Arc::clone(&self.cancelled);
        let cancellation = ResourceCancellation::new();
        let operation_cancellation = cancellation.clone();
        Some(StartedResourceDispatch {
            future: Box::pin(async move {
                started.notify_one();
                tokio::select! {
                                                _ = operation_cancellation.cancelled() => {
                                                    cancelled.store(true, Ordering::SeqCst);
                                                    ResourceDispatchCompletion {
                                                        actions: VecDeque::new(),
                                                        producer: None,
                                producer_waiting_bytes: None,
                terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                                                    }
                                                }
                                                completion = std::future::pending::<ResourceDispatchCompletion>() => completion,
                                            }
            }),
            cancellation,
        })
    }
}

#[derive(Clone)]
struct MixedResourceDispatch {
    started: Arc<Notify>,
}

impl DispatchService for MixedResourceDispatch {
    fn start(
        &self,
        _session: AuthenticatedSession,
        _stream: u64,
        _call: RawCall,
    ) -> StartedDispatch {
        panic!("resource transport test does not issue a raw call")
    }

    fn start_resource(
        &self,
        _session: AuthenticatedSession,
        request: ResourceRequest,
        _resources: LocalRawSocketResources,
        version: RawProtocolVersion,
    ) -> Option<StartedResourceDispatch> {
        if request.stream_id == 1 {
            let started = Arc::clone(&self.started);
            let cancellation = ResourceCancellation::new();
            let operation_cancellation = cancellation.clone();
            return Some(StartedResourceDispatch {
                future: Box::pin(async move {
                    started.notify_one();
                    tokio::select! {
                                                            _ = operation_cancellation.cancelled() => {
                                                                ResourceDispatchCompletion {
                                                                    actions: VecDeque::new(),
                                                                    producer: None,
                                        producer_waiting_bytes: None,
                    terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                                                                }
                                                            }
                                                            completion = std::future::pending::<ResourceDispatchCompletion>() => completion,
                                                        }
                }),
                cancellation,
            });
        }
        let actions = resource_actions(
            &version,
            &request,
            vec![RuntimeValue::Integer(7), RuntimeValue::Integer(8)],
        );
        Some(StartedResourceDispatch {
            future: Box::pin(async move {
                ResourceDispatchCompletion {
                    actions,
                    producer: None,
                    producer_waiting_bytes: None,
                    terminal_provenance: ResourceTerminalProvenance::Authenticated,
                }
            }),
            cancellation: ResourceCancellation::new(),
        })
    }
}

struct DropSignal(Arc<AtomicBool>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct ShutdownResourceDispatch {
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

impl DispatchService for ShutdownResourceDispatch {
    fn start(
        &self,
        _session: AuthenticatedSession,
        _stream: u64,
        _call: RawCall,
    ) -> StartedDispatch {
        panic!("resource transport test does not issue a raw call")
    }

    fn start_resource(
        &self,
        _session: AuthenticatedSession,
        _request: ResourceRequest,
        _resources: LocalRawSocketResources,
        _version: RawProtocolVersion,
    ) -> Option<StartedResourceDispatch> {
        let started = Arc::clone(&self.started);
        let dropped = Arc::clone(&self.dropped);
        let cancelled = Arc::clone(&self.cancelled);
        let cancellation = ResourceCancellation::new();
        let operation_cancellation = cancellation.clone();
        Some(StartedResourceDispatch {
            future: Box::pin(async move {
                let _drop_signal = DropSignal(dropped);
                started.notify_one();
                tokio::select! {
                                                _ = operation_cancellation.cancelled() => {
                                                    cancelled.store(true, Ordering::SeqCst);
                                                    ResourceDispatchCompletion {
                                                        actions: VecDeque::new(),
                                                        producer: None,
                                producer_waiting_bytes: None,
                terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                                                    }
                                                }
                                                completion = std::future::pending::<ResourceDispatchCompletion>() => completion,
                                            }
            }),
            cancellation,
        })
    }
}

#[derive(Clone)]
struct TestDispatch {
    actions: Arc<Vec<ServerAction>>,
    cancelled: Arc<AtomicBool>,
    polled: Arc<AtomicBool>,
    first_poll_saw_cancellation: Arc<AtomicBool>,
}

impl TestDispatch {
    fn new(actions: Vec<ServerAction>) -> Self {
        Self {
            actions: Arc::new(actions),
            cancelled: Arc::new(AtomicBool::new(false)),
            polled: Arc::new(AtomicBool::new(false)),
            first_poll_saw_cancellation: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl DispatchService for TestDispatch {
    fn start(
        &self,
        _session: AuthenticatedSession,
        stream: u64,
        _call: RawCall,
    ) -> StartedDispatch {
        let cancelled = Arc::clone(&self.cancelled);
        let polled = Arc::clone(&self.polled);
        let first_poll_saw_cancellation = Arc::clone(&self.first_poll_saw_cancellation);
        let actions = Arc::clone(&self.actions);
        let future = Box::pin(async move {
            poll_fn(move |_| {
                polled.store(true, Ordering::SeqCst);
                first_poll_saw_cancellation
                    .store(cancelled.load(Ordering::SeqCst), Ordering::SeqCst);
                Poll::Ready(())
            })
            .await;
            DispatchCompletion {
                sealed_producer: None,
                sealed_invocation: None,
                sealed_next_event_sequence: 1,
                sealed_next_outer_sequence: 2,
                actions: actions.iter().cloned().collect(),
                cancellation: ServerAction::Cancelled { stream },
                cancellation_token: None,
                start_gate: None,
                start_delivered: false,
                terminal_delivered: false,
                terminal_claimed: false,
                worker_completed: false,
                _guards: None,
            }
        });
        StartedDispatch {
            accepted: ServerAction::Accepted {
                stream,
                invocation: InvocationId::from_bytes([9; 16]),
            },
            started: None,
            start_gate: None,
            future,
        }
    }

    fn cancelled(&self, _stream: u64) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct SessionBridgeDispatch {
    bridge: Arc<SessionBridge>,
    received: Arc<Mutex<Option<String>>>,
}

impl DispatchService for SessionBridgeDispatch {
    fn start(
        &self,
        _session: AuthenticatedSession,
        stream: u64,
        _call: RawCall,
    ) -> StartedDispatch {
        let bridge = Arc::clone(&self.bridge);
        let received = Arc::clone(&self.received);
        let invocation = InvocationId::from_bytes([0x9; 16]);
        StartedDispatch {
            accepted: ServerAction::Accepted { stream, invocation },
            started: None,
            start_gate: None,
            future: Box::pin(async move {
                if let Ok(Ok(line)) =
                    tokio::task::spawn_blocking(move || bridge.request_input(invocation)).await
                {
                    *received.lock().expect("session input result lock") = Some(line);
                }
                DispatchCompletion {
                    sealed_producer: None,
                    sealed_invocation: None,
                    sealed_next_event_sequence: 1,
                    sealed_next_outer_sequence: 2,
                    actions: VecDeque::from([ServerAction::Completed { stream }]),
                    cancellation: ServerAction::Cancelled { stream },
                    cancellation_token: None,
                    start_gate: None,
                    start_delivered: false,
                    terminal_delivered: false,
                    terminal_claimed: false,
                    worker_completed: false,
                    _guards: None,
                }
            }),
        }
    }

    fn session_bridge(&self) -> Option<Arc<SessionBridge>> {
        Some(Arc::clone(&self.bridge))
    }
}

mod invoke_protocol;
mod lifecycle;
mod resource_socket;

use lifecycle::{
    read_catalogue_server_frame, read_constructed_server_frame, read_encoded_server_frame,
    read_server_frame, send_client_frame, test_invoke_request, test_session,
};
use resource_socket::{GatedDispatch, GatedInvokePreflightDispatch, GatedPreflightOutcome};
