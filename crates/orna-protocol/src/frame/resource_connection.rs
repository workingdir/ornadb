use super::*;

impl ResourceProtocolConnection {
    pub const fn new() -> Self {
        Self {
            high_water_mark: None,
            closed: false,
            streams: BTreeMap::new(),
            terminal: BTreeMap::new(),
        }
    }

    pub const fn high_water_mark(&self) -> Option<u64> {
        self.high_water_mark
    }

    pub fn live_resources(&self) -> usize {
        self.streams.len()
    }

    /// Returns the current item and byte credit for a retained resource stream.
    ///
    /// This inspection does not mutate connection state. The request identity is
    /// checked against the stream before its credit is returned.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceConnectionError::UnknownStream`] when the stream is not
    /// retained and [`ResourceConnectionError::MismatchedRequest`] when the
    /// request identity does not match the retained stream.
    pub fn resource_credit(
        &self,
        stream_id: u64,
        request_id: InvocationId,
    ) -> Result<ResourceCredit, ResourceConnectionError> {
        let state = self.state_for(stream_id, request_id)?;
        Ok(ResourceCredit {
            item_available: state.item_window,
            byte_available: state.byte_window,
        })
    }

    /// Returns the server-generated nested invocation identity after acceptance.
    ///
    /// Before the acceptance handshake completes, the resource has no lineage
    /// identity and this returns `Ok(None)`.
    pub fn resource_nested_invocation_id(
        &self,
        stream_id: u64,
        request_id: InvocationId,
    ) -> Result<Option<InvocationId>, ResourceConnectionError> {
        Ok(self.state_for(stream_id, request_id)?.nested_invocation_id)
    }

    pub fn receive(
        &mut self,
        frame: ResourceClientFrame,
    ) -> Result<ResourceFrameDisposition, ResourceConnectionError> {
        match frame {
            ResourceClientFrame::Request(request) => self.open(request),
            ResourceClientFrame::WindowUpdate(update) => {
                require_resource_invocation_id(update.request_id)
                    .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
                self.window_update(update)
            }
            ResourceClientFrame::Cancel(cancel) => {
                require_resource_invocation_id(cancel.request_id)
                    .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
                self.cancel(cancel)
            }
        }
    }

    /// Opens one resource stream and reserves its request identity.
    ///
    pub fn open(
        &mut self,
        request: ResourceRequest,
    ) -> Result<ResourceFrameDisposition, ResourceConnectionError> {
        if self.closed {
            return Err(ResourceConnectionError::WrongState {
                stream_id: request.stream_id,
            });
        }
        require_resource_stream(request.stream_id)
            .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        require_resource_invocation_id(request.request_id)
            .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        require_resource_invocation_id(request.parent_invocation_id)
            .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        require_resource_call_site_id(request.call_site_id)
            .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        require_resource_generation(request.generation)
            .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        require_resource_text(&request.state_profile)
            .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        require_resource_text(&request.function_instance_key)
            .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        require_resource_kind_windows(
            request.resource_kind,
            request.item_window,
            request.byte_window,
        )
        .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        validate_resource_arguments(&request.arguments)
            .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        for argument in &request.arguments {
            require_resource_value(&argument.value)
                .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        }
        if let Some(previous) = self.high_water_mark
            && request.stream_id <= previous
        {
            return Err(ResourceConnectionError::StreamNotIncreasing {
                stream_id: request.stream_id,
                previous,
            });
        }
        if self.streams.contains_key(&request.stream_id)
            || self.terminal.contains_key(&request.stream_id)
        {
            return Err(ResourceConnectionError::StreamNotIncreasing {
                stream_id: request.stream_id,
                previous: self.high_water_mark.unwrap_or(0),
            });
        }
        if self.request_id_in_use(request.request_id) {
            return Err(ResourceConnectionError::DuplicateRequestId {
                request_id: request.request_id,
            });
        }
        if self.streams.len() == MAX_LIVE_STREAMS {
            return Err(ResourceConnectionError::TooManyLiveResources);
        }
        self.high_water_mark = Some(request.stream_id);
        self.streams.insert(
            request.stream_id,
            ResourceState {
                request_id: request.request_id,
                nested_invocation_id: None,
                target_revision: request.target_revision,
                resource_kind: request.resource_kind,
                phase: ResourcePhase::Requested,
                accepted: false,
                item_window: request.item_window,
                byte_window: request.byte_window,
                next_batch_sequence: 0,
                last_batch_sequence: None,
                total_items: 0,
            },
        );
        Ok(ResourceFrameDisposition::Applied)
    }

