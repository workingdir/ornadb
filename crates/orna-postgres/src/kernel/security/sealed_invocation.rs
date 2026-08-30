use super::*;
/// The owned redacted result of one sealed `sys.invoke` dispatch.
///
/// The completed variant carries the full Event batch so a server adapter can
/// deliver `InvocationStarted(0)`, `ValueBatch(1)`, and `InvocationCompleted(2)`
/// and then complete the call (`CALL_COMPLETED`). The other variants are
/// closed and disclose no target, signature, selector, value, binding, or
/// security evidence.
#[derive(Clone, Debug, PartialEq)]
pub enum SealedInvocationResult {
    /// The invocation completed with its complete Event sequence.
    Completed {
        /// The invocation identity shared by every retained Event.
        invocation: InvocationId,
        /// The complete `InvocationStarted(0)`, `ValueBatch(1)`,
        /// `InvocationCompleted(2)` Event batch.
        events: InvocationEventBatch,
    },
    /// The accepted invocation ended with one redacted failure Event sequence.
    Failed {
        /// The invocation identity shared by every retained Event.
        invocation: InvocationId,
        /// The complete `InvocationStarted(0)`, `InvocationFailed(1)` batch.
        events: InvocationEventBatch,
    },
    /// The invocation was denied without executing any artifact.
    Denied {
        /// The invocation identity.
        invocation: InvocationId,
    },
    /// The allowed invocation executed but its output requirement could not
    /// be presented (ADR 0057 step 7).
    ///
    /// This variant is closed: it discloses no target, requirement, value,
    /// presenter, or failure detail. The CLI maps it to the presentation
    /// error exit code 5.
    PresentationFailed {
        /// The invocation identity.
        invocation: InvocationId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SealedInvocationFailureClass {
    Bind,
    Target,
    Internal,
}

/// One closed durable `sys.invoke` decision for the PostgreSQL kernel.
///
/// This is private kernel state. It does not retain Request, bind, lifecycle,
/// delivery, or error-detail data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InvocationAuditDecision {
    pub(super) invocation: InvocationId,
    pub(super) outcome: SecurityAuditOutcome,
    pub(super) session_principal: PrincipalId,
    pub(super) effective_principal: Option<PrincipalId>,
    pub(super) authorising_principal: Option<PrincipalId>,
    pub(super) target: Option<InvocationTarget>,
    pub(super) security_audit_event: Option<SecurityAuditEventId>,
}

impl InvocationAuditDecision {
    /// Creates one decision from durable matching `EXECUTE` evidence.
    pub(crate) fn from_execute_evidence(
        invocation: InvocationId,
        evidence: &SecurityAuditEvent,
    ) -> Result<Self, PostgresKernelError> {
        let decision = evidence.decision();
        if decision.kind() != SecurityAuditKind::Execute {
            return Err(invocation_audit_invariant(
                &invocation.canonical(),
                "invocation decision requires EXECUTE audit evidence",
            ));
        }
        let target = require_invocation_audit_value(
            decision.target(),
            &invocation.canonical(),
            "EXECUTE evidence requires a target",
        )?;
        let session_principal = require_invocation_audit_value(
            decision.session_principal(),
            &invocation.canonical(),
            "EXECUTE evidence requires a session principal",
        )?;
        let result = Self {
            invocation,
            outcome: decision.outcome(),
            session_principal,
            effective_principal: decision.effective_principal(),
            authorising_principal: decision.authorising_principal(),
            target: Some(target),
            security_audit_event: Some(evidence.id()),
        };
        validate_invocation_audit_decision_shape(&result, &invocation.canonical())?;
        Ok(result)
    }

