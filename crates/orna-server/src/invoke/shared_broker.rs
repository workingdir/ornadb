use super::*;

pub(super) struct BrokerWireFrame {
    pub(super) resource: bool,
    pub(super) bytes: Vec<u8>,
}

pub(super) async fn read_shared_broker_frame<R>(
    stream: &mut R,
) -> Result<BrokerWireFrame, ResourceTransportFailure>
where
    R: AsyncRead + Unpin,
{
    let mut header = vec![0_u8; SESSION_HEADER_LENGTH];
    tokio::time::timeout(
        RESOURCE_FRAME_TIMEOUT,
        stream.read_exact(&mut header[..SESSION_MARKER.len()]),
    )
    .await
    .map_err(|_| ResourceTransportFailure::Transport)?
    .map_err(|_| ResourceTransportFailure::Transport)?;
    let session = &header[..SESSION_MARKER.len()] == SESSION_MARKER;
    if !session {
        tokio::time::timeout(
            RESOURCE_FRAME_TIMEOUT,
            stream.read_exact(&mut header[SESSION_MARKER.len()..RESOURCE_MARKER.len()]),
        )
        .await
        .map_err(|_| ResourceTransportFailure::Transport)?
        .map_err(|_| ResourceTransportFailure::Transport)?;
    }
    let resource = !session && &header[..RESOURCE_MARKER.len()] == RESOURCE_MARKER;
    let header_length = if session {
        SESSION_HEADER_LENGTH
    } else if resource {
        RESOURCE_HEADER_LENGTH
    } else {
        18
    };
    let consumed = if session {
        SESSION_MARKER.len()
    } else {
        RESOURCE_MARKER.len()
    };
    tokio::time::timeout(
        RESOURCE_FRAME_TIMEOUT,
        stream.read_exact(&mut header[consumed..header_length]),
    )
    .await
    .map_err(|_| ResourceTransportFailure::Transport)?
    .map_err(|_| ResourceTransportFailure::Transport)?;
    let declared_offset = if session {
        SESSION_HEADER_LENGTH - std::mem::size_of::<u32>()..SESSION_HEADER_LENGTH
    } else if resource {
        17..21
    } else {
        14..18
    };
    let payload_length = u32::from_be_bytes(
        header[declared_offset]
            .try_into()
            .expect("shared broker frame header has a fixed length"),
    ) as usize;
    if (session && payload_length > MAX_SESSION_FRAME_LENGTH - SESSION_HEADER_LENGTH)
        || payload_length > MAX_FRAME_PAYLOAD_LENGTH
    {
        return Err(ResourceTransportFailure::Shape);
    }
    let mut bytes = header;
    bytes.resize(header_length + payload_length, 0);
    tokio::time::timeout(
        RESOURCE_FRAME_TIMEOUT,
        stream.read_exact(&mut bytes[header_length..]),
    )
    .await
    .map_err(|_| ResourceTransportFailure::Transport)?
    .map_err(|_| ResourceTransportFailure::Transport)?;
    Ok(BrokerWireFrame { resource, bytes })
}

pub(super) async fn read_shared_broker_frames<R>(
    mut stream: R,
    sender: Sender<Result<BrokerWireFrame, ResourceTransportFailure>>,
) where
    R: AsyncRead + Unpin,
{
    loop {
        let result = read_shared_broker_frame(&mut stream).await;
        let failed = result.is_err();
        if sender.send(result).await.is_err() || failed {
            return;
        }
    }
}

async fn write_shared_broker_frame<W>(
    stream: &mut W,
    bytes: &[u8],
) -> Result<(), ResourceTransportFailure>
where
    W: AsyncWrite + Unpin,
{
    tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, stream.write_all(bytes))
        .await
        .map_err(|_| ResourceTransportFailure::Transport)?
        .map_err(|_| ResourceTransportFailure::Transport)
}

fn wire_frame_is_session(frame: &BrokerWireFrame) -> bool {
    frame.bytes.len() >= SESSION_MARKER.len()
        && &frame.bytes[..SESSION_MARKER.len()] == SESSION_MARKER
}

async fn handle_shared_session_frame<W>(
    frame: BrokerWireFrame,
    stream: &mut W,
    root: &Option<BrokerRootState>,
) -> Result<(), ResourceTransportFailure>
where
    W: AsyncWrite + Unpin,
{
    let SessionServerFrame::InputRequested(request) =
        decode_session_server_frame(&frame.bytes).map_err(|_| ResourceTransportFailure::Shape)?;
    let Some(root_invocation_id) = root.as_ref().and_then(|state| state.invocation) else {
        return Err(ResourceTransportFailure::Shape);
    };
    if request.root_invocation_id != root_invocation_id || request.call_stream == 0 {
        return Err(ResourceTransportFailure::Shape);
    }
    let _ = stream;
    Err(ResourceTransportFailure::SessionInputUnavailable)
}