    pub(super) fn apply(
        &mut self,
        frame: ResourceServerFrame,
    ) -> Result<ResourceFrameDisposition, ResourceConnectionError> {
        match frame {
            ResourceServerFrame::Accepted(frame) => {
                require_resource_invocation_id(frame.request_id)
                    .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
                self.accepted(frame)
            }
            ResourceServerFrame::Values(frame) => {
                require_resource_invocation_id(frame.request_id)
                    .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
                self.values(frame)
            }
            ResourceServerFrame::Completed(frame) => {
                require_resource_invocation_id(frame.request_id)
                    .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
                self.completed(frame)
            }
            ResourceServerFrame::Failed(frame) => {
                require_resource_invocation_id(frame.request_id)
                    .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
                self.terminal_frame(
                    frame.stream_id,
                    frame.request_id,
                    frame.target_revision,
                    ResourceTerminalKind::Failed,
                )
            }
            ResourceServerFrame::Cancelled(frame) => {
                require_resource_invocation_id(frame.request_id)
                    .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
                self.terminal_frame(
                    frame.stream_id,
                    frame.request_id,
                    frame.target_revision,
                    ResourceTerminalKind::Cancelled,
                )
            }
        }
    }

    /// Applies one server frame after validating its canonical ORV5/ORV6
    /// values and declared byte count.
    ///
    /// `Self::apply` operates on an already decoded frame and therefore
    /// cannot reconstruct the active-revision-dependent value bytes. Adapters
    /// that receive values from an in-memory producer (rather than through
    /// [`decode_resource_server_frame`]) must use this entry point so a forged
    /// `byte_count` cannot consume less credit than the canonical values
    /// require. Validation happens before any state transition or credit
    /// mutation.
    pub fn apply_constructed(
        &mut self,
        active: &ActiveDatabaseRevision,
        registry: &OpaqueCodecRegistry,
        frame: ResourceServerFrame,
    ) -> Result<ResourceFrameDisposition, ResourceConnectionError> {
        if let ResourceServerFrame::Values(values) = &frame {
            if let Some(disposition) = self.check_terminal(
                values.stream_id,
                values.request_id,
                Some(values.target_revision),
            )? {
                return Ok(disposition);
            }
            let state = self.state_for(values.stream_id, values.request_id)?;
            if state.target_revision != values.target_revision {
                return Err(ResourceConnectionError::ResourceRevisionMismatch {
                    stream_id: values.stream_id,
                });
            }
            encode_resource_values(active, registry, values)
                .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        }
        self.apply(frame)
    }
    /// Applies the terminal cancellation response after the client has already
    /// moved the request into its terminal late-frame state.
    ///
    /// The ordinary `Self::apply` path treats a terminal frame as late and
    /// drops it. The authenticated server adapter must emit the one
    /// cancellation response that confirms a client cancellation, so it uses
    /// this explicit transition after [`Self::receive`] accepts the cancel.
    pub fn apply_cancelled_after_client_cancel(
        &self,
        frame: ResourceCancelled,
    ) -> Result<ResourceFrameDisposition, ResourceConnectionError> {
        require_resource_invocation_id(frame.request_id)
            .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        let Some((expected_request_id, expected_revision, terminal_kind)) =
            self.terminal.get(&frame.stream_id)
        else {
            return Err(ResourceConnectionError::UnknownStream {
                stream_id: frame.stream_id,
            });
        };
        if *expected_request_id != frame.request_id {
            return Err(ResourceConnectionError::MismatchedRequest {
                stream_id: frame.stream_id,
            });
        }
        if *expected_revision != frame.target_revision {
            return Err(ResourceConnectionError::ResourceRevisionMismatch {
                stream_id: frame.stream_id,
            });
        }
        Ok(match terminal_kind {
            ResourceTerminalKind::Cancelled => ResourceFrameDisposition::Applied,
            ResourceTerminalKind::Completed | ResourceTerminalKind::Failed => {
                ResourceFrameDisposition::DroppedLate
            }
        })
    }

