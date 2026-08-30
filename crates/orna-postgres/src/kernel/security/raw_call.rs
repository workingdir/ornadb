use super::*;

/// The owned value result of one authenticated raw call.
#[derive(Clone, Debug, PartialEq)]
pub enum AuthenticatedRawCallResult {
    /// One value evaluated by the CLIENT runtime.
    Client(RuntimeValue),
    /// Zero or more values returned in SERVER execution result order.
    Server(Vec<RuntimeValue>),
}
impl AuthenticatedRawCallResult {
    /// Transfers result values without cloning their payloads.
    pub fn into_values(self) -> Vec<RuntimeValue> {
        match self {
            Self::Client(value) => vec![value],
            Self::Server(values) => values,
        }
    }
}
/// The closed result of raw record-argument preflight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordArgumentPreflight {
    /// The call contains no record argument and needs no PostgreSQL preflight.
    NotRequired,
    /// Every record is canonical for the transaction's active revision.
    Current,
    /// At least one record is stale or incompatible with the active revision.
    Stale,
}
pub(super) fn classify_raw_server_error(error: PostgresKernelError) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerSelect(source) if raw_server_target_is_unavailable(&source) => {
            PostgresKernelError::RawServerTargetUnavailable {
                source: RawServerTargetError::Select(source),
            }
        }
        error => error,
    }
}

pub(super) fn classify_raw_identity_selected_server_error(
    error: PostgresKernelError,
    function: FunctionId,
) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerSelect(source) if raw_server_target_is_unavailable(&source) => {
            raw_call_target_unavailable(
                function,
                "raw identity-selected SERVER target is unavailable",
            )
        }
        error => error,
    }
}

pub(super) fn classify_raw_unique_text_selected_server_error(
    error: PostgresKernelError,
    function: FunctionId,
) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerSelect(source) if raw_server_target_is_unavailable(&source) => {
            raw_call_target_unavailable(
                function,
                "raw unique-Text-selected SERVER target is unavailable",
            )
        }
        error => error,
    }
}

pub(super) fn validate_raw_call_argument_shape(
    function: FunctionId,
    arguments: &[FunctionArgument],
) -> Result<(), PostgresKernelError> {
    match arguments {
        [] => Ok(()),
        [argument] if raw_call_argument_is_supported(argument) => Ok(()),
        [first, second]
            if raw_call_argument_is_supported(first) && raw_call_argument_is_supported(second) =>
        {
            Ok(())
        }
        _ => Err(raw_call_target_unavailable(
            function,
            "raw calls accept zero arguments, one supported value, or one supported argument pair",
        )),
    }
}

fn raw_call_argument_is_supported(argument: &FunctionArgument) -> bool {
    matches!(
        argument.value(),
        RuntimeValue::Boolean(_)
            | RuntimeValue::Integer(_)
            | RuntimeValue::BigInt(_)
            | RuntimeValue::Float(_)
            | RuntimeValue::Text(_)
            | RuntimeValue::Bytes(_)
            | RuntimeValue::Reference { .. }
    )
}

pub(super) fn raw_call_target_unavailable(
    function: FunctionId,
    rule: &'static str,
) -> PostgresKernelError {
    PostgresKernelError::RawCallTargetUnavailable { function, rule }
}

pub(super) fn classify_raw_server_insert_error(
    error: PostgresKernelError,
    arguments_present: bool,
    function: FunctionId,
) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerInsert(source)
            if raw_server_insert_argument_target_is_unavailable(&source, arguments_present) =>
        {
            raw_call_target_unavailable(
                function,
                "raw SERVER INSERT argument target is unavailable",
            )
        }
        PostgresKernelError::ServerInsert(source) if arguments_present => {
            PostgresKernelError::ServerInsert(source)
        }
        PostgresKernelError::ServerInsert(source)
            if raw_server_insert_target_is_unavailable(&source) =>
        {
            PostgresKernelError::RawServerTargetUnavailable {
                source: RawServerTargetError::Insert(source),
            }
        }
        error => error,
    }
}

pub(super) fn classify_raw_server_reference_mutation_error(
    error: PostgresKernelError,
    function: FunctionId,
) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerUpdate(source)
            if raw_server_update_target_is_unavailable(&source) =>
        {
            raw_call_target_unavailable(
                function,
                "raw SERVER UPDATE reference target is unavailable",
            )
        }
        PostgresKernelError::ServerDelete(source)
            if raw_server_delete_target_is_unavailable(&source) =>
        {
            raw_call_target_unavailable(
                function,
                "raw SERVER DELETE reference target is unavailable",
            )
        }
        error => error,
    }
}

