use super::*;

impl ProtocolConnection {
    /// Creates an empty connection with zero initial channel credit.
    pub const fn new() -> Self {
        Self {
            high_water_mark: None,
            streams: BTreeMap::new(),
        }
    }

    /// Returns the highest stream number that this connection accepted.
    pub const fn high_water_mark(&self) -> Option<u64> {
        self.high_water_mark
    }

    /// Returns the number of currently retained call streams.
    pub fn live_streams(&self) -> usize {
        self.streams.len()
    }

    /// Returns the current `RESULT_VALUES` byte credit for a live stream.
    ///
    /// This read-only inspection does not mutate connection state or consume
    /// credit. The returned value is the stream's current result-value window.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionError::UnknownStream`] when `stream` is not live.
    pub fn result_credit(&self, stream: u64) -> Result<u64, ConnectionError> {
        self.streams
            .get(&stream)
            .map(|state| state.windows[channel_index(Channel::ResultValues)])
            .ok_or(ConnectionError::UnknownStream { stream })
    }

    /// Receives one validated client frame and returns at most one adapter action.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the frame violates a state transition
    /// or bounded connection limit. An error leaves all prior state unchanged.
    pub fn receive(&mut self, frame: ClientFrame) -> Result<Option<ClientAction>, ConnectionError> {
        self.receive_with_version(FrameVersion::One, frame)
    }

    /// Receives one catalogue-bound version-2 client frame.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the frame violates a state transition,
    /// bounded connection limit, or active-catalogue value rule. An error leaves
    /// all prior state unchanged.
    pub fn receive_catalogue(
        &mut self,
        catalogue: &CatalogueSnapshot,
        frame: ClientFrame,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        self.receive_with_version(FrameVersion::Catalogue(catalogue), frame)
    }

    /// Receives one active-revision version-3 client frame.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the frame violates a state transition,
    /// bounded connection limit, or active-revision value rule. An error leaves
    /// all prior state unchanged.
    pub fn receive_active(
        &mut self,
        active: &ActiveDatabaseRevision,
        frame: ClientFrame,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        self.receive_with_version(FrameVersion::Active(active), frame)
    }

    /// Receives one registry-bound version-4 client frame.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the frame violates a state transition,
    /// bounded connection limit, active-revision rule, or the closed opaque
    /// argument boundary. An error leaves all prior state unchanged.
    pub fn receive_registered(
        &mut self,
        active: &ActiveDatabaseRevision,
        registry: &OpaqueCodecRegistry,
        frame: ClientFrame,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        self.receive_with_version(FrameVersion::Registered(active, registry), frame)
    }

    /// Receives one registry-bound version-5 client frame.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the frame violates a state transition,
    /// bounded connection limit, active-revision rule, opaque argument boundary,
    /// closed constructed application-value boundary, or sealed invocation
    /// carrier in an ordinary argument position. An error leaves all prior state
    /// unchanged.
    pub fn receive_constructed(
        &mut self,
        active: &ActiveDatabaseRevision,
        registry: &OpaqueCodecRegistry,
        frame: ClientFrame,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        self.receive_with_version(FrameVersion::Constructed(active, registry), frame)
    }

    fn receive_with_version(
        &mut self,
        version: FrameVersion<'_>,
        frame: ClientFrame,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        match frame {
            ClientFrame::CallRawStart { stream, function } => self.start(stream, function),
            ClientFrame::CallArgument {
                stream,
                parameter,
                value,
            } => self.argument(version, stream, parameter, value),
            ClientFrame::CallInvokeRequest { stream, request } => {
                self.invoke_argument(version, stream, request)
            }
            ClientFrame::CallArgumentsComplete { stream } => self.complete_arguments(stream),
            ClientFrame::WindowUpdate {
                stream,
                channel,
                credit,
            } => self.update_window(stream, channel, credit),
            ClientFrame::CallCancel { stream } => self.cancel(stream),
            ClientFrame::Ping { token } => {
                Ok(Some(ClientAction::Send(ServerFrame::Pong { token })))
            }
        }
    }