pub(super) async fn run_shared_invoke_broker(
    stream: tokio::net::UnixStream,
    active: ActiveDatabaseRevision,
    registry: OpaqueCodecRegistry,
    mut commands: UnboundedReceiver<BrokerCommand>,
    resource_terminal_provenance: BrokerResourceProvenance,
) {
    let (reader, mut stream) = stream.into_split();
    let (frame_sender, mut frames) = mpsc::channel(1);
    let reader_task = tokio::spawn(read_shared_broker_frames(reader, frame_sender));
    let mut connection = ProtocolConnection::new();
    let mut root: Option<BrokerRootState> = None;
    let mut resources: BTreeMap<u64, BrokerResourceState> = BTreeMap::new();
    let mut resource_tombstones = BrokerResourceTombstones::new();
    let mut resource_high_water_mark = None;
    loop {
        enum BrokerNext {
            Command(Option<BrokerCommand>),
            Frame(Option<Result<BrokerWireFrame, ResourceTransportFailure>>),
        }
        let next = tokio::select! {
            command = commands.recv() => BrokerNext::Command(command),
            frame = frames.recv() => BrokerNext::Frame(frame),
        };
        match next {
            BrokerNext::Command(Some(command)) => {
                if handle_shared_broker_command(
                    command,
                    &mut stream,
                    &active,
                    &registry,
                    &mut connection,
                    &mut root,
                    &mut resources,
                    &mut resource_high_water_mark,
                    &mut resource_tombstones,
                    &resource_terminal_provenance,
                )
                .await
                .is_err()
                {
                    break;
                }
            }
            BrokerNext::Command(None) => break,
            BrokerNext::Frame(Some(Ok(frame))) => {
                let result = if wire_frame_is_session(&frame) {
                    handle_shared_session_frame(frame, &mut stream, &root).await
                } else {
                    handle_shared_broker_frame(
                        frame,
                        &mut stream,
                        &active,
                        &registry,
                        &mut root,
                        &mut resources,
                        resource_high_water_mark,
                        &mut resource_tombstones,
                        &resource_terminal_provenance,
                    )
                    .await
                };
                if result.is_err() {
                    break;
                }
            }
            BrokerNext::Frame(Some(Err(_))) | BrokerNext::Frame(None) => break,
        }
    }
    reader_task.abort();
    let _ = reader_task.await;
    if let Some(root) = root.take() {
        let _ = root.response.send(Err(ResourceTransportFailure::Transport));
    }
    for (_, resource) in resources {
        signal_broker_resource_cleanup(resource.completion);
    }
    resource_terminal_provenance
        .lock()
        .expect("broker resource provenance lock")
        .clear();
}