    /// Creates the closed unresolved target-denied decision.
    pub(crate) fn unresolved_denied(
        invocation: InvocationId,
        session_principal: PrincipalId,
    ) -> Self {
        Self {
            invocation,
            outcome: SecurityAuditOutcome::Denied,
            session_principal,
            effective_principal: None,
            authorising_principal: None,
            target: None,
            security_audit_event: None,
        }
    }
}

/// The closed pre-accept result of one retained sealed invocation.
///
/// Entry denial and malformed/protocol-incompatible requests never receive an
/// invocation identity. An accepted continuation owns the pinned decode context
/// and can be handed to the post-accept preparation step exactly once.
#[doc(hidden)]
pub enum SealedInvocationPreflight {
    /// The request was rejected before acceptance.
    Rejected { failure: CallFailure },
    /// The request passed the protected entry and request checks.
    Accepted(SealedInvocationContinuation),
}

/// The private, one-shot continuation created after sealed preflight.
#[doc(hidden)]
pub struct SealedInvocationContinuation {
    kernel: PostgresKernel,
    authenticated_session: AuthenticatedSession,
    active: ActiveDatabaseRevision,
    security: SecuritySnapshot,
    registry: OpaqueCodecRegistry,
    decoded: orna_core::invocation::InvokeRequest,
    request: RetainedInvokeRequest,
    invocation: InvocationId,
    started_events: InvocationEventBatch,
}

impl SealedInvocationContinuation {
    /// Returns the identity that will be shared by every invocation event.
    pub const fn invocation(&self) -> InvocationId {
        self.invocation
    }

    /// Returns the start-only Event batch queued before target work.
    pub fn started_events(&self) -> &InvocationEventBatch {
        &self.started_events
    }
}

impl std::fmt::Debug for SealedInvocationContinuation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SealedInvocationContinuation")
            .field("invocation", &self.invocation)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for SealedInvocationPreflight {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected { failure } => formatter
                .debug_struct("SealedInvocationPreflight::Rejected")
                .field("failure", failure)
                .finish(),
            Self::Accepted(continuation) => formatter
                .debug_tuple("SealedInvocationPreflight::Accepted")
                .field(continuation)
                .finish(),
        }
    }
}

/// The closed result of one accepted continuation after its start Event.
#[doc(hidden)]
#[derive(Debug)]
pub enum SealedInvocationExecution {
    /// A normal sealed invocation result (including redacted failures).
    Result(SealedInvocationResult),
    /// A bounded SERVER stream whose values are pulled by the raw adapter.
    ServerStream(AuthenticatedServerResourceProducer),
    /// Cancellation won before new evaluator/target work began.
    Cancelled { invocation: InvocationId },
}

/// The post-accept operation owns all pinned state needed for one execution.
#[doc(hidden)]
pub struct SealedInvocationOperation {
    kernel: PostgresKernel,
    authenticated_session: AuthenticatedSession,
    active: ActiveDatabaseRevision,
    security: SecuritySnapshot,
    registry: OpaqueCodecRegistry,
    decoded: orna_core::invocation::InvokeRequest,
    request: RetainedInvokeRequest,
    invocation: InvocationId,
    started_events: InvocationEventBatch,
    outcome: SealedInvocationPreparedOutcome,
    consumed: bool,
}

impl SealedInvocationOperation {
    /// Returns the invocation identity.
    pub const fn invocation(&self) -> InvocationId {
        self.invocation
    }

    /// Returns the start-only Event batch. No target or result is included.
    pub fn started_events(&self) -> &InvocationEventBatch {
        &self.started_events
    }
    /// Returns the immutable active revision pinned during preflight.
    #[doc(hidden)]
    pub fn active_revision(&self) -> ActiveDatabaseRevision {
        self.active.clone()
    }
    /// Returns the operation-bound session and security evidence for nested
    /// CLIENT evaluation.
    #[doc(hidden)]
    pub fn client_evaluation_context(
        &self,
    ) -> (AuthenticatedSession, SecuritySnapshot, InvocationId) {
        (
            self.authenticated_session.clone(),
            self.security.clone(),
            self.invocation,
        )
    }
}

impl std::fmt::Debug for SealedInvocationOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SealedInvocationOperation")
            .field("invocation", &self.invocation)
            .finish_non_exhaustive()
    }
}

pub(super) enum SealedInvocationPreparedOutcome {
    TargetDenied {
        security_target: Option<InvocationTarget>,
        denial: Option<ExecuteDenial>,
    },
    BindFailure {
        target: PreparedSealedTarget,
        security_target: InvocationTarget,
        authorisation: AuthorisedInvocation,
    },
    Allowed {
        target: PreparedSealedTarget,
        security_target: InvocationTarget,
        authorisation: AuthorisedInvocation,
    },
}

#[derive(Clone)]
pub(super) enum PreparedSealedTarget {
    Application {
        definition: FunctionDefinition,
    },
    System {
        definition: SystemFunctionDefinition,
    },
    VerifiedStandard {
        definition: FunctionDefinition,
        executable: StandardExecutable,
    },
}