    /// Applies one serialised server-adapter result and returns its client frame.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the action violates the current call
    /// state, sequence, frame, or flow-control contract. An error leaves all
    /// prior state and window credit unchanged.
    pub fn apply(&mut self, action: ServerAction) -> Result<ServerFrame, ConnectionError> {
        self.apply_with_version(FrameVersion::One, action)
    }

    /// Applies one catalogue-bound version-2 server-adapter result.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the action violates the current call
    /// state, sequence, frame, flow-control, or active-catalogue value contract.
    /// An error leaves all prior state and window credit unchanged.
    pub fn apply_catalogue(
        &mut self,
        catalogue: &CatalogueSnapshot,
        action: ServerAction,
    ) -> Result<ServerFrame, ConnectionError> {
        self.apply_with_version(FrameVersion::Catalogue(catalogue), action)
    }

    /// Applies one active-revision version-3 server-adapter result.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the action violates the current call
    /// state, sequence, frame, flow-control, or active-revision value contract.
    /// An error leaves all prior state and window credit unchanged.
    pub fn apply_active(
        &mut self,
        active: &ActiveDatabaseRevision,
        action: ServerAction,
    ) -> Result<ServerFrame, ConnectionError> {
        self.apply_with_version(FrameVersion::Active(active), action)
    }

    /// Applies one registry-bound version-4 server-adapter result.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the action violates the current call
    /// state, sequence, frame, flow-control, active-revision, or opaque registry
    /// contract. An error leaves all prior state and window credit unchanged.
    pub fn apply_registered(
        &mut self,
        active: &ActiveDatabaseRevision,
        registry: &OpaqueCodecRegistry,
        action: ServerAction,
    ) -> Result<ServerFrame, ConnectionError> {
        self.apply_with_version(FrameVersion::Registered(active, registry), action)
    }

    /// Applies one registry-bound version-5 server-adapter result.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the action violates the current call
    /// state, sequence, frame, flow-control, active-revision, opaque registry,
    /// closed constructed application-value contract, or sealed invocation
    /// carrier in an ordinary event position. An error leaves all prior state
    /// and window credit unchanged.
    pub fn apply_constructed(
        &mut self,
        active: &ActiveDatabaseRevision,
        registry: &OpaqueCodecRegistry,
        action: ServerAction,
    ) -> Result<ServerFrame, ConnectionError> {
        self.apply_with_version(FrameVersion::Constructed(active, registry), action)
    }

    fn apply_with_version(
        &mut self,
        version: FrameVersion<'_>,
        action: ServerAction,
    ) -> Result<ServerFrame, ConnectionError> {
        match action {
            ServerAction::Accepted { stream, invocation } => self.accept(stream, invocation),
            ServerAction::Events { stream, events } => self.events(version, stream, events),
            ServerAction::InvokeEvents { stream, events } => {
                self.invoke_events(version, stream, events)
            }
            ServerAction::InvokeCancelled { stream } => self.invoke_cancelled(version, stream),
            ServerAction::Completed { stream } => self.complete(stream),
            ServerAction::Failed { stream, failure } => self.fail(stream, failure),
            ServerAction::Cancelled { stream } => self.cancelled(stream),
        }
    }

    fn start(
        &mut self,
        stream: u64,
        function: FunctionId,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        if self.high_water_mark == Some(u64::MAX) {
            return Err(ConnectionError::StreamNumberExhausted);
        }
        if let Some(previous) = self.high_water_mark
            && stream <= previous
        {
            return Err(ConnectionError::StreamNotIncreasing { stream, previous });
        }
        if stream == 0 {
            return Err(ConnectionError::StreamNotIncreasing {
                stream,
                previous: self.high_water_mark.unwrap_or(0),
            });
        }
        if self.streams.len() == MAX_LIVE_STREAMS {
            return Err(ConnectionError::TooManyLiveStreams);
        }
        self.streams
            .insert(stream, StreamState::receiving(function));
        self.high_water_mark = Some(stream);
        Ok(None)
    }