    fn check_terminal(
        &self,
        stream_id: u64,
        request_id: InvocationId,
        target_revision: Option<RevisionPair>,
    ) -> Result<Option<ResourceFrameDisposition>, ResourceConnectionError> {
        if let Some((expected_request_id, expected_revision, _)) = self.terminal.get(&stream_id) {
            if *expected_request_id != request_id {
                return Err(ResourceConnectionError::MismatchedRequest { stream_id });
            }
            if target_revision.is_some_and(|revision| revision != *expected_revision) {
                return Err(ResourceConnectionError::ResourceRevisionMismatch { stream_id });
            }
            return Ok(Some(ResourceFrameDisposition::DroppedLate));
        }
        Ok(None)
    }

    fn state_for(
        &self,
        stream_id: u64,
        request_id: InvocationId,
    ) -> Result<&ResourceState, ResourceConnectionError> {
        let state = self
            .streams
            .get(&stream_id)
            .ok_or(ResourceConnectionError::UnknownStream { stream_id })?;
        if state.request_id != request_id {
            return Err(ResourceConnectionError::MismatchedRequest { stream_id });
        }
        Ok(state)
    }

    fn accepted(
        &mut self,
        frame: ResourceAccepted,
    ) -> Result<ResourceFrameDisposition, ResourceConnectionError> {
        if let Some(disposition) = self.check_terminal(
            frame.stream_id,
            frame.request_id,
            Some(frame.target_revision),
        )? {
            return Ok(disposition);
        }
        let state = self.state_for(frame.stream_id, frame.request_id)?;
        if state.target_revision != frame.target_revision
            || state.resource_kind != frame.resource_kind
        {
            return Err(ResourceConnectionError::ResourceAcceptanceMismatch {
                stream_id: frame.stream_id,
            });
        }
        if state.accepted {
            return Err(ResourceConnectionError::WrongState {
                stream_id: frame.stream_id,
            });
        }
        if !matches!(state.phase, ResourcePhase::Requested) {
            return Err(ResourceConnectionError::WrongState {
                stream_id: frame.stream_id,
            });
        }
        require_resource_invocation_id(frame.nested_invocation_id)
            .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        let state = self
            .streams
            .get_mut(&frame.stream_id)
            .expect("resource state checked");
        state.accepted = true;
        state.nested_invocation_id = Some(frame.nested_invocation_id);
        if matches!(state.phase, ResourcePhase::Requested) {
            state.phase = ResourcePhase::Live;
        }
        Ok(ResourceFrameDisposition::Applied)
    }