impl PreparedSealedTarget {
    fn function(&self) -> FunctionId {
        match self {
            Self::Application { definition } | Self::VerifiedStandard { definition, .. } => {
                definition.id()
            }
            Self::System { definition } => definition.id(),
        }
    }

    pub(super) fn from_resolved(target: SealedResolvedTarget<'_>) -> Self {
        match target {
            SealedResolvedTarget::Application(definition) => Self::Application {
                definition: definition.clone(),
            },
            SealedResolvedTarget::System(definition) => Self::System { definition },
            SealedResolvedTarget::VerifiedStandard {
                definition,
                executable,
            } => Self::VerifiedStandard {
                definition: definition.clone(),
                executable: executable.clone(),
            },
        }
    }
}

fn sealed_started_events(
    invocation: InvocationId,
) -> Result<InvocationEventBatch, PostgresKernelError> {
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .map_err(PostgresKernelError::InvocationCarrier)?;
    InvocationEventBatch::new(vec![InvocationEventRecord::new(1, started)])
        .map_err(PostgresKernelError::SealedInvocation)
}

impl PostgresKernel {
    /// Validates the protected entry and retained request before acceptance.
    ///
    /// This method does not resolve the requested target. An entry denial
    /// records only closed audit evidence before returning the public rejection;
    /// an accepted continuation carries the pinned decode context.
    #[doc(hidden)]
    pub async fn validate_sealed_sys_invoke(
        &self,
        authenticated_session: &AuthenticatedSession,
        connection_protocol_major: u16,
        request: &RetainedInvokeRequest,
    ) -> Result<SealedInvocationPreflight, PostgresKernelError> {
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
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let system_target = InvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair());
            // ADR 0054 requires the protected entry decision to precede any
            // retained Request decoding. An unauthorized caller therefore gets
            // only the closed EXECUTE_DENIED result, even for malformed bytes.
            match security.authorise_system_function(authenticated_session, system_target) {
                ExecuteDecision::Denied(reason) => {
                    let invocation = InvocationId::new();
                    append_security_audit_event(
                        &transaction,
                        SecurityAuditDecision::execute_denied(
                            authenticated_session,
                            system_target,
                            reason,
                        ),
                    )
                    .await?;
                    append_unresolved_invocation_audit(
                        &transaction,
                        authenticated_session,
                        invocation,
                    )
                    .await?;
                    transaction
                        .commit()
                        .await
                        .map_err(PostgresKernelError::Database)?;
                    return Ok(SealedInvocationPreflight::Rejected {
                        failure: CallFailure::ExecuteDenied,
                    });
                }
                ExecuteDecision::Allowed(_) => {}
            }
            let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.active_revision",
                    record: active.pair().catalogue().canonical(),
                    rule: "sealed sys.invoke requires the accepted verified standard snapshot",
                }
            })?;
            let registry = registered_opaque_codecs(standard).map_err(|_| {
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.standard_library_revisions",
                    record: standard.revision().canonical(),
                    rule: "the verified standard snapshot must bind its opaque codec registry",
                }
            })?;
            let decoded = match decode_retained_invoke_request(&active, &registry, request) {
                Ok(decoded) => decoded,
                Err(_) => {
                    transaction
                        .rollback()
                        .await
                        .map_err(PostgresKernelError::Database)?;
                    return Ok(SealedInvocationPreflight::Rejected {
                        failure: CallFailure::InternalFailure,
                    });
                }
            };
            if decoded.client_offer().protocol_major() != connection_protocol_major {
                transaction
                    .rollback()
                    .await
                    .map_err(PostgresKernelError::Database)?;
                return Ok(SealedInvocationPreflight::Rejected {
                    failure: CallFailure::InternalFailure,
                });
            }
            transaction
                .rollback()
                .await
                .map_err(PostgresKernelError::Database)?;
            let invocation = InvocationId::new();
            let started_events = sealed_started_events(invocation)?;
            Ok(SealedInvocationPreflight::Accepted(
                SealedInvocationContinuation {
                    kernel: self.clone(),
                    authenticated_session: authenticated_session.clone(),
                    active,
                    security,
                    registry,
                    decoded,
                    request: request.clone(),
                    invocation,
                    started_events,
                },
            ))
        }
        .await;
        finish_authenticated_dispatch_session(operation, database_session.shutdown().await)
    }
}