pub(super) async fn handle_shared_broker_command<W>(
    command: BrokerCommand,
    stream: &mut W,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    connection: &mut ProtocolConnection,
    root: &mut Option<BrokerRootState>,
    resources: &mut BTreeMap<u64, BrokerResourceState>,
    resource_high_water_mark: &mut Option<u64>,
    resource_tombstones: &mut BrokerResourceTombstones,
    resource_terminal_provenance: &BrokerResourceProvenance,
) -> Result<(), ResourceTransportFailure>
where
    W: AsyncWrite + Unpin,
{
    match command {
        BrokerCommand::StartRoot { request, response } => {
            if root.is_some() {
                let _ = response.send(Err(ResourceTransportFailure::Transport));
                return Ok(());
            }
            let frames = [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: SYS_INVOKE_FUNCTION_ID,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: MAX_CHANNEL_WINDOW,
                },
                ClientFrame::CallInvokeRequest { stream: 1, request },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ];
            for frame in frames {
                connection
                    .receive_constructed(active, registry, frame.clone())
                    .map_err(|_| ResourceTransportFailure::Shape)?;
                let encoded = encode_constructed_client_frame(active, registry, &frame)
                    .map_err(|_| ResourceTransportFailure::Shape)?;
                write_shared_broker_frame(stream, &encoded).await?;
            }
            *root = Some(BrokerRootState {
                invocation: None,
                records: Vec::new(),
                response,
            });
        }
        BrokerCommand::StartResource {
            request,
            expected_type,
            resource_kind,
            completion,
        } => {
            let stream_id = request.stream_id;
            if resource_high_water_mark.is_some_and(|previous| stream_id <= previous) {
                return Err(ResourceTransportFailure::Shape);
            }
            let mut protocol = ResourceProtocolConnection::new();
            protocol
                .open(request.clone())
                .map_err(|_| ResourceTransportFailure::Shape)?;
            let encoded = encode_resource_client_frame(
                active,
                registry,
                &ResourceClientFrame::Request(request.clone()),
            )
            .map_err(|_| ResourceTransportFailure::Shape)?;
            resources.insert(
                stream_id,
                BrokerResourceState {
                    request,
                    expected_type,
                    resource_kind,
                    protocol,
                    completion,
                    accepted: false,
                    accepted_nested_invocation_id: None,
                    scalar_value: None,
                    cancellation_requested: false,
                    stream_values_seen: false,
                    terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                    scalar_value_after_cancellation: false,
                },
            );
            *resource_high_water_mark = Some(stream_id);
            write_shared_broker_frame(stream, &encoded).await?;
        }
        BrokerCommand::CancelResource {
            stream_id,
            request_id,
            reason,
        } => {
            let Some(state) = resources.get_mut(&stream_id) else {
                clear_resource_terminal_provenance_for_stream(
                    resource_terminal_provenance,
                    stream_id,
                );
                return Ok(());
            };
            let cancel = ResourceClientFrame::Cancel(ResourceCancel {
                stream_id,
                request_id,
                reason,
            });
            state
                .protocol
                .receive(cancel.clone())
                .map_err(|_| ResourceTransportFailure::Shape)?;
            state.cancellation_requested = true;
            let encoded = encode_resource_client_frame(active, registry, &cancel)
                .map_err(|_| ResourceTransportFailure::Shape)?;
            write_shared_broker_frame(stream, &encoded).await?;
        }
        BrokerCommand::AbandonResource {
            stream_id,
            request_id,
            reason,
        } => {
            let Some(state) = resources.get(&stream_id) else {
                clear_resource_terminal_provenance_for_stream(
                    resource_terminal_provenance,
                    stream_id,
                );
                return Ok(());
            };
            if state.request.request_id != request_id {
                return Err(ResourceTransportFailure::Shape);
            }
            clear_resource_terminal_provenance_for_stream(resource_terminal_provenance, stream_id);
            let mut state = resources
                .remove(&stream_id)
                .expect("broker resource checked above");
            if !state.cancellation_requested {
                let cancel = ResourceClientFrame::Cancel(ResourceCancel {
                    stream_id,
                    request_id,
                    reason,
                });
                state
                    .protocol
                    .receive(cancel.clone())
                    .map_err(|_| ResourceTransportFailure::Shape)?;
                state.cancellation_requested = true;
                let encoded = encode_resource_client_frame(active, registry, &cancel)
                    .map_err(|_| ResourceTransportFailure::Shape)?;
                write_shared_broker_frame(stream, &encoded).await?;
            }
            remember_broker_resource_terminal(resource_tombstones, stream_id, request_id);
        }

        BrokerCommand::Shutdown => {
            if root.is_some() {
                let cancel = ClientFrame::CallCancel { stream: 1 };
                let _ = connection.receive_constructed(active, registry, cancel.clone());
                if let Ok(encoded) = encode_constructed_client_frame(active, registry, &cancel) {
                    let _ = write_shared_broker_frame(stream, &encoded).await;
                }
            }
            for state in resources.values_mut() {
                if state.cancellation_requested {
                    continue;
                }
                let cancel = ResourceClientFrame::Cancel(ResourceCancel {
                    stream_id: state.request.stream_id,
                    request_id: state.request.request_id,
                    reason: ResourceCancellationCode::RuntimeShutdown,
                });
                let _ = state.protocol.receive(cancel.clone());
                if let Ok(encoded) = encode_resource_client_frame(active, registry, &cancel) {
                    let _ = write_shared_broker_frame(stream, &encoded).await;
                }
            }
            return Err(ResourceTransportFailure::Transport);
        }
    }
    Ok(())
}

fn resource_server_frame_identity(frame: &ResourceServerFrame) -> (u64, orna_core::InvocationId) {
    match frame {
        ResourceServerFrame::Accepted(value) => (value.stream_id, value.request_id),
        ResourceServerFrame::Values(value) => (value.stream_id, value.request_id),
        ResourceServerFrame::Completed(value) => (value.stream_id, value.request_id),
        ResourceServerFrame::Failed(value) => (value.stream_id, value.request_id),
        ResourceServerFrame::Cancelled(value) => (value.stream_id, value.request_id),
    }
}
fn resource_action_is_terminal(frame: &ResourceServerFrame) -> bool {
    matches!(
        frame,
        ResourceServerFrame::Completed(_)
            | ResourceServerFrame::Failed(_)
            | ResourceServerFrame::Cancelled(_)
    )
}

pub(super) fn remember_broker_resource_terminal(
    tombstones: &mut BrokerResourceTombstones,
    stream_id: u64,
    request_id: orna_core::InvocationId,
) {
    tombstones.insert(stream_id, request_id);
    while tombstones.len() > BROKER_RESOURCE_TOMBSTONE_CAPACITY {
        let Some(stream_id) = tombstones.keys().next().copied() else {
            break;
        };
        tombstones.remove(&stream_id);
    }
}