    fn argument(
        &mut self,
        version: FrameVersion<'_>,
        stream: u64,
        parameter: ParameterId,
        value: RuntimeValue,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        version
            .require_call_argument(&value)
            .map_err(|source| ConnectionError::InvalidFrame { source })?;
        let value_length = version
            .encode_value(&value)
            .map_err(|source| ConnectionError::InvalidFrame {
                source: FrameCodecError::Value { source },
            })?
            .len();
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        let Phase::Receiving {
            arguments,
            argument_bytes,
            ..
        } = &state.phase
        else {
            return Err(ConnectionError::WrongState { stream });
        };
        if matches!(&state.phase, Phase::Receiving { function, .. } if *function == SYS_INVOKE_FUNCTION_ID)
        {
            return Err(ConnectionError::WrongState { stream });
        }
        if arguments.contains_key(&parameter) {
            return Err(ConnectionError::DuplicateArgument { stream, parameter });
        }
        if arguments.len() == MAX_ARGUMENTS {
            return Err(ConnectionError::TooManyArguments { stream });
        }
        let next_bytes = argument_bytes
            .checked_add(16 + value_length)
            .filter(|value| *value <= MAX_ARGUMENT_BYTES)
            .ok_or(ConnectionError::ArgumentsTooLarge { stream })?;
        let state = self.streams.get_mut(&stream).expect("live stream checked");
        let Phase::Receiving {
            arguments,
            argument_bytes,
            ..
        } = &mut state.phase
        else {
            unreachable!("phase checked before mutation");
        };
        arguments.insert(parameter, value);
        *argument_bytes = next_bytes;
        Ok(None)
    }