impl SealedInvocationContinuation {
    /// Prepares the accepted invocation after the caller has sent
    /// `CALL_ACCEPTED` and before target execution starts.
    ///
    /// Target resolution, binding, and the protected decision remain private in
    /// this outcome. The operation commits their durable audit before dispatch
    /// evaluates defaults or the target.
    #[doc(hidden)]
    pub async fn prepare_sealed_sys_invoke_after_accept(
        self,
    ) -> Result<SealedInvocationOperation, PostgresKernelError> {
        let SealedInvocationContinuation {
            kernel,
            authenticated_session,
            active,
            security,
            registry,
            decoded,
            request,
            invocation,
            started_events,
        } = self;
        let outcome = match resolve_sealed_target(&active, decoded.target()) {
            Some(target) => {
                let security_target = sealed_security_target(&active, target);
                match authorise_sealed_target(&security, &authenticated_session, security_target) {
                    ExecuteDecision::Allowed(authorisation) => {
                        if !sealed_target_security_is_supported(target) {
                            SealedInvocationPreparedOutcome::TargetDenied {
                                security_target: Some(security_target),
                                denial: Some(ExecuteDenial::UnsupportedSecurityDefiner),
                            }
                        } else {
                            let bind_ok = match &target {
                                SealedResolvedTarget::Application(definition)
                                | SealedResolvedTarget::VerifiedStandard { definition, .. } => {
                                    bind_sealed_invoke_arguments(definition, decoded.arguments())
                                        .is_ok()
                                }
                                SealedResolvedTarget::System(_) => true,
                            };
                            let prepared_target = PreparedSealedTarget::from_resolved(target);
                            if bind_ok {
                                SealedInvocationPreparedOutcome::Allowed {
                                    target: prepared_target,
                                    security_target,
                                    authorisation,
                                }
                            } else {
                                SealedInvocationPreparedOutcome::BindFailure {
                                    target: prepared_target,
                                    security_target,
                                    authorisation,
                                }
                            }
                        }
                    }
                    ExecuteDecision::Denied(denial) => {
                        SealedInvocationPreparedOutcome::TargetDenied {
                            security_target: Some(security_target),
                            denial: Some(denial),
                        }
                    }
                }
            }
            None => SealedInvocationPreparedOutcome::TargetDenied {
                security_target: None,
                denial: None,
            },
        };
        Ok(SealedInvocationOperation {
            kernel,
            authenticated_session,
            active,
            security,
            registry,
            decoded,
            request,
            invocation,
            started_events,
            outcome,
            consumed: false,
        })
    }
}