    fn values(
        &mut self,
        frame: ResourceValues,
    ) -> Result<ResourceFrameDisposition, ResourceConnectionError> {
        if let Some(disposition) = self.check_terminal(
            frame.stream_id,
            frame.request_id,
            Some(frame.target_revision),
        )? {
            return Ok(disposition);
        }
        let state = self.state_for(frame.stream_id, frame.request_id)?;
        if state.target_revision != frame.target_revision {
            return Err(ResourceConnectionError::ResourceRevisionMismatch {
                stream_id: frame.stream_id,
            });
        }
        if !(state.accepted && matches!(state.phase, ResourcePhase::Live)) {
            return Err(ResourceConnectionError::WrongState {
                stream_id: frame.stream_id,
            });
        }
        if frame.values.len() > MAX_RESOURCE_BATCH_ITEMS {
            return Err(ResourceConnectionError::InvalidFrame {
                source: FrameCodecError::TooManyResourceEntries {
                    actual: frame.values.len(),
                    maximum: MAX_RESOURCE_BATCH_ITEMS,
                },
            });
        }
        if u64::from(frame.byte_count) > MAX_FRAME_PAYLOAD_LENGTH as u64 {
            return Err(ResourceConnectionError::InvalidFrame {
                source: FrameCodecError::PayloadTooLarge {
                    actual: frame.byte_count as usize,
                    maximum: MAX_FRAME_PAYLOAD_LENGTH,
                },
            });
        }
        if frame
            .values
            .iter()
            .any(|value| invocation_carrier_type_id(value).is_some())
        {
            let carrier = frame
                .values
                .iter()
                .find_map(invocation_carrier_type_id)
                .expect("carrier checked");
            return Err(ResourceConnectionError::InvalidFrame {
                source: FrameCodecError::InvocationCarrierNotAccepted { carrier },
            });
        }
        if frame.values.is_empty()
            || frame.item_count == 0
            || frame.item_count as usize != frame.values.len()
        {
            return Err(ResourceConnectionError::ResourceBatchMismatch {
                stream_id: frame.stream_id,
            });
        }
        if matches!(state.resource_kind, ResourceKind::Single) && frame.item_count != 1 {
            return Err(ResourceConnectionError::ResourceBatchMismatch {
                stream_id: frame.stream_id,
            });
        }
        if matches!(state.resource_kind, ResourceKind::Single)
            && state.last_batch_sequence.is_some()
        {
            return Err(ResourceConnectionError::WrongState {
                stream_id: frame.stream_id,
            });
        }
        if frame.batch_sequence != state.next_batch_sequence {
            return Err(ResourceConnectionError::BatchSequenceMismatch {
                stream_id: frame.stream_id,
                expected: state.next_batch_sequence,
                actual: frame.batch_sequence,
            });
        }
        if state.last_batch_sequence == Some(u64::MAX) {
            return Err(ResourceConnectionError::SequenceExhausted {
                stream_id: frame.stream_id,
            });
        }
        let required_items = u64::from(frame.item_count);
        let required_bytes = u64::from(frame.byte_count);
        if required_items > state.item_window || required_bytes > state.byte_window {
            return Err(ResourceConnectionError::InsufficientCredit {
                stream_id: frame.stream_id,
                item_available: state.item_window,
                item_required: required_items,
                byte_available: state.byte_window,
                byte_required: required_bytes,
            });
        }
        let total_items = state.total_items.checked_add(required_items).ok_or(
            ResourceConnectionError::ResourceTotalMismatch {
                stream_id: frame.stream_id,
                expected: MAX_RESOURCE_TOTAL_ITEMS,
                actual: u64::MAX,
            },
        )?;
        require_resource_total_items(total_items)
            .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        // `u64::MAX` is a valid final sequence; retain it as the exhausted
        // sentinel so the following batch is rejected without wrapping.
        let next_sequence = if frame.batch_sequence == u64::MAX {
            u64::MAX
        } else {
            state.next_batch_sequence.checked_add(1).ok_or(
                ResourceConnectionError::SequenceExhausted {
                    stream_id: frame.stream_id,
                },
            )?
        };
        let state = self
            .streams
            .get_mut(&frame.stream_id)
            .expect("resource state checked");
        state.item_window -= required_items;
        state.byte_window -= required_bytes;
        state.next_batch_sequence = next_sequence;
        state.last_batch_sequence = Some(frame.batch_sequence);
        state.total_items = total_items;
        Ok(ResourceFrameDisposition::Applied)
    }

    fn completed(
        &mut self,
        frame: ResourceCompleted,
    ) -> Result<ResourceFrameDisposition, ResourceConnectionError> {
        if let Some(disposition) = self.check_terminal(
            frame.stream_id,
            frame.request_id,
            Some(frame.target_revision),
        )? {
            return Ok(disposition);
        }
        let state = self.state_for(frame.stream_id, frame.request_id)?;
        if state.target_revision != frame.target_revision {
            return Err(ResourceConnectionError::ResourceRevisionMismatch {
                stream_id: frame.stream_id,
            });
        }
        if !(state.accepted && matches!(state.phase, ResourcePhase::Live)) {
            return Err(ResourceConnectionError::WrongState {
                stream_id: frame.stream_id,
            });
        }
        if matches!(state.resource_kind, ResourceKind::Single)
            && state.last_batch_sequence.is_none()
        {
            return Err(ResourceConnectionError::WrongState {
                stream_id: frame.stream_id,
            });
        }
        let expected_sequence = state.last_batch_sequence.unwrap_or(0);
        if frame.final_batch_sequence != expected_sequence {
            return Err(ResourceConnectionError::BatchSequenceMismatch {
                stream_id: frame.stream_id,
                expected: expected_sequence,
                actual: frame.final_batch_sequence,
            });
        }
        if frame.total_items != state.total_items {
            return Err(ResourceConnectionError::ResourceTotalMismatch {
                stream_id: frame.stream_id,
                expected: state.total_items,
                actual: frame.total_items,
            });
        }
        self.finish(
            frame.stream_id,
            frame.request_id,
            frame.target_revision,
            ResourceTerminalKind::Completed,
        );
        Ok(ResourceFrameDisposition::Applied)
    }