pub(super) async fn handle_shared_broker_frame<W>(
    frame: BrokerWireFrame,
    stream: &mut W,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    root: &mut Option<BrokerRootState>,
    resources: &mut BTreeMap<u64, BrokerResourceState>,
    resource_high_water_mark: Option<u64>,
    resource_tombstones: &mut BrokerResourceTombstones,
    resource_terminal_provenance: &BrokerResourceProvenance,
) -> Result<(), ResourceTransportFailure>
where
    W: AsyncWrite + Unpin,
{
    if frame.resource {
        let decoded = decode_resource_server_frame(active, registry, &frame.bytes)
            .map_err(|_| ResourceTransportFailure::Shape)?;
        let (stream_id, request_id) = resource_server_frame_identity(&decoded);
        if let Some(expected_request_id) = resource_tombstones.get(&stream_id) {
            // A tombstone is final for this stream identity. Clear every
            // provenance entry for the stream before either dropping a valid
            // late frame or rejecting a forged request identity.
            clear_resource_terminal_provenance_for_stream(resource_terminal_provenance, stream_id);
            if *expected_request_id != request_id {
                return Err(ResourceTransportFailure::Shape);
            }
            // The broker has already published this stream terminal outcome.
            // Keep the connection alive for the root call and every other resource.
            return Ok(());
        }
        let Some(mut state) = resources.remove(&stream_id) else {
            // No live state can accept this frame. A stream at or below the
            // broker high-water mark is an evicted tombstone, so late frames
            // are drained and cannot revive the old request or its provenance.
            clear_resource_terminal_provenance_for_stream(resource_terminal_provenance, stream_id);
            if stream_id != 0 && resource_high_water_mark.is_some_and(|high| stream_id <= high) {
                return Ok(());
            }
            return Err(ResourceTransportFailure::Shape);
        };
        let frame_terminal = resource_action_is_terminal(&decoded);
        if frame_terminal {
            state.terminal_provenance = resource_terminal_provenance
                .lock()
                .expect("broker resource provenance lock")
                .get(&(stream_id, request_id))
                .copied()
                .unwrap_or(ResourceTerminalProvenance::Uncommitted);
        }
        match handle_shared_resource_frame_classified(&mut state, decoded, stream, active, registry)
            .await
        {
            Ok(true) => {
                resources.insert(stream_id, state);
            }
            Ok(false) => {
                remember_broker_resource_terminal(
                    resource_tombstones,
                    stream_id,
                    state.request.request_id,
                );
                clear_resource_terminal_provenance_for_stream(
                    resource_terminal_provenance,
                    stream_id,
                );
            }
            Err(
                SharedResourceFrameError::Protocol | SharedResourceFrameError::RequestLocalShape,
            ) => {
                clear_resource_terminal_provenance_for_stream(
                    resource_terminal_provenance,
                    stream_id,
                );
                let _ = send_shared_resource_completion(
                    &mut state,
                    Err(ResourceTransportFailure::Shape),
                    stream,
                    active,
                    registry,
                )
                .await?;
                if !state.cancellation_requested {
                    let cancel = ResourceClientFrame::Cancel(ResourceCancel {
                        stream_id: state.request.stream_id,
                        request_id: state.request.request_id,
                        reason: ResourceCancellationCode::RuntimeShutdown,
                    });
                    state
                        .protocol
                        .receive(cancel.clone())
                        .map_err(|_| ResourceTransportFailure::Shape)?;
                    let encoded = encode_resource_client_frame(active, registry, &cancel)
                        .map_err(|_| ResourceTransportFailure::Shape)?;
                    write_shared_broker_frame(stream, &encoded).await?;
                    state.cancellation_requested = true;
                }
                remember_broker_resource_terminal(
                    resource_tombstones,
                    stream_id,
                    state.request.request_id,
                );
            }
            Err(SharedResourceFrameError::Transport(error)) => {
                clear_resource_terminal_provenance_for_stream(
                    resource_terminal_provenance,
                    stream_id,
                );
                return Err(error);
            }
        }
        return Ok(());
    }
    let decoded = decode_constructed_invocation_event_frame(active, registry, &frame.bytes)
        .or_else(|_| decode_constructed_server_frame(active, registry, &frame.bytes))
        .map_err(|_| ResourceTransportFailure::Shape)?;
    let Some(state) = root.as_mut() else {
        return Err(ResourceTransportFailure::Shape);
    };
    match decoded {
        ServerFrame::CallAccepted {
            stream: 1,
            invocation,
        } => {
            if state.invocation.replace(invocation).is_some() {
                return Err(ResourceTransportFailure::Shape);
            }
        }
        ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events,
        } => {
            let Some(invocation) = state.invocation else {
                return Err(ResourceTransportFailure::Shape);
            };
            if state.records.last().is_some_and(|last| {
                matches!(
                    last.event().body(),
                    InvocationEventBody::Completed { .. }
                        | InvocationEventBody::Failed(_)
                        | InvocationEventBody::Cancelled { .. }
                )
            }) {
                return Err(ResourceTransportFailure::Shape);
            }
            let event_count = events.len();
            for (index, record) in events.into_iter().enumerate() {
                let Event::Value(RuntimeValue::InvokeEvent(event)) = record.event else {
                    return Err(ResourceTransportFailure::Shape);
                };
                if event.invocation_id() != invocation {
                    return Err(ResourceTransportFailure::Shape);
                }
                if state.records.is_empty()
                    && !matches!(event.body(), InvocationEventBody::Started { .. })
                {
                    return Err(ResourceTransportFailure::Shape);
                }
                if !state.records.is_empty()
                    && matches!(event.body(), InvocationEventBody::Started { .. })
                {
                    return Err(ResourceTransportFailure::Shape);
                }
                if state.records.is_empty() && (record.sequence != 1 || event.sequence() != 0) {
                    return Err(ResourceTransportFailure::Shape);
                }
                if state.records.last().is_some_and(|last| {
                    last.outer_sequence().checked_add(1) != Some(record.sequence)
                        || last.event().sequence().checked_add(1) != Some(event.sequence())
                }) {
                    return Err(ResourceTransportFailure::Shape);
                }
                if matches!(
                    event.body(),
                    InvocationEventBody::Completed { .. }
                        | InvocationEventBody::Failed(_)
                        | InvocationEventBody::Cancelled { .. }
                ) && index + 1 != event_count
                {
                    return Err(ResourceTransportFailure::Shape);
                }
                state
                    .records
                    .push(InvocationEventRecord::new(record.sequence, event));
            }
        }
        ServerFrame::CallCompleted { stream: 1 } => {
            let state = root.take().expect("root state checked above");
            let Some(invocation) = state.invocation else {
                return Err(ResourceTransportFailure::Shape);
            };
            if state.records.is_empty() {
                let _ = state.response.send(Err(ResourceTransportFailure::Shape));
                return Err(ResourceTransportFailure::Shape);
            } else {
                match reconstruct_shared_root_result(invocation, state.records) {
                    Ok(result) => {
                        let _ = state.response.send(Ok(result));
                    }
                    Err(ResourceTransportFailure::Cancelled) => {
                        let _ = state
                            .response
                            .send(Err(ResourceTransportFailure::Cancelled));
                    }
                    Err(error) => {
                        let _ = state.response.send(Err(error));
                        return Err(ResourceTransportFailure::Shape);
                    }
                }
            }
        }
        ServerFrame::CallFailed { stream: 1, failure } => {
            let state = root.take().expect("root state checked above");
            let result = match (state.invocation, failure) {
                // Entry denial is the one legal pre-accept terminal result. It
                // has no InvocationId by contract, so keep it out of the
                // accepted-only SealedInvocationResult::Denied variant.
                (None, CallFailure::ExecuteDenied) if state.records.is_empty() => {
                    Err(ResourceTransportFailure::RootPreflightDenied)
                }
                // Request decode, protocol-major, and standard-snapshot failures
                // are terminal internal outcomes before acceptance.
                (None, CallFailure::InternalFailure) if state.records.is_empty() => {
                    Err(ResourceTransportFailure::RootSealedDispatchInternal)
                }
                // Every other missing identity is an invalid root frame.
                (None, _) => {
                    let _ = state.response.send(Err(ResourceTransportFailure::Shape));
                    return Err(ResourceTransportFailure::Shape);
                }
                // Accepted invocations must terminate with a terminal Event
                // followed by CALL_COMPLETED. Raw CALL_FAILED is pre-accept
                // only, so notify the waiting caller before rejecting it.
                (Some(_), _) => {
                    let _ = state.response.send(Err(ResourceTransportFailure::Shape));
                    return Err(ResourceTransportFailure::Shape);
                }
            };
            let _ = state.response.send(result);
        }
        ServerFrame::CallCancelled { stream: 1 } => {
            let state = root.take().expect("root state checked above");
            if state.invocation.is_some() {
                // Accepted invocations must publish InvocationCancelled as a
                // terminal Event and then CALL_COMPLETED. Raw CALL_CANCELLED
                // is pre-accept only, so do not expose a public cancellation.
                let _ = state.response.send(Err(ResourceTransportFailure::Shape));
                return Err(ResourceTransportFailure::Shape);
            }
            if state.invocation.is_none() && !state.records.is_empty() {
                let _ = state.response.send(Err(ResourceTransportFailure::Shape));
                return Err(ResourceTransportFailure::Shape);
            }
            let _ = state
                .response
                .send(Err(ResourceTransportFailure::Cancelled));
        }
        _ => return Err(ResourceTransportFailure::Shape),
    }
    Ok(())
}