impl SealedInvocationOperation {
    async fn append_prepared_audit(&self) -> Result<(), PostgresKernelError> {
        let mut database_session = self.kernel.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            if active.pair() != self.active.pair() {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.active_revision",
                    record: self.invocation.canonical(),
                    rule: "sealed invocation active revision changed before audit",
                });
            }
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            if !security_snapshots_match(&security, &self.security) {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    record: self.invocation.canonical(),
                    rule: "sealed invocation security snapshot changed before audit",
                });
            }
            match &self.outcome {
                SealedInvocationPreparedOutcome::Allowed {
                    target,
                    security_target,
                    authorisation,
                }
                | SealedInvocationPreparedOutcome::BindFailure {
                    target,
                    security_target,
                    authorisation,
                } => {
                    if target.function() != security_target.function()
                        || authorisation.target() != *security_target
                        || authorisation.target().revision() != self.active.pair()
                    {
                        return Err(PostgresKernelError::DurableInvariant {
                            relation: "_orna_kernel.invocation_audit_events",
                            record: self.invocation.canonical(),
                            rule: "prepared invocation authorisation must retain the pinned revision",
                        });
                    }
                    append_allowed_invocation_audit_evidence(
                        &transaction,
                        authorisation,
                        self.invocation,
                    )
                    .await?;
                }
                SealedInvocationPreparedOutcome::TargetDenied {
                    security_target: Some(target),
                    denial: Some(reason),
                } => {
                    let event_id = append_security_audit_event(
                        &transaction,
                        SecurityAuditDecision::execute_denied(
                            &self.authenticated_session,
                            *target,
                            *reason,
                        ),
                    )
                    .await?;
                    append_linked_invocation_audit(&transaction, self.invocation, event_id).await?;
                }
                SealedInvocationPreparedOutcome::TargetDenied {
                    security_target: None,
                    denial: None,
                } => {
                    append_unresolved_invocation_audit(
                        &transaction,
                        &self.authenticated_session,
                        self.invocation,
                    )
                    .await?;
                }
                SealedInvocationPreparedOutcome::TargetDenied { .. } => {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: "_orna_kernel.invocation_audit_events",
                        record: self.invocation.canonical(),
                        rule: "target denial must retain both target and denial or neither",
                    });
                }
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(())
        }
        .await;
        finish_authenticated_dispatch_session(operation, database_session.shutdown().await)
    }

    /// Executes the accepted invocation after its start Event is delivered.
    /// Long-lived SERVER stream producers are spawned on `resource_runtime`,
    /// not on the short-lived worker runtime that calls this method.
    #[doc(hidden)]
    pub async fn execute_after_started(
        &mut self,
        resource_executor: Option<&mut dyn ClientResourceExecutor>,
        state: &mut ClientStateStore,
        capability_audit_appended: &mut bool,
        cancellation: &ResourceCancellation,
        resource_runtime: tokio::runtime::Handle,
    ) -> Result<SealedInvocationExecution, PostgresKernelError> {
        if self.consumed {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "sealed invocation operation",
                record: self.invocation.canonical(),
                rule: "execute_after_started may only be called once",
            });
        }
        self.consumed = true;
        self.append_prepared_audit().await?;
        if cancellation.is_requested() {
            return Ok(SealedInvocationExecution::Cancelled {
                invocation: self.invocation,
            });
        }
        let bind_failure = matches!(
            &self.outcome,
            SealedInvocationPreparedOutcome::BindFailure { .. }
        );
        let target_denied = matches!(
            &self.outcome,
            SealedInvocationPreparedOutcome::TargetDenied { .. }
        );
        if bind_failure {
            return Ok(SealedInvocationExecution::Result(sealed_failure_result(
                self.invocation,
                SealedInvocationFailureClass::Bind,
            )?));
        }
        if target_denied {
            return Ok(SealedInvocationExecution::Result(
                SealedInvocationResult::Denied {
                    invocation: self.invocation,
                },
            ));
        }
        // Native STREAM and accepted mutation ROWS targets use a live
        // producer. Read-only ROWS targets use the existing sealed executor.
        if let SealedInvocationPreparedOutcome::Allowed {
            target: PreparedSealedTarget::Application { definition },
            authorisation,
            ..
        } = &self.outcome
            && definition.domain() == FunctionDomain::Server
            && (matches!(definition.return_type(), FunctionReturn::Stream(_))
                || (matches!(definition.return_type(), FunctionReturn::Rows(_))
                    && sealed_server_target_is_mutation(&self.active, definition.id())))
        {
            let arguments = match bind_sealed_invoke_arguments(definition, self.decoded.arguments())
            {
                Ok(arguments) => arguments,
                Err(_) => {
                    return Ok(SealedInvocationExecution::Result(sealed_failure_result(
                        self.invocation,
                        SealedInvocationFailureClass::Bind,
                    )?));
                }
            };
            let producer = start_sealed_server_stream_producer(
                self.kernel.clone(),
                self.active.clone(),
                self.security.clone(),
                authorisation.clone(),
                arguments,
                self.invocation,
                cancellation.clone(),
                resource_runtime,
            )
            .await;
            return match producer {
                Ok(producer) => Ok(SealedInvocationExecution::ServerStream(producer)),
                Err(failure) => Ok(SealedInvocationExecution::Result(sealed_failure_result(
                    self.invocation,
                    failure,
                )?)),
            };
        }

        let result = self
            .kernel
            .dispatch_sealed_sys_invoke_with_resource_executor_and_state_internal(
                &self.authenticated_session,
                self.decoded.client_offer().protocol_major(),
                &self.request,
                resource_executor,
                state,
                self.invocation,
                capability_audit_appended,
                Some(&self.decoded),
                Some((&self.active, &self.security)),
                Some(&self.registry),
                Some(&self.outcome),
                true,
                Some(cancellation),
            )
            .await;
        match result {
            Ok(result) => Ok(SealedInvocationExecution::Result(result)),
            Err(PostgresKernelError::ClientExecution(
                ClientExecutionError::ResourceEvaluation {
                    source: ClientResourceExecutionError::Cancelled,
                    ..
                },
            )) => Ok(SealedInvocationExecution::Cancelled {
                invocation: self.invocation,
            }),
            Err(error) => Err(error),
        }
    }
}