fn raw_server_insert_argument_target_is_unavailable(
    error: &ServerInsertError,
    arguments_present: bool,
) -> bool {
    match error {
        ServerInsertError::NotCommitted { source, .. } => {
            raw_server_insert_argument_target_is_unavailable(source, arguments_present)
        }
        ServerInsertError::Argument { .. } => true,
        ServerInsertError::FunctionNotActive { .. }
        | ServerInsertError::FunctionSignature { .. }
        | ServerInsertError::Artifact { .. }
        | ServerInsertError::PlanDecode(_)
        | ServerInsertError::PlanInvariant { .. }
        | ServerInsertError::ReferenceEvidence { .. }
        | ServerInsertError::ComplexityLimit { .. } => arguments_present,
        _ => false,
    }
}

#[cfg(feature = "test-hooks")]
struct RawDispatchTestBarrier {
    reached: std::sync::Arc<tokio::sync::Barrier>,
    resume: std::sync::Arc<tokio::sync::Barrier>,
}

#[cfg(feature = "test-hooks")]
async fn pause_after_raw_dispatch_recovery(test_barrier: Option<&RawDispatchTestBarrier>) {
    if let Some(test_barrier) = test_barrier {
        test_barrier.reached.wait().await;
        test_barrier.resume.wait().await;
    }
}

#[cfg(not(feature = "test-hooks"))]
struct RawDispatchTestBarrier;

#[cfg(not(feature = "test-hooks"))]
async fn pause_after_raw_dispatch_recovery(_test_barrier: Option<&RawDispatchTestBarrier>) {}
impl PostgresKernel {
    /// Dispatches one authenticated parameter-free raw call inside one transaction.
    ///
    /// The kernel authorises the exact active target before it selects the
    /// function domain. An allowed CLIENT target evaluates through the current
    /// CLIENT evaluator. An allowed SERVER target must satisfy either the
    /// closed one-column raw SELECT boundary or the parameter-free raw INSERT
    /// boundary before it can return values.
    pub async fn dispatch_authenticated_raw_call(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
    ) -> Result<AuthenticatedRawCallResult, PostgresKernelError> {
        self.dispatch_authenticated_raw_call_with_arguments(authenticated_session, function, &[])
            .await
    }

    /// Dispatches one authenticated raw call with zero arguments, one
    /// supported scalar or Reference argument, or one bounded pair of those
    /// values.
    ///
    /// Other argument shapes fail before PostgreSQL is opened. An admitted
    /// shape is authorised and audited before the active target or parameter
    /// declaration is inspected.
    pub async fn dispatch_authenticated_raw_call_with_arguments(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<AuthenticatedRawCallResult, PostgresKernelError> {
        validate_raw_call_argument_shape(function, arguments)?;
        self.dispatch_authenticated_raw_call_with_options(
            authenticated_session,
            function,
            arguments,
            None,
        )
        .await
    }

    /// Pauses raw dispatch after one active and security snapshot is recovered.
    ///
    /// The hook exposes one deterministic point to the integration harness. It
    /// is absent from production builds and does not alter transaction state.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn dispatch_authenticated_raw_call_with_test_barrier(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<AuthenticatedRawCallResult, PostgresKernelError> {
        self.dispatch_authenticated_raw_call_with_arguments_and_test_barrier(
            authenticated_session,
            function,
            &[],
            reached,
            resume,
        )
        .await
    }