    fn invoke_argument(
        &mut self,
        version: FrameVersion<'_>,
        stream: u64,
        request: RetainedInvokeRequest,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        if !version.is_constructed() {
            return Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::InvocationCarrierNotAccepted {
                    carrier: SYS_INVOKE_REQUEST_TYPE_ID,
                },
            });
        }
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        if !matches!(state.phase, Phase::Receiving { function, .. } if function == SYS_INVOKE_FUNCTION_ID)
        {
            return Err(ConnectionError::WrongState { stream });
        }
        self.streams
            .get_mut(&stream)
            .expect("live stream checked")
            .phase = Phase::InvokeReceiving { request };
        self.streams
            .get_mut(&stream)
            .expect("live stream checked")
            .is_invocation = true;
        Ok(None)
    }

    fn complete_arguments(&mut self, stream: u64) -> Result<Option<ClientAction>, ConnectionError> {
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        match &state.phase {
            Phase::InvokeReceiving { request } => {
                let request = request.clone();
                self.streams
                    .get_mut(&stream)
                    .expect("live stream checked")
                    .phase = Phase::Dispatching;
                Ok(Some(ClientAction::InvokeDispatch { stream, request }))
            }
            Phase::Receiving {
                function,
                arguments,
                ..
            } => {
                if *function == SYS_INVOKE_FUNCTION_ID {
                    return Err(ConnectionError::WrongState { stream });
                }
                let call = RawCall {
                    function: *function,
                    arguments: arguments
                        .iter()
                        .map(|(parameter, value)| CallArgument {
                            parameter: *parameter,
                            value: value.clone(),
                        })
                        .collect(),
                };
                self.streams
                    .get_mut(&stream)
                    .expect("live stream checked")
                    .phase = Phase::Dispatching;
                Ok(Some(ClientAction::Dispatch { stream, call }))
            }
            _ => Err(ConnectionError::WrongState { stream }),
        }
    }

    fn update_window(
        &mut self,
        stream: u64,
        channel: Channel,
        credit: u64,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        if credit == 0 {
            return Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::ZeroWindowCredit,
            });
        }
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        if state.is_invocation && channel != Channel::ResultValues {
            return Err(ConnectionError::WrongState { stream });
        }
        let index = channel_index(channel);
        let next = state.windows[index]
            .checked_add(credit)
            .filter(|value| *value <= MAX_CHANNEL_WINDOW)
            .ok_or(ConnectionError::WindowOverflow { stream, channel })?;
        self.streams
            .get_mut(&stream)
            .expect("live stream checked")
            .windows[index] = next;
        Ok(None)
    }

    fn cancel(&mut self, stream: u64) -> Result<Option<ClientAction>, ConnectionError> {
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        match state.phase {
            Phase::Receiving { .. } | Phase::InvokeReceiving { .. } => {
                self.streams.remove(&stream);
                Ok(Some(ClientAction::Send(ServerFrame::CallCancelled {
                    stream,
                })))
            }
            Phase::Dispatching => {
                self.streams
                    .get_mut(&stream)
                    .expect("live stream checked")
                    .phase = Phase::DispatchCancelling;
                Ok(Some(ClientAction::Cancel {
                    stream,
                    invocation: None,
                }))
            }
            Phase::Running { invocation } => {
                self.streams
                    .get_mut(&stream)
                    .expect("live stream checked")
                    .phase = Phase::RunningCancelling { invocation };
                Ok(Some(ClientAction::Cancel {
                    stream,
                    invocation: Some(invocation),
                }))
            }
            Phase::DispatchCancelling | Phase::RunningCancelling { .. } => {
                Err(ConnectionError::WrongState { stream })
            }
        }
    }

    fn accept(
        &mut self,
        stream: u64,
        invocation: InvocationId,
    ) -> Result<ServerFrame, ConnectionError> {
        require_non_zero_invocation_id(invocation)
            .map_err(|source| ConnectionError::InvalidFrame { source })?;
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        if !matches!(state.phase, Phase::Dispatching) {
            return Err(ConnectionError::WrongState { stream });
        }
        self.streams
            .get_mut(&stream)
            .expect("live stream checked")
            .phase = Phase::Running { invocation };
        Ok(ServerFrame::CallAccepted { stream, invocation })
    }

    fn events(
        &mut self,
        version: FrameVersion<'_>,
        stream: u64,
        events: Vec<Event>,
    ) -> Result<ServerFrame, ConnectionError> {
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        if !matches!(
            state.phase,
            Phase::Running { .. } | Phase::RunningCancelling { .. }
        ) {
            return Err(ConnectionError::WrongState { stream });
        }
        if state.is_invocation {
            return Err(ConnectionError::WrongState { stream });
        }
        if let Some(carrier) = events.iter().find_map(|event| match event {
            Event::Value(value) => invocation_carrier_type_id(value),
            Event::Bytes(_) | Event::Failure(_) => None,
        }) {
            return Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::InvocationCarrierNotAccepted { carrier },
            });
        }
        let Some(first) = events.first() else {
            return Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::EmptyEventBatch,
            });
        };
        let channel = first.channel();
        if events.iter().any(|event| event.channel() != channel) {
            return Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::InvalidEventChannel {
                    channel,
                    kind: events
                        .iter()
                        .find(|event| event.channel() != channel)
                        .expect("mismatched event found")
                        .kind(),
                },
            });
        }
        let count = u64::try_from(events.len()).expect("usize fits u64");
        let first_sequence = state
            .last_sequence
            .checked_add(1)
            .ok_or(ConnectionError::EventSequenceExhausted { stream })?;
        state
            .last_sequence
            .checked_add(count)
            .ok_or(ConnectionError::EventSequenceExhausted { stream })?;
        let records: Vec<_> = events
            .into_iter()
            .enumerate()
            .map(|(index, event)| EventRecord {
                sequence: first_sequence + index as u64,
                event,
            })
            .collect();
        let frame = ServerFrame::EventBatch {
            stream,
            channel,
            events: records,
        };
        let payload_length = encode_server_frame_with_version(version, &frame)
            .map_err(|source| ConnectionError::InvalidFrame { source })?
            .len()
            - HEADER_LENGTH;
        let required = payload_length as u64;
        let index = channel_index(channel);
        let available = state.windows[index];
        if available < required {
            return Err(ConnectionError::InsufficientCredit {
                stream,
                channel,
                available,
                required,
            });
        }
        let state = self.streams.get_mut(&stream).expect("live stream checked");
        state.windows[index] -= required;
        state.last_sequence += count;
        Ok(frame)
    }

    fn invoke_events(
        &mut self,
        version: FrameVersion<'_>,
        stream: u64,
        batch: InvocationEventBatch,
    ) -> Result<ServerFrame, ConnectionError> {
        self.invoke_events_with_options(version, stream, batch, false)
    }

    fn invoke_events_with_options(
        &mut self,
        version: FrameVersion<'_>,
        stream: u64,
        batch: InvocationEventBatch,
        allow_cancellation_terminal: bool,
    ) -> Result<ServerFrame, ConnectionError> {
        if !version.is_constructed() {
            return Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::InvocationCarrierNotAccepted {
                    carrier: SYS_INVOKE_EVENT_TYPE_ID,
                },
            });
        }
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        let (invocation, is_cancelling) = match state.phase {
            Phase::Running { invocation } => (invocation, false),
            Phase::RunningCancelling { invocation } => (invocation, true),
            _ => return Err(ConnectionError::WrongState { stream }),
        };
        if !state.is_invocation || state.invocation_terminal {
            return Err(ConnectionError::WrongState { stream });
        }
        let operational_failure_during_cancellation = batch.records().iter().all(|record| {
            matches!(
                record.event().body(),
                InvocationEventBody::Failed(failure)
                    if failure.phase() == InvocationFailurePhase::Internal
            )
        });
        if is_cancelling && !allow_cancellation_terminal && !operational_failure_during_cancellation
        {
            return Err(ConnectionError::WrongState { stream });
        }
        validate_invocation_event_records(batch.records())
            .map_err(|source| ConnectionError::InvalidFrame { source })?;
        if !is_cancelling
            && batch
                .records()
                .iter()
                .any(|record| record.event().kind() == InvocationEventKind::InvocationCancelled)
        {
            return Err(ConnectionError::WrongState { stream });
        }
        let expected_outer = state
            .last_invocation_outer_sequence
            .checked_add(1)
            .ok_or(ConnectionError::EventSequenceExhausted { stream })?;
        let expected_inner = state
            .last_invocation_event_sequence
            .map(|value| {
                value
                    .checked_add(1)
                    .ok_or(ConnectionError::EventSequenceExhausted { stream })
            })
            .transpose()?
            .unwrap_or(0);
        let records = batch.records();
        if records[0].outer_sequence() != expected_outer
            || records[0].event().sequence() != expected_inner
            || (state.last_invocation_event_sequence.is_none()
                && records[0].event().kind() != InvocationEventKind::InvocationStarted)
        {
            return Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::InvalidInvocationEventSequence,
            });
        }
        if records
            .iter()
            .any(|record| record.event().invocation_id() != invocation)
        {
            return Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::MismatchedInvocationEvent,
            });
        }
        let mut terminal = state.invocation_terminal;
        for (index, record) in records.iter().enumerate() {
            let kind = record.event().kind();
            if kind == InvocationEventKind::InvocationStarted
                && (index > 0 || state.last_invocation_event_sequence.is_some())
            {
                return Err(ConnectionError::InvalidFrame {
                    source: FrameCodecError::InvalidInvocationEventSequence,
                });
            }
            let is_terminal = matches!(
                kind,
                InvocationEventKind::InvocationCompleted
                    | InvocationEventKind::InvocationFailed
                    | InvocationEventKind::InvocationCancelled
            );
            if terminal || (is_terminal && index + 1 != records.len()) {
                return Err(ConnectionError::InvalidFrame {
                    source: FrameCodecError::InvalidInvocationEventSequence,
                });
            }
            terminal |= is_terminal;
        }
        let payload = invocation_event_batch_payload(version, &batch)
            .map_err(|source| ConnectionError::InvalidFrame { source })?;
        let frame = ServerFrame::EventBatch {
            stream,
            channel: Channel::ResultValues,
            events: records
                .iter()
                .map(|record| EventRecord {
                    sequence: record.outer_sequence(),
                    event: Event::Value(RuntimeValue::InvokeEvent(record.event().clone())),
                })
                .collect(),
        };
        let required = encode(version, EVENT_BATCH_TAG, stream, &payload)
            .map_err(|source| ConnectionError::InvalidFrame { source })?
            .len()
            .checked_sub(HEADER_LENGTH)
            .expect("encoded event frame includes its header") as u64;
        let available = state.windows[channel_index(Channel::ResultValues)];
        if available < required {
            return Err(ConnectionError::InsufficientCredit {
                stream,
                channel: Channel::ResultValues,
                available,
                required,
            });
        }
        let state = self.streams.get_mut(&stream).expect("live stream checked");
        state.windows[channel_index(Channel::ResultValues)] -= required;
        state.last_invocation_outer_sequence = records
            .last()
            .expect("sealed event batch is non-empty")
            .outer_sequence();
        state.last_sequence = state.last_invocation_outer_sequence;
        state.last_invocation_event_sequence =
            records.last().map(|record| record.event().sequence());
        state.invocation_terminal = terminal;
        Ok(frame)
    }

    fn invoke_cancelled(
        &mut self,
        version: FrameVersion<'_>,
        stream: u64,
    ) -> Result<ServerFrame, ConnectionError> {
        if !version.is_constructed() {
            return Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::InvocationCarrierNotAccepted {
                    carrier: SYS_INVOKE_EVENT_TYPE_ID,
                },
            });
        }
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        let invocation = match state.phase {
            Phase::RunningCancelling { invocation } => invocation,
            _ => return Err(ConnectionError::WrongState { stream }),
        };
        if !state.is_invocation || state.invocation_terminal {
            return Err(ConnectionError::WrongState { stream });
        }
        let (outer, sequence, started) = match state.last_invocation_event_sequence {
            Some(sequence) => (
                state
                    .last_invocation_outer_sequence
                    .checked_add(1)
                    .ok_or(ConnectionError::EventSequenceExhausted { stream })?,
                sequence
                    .checked_add(1)
                    .ok_or(ConnectionError::EventSequenceExhausted { stream })?,
                None,
            ),
            None => (
                2,
                1,
                Some(
                    InvokeEvent::new(
                        invocation,
                        0,
                        orna_core::invocation::InvocationEventBody::Started {
                            visible_principal: None,
                        },
                    )
                    .map_err(|_| ConnectionError::InvalidFrame {
                        source: FrameCodecError::InvalidInvocationEventSequence,
                    })?,
                ),
            ),
        };
        let cancelled = InvokeEvent::new(
            invocation,
            sequence,
            orna_core::invocation::InvocationEventBody::Cancelled { reason: None },
        )
        .map_err(|_| ConnectionError::InvalidFrame {
            source: FrameCodecError::InvalidInvocationEventSequence,
        })?;
        let records = match started {
            Some(started) => vec![
                InvocationEventRecord::new(outer - 1, started),
                InvocationEventRecord::new(outer, cancelled),
            ],
            None => vec![InvocationEventRecord::new(outer, cancelled)],
        };
        self.invoke_events_with_options(
            version,
            stream,
            InvocationEventBatch::new(records)
                .map_err(|source| ConnectionError::InvalidFrame { source })?,
            true,
        )
    }

    fn complete(&mut self, stream: u64) -> Result<ServerFrame, ConnectionError> {
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        if !matches!(
            state.phase,
            Phase::Running { .. } | Phase::RunningCancelling { .. }
        ) {
            return Err(ConnectionError::WrongState { stream });
        }
        if state.is_invocation && !state.invocation_terminal {
            return Err(ConnectionError::WrongState { stream });
        }
        self.streams.remove(&stream);
        Ok(ServerFrame::CallCompleted { stream })
    }

    fn fail(&mut self, stream: u64, failure: CallFailure) -> Result<ServerFrame, ConnectionError> {
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        if !matches!(
            state.phase,
            Phase::Dispatching
                | Phase::DispatchCancelling
                | Phase::Running { .. }
                | Phase::RunningCancelling { .. }
        ) {
            return Err(ConnectionError::WrongState { stream });
        }
        if state.is_invocation
            && matches!(
                state.phase,
                Phase::Running { .. } | Phase::RunningCancelling { .. }
            )
        {
            return Err(ConnectionError::WrongState { stream });
        }
        self.streams.remove(&stream);
        Ok(ServerFrame::CallFailed { stream, failure })
    }

    fn cancelled(&mut self, stream: u64) -> Result<ServerFrame, ConnectionError> {
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        if !matches!(
            state.phase,
            Phase::DispatchCancelling | Phase::RunningCancelling { .. }
        ) {
            return Err(ConnectionError::WrongState { stream });
        }
        if state.is_invocation && matches!(state.phase, Phase::RunningCancelling { .. }) {
            return Err(ConnectionError::WrongState { stream });
        }
        self.streams.remove(&stream);
        Ok(ServerFrame::CallCancelled { stream })
    }
}