pub(super) fn reconstruct_shared_root_result(
    invocation: orna_core::InvocationId,
    records: Vec<InvocationEventRecord>,
) -> Result<SealedInvocationResult, ResourceTransportFailure> {
    let events = orna_protocol::InvocationEventBatch::new(records)
        .map_err(|_| ResourceTransportFailure::Shape)?;
    let Some(first) = events.records().first() else {
        return Err(ResourceTransportFailure::Shape);
    };
    if !matches!(first.event().body(), InvocationEventBody::Started { .. }) {
        return Err(ResourceTransportFailure::Shape);
    }
    let mut started_seen = false;
    let mut terminal_seen = false;
    for record in events.records() {
        if matches!(record.event().body(), InvocationEventBody::Started { .. }) {
            if started_seen {
                return Err(ResourceTransportFailure::Shape);
            }
            started_seen = true;
        }
        let terminal = matches!(
            record.event().body(),
            InvocationEventBody::Completed { .. }
                | InvocationEventBody::Failed(_)
                | InvocationEventBody::Cancelled { .. }
        );
        if terminal_seen {
            return Err(ResourceTransportFailure::Shape);
        }
        terminal_seen = terminal;
    }
    let Some(last) = events.records().last() else {
        return Err(ResourceTransportFailure::Shape);
    };
    match last.event().body() {
        InvocationEventBody::Failed(failure) if failure.code() == "INVOKE_DENIED" => {
            Ok(SealedInvocationResult::Denied { invocation })
        }
        InvocationEventBody::Failed(failure) if failure.code() == "INVOKE_INTERNAL_FAILURE" => {
            Err(ResourceTransportFailure::RootSealedDispatchInternal)
        }
        InvocationEventBody::Failed(_) => Ok(SealedInvocationResult::Failed { invocation, events }),
        InvocationEventBody::Completed { .. } => {
            Ok(SealedInvocationResult::Completed { invocation, events })
        }
        InvocationEventBody::Cancelled { .. } => Err(ResourceTransportFailure::Cancelled),
        _ => Err(ResourceTransportFailure::Shape),
    }
}

