use std::time::SystemTime;

use orna_client::{
    ClientExecutionResult, evaluate_client_function as evaluate_authorised_client_function,
};
use orna_core::{
    CatalogueRevisionId, FunctionId, PrincipalId, SecurityAuditEventId, SourceRevisionId,
    catalogue::FunctionDomain,
    revision::{ActiveDatabaseRevision, RevisionPair},
    security::{
        AuthenticatedSession, CATALOGUE_HEALTH_FUNCTION_ID, CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
        ExecuteDecision, ExecuteGrant, InvocationTarget, LocalPeerAuthenticationError,
        LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus, RoleMembership,
        SecurityAuditDecision, SecurityAuditDenial, SecurityAuditEvent, SecurityAuditKind,
        SecurityAuditOutcome, SecuritySnapshot, SessionBindingError,
    },
    system::{SYS_INVOKE_FUNCTION_ID, system_function_by_id},
    value::{FunctionArgument, RecordValue, RuntimeValue},
};
use orna_protocol::encode_active_value;
use tokio_postgres::{IsolationLevel, Row, Transaction, types::FromSqlOwned};

use crate::{
    PostgresKernel, PostgresKernelError, RawServerTargetError,
    bootstrap::require_current_migrations,
    recovery::recover_active_revision,
    server_execution::{
        ServerSelectResult, execute_authorised_raw_server_select, execute_authorised_server_select,
        raw_identity_selected_server_select_target_is_selected, raw_server_target_is_unavailable,
    },
    server_mutation_execution::{
        ServerInsertError, execute_authorised_raw_server_insert,
        execute_authorised_raw_server_insert_with_arguments,
        execute_authorised_raw_server_reference_mutation, raw_server_delete_target_is_unavailable,
        raw_server_insert_target_is_selected, raw_server_insert_target_is_unavailable,
        raw_server_reference_mutation_target, raw_server_update_target_is_unavailable,
    },
    server_runtime::configure_and_recover,
};

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

            let decision = if system_function_by_id(function).is_some() {
                security.authorise_system_function(authenticated_session, target)
            } else {
                security.authorise_execute(authenticated_session, target)
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
                            if arguments.is_empty() {
                                evaluate_authorised_client_function(&active, &authorisation)
                                    .map(|result| {
                                        AuthenticatedRawCallResult::Client(result.into_value())
                                    })
                                    .map_err(PostgresKernelError::ClientExecution)
                            } else {
                                Err(raw_call_target_unavailable(
                                    function,
                                    "raw call arguments require a supported active SERVER mutation target",
                                ))
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
                            let identity_selected_select = reference_argument
                                && raw_identity_selected_server_select_target_is_selected(
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
                            } else if !arguments.is_empty() && !identity_selected_select {
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
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            execution
        }
        .await;
        finish_authenticated_server_select_session(operation, database_session.shutdown().await)
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

    /// Executes one authorised SERVER `SELECT` against one active snapshot.
    ///
    /// The operation records and commits its protected `EXECUTE` decision. It
    /// executes an allowed target through a savepoint. A target failure rolls
    /// back only that savepoint, then commits the allowed audit decision before
    /// it returns the original target error.
    pub async fn execute_authenticated_server_select(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_authenticated_server_select_with_options(
            authenticated_session,
            function,
            arguments,
            None,
            false,
        )
        .await
    }

    /// Pauses protected SERVER execution after security recovery for race proof.
    ///
    /// The hook exposes one deterministic point to the integration harness. It
    /// is absent from production builds and deliberately does not alter the
    /// transaction or decision authority.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_authenticated_server_select_with_test_barrier(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_authenticated_server_select_with_options(
            authenticated_session,
            function,
            arguments,
            Some(AuthenticatedSelectTestBarrier { reached, resume }),
            false,
        )
        .await
    }

    /// Forces driver shutdown after commit for cleanup-failure proof.
    ///
    /// The hook lets the integration harness prove that cleanup failure
    /// overrides a committed result. It is absent from production builds.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_authenticated_server_select_with_forced_post_commit_driver_shutdown(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_authenticated_server_select_with_options(
            authenticated_session,
            function,
            arguments,
            None,
            true,
        )
        .await
    }

    async fn execute_authenticated_server_select_with_options(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
        test_barrier: Option<AuthenticatedSelectTestBarrier>,
        force_post_commit_driver_shutdown: bool,
    ) -> Result<ServerSelectResult, PostgresKernelError> {
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
            pause_after_authenticated_select_recovery(test_barrier.as_ref()).await;
            let target = InvocationTarget::new(function, active.pair());

            match security.authorise_execute(authenticated_session, target) {
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
                    transaction
                        .commit()
                        .await
                        .map_err(PostgresKernelError::Database)?;
                    Err(PostgresKernelError::ServerExecuteDenied {
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
                    let savepoint = transaction
                        .savepoint("server_select_execution")
                        .await
                        .map_err(PostgresKernelError::Database)?;
                    let execution = execute_authorised_server_select(
                        &savepoint,
                        &active,
                        &authorisation,
                        arguments,
                    )
                    .await;
                    match execution {
                        Ok(result) => {
                            savepoint
                                .commit()
                                .await
                                .map_err(PostgresKernelError::Database)?;
                            transaction
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
                            transaction
                                .commit()
                                .await
                                .map_err(PostgresKernelError::Database)?;
                            Err(error)
                        }
                    }
                }
            }
        }
        .await;
        #[cfg(feature = "test-hooks")]
        if operation.is_ok() && force_post_commit_driver_shutdown {
            database_session.abort_driver();
        }
        #[cfg(not(feature = "test-hooks"))]
        let _ = force_post_commit_driver_shutdown;
        finish_authenticated_server_select_session(operation, database_session.shutdown().await)
    }

    /// Authenticates a kernel-supplied Linux peer UID with no selected roles.
    ///
    /// The operation appends and commits one protected audit record before it
    /// returns either the authenticated session or an expected typed denial.
    /// Database insertion, commit, or session shutdown failure replaces the
    /// authentication result with a kernel failure.
    pub async fn authenticate_local_peer(
        &self,
        uid: u32,
    ) -> Result<AuthenticatedSession, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let security = recover_security_snapshot(&transaction).await?;
            let mapped_principal = security
                .local_peer_credentials()
                .find(|credential| credential.uid() == uid)
                .map(LocalPeerCredential::principal);
            let authentication = security.authenticate_local_peer(uid);
            let decision = match &authentication {
                Ok(session) => SecurityAuditDecision::authentication_allowed(session),
                Err(reason) => SecurityAuditDecision::authentication_denied(
                    mapped_principal,
                    *reason,
                )
                .map_err(|_| PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel security snapshot",
                    record: "local peer authentication".to_owned(),
                    rule: "mapped principal evidence must agree with the authentication result",
                })?,
            };
            append_security_audit_event(&transaction, decision).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(authentication)
        }
        .await;
        finish_security_session(operation, database_session.shutdown().await)?
            .map_err(PostgresKernelError::LocalPeerAuthentication)
    }

    /// Authorises and evaluates one CLIENT function against one active snapshot.
    ///
    /// The operation appends and commits the protected `EXECUTE` decision
    /// before it returns a value, a typed denial, or a pure evaluator failure.
    /// Database insertion, commit, or session shutdown failure replaces that
    /// operation result with a kernel failure.
    pub async fn evaluate_client_function(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
    ) -> Result<ClientExecutionResult, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = recover_active_revision(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let target = InvocationTarget::new(function, active.pair());
            let (decision, execution) = match security
                .authorise_execute(authenticated_session, target)
            {
                ExecuteDecision::Allowed(authorisation) => {
                    let decision = SecurityAuditDecision::execute_allowed(&authorisation);
                    let execution = evaluate_authorised_client_function(&active, &authorisation)
                        .map_err(PostgresKernelError::ClientExecution);
                    (decision, execution)
                }
                ExecuteDecision::Denied(reason) => {
                    let decision = SecurityAuditDecision::execute_denied(
                        authenticated_session,
                        target,
                        reason,
                    );
                    let execution = Err(PostgresKernelError::ClientExecuteDenied {
                        pair: active.pair(),
                        function,
                        reason,
                    });
                    (decision, execution)
                }
            };
            append_security_audit_event(&transaction, decision).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(execution)
        }
        .await;
        finish_security_session(operation, database_session.shutdown().await)?
    }

    /// Recovers the security decision snapshot for the active revision.
    pub async fn recover_security_snapshot(&self) -> Result<SecuritySnapshot, PostgresKernelError> {
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
            let snapshot = recover_security_snapshot(&transaction).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(snapshot)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Recovers protected security audit history in database sequence order.
    pub async fn recover_security_audit_events(
        &self,
    ) -> Result<Vec<SecurityAuditEvent>, PostgresKernelError> {
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
            let events = load_security_audit_events(&transaction).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(events)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Installs the fixed local service identity used by catalogue health.
    ///
    /// Repeating the exact UID is idempotent. A partial or conflicting durable
    /// identity fails without repair.
    pub async fn install_catalogue_health_service(
        &self,
        uid: u32,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::Serializable)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            if active.catalogue_hash_context().standard().is_none() {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.active_revision",
                    record: active.pair().catalogue().canonical(),
                    rule: "catalogue health service requires the accepted standard context",
                });
            }
            if active
                .catalogue()
                .function_by_id(CATALOGUE_HEALTH_FUNCTION_ID)
                .is_some()
            {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.catalogue_functions",
                    record: CATALOGUE_HEALTH_FUNCTION_ID.canonical(),
                    rule: "application catalogue uses the reserved catalogue health identity",
                });
            }
            lock_catalogue_health_identity(&transaction).await?;
            let current = recover_security_snapshot_for_active(&transaction, &active).await?;
            match catalogue_health_service_uid(&current)? {
                None => {
                    if current
                        .local_peer_credentials()
                        .any(|credential| credential.uid() == uid)
                    {
                        return Err(catalogue_health_identity_error(
                            "_orna_kernel.security_local_peer_credentials",
                            "the catalogue health UID already selects another principal",
                        ));
                    }
                    insert_catalogue_health_identity(&transaction, uid).await?;
                }
                Some(installed_uid) if installed_uid == uid => {}
                Some(_) => {
                    return Err(catalogue_health_identity_error(
                        "_orna_kernel.security_local_peer_credentials",
                        "the reserved catalogue health service identity must be complete",
                    ));
                }
            }
            let recovered = recover_security_snapshot_for_active(&transaction, &active).await?;
            require_catalogue_health_snapshot(&recovered, uid)?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(recovered)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Grants the fixed catalogue-health service exactly one active application function.
    ///
    /// The expected pair prevents a stale source-apply caller from changing
    /// security for a later catalogue. The operation rebuilds the complete
    /// snapshot in one serializable transaction and is idempotent for the
    /// exact existing grant.
    pub async fn grant_catalogue_health_service_execute(
        &self,
        expected: RevisionPair,
        function: FunctionId,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::Serializable)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            lock_active_revision(&transaction, expected).await?;
            let active = configure_and_recover(&transaction).await?;
            lock_catalogue_health_identity(&transaction).await?;
            let current = recover_security_snapshot_for_active(&transaction, &active).await?;
            let uid = catalogue_health_service_uid(&current)?.ok_or_else(|| {
                catalogue_health_identity_error(
                    "_orna_kernel.security_principals",
                    "the reserved catalogue health service identity must be complete",
                )
            })?;
            require_catalogue_health_snapshot(&current, uid)?;
            if function == CATALOGUE_HEALTH_FUNCTION_ID {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "active catalogue",
                    record: function.canonical(),
                    rule: "the catalogue health intrinsic cannot receive an application grant",
                });
            }
            if active.catalogue().function_by_id(function).is_none() {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "active catalogue",
                    record: function.canonical(),
                    rule: "the requested function must exist in the active application catalogue",
                });
            }
            let mut grants = current.execute_grants().collect::<Vec<_>>();
            let requested_grant = ExecuteGrant::new(
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                function,
            );
            if !grants.contains(&requested_grant) {
                grants.push(requested_grant);
            }
            let candidate = SecuritySnapshot::new_with_local_peer_credentials(
                active.pair(),
                current.functions().collect(),
                current.principals().collect(),
                current.memberships().collect(),
                grants,
                current.local_peer_credentials().collect(),
            )
            .map_err(PostgresKernelError::SecuritySnapshot)?;
            require_complete_function_set(&transaction, &candidate).await?;
            insert_execute_grant_if_absent(&transaction, requested_grant).await?;
            let recovered = recover_security_snapshot_for_active(&transaction, &active).await?;
            if !security_snapshots_match(&candidate, &recovered) {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_execute_grants",
                    record: function.canonical(),
                    rule: "recovered fixed-service grant does not match the persisted security snapshot",
                });
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(recovered)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Atomically replaces all durable security decision records.
    pub async fn replace_security_snapshot(
        &self,
        snapshot: &SecuritySnapshot,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::Serializable)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            lock_active_revision(&transaction, snapshot.revision()).await?;
            require_complete_function_set(&transaction, snapshot).await?;
            lock_catalogue_health_identity(&transaction).await?;
            let active = recover_active_revision(&transaction).await?;
            let current = recover_security_snapshot_for_active(&transaction, &active).await?;
            require_catalogue_health_identity_preserved(&current, snapshot)?;
            replace_security_rows(&transaction, snapshot).await?;
            let recovered = recover_security_snapshot(&transaction).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(recovered)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }
}