    fn terminal_frame(
        &mut self,
        stream_id: u64,
        request_id: InvocationId,
        target_revision: RevisionPair,
        terminal_kind: ResourceTerminalKind,
    ) -> Result<ResourceFrameDisposition, ResourceConnectionError> {
        if let Some(disposition) =
            self.check_terminal(stream_id, request_id, Some(target_revision))?
        {
            return Ok(disposition);
        }
        let state = self.state_for(stream_id, request_id)?;
        if state.target_revision != target_revision {
            return Err(ResourceConnectionError::ResourceRevisionMismatch { stream_id });
        }
        if matches!(state.resource_kind, ResourceKind::Single)
            && state.last_batch_sequence.is_some()
        {
            return Err(ResourceConnectionError::WrongState { stream_id });
        }
        self.finish(stream_id, request_id, target_revision, terminal_kind);
        Ok(ResourceFrameDisposition::Applied)
    }

    fn window_update(
        &mut self,
        update: ResourceWindowUpdate,
    ) -> Result<ResourceFrameDisposition, ResourceConnectionError> {
        if let Some(disposition) = self.check_terminal(update.stream_id, update.request_id, None)? {
            return Ok(disposition);
        }
        let state = self.state_for(update.stream_id, update.request_id)?;
        if state.resource_kind != ResourceKind::Stream
            || !state.accepted
            || !matches!(state.phase, ResourcePhase::Live)
        {
            return Err(ResourceConnectionError::WrongState {
                stream_id: update.stream_id,
            });
        }
        require_resource_window_addition(update.add_items, update.add_bytes)
            .map_err(|source| ResourceConnectionError::InvalidFrame { source })?;
        let items = state
            .item_window
            .checked_add(update.add_items)
            .filter(|value| *value <= MAX_RESOURCE_WINDOW);
        let bytes = state
            .byte_window
            .checked_add(update.add_bytes)
            .filter(|value| *value <= MAX_RESOURCE_WINDOW);
        let (Some(items), Some(bytes)) = (items, bytes) else {
            return Err(ResourceConnectionError::InvalidFrame {
                source: FrameCodecError::ResourceWindowOverflow,
            });
        };
        let state = self
            .streams
            .get_mut(&update.stream_id)
            .expect("resource state checked");
        state.item_window = items;
        state.byte_window = bytes;
        Ok(ResourceFrameDisposition::Applied)
    }

    fn cancel(
        &mut self,
        cancel: ResourceCancel,
    ) -> Result<ResourceFrameDisposition, ResourceConnectionError> {
        if let Some(disposition) = self.check_terminal(cancel.stream_id, cancel.request_id, None)? {
            return Ok(disposition);
        }
        let state = self.state_for(cancel.stream_id, cancel.request_id)?;
        if matches!(state.phase, ResourcePhase::Requested | ResourcePhase::Live) {
            self.finish(
                cancel.stream_id,
                cancel.request_id,
                state.target_revision,
                ResourceTerminalKind::Cancelled,
            );
            return Ok(ResourceFrameDisposition::Applied);
        }
        Err(ResourceConnectionError::WrongState {
            stream_id: cancel.stream_id,
        })
    }

    fn request_id_in_use(&self, request_id: InvocationId) -> bool {
        self.streams
            .values()
            .any(|state| state.request_id == request_id)
            || self
                .terminal
                .values()
                .any(|(retained_request_id, _, _)| *retained_request_id == request_id)
    }

    fn finish(
        &mut self,
        stream_id: u64,
        request_id: InvocationId,
        target_revision: RevisionPair,
        terminal_kind: ResourceTerminalKind,
    ) {
        self.streams.remove(&stream_id);
        self.terminal
            .insert(stream_id, (request_id, target_revision, terminal_kind));
        self.retain_terminal_history();
    }

    fn retain_terminal_history(&mut self) {
        while self.terminal.len() > MAX_REQUEST_ID_HISTORY {
            let Some(stream) = self.terminal.keys().next().copied() else {
                break;
            };
            self.terminal.remove(&stream);
        }
    }

    pub fn shutdown(&mut self) -> usize {
        let finished = self.streams.len();
        self.closed = true;
        let streams: Vec<_> = self
            .streams
            .iter()
            .map(|(stream, state)| {
                (
                    *stream,
                    (
                        state.request_id,
                        state.target_revision,
                        ResourceTerminalKind::Cancelled,
                    ),
                )
            })
            .collect();
        self.streams.clear();
        for (stream, identity) in streams {
            self.terminal.insert(stream, identity);
        }
        self.retain_terminal_history();
        finished
    }
}