    /// Pauses raw dispatch with arguments after active recovery.
    ///
    /// The hook lets the integration harness alter only its disposable test
    /// database after recovery has verified the durable catalogue. It is absent
    /// from production builds and does not alter transaction state.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn dispatch_authenticated_raw_call_with_arguments_and_test_barrier(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<AuthenticatedRawCallResult, PostgresKernelError> {
        validate_raw_call_argument_shape(function, arguments)?;
        self.dispatch_authenticated_raw_call_with_options(
            authenticated_session,
            function,
            arguments,
            Some(RawDispatchTestBarrier { reached, resume }),
        )
        .await
    }

    async fn dispatch_authenticated_raw_call_with_options(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
        test_barrier: Option<RawDispatchTestBarrier>,
    ) -> Result<AuthenticatedRawCallResult, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let mut transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            pause_after_raw_dispatch_recovery(test_barrier.as_ref()).await;
            let target = InvocationTarget::new(function, active.pair());

            let decision = match system_function_by_id(function) {
                // Catalogue health is the one separately admitted raw system
                // entry. Other registry entries may enter raw dispatch only
                // when their sealed security identity is admitted.
                Some(_) if function == CATALOGUE_HEALTH_FUNCTION_ID => {
                    security.authorise_catalogue_health(authenticated_session, target)
                }
                Some(definition) if is_admitted_security_identity(definition) => {
                    security.authorise_system_function(authenticated_session, target)
                }
                Some(_) => ExecuteDecision::Denied(ExecuteDenial::UnknownFunction),
                None if active.catalogue().function_by_id(function).is_none() => {
                    // A verified-standard target can enter only through the sealed
                    // invocation boundary. Raw dispatch has no standard target path.
                    ExecuteDecision::Denied(ExecuteDenial::UnknownFunction)
                }
                None => security.authorise_execute(authenticated_session, target),
            };
            let decision = match decision {
                ExecuteDecision::Allowed(_)
                    if active
                        .catalogue()
                        .function_by_id(function)
                        .is_some_and(|definition| {
                            definition.domain() == FunctionDomain::Server
                                && !resource_target_security_is_supported(definition)
                        }) => ExecuteDecision::Denied(ExecuteDenial::UnsupportedSecurityDefiner),
                decision => decision,
            };
            let execution = match decision {
                ExecuteDecision::Denied(reason) => {
                    append_security_audit_event(
                        &transaction,
                        SecurityAuditDecision::execute_denied(
                            authenticated_session,
                            target,
                            reason,
                        ),
                    )
                    .await?;
                    Err(PostgresKernelError::RawExecuteDenied {
                        pair: active.pair(),
                        function,
                        reason,
                    })
                }
                ExecuteDecision::Allowed(authorisation) => {
                    append_security_audit_event(
                        &transaction,
                        SecurityAuditDecision::execute_allowed(&authorisation),
                    )
                    .await?;
                    match active.catalogue().function_by_id(function) {
                        None if function == CATALOGUE_HEALTH_FUNCTION_ID => {
                            if active.catalogue_hash_context().standard().is_none() {
                                Err(PostgresKernelError::DurableInvariant {
                                    relation: "_orna_kernel.active_revision",
                                    record: active.pair().catalogue().canonical(),
                                    rule: "catalogue health requires the accepted standard context",
                                })
                            } else if !arguments.is_empty() {
                                Err(raw_call_target_unavailable(
                                    function,
                                    "raw call arguments require a supported active SERVER mutation target",
                                ))
                            } else {
                                Ok(AuthenticatedRawCallResult::Client(RuntimeValue::Boolean(
                                    true,
                                )))
                            }
                        }
                        None if function == SYS_INVOKE_FUNCTION_ID => Err(
                            raw_call_target_unavailable(
                                function,
                                "sys.invoke requires its sealed request carrier",
                            ),
                        ),
                        Some(definition) if definition.domain() == FunctionDomain::Client => {
                            if !client_function_arguments_match(&active, definition, arguments) {
                                Err(raw_call_target_unavailable(
                                    function,
                                    "raw CLIENT arguments do not match the declared parameter set",
                                ))
                            } else {
                                match load_client_reference_loader(
                                    &transaction,
                                    &active,
                                    authorisation.session_principal(),
                                    client_security_context_digest(&authorisation),
                                    arguments,
                                )
                                .await
                                {
                                    Ok(loader) => {
                                        let mut state = ClientStateStore::new();
                                        state.install_reference_loader(loader);
                                        let state_context =
                                            ClientStateContext::default_for(definition.id());
                                        evaluate_authorised_client_function_with_state_context_and_arguments(
                                            &active,
                                            &authorisation,
                                            &state_context,
                                            arguments,
                                            &[],
                                            &self.capability_grants,
                                            &mut state,
                                        )
                                        .map(|result| {
                                            AuthenticatedRawCallResult::Client(result.into_value())
                                        })
                                        .map_err(PostgresKernelError::ClientExecution)
                                    }
                                    Err(error) => Err(error),
                                }
                            }
                        }
                        Some(definition) if definition.domain() == FunctionDomain::Server => {
                            let reference_argument = matches!(
                                arguments,
                                [argument]
                                    if matches!(
                                        argument.value(),
                                        RuntimeValue::Reference { .. }
                                    )
                            );
                            let reference_mutation = reference_argument
                                .then(|| raw_server_reference_mutation_target(&active, function))
                                .flatten();
                            let reference_mutation = if matches!(arguments, [_, _])
                                && raw_server_reference_value_update_target_is_selected(
                                    &active, function,
                                )
                            {
                                Some(RawServerReferenceMutation::Update)
                            } else {
                                reference_mutation
                            };
                            let identity_selected_select = reference_argument
                                && raw_identity_selected_server_select_target_is_selected(
                                    &active, function,
                                );
                            let unique_text_selected_select =
                                raw_unique_text_selected_server_select_target_is_selected(
                                    &active, function,
                                );
                            if raw_server_insert_target_is_selected(&active, function) {
                                let savepoint = transaction
                                    .savepoint("raw_server_insert_execution")
                                    .await
                                    .map_err(PostgresKernelError::Database)?;
                                let insert = if arguments.is_empty() {
                                    execute_authorised_raw_server_insert(
                                        &savepoint,
                                        &active,
                                        &authorisation,
                                    )
                                    .await
                                } else {
                                    execute_authorised_raw_server_insert_with_arguments(
                                        &savepoint,
                                        &active,
                                        &authorisation,
                                        arguments,
                                    )
                                    .await
                                };
                                match insert {
                                    Ok(value) => {
                                        savepoint
                                            .commit()
                                            .await
                                            .map_err(PostgresKernelError::Database)?;
                                        Ok(AuthenticatedRawCallResult::Server(vec![value]))
                                    }
                                    Err(error) => {
                                        savepoint
                                            .rollback()
                                            .await
                                            .map_err(PostgresKernelError::Database)?;
                                        Err(classify_raw_server_insert_error(
                                            error,
                                            !arguments.is_empty(),
                                            function,
                                        ))
                                    }
                                }
                            } else if let Some(operation) = reference_mutation {
                                let savepoint = transaction
                                    .savepoint("raw_server_reference_mutation_execution")
                                    .await
                                    .map_err(PostgresKernelError::Database)?;
                                let mutation = execute_authorised_raw_server_reference_mutation(
                                    &savepoint,
                                    &active,
                                    &authorisation,
                                    operation,
                                    arguments,
                                )
                                .await;
                                match mutation {
                                    Ok(values) => {
                                        savepoint
                                            .commit()
                                            .await
                                            .map_err(PostgresKernelError::Database)?;
                                        Ok(AuthenticatedRawCallResult::Server(values))
                                    }
                                    Err(error) => {
                                        savepoint
                                            .rollback()
                                            .await
                                            .map_err(PostgresKernelError::Database)?;
                                        Err(classify_raw_server_reference_mutation_error(
                                            error, function,
                                        ))
                                    }
                                }
                            } else if !arguments.is_empty()
                                && !identity_selected_select
                                && !unique_text_selected_select
                            {
                                Err(raw_call_target_unavailable(
                                    function,
                                    "raw call arguments require a supported active SERVER mutation target",
                                ))
                            } else {
                                let savepoint = transaction
                                    .savepoint("raw_server_select_execution")
                                    .await
                                    .map_err(PostgresKernelError::Database)?;
                                let server = execute_authorised_raw_server_select(
                                    &savepoint,
                                    &active,
                                    &authorisation,
                                    arguments,
                                )
                                .await
                                .map(AuthenticatedRawCallResult::Server);
                                match server {
                                    Ok(result) => {
                                        savepoint
                                            .commit()
                                            .await
                                            .map_err(PostgresKernelError::Database)?;
                                        Ok(result)
                                    }
                                    Err(error) => {
                                        savepoint
                                            .rollback()
                                            .await
                                            .map_err(PostgresKernelError::Database)?;
                                        Err(if identity_selected_select {
                                            classify_raw_identity_selected_server_error(
                                                error, function,
                                            )
                                        } else if unique_text_selected_select {
                                            classify_raw_unique_text_selected_server_error(
                                                error, function,
                                            )
                                        } else {
                                            classify_raw_server_error(error)
                                        })
                                    }
                                }
                            }
                        }
                        Some(_) if !arguments.is_empty() => Err(raw_call_target_unavailable(
                            function,
                            "raw call arguments require a supported active SERVER mutation target",
                        )),
                        Some(_) => Err(PostgresKernelError::DurableInvariant {
                            relation: "active catalogue",
                            record: function.canonical(),
                            rule: "active function domain is unsupported by raw dispatch",
                        }),
                        None => Err(PostgresKernelError::DurableInvariant {
                            relation: "active catalogue",
                            record: function.canonical(),
                            rule: "allowed raw target must exist in the active catalogue",
                        }),
                    }
                }
            };
            append_client_capability_audit(
                &transaction,
                authenticated_session,
                &active,
                target,
                &execution,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            execution
        }
        .await;
        finish_authenticated_dispatch_session(operation, database_session.shutdown().await)
    }
    /// Revalidates raw record arguments against one transactional active revision.
    ///
    /// An empty list performs no PostgreSQL operation. A non-empty list opens
    /// one read-only, repeatable-read transaction and returns only whether all
    /// record values remain canonical for its recovered active revision. This
    /// operation does not select, authorise, audit, or execute a target.
    pub async fn preflight_record_arguments(
        &self,
        records: Vec<RecordValue>,
    ) -> Result<RecordArgumentPreflight, PostgresKernelError> {
        if records.is_empty() {
            return Ok(RecordArgumentPreflight::NotRequired);
        }
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = recover_active_revision(&transaction).await?;
            let mut outcome = RecordArgumentPreflight::Current;
            for record in records {
                if encode_active_value(&active, &RuntimeValue::Record(record)).is_err() {
                    outcome = RecordArgumentPreflight::Stale;
                }
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(outcome)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }
}