async fn send_shared_resource_completion<W>(
    state: &mut BrokerResourceState,
    outcome: Result<ResourceTransportOutcome, ResourceTransportFailure>,
    stream: &mut W,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<bool, ResourceTransportFailure>
where
    W: AsyncWrite + Unpin,
{
    if state.completion.send(outcome).await.is_ok() {
        return Ok(true);
    }
    if state.cancellation_requested {
        return Ok(false);
    }
    let cancel = ResourceClientFrame::Cancel(ResourceCancel {
        stream_id: state.request.stream_id,
        request_id: state.request.request_id,
        reason: ResourceCancellationCode::RuntimeShutdown,
    });
    state
        .protocol
        .receive(cancel.clone())
        .map_err(|_| ResourceTransportFailure::Shape)?;
    state.cancellation_requested = true;
    let encoded = encode_resource_client_frame(active, registry, &cancel)
        .map_err(|_| ResourceTransportFailure::Shape)?;
    write_shared_broker_frame(stream, &encoded).await?;
    Ok(true)
}

#[derive(Debug)]
enum SharedResourceFrameError {
    Protocol,
    RequestLocalShape,
    Transport(ResourceTransportFailure),
}

impl From<ResourceTransportFailure> for SharedResourceFrameError {
    fn from(error: ResourceTransportFailure) -> Self {
        Self::Transport(error)
    }
}

#[cfg(test)]
pub(super) async fn handle_shared_resource_frame<W>(
    state: &mut BrokerResourceState,
    frame: ResourceServerFrame,
    stream: &mut W,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<bool, ResourceTransportFailure>
where
    W: AsyncWrite + Unpin,
{
    match handle_shared_resource_frame_classified(state, frame, stream, active, registry).await {
        Ok(keep) => Ok(keep),
        Err(SharedResourceFrameError::Protocol | SharedResourceFrameError::RequestLocalShape) => {
            Err(ResourceTransportFailure::Shape)
        }
        Err(SharedResourceFrameError::Transport(error)) => Err(error),
    }
}

async fn handle_shared_resource_frame_classified<W>(
    state: &mut BrokerResourceState,
    frame: ResourceServerFrame,
    stream: &mut W,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<bool, SharedResourceFrameError>
where
    W: AsyncWrite + Unpin,
{
    // Validate against a candidate so a request-local error cannot consume the
    // live stream credit or state before the broker publishes its terminal.
    let mut protocol = state.protocol.clone();
    let disposition = if state.cancellation_requested {
        if let ResourceServerFrame::Cancelled(cancelled) = &frame {
            protocol
                .apply_cancelled_after_client_cancel(*cancelled)
                .map_err(|_| SharedResourceFrameError::Protocol)?
        } else {
            protocol
                .apply_constructed(active, registry, frame.clone())
                .map_err(|_| SharedResourceFrameError::Protocol)?
        }
    } else {
        protocol
            .apply_constructed(active, registry, frame.clone())
            .map_err(|_| SharedResourceFrameError::Protocol)?
    };
    // A terminal frame marked DroppedLate can still be the committed server
    // result: cancellation closed the client-side protocol before that result
    // reached this broker. Drain late non-terminals, but publish this terminal
    // before removing the broker state.
    let late_terminal = state.cancellation_requested
        && matches!(
            disposition,
            orna_protocol::ResourceFrameDisposition::DroppedLate
        )
        && state.terminal_provenance.is_committed()
        && matches!(
            &frame,
            ResourceServerFrame::Completed(_) | ResourceServerFrame::Failed(_)
        );
    // Once cancellation has moved the protocol into a terminal tombstone,
    // validate late non-terminals and advance only the private candidate state
    // needed to validate a later terminal. No late value is published; a scalar
    // value and accepted lineage are retained only until a committed terminal
    // proves they may be delivered.
    if state.cancellation_requested
        && matches!(
            disposition,
            orna_protocol::ResourceFrameDisposition::DroppedLate
        )
        && !late_terminal
        && !matches!(
            &frame,
            ResourceServerFrame::Completed(_)
                | ResourceServerFrame::Failed(_)
                | ResourceServerFrame::Cancelled(_)
        )
    {
        match &frame {
            ResourceServerFrame::Accepted(value) => {
                if value.request_id != state.request.request_id
                    || value.target_revision != state.request.target_revision
                    || value.resource_kind != state.resource_kind
                    || !valid_resource_invocation_id(value.nested_invocation_id)
                {
                    return Err(SharedResourceFrameError::RequestLocalShape);
                }
                // Keep the authenticated lineage identity while draining a
                // late acceptance. A committed terminal may arrive after it
                // and still needs the nested invocation identity.
                state.accepted = true;
                state.accepted_nested_invocation_id = Some(value.nested_invocation_id);
            }
            ResourceServerFrame::Values(value) => {
                if value.request_id != state.request.request_id
                    || value.target_revision != state.request.target_revision
                    || value.values.is_empty()
                    || value.item_count == 0
                    || value.item_count as usize != value.values.len()
                    || value.byte_count == 0
                    || (matches!(state.resource_kind, ProtocolResourceKind::Single)
                        && (value.values.len() != 1 || state.scalar_value.is_some()))
                    || value
                        .values
                        .iter()
                        .any(|item| !runtime_value_matches_type(active, item, state.expected_type))
                {
                    return Err(SharedResourceFrameError::RequestLocalShape);
                }
                if matches!(state.resource_kind, ProtocolResourceKind::Single) {
                    state.scalar_value = value.values.first().cloned();
                    state.scalar_value_after_cancellation = true;
                }
            }
            ResourceServerFrame::Completed(_)
            | ResourceServerFrame::Failed(_)
            | ResourceServerFrame::Cancelled(_) => unreachable!("late terminal handled above"),
        }
        // Retain the validated candidate so repeated late frames and the
        // eventual terminal are checked against the drained batch sequence
        // and credit state, without publishing those frames.
        state.protocol = protocol;
        return Ok(true);
    }
    if state.cancellation_requested
        && !late_terminal
        && matches!(
            &frame,
            ResourceServerFrame::Completed(_) | ResourceServerFrame::Failed(_)
        )
    {
        state.scalar_value.take();
        state.scalar_value_after_cancellation = false;
        let _ = send_shared_resource_completion(
            state,
            Ok(ResourceTransportOutcome::Cancelled {
                nested_invocation_id: state.accepted_nested_invocation_id,
            }),
            stream,
            active,
            registry,
        )
        .await?;
        return Ok(false);
    }
    match frame {
        ResourceServerFrame::Accepted(value) => {
            if value.request_id != state.request.request_id
                || value.target_revision != state.request.target_revision
                || value.resource_kind != state.resource_kind
                || !valid_resource_invocation_id(value.nested_invocation_id)
            {
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            state.protocol = protocol;
            state.accepted = true;
            state.accepted_nested_invocation_id = Some(value.nested_invocation_id);
        }
        ResourceServerFrame::Values(value) => {
            if value.request_id != state.request.request_id
                || value.values.is_empty()
                || value.item_count == 0
                || value.item_count as usize != value.values.len()
                || value.byte_count == 0
                || (matches!(state.resource_kind, ProtocolResourceKind::Single)
                    && (value.values.len() != 1 || state.scalar_value.is_some()))
                || value
                    .values
                    .iter()
                    .any(|item| !runtime_value_matches_type(active, item, state.expected_type))
            {
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            if state.cancellation_requested {
                return Ok(true);
            }
            match state.resource_kind {
                ProtocolResourceKind::Single => {
                    state.protocol = protocol;
                    state.scalar_value = value.values.into_iter().next()
                }
                ProtocolResourceKind::Stream => {
                    state.protocol = protocol;
                    state.stream_values_seen = true;
                    if !send_shared_resource_completion(
                        state,
                        Ok(ResourceTransportOutcome::StreamValues(value.values)),
                        stream,
                        active,
                        registry,
                    )
                    .await?
                    {
                        return Ok(false);
                    }
                    if state.cancellation_requested {
                        return Ok(true);
                    }
                    let update = ResourceWindowUpdate {
                        stream_id: value.stream_id,
                        request_id: value.request_id,
                        add_items: u64::from(value.item_count),
                        add_bytes: u64::from(value.byte_count),
                    };
                    state
                        .protocol
                        .receive(orna_protocol::ResourceClientFrame::WindowUpdate(update))
                        .map_err(|_| ResourceTransportFailure::Shape)?;
                    let encoded = encode_resource_client_frame(
                        active,
                        registry,
                        &orna_protocol::ResourceClientFrame::WindowUpdate(update),
                    )
                    .map_err(|_| ResourceTransportFailure::Shape)?;
                    write_shared_broker_frame(stream, &encoded).await?;
                }
            }
        }
        ResourceServerFrame::Completed(value) => {
            if value.request_id != state.request.request_id {
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            if !state.accepted {
                if late_terminal {
                    let _ = send_shared_resource_completion(
                        state,
                        Err(ResourceTransportFailure::Shape),
                        stream,
                        active,
                        registry,
                    )
                    .await?;
                    return Ok(false);
                }
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            let outcome = match state.resource_kind {
                ProtocolResourceKind::Single => {
                    let Some(value) = state.scalar_value.take() else {
                        if late_terminal {
                            let _ = send_shared_resource_completion(
                                state,
                                Err(ResourceTransportFailure::Shape),
                                stream,
                                active,
                                registry,
                            )
                            .await?;
                            return Ok(false);
                        }
                        return Err(SharedResourceFrameError::RequestLocalShape);
                    };
                    ResourceTransportOutcome::Ready {
                        value,
                        nested_invocation_id: state
                            .accepted_nested_invocation_id
                            .ok_or(ResourceTransportFailure::Shape)?,
                    }
                }
                ProtocolResourceKind::Stream => ResourceTransportOutcome::StreamCompleted {
                    nested_invocation_id: state
                        .accepted_nested_invocation_id
                        .ok_or(ResourceTransportFailure::Shape)?,
                },
            };
            state.protocol = protocol;
            let _ = send_shared_resource_completion(state, Ok(outcome), stream, active, registry)
                .await?;
            return Ok(false);
        }
        ResourceServerFrame::Failed(value) => {
            if value.request_id != state.request.request_id {
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            if state.scalar_value.is_some() && !late_terminal {
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            if late_terminal {
                state.scalar_value.take();
            }
            state.protocol = protocol;
            let _ = send_shared_resource_completion(
                state,
                Ok(ResourceTransportOutcome::Failed {
                    failure: value.failure,
                    nested_invocation_id: state.accepted_nested_invocation_id,
                }),
                stream,
                active,
                registry,
            )
            .await?;
            return Ok(false);
        }
        ResourceServerFrame::Cancelled(value) => {
            if value.request_id != state.request.request_id {
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            state.protocol = protocol;
            let _ = send_shared_resource_completion(
                state,
                Ok(ResourceTransportOutcome::Cancelled {
                    nested_invocation_id: state.accepted_nested_invocation_id,
                }),
                stream,
                active,
                registry,
            )
            .await?;
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) async fn send_resource_cancel(
    stream: &mut tokio::net::UnixStream,
    active: &ActiveDatabaseRevision,
    registry: &orna_core::value::OpaqueCodecRegistry,
    stream_id: u64,
    request_id: orna_core::InvocationId,
    reason: ResourceCancellationCode,
) -> Result<(), ResourceTransportFailure> {
    let encoded_cancel = encode_resource_client_frame(
        active,
        registry,
        &ResourceClientFrame::Cancel(ResourceCancel {
            stream_id,
            request_id,
            reason,
        }),
    )
    .map_err(|_| ResourceTransportFailure::Shape)?;
    tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, stream.write_all(&encoded_cancel))
        .await
        .map_err(|_| ResourceTransportFailure::Transport)?
        .map_err(|_| ResourceTransportFailure::Transport)
}
