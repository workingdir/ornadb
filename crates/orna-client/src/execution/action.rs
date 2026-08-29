use super::*;

fn action_payload_error(message: impl Into<String>) -> ClientActionError {
    ClientActionError::InvalidPayload(message.into())
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
pub fn encode_action_payload(
    active: &ActiveDatabaseRevision,
    descriptor: &ClientActionDescriptor,
) -> Result<Vec<u8>, ClientActionError> {
    for identity in [
        descriptor.target.to_bytes(),
        descriptor.target_revision.source().to_bytes(),
        descriptor.target_revision.catalogue().to_bytes(),
        descriptor.call_site.to_bytes(),
        descriptor.result_type.to_bytes(),
    ] {
        if identity == [0; 16] {
            return Err(action_payload_error("invalid action identity"));
        }
    }
    for pair in descriptor.arguments.windows(2) {
        if pair[0].parameter() >= pair[1].parameter() {
            return Err(action_payload_error(
                "arguments are not in ascending parameter order",
            ));
        }
    }
    if descriptor.arguments.len() > orna_artifact::client_plan::MAX_ACTION_ARGUMENTS {
        return Err(action_payload_error("too many action arguments"));
    }
    for argument in &descriptor.arguments {
        if argument.parameter().to_bytes() == [0; 16] {
            return Err(action_payload_error("invalid action identity"));
        }
    }
    let mut body = Vec::new();
    body.push(match descriptor.domain {
        ActionTargetDomain::Client => 1,
        ActionTargetDomain::Server => 2,
    });
    body.extend_from_slice(&descriptor.target.to_bytes());
    body.extend_from_slice(&descriptor.target_revision.source().to_bytes());
    body.extend_from_slice(&descriptor.target_revision.catalogue().to_bytes());
    body.extend_from_slice(&descriptor.call_site.to_bytes());
    body.extend_from_slice(&descriptor.result_type.to_bytes());
    body.extend_from_slice(&(descriptor.arguments.len() as u32).to_be_bytes());
    for argument in &descriptor.arguments {
        body.extend_from_slice(&argument.parameter().to_bytes());
        let frame = encode_active_value(active, argument.value())
            .map_err(|source| action_payload_error(source.to_string()))?;
        let length = u32::try_from(frame.len())
            .map_err(|_| action_payload_error("argument frame is too large"))?;
        let additional = 4usize
            .checked_add(frame.len())
            .ok_or_else(|| action_payload_error("action payload is too large"))?;
        let next_len = body
            .len()
            .checked_add(additional)
            .ok_or_else(|| action_payload_error("action payload is too large"))?;
        let payload_len = ACTION_MAGIC
            .len()
            .checked_add(4)
            .and_then(|prefix| prefix.checked_add(next_len))
            .ok_or_else(|| action_payload_error("action payload is too large"))?;
        if payload_len > MAX_ACTION_PAYLOAD_LENGTH {
            return Err(action_payload_error("action payload is too large"));
        }
        body.try_reserve(additional)
            .map_err(|_| action_payload_error("action payload allocation failed"))?;
        body.extend_from_slice(&length.to_be_bytes());
        body.extend_from_slice(&frame);
    }
    let length = u32::try_from(body.len())
        .map_err(|_| action_payload_error("action payload is too large"))?;
    let payload_len = ACTION_MAGIC
        .len()
        .checked_add(4)
        .and_then(|prefix| prefix.checked_add(body.len()))
        .ok_or_else(|| action_payload_error("action payload is too large"))?;
    if payload_len > MAX_ACTION_PAYLOAD_LENGTH {
        return Err(action_payload_error("action payload is too large"));
    }
    let mut payload = Vec::new();
    payload
        .try_reserve(payload_len)
        .map_err(|_| action_payload_error("action payload allocation failed"))?;
    payload.extend_from_slice(ACTION_MAGIC.as_bytes());
    payload.extend_from_slice(&length.to_be_bytes());
    payload.extend_from_slice(&body);
    Ok(payload)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn action_take<'a>(
    body: &'a [u8],
    offset: &mut usize,
    length: usize,
) -> Result<&'a [u8], ClientActionError> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| action_payload_error("action payload overflow"))?;
    if end > body.len() {
        return Err(action_payload_error("truncated action payload"));
    }
    let value = &body[*offset..end];
    *offset = end;
    Ok(value)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn action_identity_bytes(body: &[u8], offset: &mut usize) -> Result<[u8; 16], ClientActionError> {
    let identity = action_take(body, offset, 16)?
        .try_into()
        .expect("action identities are exactly sixteen bytes");
    if identity == [0; 16] {
        return Err(action_payload_error("invalid action identity"));
    }
    Ok(identity)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
pub fn decode_action_payload(
    active: &ActiveDatabaseRevision,
    payload: &[u8],
) -> Result<ClientActionDescriptor, ClientActionError> {
    let magic = ACTION_MAGIC.as_bytes();
    if payload.len() < magic.len() + 4 || !payload.starts_with(magic) {
        return Err(action_payload_error("invalid action magic"));
    }
    let mut cursor = magic.len();
    let body_length = u32::from_be_bytes(payload[cursor..cursor + 4].try_into().unwrap()) as usize;
    cursor += 4;
    if payload.len() > MAX_ACTION_PAYLOAD_LENGTH || body_length > MAX_ACTION_PAYLOAD_LENGTH {
        return Err(action_payload_error("action payload is too large"));
    }
    if body_length != payload.len() - cursor {
        return Err(action_payload_error("action payload length does not match"));
    }
    let body = &payload[cursor..];
    let mut offset = 0usize;
    let domain = match action_take(body, &mut offset, 1)?[0] {
        1 => ActionTargetDomain::Client,
        2 => ActionTargetDomain::Server,
        _ => return Err(action_payload_error("unknown action domain")),
    };
    let target = FunctionId::from_bytes(action_identity_bytes(body, &mut offset)?);
    let source = orna_core::SourceRevisionId::from_bytes(action_identity_bytes(body, &mut offset)?);
    let catalogue =
        orna_core::CatalogueRevisionId::from_bytes(action_identity_bytes(body, &mut offset)?);
    let target_revision = RevisionPair::new(source, catalogue);
    if target_revision != active.pair() {
        return Err(ClientActionError::RevisionMismatch);
    }
    let call_site = CallSiteId::from_bytes(action_identity_bytes(body, &mut offset)?);
    let result_type = TypeId::from_bytes(action_identity_bytes(body, &mut offset)?);
    let count = u32::from_be_bytes(action_take(body, &mut offset, 4)?.try_into().unwrap()) as usize;
    if count > orna_artifact::client_plan::MAX_ACTION_ARGUMENTS {
        return Err(action_payload_error("too many action arguments"));
    }
    let mut arguments = Vec::with_capacity(count);
    let mut previous = None;
    for _ in 0..count {
        let parameter = ParameterId::from_bytes(action_identity_bytes(body, &mut offset)?);
        if previous.is_some_and(|value| parameter <= value) {
            return Err(action_payload_error("action arguments are not canonical"));
        }
        previous = Some(parameter);
        let frame_length =
            u32::from_be_bytes(action_take(body, &mut offset, 4)?.try_into().unwrap()) as usize;
        let frame = action_take(body, &mut offset, frame_length)?;
        let value = decode_active_value(active, frame)
            .map_err(|source| action_payload_error(source.to_string()))?;
        arguments.push(
            FunctionArgument::new(parameter, value)
                .map_err(|source| action_payload_error(source.to_string()))?,
        );
    }
    if offset != body.len() {
        return Err(action_payload_error("trailing action payload bytes"));
    }
    let descriptor = ClientActionDescriptor::new(
        domain,
        target,
        target_revision,
        call_site,
        arguments,
        result_type,
    );
    if encode_action_payload(active, &descriptor)? != payload {
        return Err(action_payload_error("non-canonical action payload"));
    }
    Ok(descriptor)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
pub(super) fn action_target_result_type(
    active: &ActiveDatabaseRevision,
    descriptor: &ClientActionDescriptor,
) -> Result<(ResourceKind, ResolvedType), ClientActionError> {
    let resolved_target = resolve_action_target(active, descriptor)?;
    let resolved = match resolved_target.definition.return_type() {
        FunctionReturn::Single(resolved) => *resolved,
        FunctionReturn::Stream(_) | FunctionReturn::Rows(_) => {
            return Err(ClientActionError::ResultTypeMismatch);
        }
    };
    let kind = ResourceKind::Scalar;
    if !resource_type_matches_id(active, resolved, descriptor.result_type) {
        return Err(ClientActionError::ResultTypeMismatch);
    }
    Ok((kind, resolved))
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn validate_action_arguments(
    active: &ActiveDatabaseRevision,
    descriptor: &ClientActionDescriptor,
) -> Result<Vec<FunctionArgument>, ClientActionError> {
    let resolved_target = resolve_action_target(active, descriptor)?;
    let definition = resolved_target.definition;
    if descriptor.arguments.len() != definition.parameters().len() {
        return Err(ClientActionError::Arguments(Box::new(
            ClientResourceError::TypeMismatch,
        )));
    }
    let mut previous = None;
    for argument in &descriptor.arguments {
        if previous.is_some_and(|value| argument.parameter() <= value) {
            return Err(ClientActionError::Arguments(Box::new(
                ClientResourceError::DuplicateArgument {
                    parameter: argument.parameter(),
                },
            )));
        }
        previous = Some(argument.parameter());
        let Some(parameter) = definition
            .parameters()
            .iter()
            .find(|candidate| candidate.id() == argument.parameter())
        else {
            return Err(ClientActionError::Arguments(Box::new(
                ClientResourceError::UnknownArgument {
                    parameter: argument.parameter(),
                },
            )));
        };
        if !runtime_value_matches(active, argument.value(), parameter.resolved_type()) {
            return Err(ClientActionError::Arguments(Box::new(
                ClientResourceError::TypeMismatch,
            )));
        }
    }
    Ok(descriptor.arguments.clone())
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub(super) fn evaluate_action_operation(
    active: &ActiveDatabaseRevision,
    operation: &orna_artifact::client_plan::ActionOperationNode,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    let mut values = Vec::with_capacity(operation.arguments().len());
    for (parameter, expression) in operation.arguments() {
        let value = evaluate_expression_with_fuel(
            active,
            expression,
            context,
            &lineage,
            arguments,
            declarations,
            grants,
            state,
            depth,
            principal,
            executor,
            local_environment,
            fuel,
        )?;
        values.push(
            FunctionArgument::new(*parameter, value)
                .map_err(|_| expression_error(context, ClientExpressionError::InvalidCall))?,
        );
    }
    let descriptor = ClientActionDescriptor::new(
        operation.domain(),
        operation.target(),
        operation.target_revision(),
        operation.call_site_id(),
        values,
        operation.result_type(),
    );
    action_target_result_type(active, &descriptor)
        .map_err(|_| expression_error(context, ClientExpressionError::InvalidCall))?;
    validate_action_arguments(active, &descriptor)
        .map_err(|_| expression_error(context, ClientExpressionError::InvalidCall))?;
    let payload = encode_action_payload(active, &descriptor)
        .map_err(|_| expression_error(context, ClientExpressionError::InvalidCall))?;
    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| expression_error(context, ClientExpressionError::TypeMismatch))?;
    let registry = registered_opaque_codecs(standard)
        .map_err(|_| expression_error(context, ClientExpressionError::TypeMismatch))?;
    let value = OpaqueValue::new(active, &registry, STD_ACTION_TYPE_ID, payload)
        .map_err(|_| expression_error(context, ClientExpressionError::TypeMismatch))?;
    Ok(RuntimeValue::Opaque(value))
}

// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn complete_client_action(
    active: &ActiveDatabaseRevision,
    action_state: &mut ClientActionState,
    completion: ClientResourceCompletion,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientActionOutcome, ClientActionError> {
    complete_client_action_inner(active, action_state, completion, executor, true)
}

// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn complete_client_action_inner(
    active: &ActiveDatabaseRevision,
    action_state: &mut ClientActionState,
    completion: ClientResourceCompletion,
    executor: &mut dyn ClientResourceExecutor,
    cancel_on_invalid: bool,
) -> Result<ClientActionOutcome, ClientActionError> {
    let completion_request_id = completion.request_id();
    let (completion_key, completion_generation) = match &completion {
        ClientResourceCompletion::Ready {
            key, generation, ..
        }
        | ClientResourceCompletion::StreamValues {
            key, generation, ..
        }
        | ClientResourceCompletion::StreamCompleted {
            key, generation, ..
        }
        | ClientResourceCompletion::Pending {
            key, generation, ..
        }
        | ClientResourceCompletion::Failed {
            key, generation, ..
        }
        | ClientResourceCompletion::Cancelled {
            key, generation, ..
        } => (*key, *generation),
    };
    let Some(resource) = action_state.resource.as_ref() else {
        return if action_state.is_stale(completion_generation) {
            Err(ClientActionError::StaleCompletion)
        } else {
            Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()))
        };
    };
    if completion_generation != resource.generation()
        || completion_key != resource.key()
        || resource.request_id() != Some(completion_request_id)
    {
        return Err(ClientActionError::StaleCompletion);
    }
    let completion_is_non_terminal = matches!(
        &completion,
        ClientResourceCompletion::Pending { .. } | ClientResourceCompletion::StreamValues { .. }
    );
    let apply_result = action_state
        .resource_mut()
        .expect("action resource was checked above")
        .apply_completion(active, completion);
    if apply_result.is_err() {
        // A same-generation malformed completion must not strand the request
        // owned by the executor. Generation and key mismatches remain stale
        // and do not cancel a newer or unrelated request. A valid pending
        // cancellation retains Loading state because the executor still owns
        // the request; a malformed terminal cancellation is treated as
        // consumed and moves the resource to the explicit Cancelled state.
        if cancel_on_invalid {
            let cancel_request = action_state
                .resource
                .as_ref()
                .and_then(|resource| resource.active_request());
            if let Some(request) = cancel_request {
                let cancellation = executor.cancel(request);
                let cancellation_is_non_terminal = matches!(
                    &cancellation,
                    ClientResourceCompletion::Pending { .. }
                        | ClientResourceCompletion::StreamValues { .. }
                );
                match action_state
                    .resource_mut()
                    .expect("action resource remains after malformed completion")
                    .apply_completion(active, cancellation)
                {
                    Ok(()) => {
                        let status = action_state
                            .resource
                            .as_ref()
                            .expect("action resource remains after cancellation")
                            .status();
                        if status == ClientResourceStatus::Loading {
                            return Err(ClientActionError::Pending);
                        }
                        let outcome = match status {
                            ClientResourceStatus::Ready => ClientActionOutcome::Completed,
                            ClientResourceStatus::Failed => redacted_action_failure(),
                            ClientResourceStatus::Cancelled => ClientActionOutcome::Cancelled,
                            ClientResourceStatus::Idle | ClientResourceStatus::Loading => {
                                unreachable!()
                            }
                        };
                        action_state.clear();
                        return Ok(outcome);
                    }
                    Err(error) => {
                        if matches!(
                            error,
                            ClientResourceError::StaleGeneration { .. }
                                | ClientResourceError::RequestKeyMismatch { .. }
                                | ClientResourceError::RequestIdMismatch { .. }
                        ) {
                            return Err(ClientActionError::StaleCompletion);
                        }
                        if cancellation_is_non_terminal {
                            return Err(ClientActionError::Pending);
                        }
                        action_state
                            .resource_mut()
                            .expect("action resource remains after consumed cancellation")
                            .mark_executor_released_cancelled();
                        return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
                    }
                }
            }
        } else if completion_is_non_terminal {
            return Err(ClientActionError::Pending);
        } else {
            action_state
                .resource_mut()
                .expect("action resource remains after consumed cancellation")
                .mark_executor_released_cancelled();
            return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
        }
        action_state.clear();
        return Ok(redacted_action_failure());
    }
    let status = action_state
        .resource
        .as_ref()
        .expect("action resource remains after completion")
        .status();
    if status == ClientResourceStatus::Loading {
        return Err(ClientActionError::Pending);
    }
    let outcome = match status {
        ClientResourceStatus::Ready => ClientActionOutcome::Completed,
        ClientResourceStatus::Failed => redacted_action_failure(),
        ClientResourceStatus::Cancelled => ClientActionOutcome::Cancelled,
        ClientResourceStatus::Idle | ClientResourceStatus::Loading => unreachable!(),
    };
    action_state.clear();
    Ok(outcome)
}

/// Cancels one pending SERVER action through its resource executor.
///
/// The executor owns the transport control. A terminal completion clears the
/// action state; a pending completion retains it for a later completion.
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn cancel_client_action_with_executor(
    active: &ActiveDatabaseRevision,
    action_state: &mut ClientActionState,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientActionOutcome, ClientActionError> {
    let Some(resource) = action_state.resource.as_ref() else {
        return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
    };
    if resource.status() != ClientResourceStatus::Loading {
        return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
    }
    let Some(request) = action_state.request.clone() else {
        return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
    };
    let completion = executor.cancel(request);
    complete_client_action_inner(active, action_state, completion, executor, false)
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub fn trigger_client_action(
    active: &ActiveDatabaseRevision,
    action: &RuntimeValue,
    authorisation: &AuthorisedInvocation,
    parent: &ClientExecutionContext,
    action_state: &mut ClientActionState,
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientActionOutcome, ClientActionError> {
    trigger_client_action_with_lineage(
        active,
        action,
        authorisation,
        parent,
        action_state,
        declarations,
        grants,
        state,
        parent.observer_lineage(),
        executor,
    )
}

fn client_action_target_is_provenance_safe(
    active: &ActiveDatabaseRevision,
    parent: ClientExecutionContext,
    target: FunctionId,
) -> bool {
    let Some(owner) = resolve_client_function(active, parent.function()) else {
        return false;
    };
    owner.revision.id() == parent.function_revision()
        && owner.definition.domain() == FunctionDomain::Client
        && owner.references.iter().any(|reference| {
            reference.source_function() == parent.function()
                && reference.source_revision() == parent.function_revision()
                && reference.kind() == DefinitionReferenceKind::FunctionCall
                && reference.target() == DefinitionReferenceTarget::Function(target)
        })
}

/// Adapts nested CLIENT resource execution to the terminal action contract.
///
/// A nested resource has no independent action completion surface. If its
/// executor reports `Pending`, the adapter cannot create a local cancellation:
/// the remote executor may still publish a committed terminal result. It
/// retains the request for the caller instead.
pub(super) struct ClientActionNestedExecutor<'a> {
    pub(super) inner: &'a mut dyn ClientResourceExecutor,
    pub(super) pending_request: Option<ClientResourceRequest>,
}

impl ClientActionNestedExecutor<'_> {
    pub(super) fn release_failed(&self) -> bool {
        self.pending_request.is_some()
    }

    pub(super) fn pending_request_identity(
        &self,
    ) -> Option<(InvocationId, ClientResourceKey, ClientResourceGeneration)> {
        self.pending_request
            .as_ref()
            .map(|request| (request.request_id(), request.key(), request.generation()))
    }

    fn pending_matches(&self, request: &ClientResourceRequest) -> bool {
        self.pending_request.as_ref().is_none_or(|pending| {
            pending.request_id() == request.request_id()
                && pending.key() == request.key()
                && pending.generation() == request.generation()
        })
    }
}

impl ClientResourceExecutor for ClientActionNestedExecutor<'_> {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        if !self.pending_matches(&request) {
            return request.failed(ACTION_FAILURE_CODE.to_owned());
        }
        let completion = self.inner.execute(request.clone());
        if !completion.matches_request(&request) {
            // A mismatched child result cannot prove that the original
            // request was released. Retain the original until explicit
            // abandonment.
            self.pending_request = Some(request.clone());
            return request.pending();
        }

        if matches!(completion, ClientResourceCompletion::Pending { .. }) {
            return self.cancel(request);
        }
        if matches!(completion, ClientResourceCompletion::StreamValues { .. }) {
            // A nested action has no poll surface of its own. Retain the
            // executor-owned request until a later terminal completion or
            // explicit abandonment arrives.
            self.pending_request = Some(request);
        } else if self.pending_request.is_some() {
            // A matching terminal completion proves that the child executor
            // consumed its request. Do not report a released child as still
            // owned when a prior stream batch was followed by completion.
            self.pending_request = None;
        }
        completion
    }

    fn abandon(&mut self, request: ClientResourceRequest) -> Result<(), String> {
        if !self.pending_matches(&request) {
            return Err("resource executor request mismatch".to_owned());
        }
        match self.inner.abandon(request.clone()) {
            Ok(()) => {
                if self.pending_request.is_some() {
                    self.pending_request = None;
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        if !self.pending_matches(&request) {
            return request.failed(ACTION_FAILURE_CODE.to_owned());
        }
        let completion = self.inner.cancel(request.clone());
        if !completion.matches_request(&request) {
            // A mismatched child result cannot prove that the original
            // request was released. Retain the original until explicit
            // abandonment.
            self.pending_request = Some(request.clone());
            return request.pending();
        }
        if matches!(
            completion,
            ClientResourceCompletion::Pending { .. }
                | ClientResourceCompletion::StreamValues { .. },
        ) {
            self.pending_request = Some(request);
        } else if self.pending_request.is_some() {
            self.pending_request = None;
        }
        completion
    }

    fn read_input(&mut self, context: ClientExecutionContext) -> Result<RuntimeValue, String> {
        self.inner.read_input(context)
    }

    fn evaluate_command(
        &mut self,
        context: ClientExecutionContext,
        command: &str,
    ) -> Result<RuntimeValue, String> {
        self.inner.evaluate_command(context, command)
    }
    fn inspect(&mut self, request: ClientInspectRequest) -> Result<RuntimeValue, String> {
        self.inner.inspect(request)
    }

    fn external_contract(
        &mut self,
        request: ClientExternalContractRequest,
    ) -> Result<RuntimeValue, String> {
        self.inner.external_contract(request)
    }
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn trigger_client_action_with_lineage(
    active: &ActiveDatabaseRevision,
    action: &RuntimeValue,
    authorisation: &AuthorisedInvocation,
    parent: &ClientExecutionContext,
    action_state: &mut ClientActionState,
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    lineage: ObserverLineage,
    executor: &mut dyn ClientResourceExecutor,
) -> Result<ClientActionOutcome, ClientActionError> {
    if parent.pair() != active.pair()
        || authorisation.target().revision() != active.pair()
        || authorisation.target().function() != parent.function()
    {
        return Err(ClientActionError::RevisionMismatch);
    }
    validate_active_catalogue(active, parent.function())
        .map_err(|_| ClientActionError::TargetMismatch)?;
    let RuntimeValue::Opaque(value) = action else {
        return Err(ClientActionError::InvalidValue);
    };
    if value.opaque_type() != STD_ACTION_TYPE_ID {
        return Err(ClientActionError::InvalidValue);
    }
    let descriptor = decode_action_payload(active, value.canonical_payload())?;
    let (kind, expected) = action_target_result_type(active, &descriptor)?;
    let values = validate_action_arguments(active, &descriptor)?;
    let target = resolve_action_target(active, &descriptor)?.target;
    let digest = ClientResourceKey::canonical_arguments_digest(active, &values)
        .map_err(|error| ClientActionError::Arguments(Box::new(error)))?;
    if !client_action_target_is_provenance_safe(active, *parent, descriptor.target) {
        return Err(ClientActionError::TargetMismatch);
    }
    // Call-site metadata in a transient action payload is caller-controlled.
    // Keep it out of the invocation context until the reference schema carries
    // an authenticated binding for it; a fresh identity prevents forged
    // metadata from spoofing nested audit correlation.
    let call_site = CallSiteId::new();
    match descriptor.domain {
        ActionTargetDomain::Server => {
            let key = ClientResourceKey::new(
                target,
                authorisation.session_principal(),
                digest,
                resource_invalidation_identity(
                    active.catalogue_hash(),
                    state.context().data_invalidation_token(),
                    security_context_digest(authorisation),
                    state.context(),
                    state.user_state_epoch(),
                ),
            );
            if let Some(resource) = action_state.resource_mut() {
                if resource.status() == ClientResourceStatus::Loading {
                    if resource.key() != key {
                        return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
                    }
                    return Err(ClientActionError::Pending);
                }
                action_state.clear();
            }
            let mut resource = ClientResource::new_with_kind(key, kind, expected);
            // Preserve a monotonic generation across terminal clears so an old
            // completion can never be accepted by a later action.
            resource.generation = ClientResourceGeneration(action_state.tombstone.value());
            let request = resource
                .begin_request_with_context_and_kind(
                    active,
                    kind,
                    ClientResourceInvocationContext::new(
                        lineage.current,
                        call_site,
                        state.context().state_profile().to_owned(),
                        state.context().instance_key().to_owned(),
                    ),
                    values,
                )
                .map_err(|error| ClientActionError::Arguments(Box::new(error)))?;
            action_state.stage_invocation(request.request_id());
            action_state.stage_request(request.clone());
            action_state.set_resource(resource);
            let completion = executor.execute(request);
            complete_client_action(active, action_state, completion, executor)
        }
        ActionTargetDomain::Client => {
            let key = ClientResourceKey::new(
                target,
                authorisation.session_principal(),
                digest,
                resource_invalidation_identity(
                    active.catalogue_hash(),
                    state.context().data_invalidation_token(),
                    security_context_digest(authorisation),
                    state.context(),
                    state.user_state_epoch(),
                ),
            );
            if let Some(resource) = action_state.resource_mut() {
                if resource.status() == ClientResourceStatus::Loading {
                    if resource.key() != key {
                        return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
                    }
                    return Err(ClientActionError::Pending);
                }
                action_state.clear();
            }
            let mut resource = ClientResource::new_with_kind(key, kind, expected);
            // Preserve a monotonic generation across terminal clears so an old
            // completion can never be accepted by a later action.
            resource.generation = ClientResourceGeneration(action_state.tombstone.value());
            let request = resource
                .begin_request_with_context_and_kind(
                    active,
                    kind,
                    ClientResourceInvocationContext::new(
                        lineage.current,
                        call_site,
                        state.context().state_profile().to_owned(),
                        state.context().instance_key().to_owned(),
                    ),
                    values,
                )
                .map_err(|error| ClientActionError::Arguments(Box::new(error)))?;
            action_state.stage_invocation(request.request_id());
            action_state.stage_request(request.clone());
            action_state.set_resource(resource);

            let mut staged = state.clone();
            staged.set_security_context_digest(security_context_digest(authorisation));
            let mut nested_executor = ClientActionNestedExecutor {
                inner: executor,
                pending_request: None,
            };
            let mut nested = Some(&mut nested_executor as &mut dyn ClientResourceExecutor);
            let result = evaluate_function(
                active,
                descriptor.target,
                request
                    .arguments()
                    .iter()
                    .map(|argument| (argument.parameter(), argument.value().clone()))
                    .collect(),
                declarations,
                grants,
                &mut staged,
                0,
                authorisation.session_principal(),
                lineage.with_current(request.request_id()),
                &mut nested,
            );
            if nested_executor.release_failed() {
                let changed_resources: Vec<_> = staged
                    .resources
                    .iter()
                    .filter_map(|(candidate_key, resource)| {
                        let replacement_cancelled =
                            state.resources.get(candidate_key).is_some_and(|previous| {
                                previous.status() == ClientResourceStatus::Loading
                                    && resource.status() == ClientResourceStatus::Idle
                                    && resource.generation().value() > previous.generation().value()
                            });
                        let replacement_terminal = same_revision_terminal_replacement(
                            active,
                            state,
                            candidate_key,
                            resource,
                        );
                        let pending_resource = nested_executor
                            .pending_request_identity()
                            .is_some_and(|(_, pending_key, pending_generation)| {
                                resource.key() == pending_key
                                    && resource.generation() == pending_generation
                                    && resource.status() == ClientResourceStatus::Loading
                            });
                        (pending_resource || replacement_cancelled || replacement_terminal)
                            .then_some((*candidate_key, resource.clone()))
                    })
                    .collect();
                for (_, resource) in changed_resources {
                    state.retain_resource(resource);
                }
                if let Some((request_id, key, generation)) =
                    nested_executor.pending_request_identity()
                {
                    action_state.clear();
                    return Err(ClientActionError::ExecutorPending {
                        code: ACTION_FAILURE_CODE.to_owned(),
                        request_id,
                        key,
                        generation,
                    });
                }
                // The child request remains owned by the executor, but no
                // retained resource can safely consume it until the caller
                // resumes the handoff. Do not retain the synthetic outer
                // request.
                action_state.clear();
                return Err(ClientActionError::Executor(ACTION_FAILURE_CODE.to_owned()));
            }
            let result_is_err = result.is_err();
            let completion = match result {
                Ok((_, value)) => request.ready(value),
                Err(ClientExecutionError::ResourceEvaluation {
                    source: ClientResourceExecutionError::Cancelled,
                    ..
                }) => request.cancelled(),
                Err(ClientExecutionError::ResourceEvaluation {
                    source: ClientResourceExecutionError::Pending { .. },
                    ..
                }) => request.cancelled(),
                Err(_) => request.failed(ACTION_FAILURE_CODE.to_owned()),
            };
            if result_is_err {
                for (key, resource) in &staged.resources {
                    let replacement_cancelled = state.resources.get(key).is_some_and(|previous| {
                        previous.status() == ClientResourceStatus::Loading
                            && resource.status() == ClientResourceStatus::Idle
                            && resource.generation().value() > previous.generation().value()
                    });
                    let replacement_terminal =
                        same_revision_terminal_replacement(active, state, key, resource);
                    if replacement_cancelled || replacement_terminal {
                        state.retain_resource(resource.clone());
                    }
                }
            }

            let outcome =
                complete_client_action(active, action_state, completion, &mut nested_executor)?;
            if outcome == ClientActionOutcome::Completed {
                *state = staged;
            }
            Ok(outcome)
        }
    }
}