async fn lock_catalogue_health_identity(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    transaction
        .batch_execute(
            "LOCK TABLE _orna_kernel.security_principals,
                        _orna_kernel.security_local_peer_credentials
             IN SHARE ROW EXCLUSIVE MODE",
        )
        .await
        .map_err(PostgresKernelError::Database)
}

async fn insert_catalogue_health_identity(
    transaction: &Transaction<'_>,
    uid: u32,
) -> Result<(), PostgresKernelError> {
    let principal = CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID.to_bytes().to_vec();
    transaction
        .execute(
            "INSERT INTO _orna_kernel.security_principals (id, kind, status)
             VALUES ($1, 'service', 'active')",
            &[&principal],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.security_local_peer_credentials (uid, principal_id)
             VALUES ($1, $2)",
            &[&i64::from(uid), &principal],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(())
}

fn require_catalogue_health_identity_preserved(
    current: &SecuritySnapshot,
    candidate: &SecuritySnapshot,
) -> Result<(), PostgresKernelError> {
    match catalogue_health_service_uid(current)? {
        None => {
            if snapshot_contains_catalogue_health_identity(candidate) {
                return Err(catalogue_health_identity_error(
                    "_orna_kernel.security_principals",
                    "the reserved catalogue health service identity must be installed through its fixed setup",
                ));
            }
            Ok(())
        }
        Some(uid) => require_catalogue_health_snapshot(candidate, uid),
    }
}

fn snapshot_contains_catalogue_health_identity(snapshot: &SecuritySnapshot) -> bool {
    snapshot
        .principals()
        .any(|principal| principal.id() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
        || snapshot
            .local_peer_credentials()
            .any(|credential| credential.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
}

fn security_snapshots_match(left: &SecuritySnapshot, right: &SecuritySnapshot) -> bool {
    left.revision() == right.revision()
        && left.functions().eq(right.functions())
        && left.principals().eq(right.principals())
        && left.memberships().eq(right.memberships())
        && left.execute_grants().eq(right.execute_grants())
        && left
            .local_peer_credentials()
            .eq(right.local_peer_credentials())
}

fn require_catalogue_health_snapshot(
    snapshot: &SecuritySnapshot,
    uid: u32,
) -> Result<(), PostgresKernelError> {
    let principal = snapshot
        .principals()
        .find(|principal| principal.id() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID);
    let credential = snapshot.local_peer_credentials().find(|credential| {
        credential.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID || credential.uid() == uid
    });
    if principal
        != Some(Principal::new(
            CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            PrincipalKind::Service,
            PrincipalStatus::Active,
        ))
        || credential
            != Some(LocalPeerCredential::new(
                uid,
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            ))
    {
        return Err(catalogue_health_identity_error(
            "_orna_kernel.security_principals",
            "the reserved catalogue health service identity must be preserved",
        ));
    }
    Ok(())
}

fn catalogue_health_service_uid(
    snapshot: &SecuritySnapshot,
) -> Result<Option<u32>, PostgresKernelError> {
    let principal = snapshot
        .principals()
        .find(|principal| principal.id() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID);
    let credential = snapshot
        .local_peer_credentials()
        .find(|credential| credential.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID);
    match (principal, credential) {
        (None, None) => Ok(None),
        (Some(principal), Some(credential))
            if principal
                == Principal::new(
                    CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                    PrincipalKind::Service,
                    PrincipalStatus::Active,
                ) =>
        {
            Ok(Some(credential.uid()))
        }
        (Some(_), None) => Err(catalogue_health_identity_error(
            "_orna_kernel.security_local_peer_credentials",
            "the reserved catalogue health service identity must be complete",
        )),
        _ => Err(catalogue_health_identity_error(
            "_orna_kernel.security_principals",
            "the reserved catalogue health principal must be an active service",
        )),
    }
}

fn catalogue_health_identity_error(
    relation: &'static str,
    rule: &'static str,
) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation,
        record: CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID.canonical(),
        rule,
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

#[cfg(feature = "test-hooks")]
struct AuthenticatedSelectTestBarrier {
    reached: std::sync::Arc<tokio::sync::Barrier>,
    resume: std::sync::Arc<tokio::sync::Barrier>,
}

#[cfg(feature = "test-hooks")]
async fn pause_after_authenticated_select_recovery(
    test_barrier: Option<&AuthenticatedSelectTestBarrier>,
) {
    if let Some(test_barrier) = test_barrier {
        test_barrier.reached.wait().await;
        test_barrier.resume.wait().await;
    }
}

#[cfg(not(feature = "test-hooks"))]
struct AuthenticatedSelectTestBarrier;

#[cfg(not(feature = "test-hooks"))]
async fn pause_after_authenticated_select_recovery(
    _test_barrier: Option<&AuthenticatedSelectTestBarrier>,
) {
}

fn finish_security_session<T>(
    operation: Result<T, PostgresKernelError>,
    shutdown: Result<(), PostgresKernelError>,
) -> Result<T, PostgresKernelError> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn finish_authenticated_server_select_session<T>(
    operation: Result<T, PostgresKernelError>,
    shutdown: Result<(), PostgresKernelError>,
) -> Result<T, PostgresKernelError> {
    shutdown?;
    operation
}

fn classify_raw_server_error(error: PostgresKernelError) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerSelect(source) if raw_server_target_is_unavailable(&source) => {
            PostgresKernelError::RawServerTargetUnavailable {
                source: RawServerTargetError::Select(source),
            }
        }
        error => error,
    }
}

fn classify_raw_identity_selected_server_error(
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

fn validate_raw_call_argument_shape(
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

fn raw_call_target_unavailable(function: FunctionId, rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::RawCallTargetUnavailable { function, rule }
}

fn classify_raw_server_insert_error(
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

fn classify_raw_server_reference_mutation_error(
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

async fn append_security_audit_event(
    transaction: &Transaction<'_>,
    decision: SecurityAuditDecision,
) -> Result<(), PostgresKernelError> {
    let event = SecurityAuditEventId::new();
    let event_id = event.to_bytes().to_vec();
    let kind = match decision.kind() {
        SecurityAuditKind::Authentication => "authentication",
        SecurityAuditKind::Execute => "execute",
    };
    let outcome = match decision.outcome() {
        SecurityAuditOutcome::Allowed => "allowed",
        SecurityAuditOutcome::Denied => "denied",
    };
    let session_principal = decision
        .session_principal()
        .map(|principal| principal.to_bytes().to_vec());
    let effective_principal = decision
        .effective_principal()
        .map(|principal| principal.to_bytes().to_vec());
    let authorising_principal = decision
        .authorising_principal()
        .map(|principal| principal.to_bytes().to_vec());
    let (function, source_revision, catalogue_revision) = match decision.target() {
        Some(target) => (
            Some(target.function().to_bytes().to_vec()),
            Some(target.revision().source().to_bytes().to_vec()),
            Some(target.revision().catalogue().to_bytes().to_vec()),
        ),
        None => (None, None, None),
    };
    let denial_reason = match decision.denial() {
        None => None,
        Some(SecurityAuditDenial::Authentication(reason)) => {
            Some(encode_authentication_audit_denial(reason))
        }
        Some(SecurityAuditDenial::Execute(reason)) => Some(encode_execute_audit_denial(reason)),
    };
    transaction
        .execute(
            "INSERT INTO _orna_kernel.security_audit_events
                 (event_id, event_kind, outcome, session_principal_id,
                  effective_principal_id, authorising_principal_id, function_id,
                  source_revision_id, catalogue_revision_id, denial_reason)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &event_id,
                &kind,
                &outcome,
                &session_principal,
                &effective_principal,
                &authorising_principal,
                &function,
                &source_revision,
                &catalogue_revision,
                &denial_reason,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(())
}

async fn lock_active_revision(
    transaction: &Transaction<'_>,
    expected: RevisionPair,
) -> Result<(), PostgresKernelError> {
    let row = transaction
        .query_one(
            "SELECT source_revision_id, catalogue_revision_id
             FROM _orna_kernel.active_revision
             WHERE singleton = true
             FOR UPDATE",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let active = RevisionPair::new(
        orna_core::SourceRevisionId::from_bytes(exact_id(
            &row,
            "source_revision_id",
            "active source revision is not exactly 16 bytes",
        )?),
        orna_core::CatalogueRevisionId::from_bytes(exact_id(
            &row,
            "catalogue_revision_id",
            "active catalogue revision is not exactly 16 bytes",
        )?),
    );
    if expected != active {
        return Err(PostgresKernelError::SecurityRevisionMismatch { expected, active });
    }
    Ok(())
}

async fn require_complete_function_set(
    transaction: &Transaction<'_>,
    snapshot: &SecuritySnapshot,
) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT function_id
             FROM _orna_kernel.catalogue_functions
             WHERE catalogue_revision_id = (
                 SELECT catalogue_revision_id
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true
             )
             ORDER BY function_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let active = rows
        .iter()
        .map(|row| {
            exact_id(
                row,
                "function_id",
                "active function identity is not exactly 16 bytes",
            )
            .map(FunctionId::from_bytes)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if active != snapshot.functions().collect::<Vec<_>>() {
        return Err(PostgresKernelError::SecurityFunctionSetMismatch);
    }
    Ok(())
}

async fn replace_security_rows(
    transaction: &Transaction<'_>,
    snapshot: &SecuritySnapshot,
) -> Result<(), PostgresKernelError> {
    transaction
        .batch_execute(
            "DELETE FROM _orna_kernel.security_local_peer_credentials;
             DELETE FROM _orna_kernel.security_execute_grants;
             DELETE FROM _orna_kernel.security_role_memberships;
             DELETE FROM _orna_kernel.security_principals;",
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    for principal in snapshot.principals() {
        let id = principal.id().to_bytes().to_vec();
        let kind = encode_principal_kind(principal.kind());
        let status = encode_principal_status(principal.status());
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_principals (id, kind, status)
                 VALUES ($1, $2, $3)",
                &[&id, &kind, &status],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for credential in snapshot.local_peer_credentials() {
        let uid = i64::from(credential.uid());
        let principal = credential.principal().to_bytes().to_vec();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_local_peer_credentials (uid, principal_id)
                 VALUES ($1, $2)",
                &[&uid, &principal],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for membership in snapshot.memberships() {
        let role = membership.role().to_bytes().to_vec();
        let member = membership.member().to_bytes().to_vec();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_role_memberships (role_id, member_id)
                 VALUES ($1, $2)",
                &[&role, &member],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for grant in snapshot.execute_grants() {
        let grantee = grant.grantee().to_bytes().to_vec();
        let function = grant.function().to_bytes().to_vec();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_execute_grants (grantee_id, function_id)
                 VALUES ($1, $2)",
                &[&grantee, &function],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    Ok(())
}

async fn insert_execute_grant_if_absent(
    transaction: &Transaction<'_>,
    grant: ExecuteGrant,
) -> Result<(), PostgresKernelError> {
    let grantee = grant.grantee().to_bytes().to_vec();
    let function = grant.function().to_bytes().to_vec();
    transaction
        .execute(
            "INSERT INTO _orna_kernel.security_execute_grants (grantee_id, function_id)
             VALUES ($1, $2)
             ON CONFLICT (grantee_id, function_id) DO NOTHING",
            &[&grantee, &function],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(())
}

async fn recover_security_snapshot(
    transaction: &Transaction<'_>,
) -> Result<SecuritySnapshot, PostgresKernelError> {
    let active = recover_active_revision(transaction).await?;
    recover_security_snapshot_for_active(transaction, &active).await
}

async fn recover_security_snapshot_for_active(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
) -> Result<SecuritySnapshot, PostgresKernelError> {
    let functions = active
        .catalogue()
        .functions()
        .iter()
        .map(|function| function.id())
        .collect::<Vec<_>>();
    let principals = load_principals(transaction).await?;
    let memberships = load_memberships(transaction).await?;
    let grants = load_grants(transaction).await?;
    let local_peer_credentials = load_local_peer_credentials(transaction).await?;

    SecuritySnapshot::new_with_local_peer_credentials(
        active.pair(),
        functions,
        principals,
        memberships,
        grants,
        local_peer_credentials,
    )
    .map_err(PostgresKernelError::SecuritySnapshot)
}

async fn load_principals(
    transaction: &Transaction<'_>,
) -> Result<Vec<Principal>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT id, kind, status
             FROM _orna_kernel.security_principals
             ORDER BY id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    rows.iter()
        .map(|row| {
            let id = PrincipalId::from_bytes(exact_id(
                row,
                "id",
                "security principal identity is not exactly 16 bytes",
            )?);
            let kind = decode_principal_kind(row.try_get("kind").map_err(|source| {
                row_decode(
                    "_orna_kernel.security_principals",
                    id.canonical(),
                    "kind",
                    source,
                )
            })?)?;
            let status = decode_principal_status(row.try_get("status").map_err(|source| {
                row_decode(
                    "_orna_kernel.security_principals",
                    id.canonical(),
                    "status",
                    source,
                )
            })?)?;
            Ok(Principal::new(id, kind, status))
        })
        .collect()
}

async fn load_memberships(
    transaction: &Transaction<'_>,
) -> Result<Vec<RoleMembership>, PostgresKernelError> {
    transaction
        .query(
            "SELECT role_id, member_id
             FROM _orna_kernel.security_role_memberships
             ORDER BY member_id, role_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .iter()
        .map(|row| {
            Ok(RoleMembership::new(
                PrincipalId::from_bytes(exact_id(
                    row,
                    "role_id",
                    "security role identity is not exactly 16 bytes",
                )?),
                PrincipalId::from_bytes(exact_id(
                    row,
                    "member_id",
                    "security member identity is not exactly 16 bytes",
                )?),
            ))
        })
        .collect()
}

async fn load_grants(
    transaction: &Transaction<'_>,
) -> Result<Vec<ExecuteGrant>, PostgresKernelError> {
    transaction
        .query(
            "SELECT grantee_id, function_id
             FROM _orna_kernel.security_execute_grants
             ORDER BY grantee_id, function_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .iter()
        .map(|row| {
            Ok(ExecuteGrant::new(
                PrincipalId::from_bytes(exact_id(
                    row,
                    "grantee_id",
                    "security grantee identity is not exactly 16 bytes",
                )?),
                FunctionId::from_bytes(exact_id(
                    row,
                    "function_id",
                    "security grant function identity is not exactly 16 bytes",
                )?),
            ))
        })
        .collect()
}

async fn load_local_peer_credentials(
    transaction: &Transaction<'_>,
) -> Result<Vec<LocalPeerCredential>, PostgresKernelError> {
    transaction
        .query(
            "SELECT uid, principal_id
             FROM _orna_kernel.security_local_peer_credentials
             ORDER BY uid",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .iter()
        .map(|row| {
            let stored_uid: i64 = row.try_get("uid").map_err(|source| {
                row_decode(
                    "_orna_kernel.security_local_peer_credentials",
                    "selected row".to_owned(),
                    "uid",
                    source,
                )
            })?;
            let uid =
                u32::try_from(stored_uid).map_err(|_| PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_local_peer_credentials",
                    record: stored_uid.to_string(),
                    rule: "local peer UID must fit the unsigned 32-bit range",
                })?;
            let principal = PrincipalId::from_bytes(exact_id(
                row,
                "principal_id",
                "local peer principal identity is not exactly 16 bytes",
            )?);
            Ok(LocalPeerCredential::new(uid, principal))
        })
        .collect()
}

async fn load_security_audit_events(
    transaction: &Transaction<'_>,
) -> Result<Vec<SecurityAuditEvent>, PostgresKernelError> {
    transaction
        .query(
            "SELECT sequence, event_id, recorded_at, event_kind, outcome,
                    session_principal_id, effective_principal_id,
                    authorising_principal_id, function_id, source_revision_id,
                    catalogue_revision_id, denial_reason
             FROM _orna_kernel.security_audit_events
             ORDER BY sequence",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .iter()
        .map(decode_security_audit_event)
        .collect()
}

fn decode_security_audit_event(row: &Row) -> Result<SecurityAuditEvent, PostgresKernelError> {
    let sequence: i64 = audit_column(row, "selected row", "sequence")?;
    let record = sequence.to_string();
    let id = SecurityAuditEventId::from_bytes(audit_id(row, &record, "event_id")?);
    let recorded_at: SystemTime = audit_column(row, &record, "recorded_at")?;
    let kind: String = audit_column(row, &record, "event_kind")?;
    let outcome: String = audit_column(row, &record, "outcome")?;
    let session_principal =
        audit_optional_id(row, &record, "session_principal_id")?.map(PrincipalId::from_bytes);
    let effective_principal =
        audit_optional_id(row, &record, "effective_principal_id")?.map(PrincipalId::from_bytes);
    let authorising_principal =
        audit_optional_id(row, &record, "authorising_principal_id")?.map(PrincipalId::from_bytes);
    let function = audit_optional_id(row, &record, "function_id")?.map(FunctionId::from_bytes);
    let source_revision =
        audit_optional_id(row, &record, "source_revision_id")?.map(SourceRevisionId::from_bytes);
    let catalogue_revision = audit_optional_id(row, &record, "catalogue_revision_id")?
        .map(CatalogueRevisionId::from_bytes);
    let denial_reason: Option<String> = audit_column(row, &record, "denial_reason")?;

    let decision = match (kind.as_str(), outcome.as_str()) {
        ("authentication", "allowed")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_none()
                && source_revision.is_none()
                && catalogue_revision.is_none()
                && denial_reason.is_none() =>
        {
            SecurityAuditDecision::recover_authentication_allowed(require_audit_value(
                session_principal,
                &record,
                "allowed authentication requires a session principal",
            )?)
        }
        ("authentication", "denied")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_none()
                && source_revision.is_none()
                && catalogue_revision.is_none() =>
        {
            let reason = decode_authentication_audit_denial(
                require_audit_value(
                    denial_reason,
                    &record,
                    "denied authentication requires a reason",
                )?,
                &record,
            )?;
            SecurityAuditDecision::authentication_denied(session_principal, reason).map_err(
                |_| audit_invariant(&record, "authentication principal and reason must agree"),
            )?
        }
        ("execute", "allowed") if denial_reason.is_none() => {
            let target = audit_target(function, source_revision, catalogue_revision, &record)?;
            SecurityAuditDecision::recover_execute_allowed(
                require_audit_value(
                    session_principal,
                    &record,
                    "allowed EXECUTE requires a session principal",
                )?,
                require_audit_value(
                    effective_principal,
                    &record,
                    "allowed EXECUTE requires an effective principal",
                )?,
                require_audit_value(
                    authorising_principal,
                    &record,
                    "allowed EXECUTE requires an authorising principal",
                )?,
                target,
            )
        }
        ("execute", "denied")
            if effective_principal.is_none() && authorising_principal.is_none() =>
        {
            let target = audit_target(function, source_revision, catalogue_revision, &record)?;
            let reason = decode_execute_audit_denial(
                require_audit_value(denial_reason, &record, "denied EXECUTE requires a reason")?,
                &record,
            )?;
            SecurityAuditDecision::recover_execute_denied(
                require_audit_value(
                    session_principal,
                    &record,
                    "denied EXECUTE requires a session principal",
                )?,
                target,
                reason,
            )
        }
        _ => {
            return Err(audit_invariant(
                &record,
                "audit event shape is not recognised",
            ));
        }
    };

    Ok(SecurityAuditEvent::new(id, sequence, recorded_at, decision))
}

fn audit_target(
    function: Option<FunctionId>,
    source: Option<SourceRevisionId>,
    catalogue: Option<CatalogueRevisionId>,
    record: &str,
) -> Result<InvocationTarget, PostgresKernelError> {
    Ok(InvocationTarget::new(
        require_audit_value(function, record, "EXECUTE requires a function")?,
        RevisionPair::new(
            require_audit_value(source, record, "EXECUTE requires a source revision")?,
            require_audit_value(catalogue, record, "EXECUTE requires a catalogue revision")?,
        ),
    ))
}

fn decode_authentication_audit_denial(
    value: String,
    record: &str,
) -> Result<LocalPeerAuthenticationError, PostgresKernelError> {
    let invalid = |reason| LocalPeerAuthenticationError::InvalidPrincipal(reason);
    match value.as_str() {
        "authentication_unknown_uid" => Ok(LocalPeerAuthenticationError::UnknownUid),
        "authentication_unknown_session_principal" => {
            Ok(invalid(SessionBindingError::UnknownSessionPrincipal))
        }
        "authentication_disabled_session_principal" => {
            Ok(invalid(SessionBindingError::DisabledSessionPrincipal))
        }
        "authentication_role_cannot_authenticate" => {
            Ok(invalid(SessionBindingError::RoleCannotAuthenticate))
        }
        "authentication_duplicate_active_role" => {
            Ok(invalid(SessionBindingError::DuplicateActiveRole))
        }
        "authentication_unknown_active_role" => Ok(invalid(SessionBindingError::UnknownActiveRole)),
        "authentication_disabled_active_role" => {
            Ok(invalid(SessionBindingError::DisabledActiveRole))
        }
        "authentication_active_principal_is_not_role" => {
            Ok(invalid(SessionBindingError::ActivePrincipalIsNotRole))
        }
        "authentication_unreachable_active_role" => {
            Ok(invalid(SessionBindingError::UnreachableActiveRole))
        }
        _ => Err(audit_invariant(
            record,
            "authentication denial reason is unsupported",
        )),
    }
}

fn encode_authentication_audit_denial(reason: LocalPeerAuthenticationError) -> &'static str {
    match reason {
        LocalPeerAuthenticationError::UnknownUid => "authentication_unknown_uid",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::UnknownSessionPrincipal,
        ) => "authentication_unknown_session_principal",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::DisabledSessionPrincipal,
        ) => "authentication_disabled_session_principal",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::RoleCannotAuthenticate,
        ) => "authentication_role_cannot_authenticate",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::DuplicateActiveRole,
        ) => "authentication_duplicate_active_role",
        LocalPeerAuthenticationError::InvalidPrincipal(SessionBindingError::UnknownActiveRole) => {
            "authentication_unknown_active_role"
        }
        LocalPeerAuthenticationError::InvalidPrincipal(SessionBindingError::DisabledActiveRole) => {
            "authentication_disabled_active_role"
        }
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::ActivePrincipalIsNotRole,
        ) => "authentication_active_principal_is_not_role",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::UnreachableActiveRole,
        ) => "authentication_unreachable_active_role",
    }
}

fn decode_execute_audit_denial(
    value: String,
    record: &str,
) -> Result<orna_core::security::ExecuteDenial, PostgresKernelError> {
    use orna_core::security::ExecuteDenial;

    match value.as_str() {
        "execute_invalid_session" => Ok(ExecuteDenial::InvalidSession),
        "execute_unknown_function" => Ok(ExecuteDenial::UnknownFunction),
        "execute_revision_mismatch" => Ok(ExecuteDenial::RevisionMismatch),
        "execute_missing_grant" => Ok(ExecuteDenial::MissingExecuteGrant),
        _ => Err(audit_invariant(
            record,
            "EXECUTE denial reason is unsupported",
        )),
    }
}

fn encode_execute_audit_denial(reason: orna_core::security::ExecuteDenial) -> &'static str {
    use orna_core::security::ExecuteDenial;

    match reason {
        ExecuteDenial::InvalidSession => "execute_invalid_session",
        ExecuteDenial::UnknownFunction => "execute_unknown_function",
        ExecuteDenial::RevisionMismatch => "execute_revision_mismatch",
        ExecuteDenial::MissingExecuteGrant => "execute_missing_grant",
    }
}

fn require_audit_value<T>(
    value: Option<T>,
    record: &str,
    rule: &'static str,
) -> Result<T, PostgresKernelError> {
    value.ok_or_else(|| audit_invariant(record, rule))
}

fn audit_optional_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<Option<[u8; 16]>, PostgresKernelError> {
    let value: Option<Vec<u8>> = audit_column(row, record, column)?;
    value
        .map(|bytes| {
            bytes.try_into().map_err(|_| {
                audit_invariant(record, "audit identity must be exactly sixteen bytes")
            })
        })
        .transpose()
}

fn audit_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    let bytes: Vec<u8> = audit_column(row, record, column)?;
    bytes
        .try_into()
        .map_err(|_| audit_invariant(record, "audit event identity must be exactly sixteen bytes"))
}

fn audit_column<T: FromSqlOwned>(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<T, PostgresKernelError> {
    row.try_get(column)
        .map_err(|source| PostgresKernelError::RowDecode {
            relation: "_orna_kernel.security_audit_events",
            record: record.to_owned(),
            column,
            rule: "security audit column must use its exact PostgreSQL type",
            source,
        })
}

fn audit_invariant(record: &str, rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.security_audit_events",
        record: record.to_owned(),
        rule,
    }
}

fn exact_id(
    row: &Row,
    column: &'static str,
    rule: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    let bytes: Vec<u8> = row.try_get(column).map_err(|source| {
        row_decode(
            "_orna_kernel security snapshot",
            "selected row".to_owned(),
            column,
            source,
        )
    })?;
    bytes
        .try_into()
        .map_err(|_| PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel security snapshot",
            record: "selected row".to_owned(),
            rule,
        })
}

fn row_decode(
    relation: &'static str,
    record: String,
    column: &'static str,
    source: tokio_postgres::Error,
) -> PostgresKernelError {
    PostgresKernelError::RowDecode {
        relation,
        record,
        column,
        rule: "security snapshot column must use its exact PostgreSQL type",
        source,
    }
}

fn encode_principal_kind(kind: PrincipalKind) -> &'static str {
    match kind {
        PrincipalKind::User => "user",
        PrincipalKind::Role => "role",
        PrincipalKind::Service => "service",
    }
}

fn decode_principal_kind(value: String) -> Result<PrincipalKind, PostgresKernelError> {
    match value.as_str() {
        "user" => Ok(PrincipalKind::User),
        "role" => Ok(PrincipalKind::Role),
        "service" => Ok(PrincipalKind::Service),
        _ => Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_principals",
            record: value,
            rule: "principal kind must be user, role, or service",
        }),
    }
}

fn encode_principal_status(status: PrincipalStatus) -> &'static str {
    match status {
        PrincipalStatus::Active => "active",
        PrincipalStatus::Disabled => "disabled",
    }
}

fn decode_principal_status(value: String) -> Result<PrincipalStatus, PostgresKernelError> {
    match value.as_str() {
        "active" => Ok(PrincipalStatus::Active),
        "disabled" => Ok(PrincipalStatus::Disabled),
        _ => Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_principals",
            record: value,
            rule: "principal status must be active or disabled",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::{
        CatalogueRevisionId, FieldId, ObjectId, ParameterId, TypeId,
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName, SchemaDefinition,
        },
        value::{EnumValue, RuntimeFloat},
    };

    const RAW_CALL_FUNCTION: FunctionId = FunctionId::from_bytes([0x61; 16]);
    const RAW_CALL_PARAMETER: ParameterId = ParameterId::from_bytes([0x62; 16]);

    #[test]
    fn raw_call_argument_shape_accepts_zero_one_and_supported_pairs() {
        validate_raw_call_argument_shape(RAW_CALL_FUNCTION, &[])
            .expect("zero arguments must be accepted");
        for value in [
            RuntimeValue::Boolean(true),
            RuntimeValue::Integer(1),
            RuntimeValue::BigInt(2),
            RuntimeValue::Float(RuntimeFloat::new(3.5).expect("finite Float argument")),
            RuntimeValue::Text("text".to_string()),
            RuntimeValue::Bytes(vec![0x00, 0xff]),
        ] {
            let argument = FunctionArgument::new(RAW_CALL_PARAMETER, value)
                .expect("supported scalar argument is valid");
            validate_raw_call_argument_shape(RAW_CALL_FUNCTION, std::slice::from_ref(&argument))
                .expect("one supported scalar argument must be accepted");
        }
        let reference = FunctionArgument::new(
            RAW_CALL_PARAMETER,
            RuntimeValue::Reference {
                target: TypeId::from_bytes([0x65; 16]),
                object: ObjectId::from_bytes([0x66; 16]),
            },
        )
        .expect("Reference argument is valid");
        assert_eq!(reference.parameter(), RAW_CALL_PARAMETER);
        assert_eq!(
            reference.value(),
            &RuntimeValue::Reference {
                target: TypeId::from_bytes([0x65; 16]),
                object: ObjectId::from_bytes([0x66; 16]),
            }
        );
        validate_raw_call_argument_shape(RAW_CALL_FUNCTION, std::slice::from_ref(&reference))
            .expect("one Reference argument must be accepted");

        let supported = [
            RuntimeValue::Boolean(false),
            RuntimeValue::Integer(1),
            RuntimeValue::BigInt(2),
            RuntimeValue::Float(RuntimeFloat::new(3.5).expect("finite Float argument")),
            RuntimeValue::Text("text".to_string()),
            RuntimeValue::Bytes(vec![0x00, 0xff]),
            reference.value().clone(),
        ];
        for (index, value) in supported.into_iter().enumerate() {
            let pair = [
                FunctionArgument::new(RAW_CALL_PARAMETER, RuntimeValue::Boolean(true))
                    .expect("Boolean argument is valid"),
                FunctionArgument::new(ParameterId::from_bytes([0x70 + index as u8; 16]), value)
                    .expect("supported pair argument is valid"),
            ];
            validate_raw_call_argument_shape(RAW_CALL_FUNCTION, &pair)
                .expect("a pair of supported arguments must be accepted");
        }
    }

    #[test]
    fn raw_call_argument_shape_rejects_other_argument_sets() {
        let enum_type = TypeId::from_bytes([0x67; 16]);
        let catalogue = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::new(),
            vec![SchemaDefinition::new(
                orna_core::SchemaId::new(),
                QualifiedSemanticName::new(["app"]).expect("schema name"),
            )],
            Vec::new(),
            Vec::new(),
            vec![EnumTypeDefinition::new(
                enum_type,
                QualifiedSemanticName::new(["app", "stage"]).expect("qualified enum name"),
                ["lead"],
            )],
            Vec::new(),
        )
        .expect("enum catalogue");
        let enum_argument = FunctionArgument::new(
            RAW_CALL_PARAMETER,
            RuntimeValue::Enum(
                EnumValue::new(&catalogue, enum_type, "lead").expect("declared enum label"),
            ),
        )
        .expect("Enum argument is valid");
        assert!(matches!(
            validate_raw_call_argument_shape(
                RAW_CALL_FUNCTION,
                std::slice::from_ref(&enum_argument),
            )
            .expect_err("one Enum argument must be rejected"),
            PostgresKernelError::RawCallTargetUnavailable {
                function: RAW_CALL_FUNCTION,
                rule: "raw calls accept zero arguments, one supported value, or one supported argument pair",
            }
        ));

        let unsupported_pair = [
            FunctionArgument::new(RAW_CALL_PARAMETER, RuntimeValue::Boolean(true))
                .expect("Boolean argument is valid"),
            enum_argument.clone(),
        ];
        assert!(matches!(
            validate_raw_call_argument_shape(RAW_CALL_FUNCTION, &unsupported_pair)
                .expect_err("a pair with an Enum argument must be rejected"),
            PostgresKernelError::RawCallTargetUnavailable {
                function: RAW_CALL_FUNCTION,
                rule: "raw calls accept zero arguments, one supported value, or one supported argument pair",
            }
        ));

        let three = [
            FunctionArgument::new(RAW_CALL_PARAMETER, RuntimeValue::Boolean(true))
                .expect("Boolean argument is valid"),
            FunctionArgument::new(
                ParameterId::from_bytes([0x64; 16]),
                RuntimeValue::Boolean(false),
            )
            .expect("Boolean argument is valid"),
            FunctionArgument::new(
                ParameterId::from_bytes([0x65; 16]),
                RuntimeValue::Boolean(true),
            )
            .expect("Boolean argument is valid"),
        ];
        assert!(matches!(
            validate_raw_call_argument_shape(RAW_CALL_FUNCTION, &three)
                .expect_err("three arguments must be rejected"),
            PostgresKernelError::RawCallTargetUnavailable {
                function: RAW_CALL_FUNCTION,
                rule: "raw calls accept zero arguments, one supported value, or one supported argument pair",
            }
        ));
    }

    #[test]
    fn raw_insert_argument_errors_classify_to_generic_unavailable() {
        let argument_error = PostgresKernelError::ServerInsert(
            crate::ServerInsertError::Argument {
                parameter: Some(RAW_CALL_PARAMETER),
                rule: "an argument was supplied for a parameter that this function does not declare",
            },
        );
        assert!(matches!(
            classify_raw_server_insert_error(argument_error, true, RAW_CALL_FUNCTION),
            PostgresKernelError::RawCallTargetUnavailable {
                function: RAW_CALL_FUNCTION,
                rule: "raw SERVER INSERT argument target is unavailable",
            }
        ));

        let missing_required =
            PostgresKernelError::ServerInsert(crate::ServerInsertError::Argument {
                parameter: Some(RAW_CALL_PARAMETER),
                rule: "a required argument is missing",
            });
        assert!(matches!(
            classify_raw_server_insert_error(missing_required, false, RAW_CALL_FUNCTION),
            PostgresKernelError::RawCallTargetUnavailable {
                function: RAW_CALL_FUNCTION,
                rule: "raw SERVER INSERT argument target is unavailable",
            }
        ));
    }

    #[test]
    fn raw_insert_parameter_free_target_failure_stays_typed() {
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x71; 16]),
            CatalogueRevisionId::from_bytes([0x72; 16]),
        );
        let target_error =
            PostgresKernelError::ServerInsert(crate::ServerInsertError::FunctionNotActive {
                pair,
                function: RAW_CALL_FUNCTION,
            });
        assert!(matches!(
            classify_raw_server_insert_error(target_error, false, RAW_CALL_FUNCTION),
            PostgresKernelError::RawServerTargetUnavailable {
                source: RawServerTargetError::Insert(
                    crate::ServerInsertError::FunctionNotActive {
                        pair: actual_pair,
                        function: RAW_CALL_FUNCTION,
                    },
                ),
            } if actual_pair == pair
        ));
    }

    #[test]
    fn raw_insert_operational_error_stays_unchanged() {
        let operational = PostgresKernelError::ServerInsert(crate::ServerInsertError::Kernel {
            source: Box::new(PostgresKernelError::DurableInvariant {
                relation: "test relation",
                record: "test record".to_owned(),
                rule: "test rule",
            }),
        });
        assert!(matches!(
            classify_raw_server_insert_error(operational, true, RAW_CALL_FUNCTION),
            PostgresKernelError::ServerInsert(crate::ServerInsertError::Kernel {
                source,
            }) if matches!(
                *source,
                PostgresKernelError::DurableInvariant {
                    relation: "test relation",
                    ref record,
                    rule: "test rule",
                } if record == "test record"
            )
        ));
    }

    #[test]
    fn raw_insert_value_codec_error_stays_unchanged_with_arguments_present() {
        let unsupported = PostgresKernelError::ServerInsert(crate::ServerInsertError::ValueCodec(
            orna_protocol::ValueCodecError::UnsupportedValue,
        ));
        assert!(matches!(
            classify_raw_server_insert_error(unsupported, true, RAW_CALL_FUNCTION),
            PostgresKernelError::ServerInsert(crate::ServerInsertError::ValueCodec(
                orna_protocol::ValueCodecError::UnsupportedValue,
            ))
        ));
    }

    #[test]
    fn raw_insert_unique_reference_conflict_stays_typed_with_arguments_present() {
        const CONFLICT_OWNER: TypeId = TypeId::from_bytes([0x41; 16]);
        const CONFLICT_FIELD: FieldId = FieldId::from_bytes([0x42; 16]);
        const CONFLICT_REFERENCED: TypeId = TypeId::from_bytes([0x43; 16]);
        let config_error = "port=invalid"
            .parse::<tokio_postgres::Config>()
            .expect_err("invalid port must fail to parse");
        let conflict =
            PostgresKernelError::ServerInsert(crate::ServerInsertError::UniqueReferenceConflict {
                owner: CONFLICT_OWNER,
                field: CONFLICT_FIELD,
                referenced_type: CONFLICT_REFERENCED,
                source: config_error,
            });
        assert!(matches!(
            classify_raw_server_insert_error(conflict, true, RAW_CALL_FUNCTION),
            PostgresKernelError::ServerInsert(crate::ServerInsertError::UniqueReferenceConflict {
                owner: CONFLICT_OWNER,
                field: CONFLICT_FIELD,
                referenced_type: CONFLICT_REFERENCED,
                source,
            }) if source.as_db_error().is_none()
        ));
    }

    #[test]
    fn raw_call_results_transfer_owned_values_in_execution_order() {
        let client = AuthenticatedRawCallResult::Client(RuntimeValue::Boolean(true));
        assert_eq!(client.into_values(), vec![RuntimeValue::Boolean(true)]);

        let server = AuthenticatedRawCallResult::Server(vec![
            RuntimeValue::Integer(1),
            RuntimeValue::Integer(2),
        ]);
        assert_eq!(
            server.into_values(),
            vec![RuntimeValue::Integer(1), RuntimeValue::Integer(2)]
        );
    }

    #[tokio::test]
    async fn empty_record_preflight_does_not_open_postgres() {
        let kernel = "host=127.0.0.1 port=1 dbname=absent"
            .parse::<PostgresKernel>()
            .expect("unavailable test configuration is valid");

        assert_eq!(
            kernel.preflight_record_arguments(Vec::new()).await.unwrap(),
            RecordArgumentPreflight::NotRequired,
        );
    }
    use orna_core::security::ExecuteDenial;

    #[test]
    fn audit_denial_decoder_maps_the_complete_closed_vocabulary() {
        let authentication = [
            (
                "authentication_unknown_uid",
                LocalPeerAuthenticationError::UnknownUid,
            ),
            (
                "authentication_unknown_session_principal",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::UnknownSessionPrincipal,
                ),
            ),
            (
                "authentication_disabled_session_principal",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::DisabledSessionPrincipal,
                ),
            ),
            (
                "authentication_role_cannot_authenticate",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::RoleCannotAuthenticate,
                ),
            ),
            (
                "authentication_duplicate_active_role",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::DuplicateActiveRole,
                ),
            ),
            (
                "authentication_unknown_active_role",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::UnknownActiveRole,
                ),
            ),
            (
                "authentication_disabled_active_role",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::DisabledActiveRole,
                ),
            ),
            (
                "authentication_active_principal_is_not_role",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::ActivePrincipalIsNotRole,
                ),
            ),
            (
                "authentication_unreachable_active_role",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::UnreachableActiveRole,
                ),
            ),
        ];
        for (stored, expected) in authentication {
            assert_eq!(encode_authentication_audit_denial(expected), stored);
            assert_eq!(
                decode_authentication_audit_denial(stored.to_owned(), "41")
                    .expect("closed authentication reason must decode"),
                expected
            );
        }

        for (stored, expected) in [
            ("execute_invalid_session", ExecuteDenial::InvalidSession),
            ("execute_unknown_function", ExecuteDenial::UnknownFunction),
            ("execute_revision_mismatch", ExecuteDenial::RevisionMismatch),
            ("execute_missing_grant", ExecuteDenial::MissingExecuteGrant),
        ] {
            assert_eq!(encode_execute_audit_denial(expected), stored);
            assert_eq!(
                decode_execute_audit_denial(stored.to_owned(), "42")
                    .expect("closed EXECUTE reason must decode"),
                expected
            );
        }

        assert!(matches!(
            decode_authentication_audit_denial("authentication_other".to_owned(), "43"),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                ref record,
                rule: "authentication denial reason is unsupported",
            }) if record == "43"
        ));
        assert!(matches!(
            decode_execute_audit_denial("execute_other".to_owned(), "44"),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                ref record,
                rule: "EXECUTE denial reason is unsupported",
            }) if record == "44"
        ));
    }

    #[test]
    fn expected_security_result_does_not_hide_session_shutdown_failure() {
        let operation: Result<Result<(), LocalPeerAuthenticationError>, PostgresKernelError> =
            Ok(Err(LocalPeerAuthenticationError::UnknownUid));
        let shutdown = PostgresKernelError::DurableInvariant {
            relation: "test session",
            record: "shutdown".to_owned(),
            rule: "driver failed during shutdown",
        };

        assert!(matches!(
            finish_security_session(operation, Err(shutdown)),
            Err(PostgresKernelError::DurableInvariant {
                relation: "test session",
                ref record,
                rule: "driver failed during shutdown",
            }) if record == "shutdown"
        ));

        let operation: Result<Result<(), PostgresKernelError>, PostgresKernelError> =
            Ok(Err(PostgresKernelError::ClientExecuteDenied {
                pair: RevisionPair::new(
                    SourceRevisionId::from_bytes([0x11; 16]),
                    CatalogueRevisionId::from_bytes([0x12; 16]),
                ),
                function: FunctionId::from_bytes([0x13; 16]),
                reason: ExecuteDenial::MissingExecuteGrant,
            }));
        let shutdown = PostgresKernelError::DurableInvariant {
            relation: "test session",
            record: "shutdown".to_owned(),
            rule: "driver failed during shutdown",
        };
        assert!(matches!(
            finish_security_session(operation, Err(shutdown)),
            Err(PostgresKernelError::DurableInvariant {
                relation: "test session",
                ref record,
                rule: "driver failed during shutdown",
            }) if record == "shutdown"
        ));
    }
}
