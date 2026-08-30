//! Server mutation transaction orchestration and error contextualization.

use super::*;

pub(super) enum ServerMutationResult {
    Insert {
        result: ServerInsertResult,
        unique_constraints: UniqueConstraints,
    },
    Update {
        result: ServerUpdateResult,
        unique_constraints: UniqueConstraints,
    },
    Delete(ServerDeleteResult),
}

impl ServerMutationResult {
    const fn context(&self) -> ServerMutationContext {
        match self {
            Self::Insert { result, .. } => result.context(),
            Self::Update { result, .. } => result.context(),
            Self::Delete(result) => result.context(),
        }
    }

    fn unique_conflict(&self, source: &tokio_postgres::Error) -> Option<UniqueConstraint> {
        match self {
            Self::Insert {
                unique_constraints, ..
            }
            | Self::Update {
                unique_constraints, ..
            } => unique_constraints.conflict(source),
            Self::Delete(_) => None,
        }
    }

    fn committed_shutdown_error(self, source: PostgresKernelError) -> PostgresKernelError {
        match self {
            Self::Insert { result, .. } => {
                server_error(ServerMutationError::CommittedButShutdownFailed {
                    result: Box::new(result),
                    source: Box::new(source),
                })
            }
            Self::Update { result, .. } => {
                update_error(ServerUpdateError::CommittedButShutdownFailed {
                    result: Box::new(result),
                    source: Box::new(source),
                })
            }
            Self::Delete(result) => delete_error(ServerDeleteError::CommittedButShutdownFailed {
                result: Box::new(result),
                source: Box::new(source),
            }),
        }
    }

    fn into_insert(self) -> Result<ServerInsertResult, PostgresKernelError> {
        match self {
            Self::Insert { result, .. } => Ok(result),
            Self::Update { .. } | Self::Delete(_) => {
                Err(server_error(ServerMutationError::ValueInvariant {
                    rule: "INSERT execution produced a different mutation result",
                }))
            }
        }
    }

    fn into_update(self) -> Result<ServerUpdateResult, PostgresKernelError> {
        match self {
            Self::Update { result, .. } => Ok(result),
            Self::Insert { .. } | Self::Delete(_) => {
                Err(update_unavailable(PostgresKernelError::CatalogueInvariant(
                    "UPDATE execution produced a different mutation result",
                )))
            }
        }
    }

    fn into_delete(self) -> Result<ServerDeleteResult, PostgresKernelError> {
        match self {
            Self::Delete(result) => Ok(result),
            Self::Insert { .. } | Self::Update { .. } => {
                Err(delete_unavailable(PostgresKernelError::CatalogueInvariant(
                    "DELETE execution produced a different mutation result",
                )))
            }
        }
    }
}

impl PostgresKernel {
    /// Executes one active single-row SERVER `INSERT` by stable function identity.
    ///
    /// Arguments are matched by stable [`ParameterId`] and can arrive in any
    /// order. This operation does not resolve names, accept source SQL, expose
    /// an invocation identity, or provide automatic retry behaviour.
    pub async fn execute_server_insert(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerInsertResult, PostgresKernelError> {
        self.execute_server_insert_with_options(function, arguments, None, false)
            .await
    }

    /// Pauses a live insert after it has recovered and pinned its active snapshot.
    ///
    /// This hook is compiled only for the PostgreSQL integration harness. Both
    /// barriers must have exactly two participants: the executor and the test.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_server_insert_with_test_barrier(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<ServerInsertResult, PostgresKernelError> {
        self.execute_server_insert_with_options(
            function,
            arguments,
            Some(MutationTestBarrier { reached, resume }),
            false,
        )
        .await
    }

    /// Forces the driver to fail after PostgreSQL has confirmed COMMIT.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_server_insert_with_forced_post_commit_driver_shutdown(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerInsertResult, PostgresKernelError> {
        self.execute_server_insert_with_options(function, arguments, None, true)
            .await
    }

    async fn execute_server_insert_with_options(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
        barrier: Option<MutationTestBarrier>,
        force_post_commit_driver_shutdown: bool,
    ) -> Result<ServerInsertResult, PostgresKernelError> {
        self.execute_server_mutation_with_options(
            MutationExecutionKind::Insert,
            function,
            arguments,
            barrier,
            force_post_commit_driver_shutdown,
        )
        .await?
        .into_insert()
    }

    async fn execute_server_mutation_with_options(
        &self,
        operation: MutationExecutionKind,
        function: FunctionId,
        arguments: &[FunctionArgument],
        barrier: Option<MutationTestBarrier>,
        force_post_commit_driver_shutdown: bool,
    ) -> Result<ServerMutationResult, PostgresKernelError> {
        let mut session = self
            .open()
            .await
            .map_err(|error| pre_transaction_mutation_error(operation, error))?;
        let execution = execute_mutation_client(
            &mut session.client,
            operation,
            function,
            arguments,
            barrier.as_ref(),
        )
        .await;
        #[cfg(feature = "test-hooks")]
        if force_post_commit_driver_shutdown && execution.is_ok() {
            session.abort_driver();
        }
        #[cfg(not(feature = "test-hooks"))]
        let _ = force_post_commit_driver_shutdown;
        let shutdown = session.shutdown().await;
        match (execution, shutdown) {
            (Ok(result), Ok(())) => Ok(result),
            (Err(error), _) => Err(error),
            (Ok(result), Err(source)) => Err(result.committed_shutdown_error(source)),
        }
    }
}

impl PostgresKernel {
    /// Executes one active single-object SERVER `UPDATE` by stable function identity.
    ///
    /// Arguments are matched by stable [`ParameterId`] and can arrive in any
    /// order. A missing target commits successfully and returns zero rows.
    pub async fn execute_server_update(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerUpdateResult, PostgresKernelError> {
        self.execute_server_mutation_with_options(
            MutationExecutionKind::Update,
            function,
            arguments,
            None,
            false,
        )
        .await?
        .into_update()
    }

    /// Executes one active single-object SERVER `DELETE` by stable function identity.
    ///
    /// Arguments are matched by stable [`ParameterId`] and can arrive in any
    /// order. A missing target commits successfully and returns zero rows.
    pub async fn execute_server_delete(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerDeleteResult, PostgresKernelError> {
        self.execute_server_delete_with_options(function, arguments, None, false)
            .await
    }

    /// Pauses a live delete after it has recovered and pinned its active snapshot.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_server_delete_with_test_barrier(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<ServerDeleteResult, PostgresKernelError> {
        self.execute_server_delete_with_options(
            function,
            arguments,
            Some(MutationTestBarrier { reached, resume }),
            false,
        )
        .await
    }

    /// Forces the driver to fail after PostgreSQL has confirmed a delete COMMIT.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_server_delete_with_forced_post_commit_driver_shutdown(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerDeleteResult, PostgresKernelError> {
        self.execute_server_delete_with_options(function, arguments, None, true)
            .await
    }

    async fn execute_server_delete_with_options(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
        barrier: Option<MutationTestBarrier>,
        force_post_commit_driver_shutdown: bool,
    ) -> Result<ServerDeleteResult, PostgresKernelError> {
        self.execute_server_mutation_with_options(
            MutationExecutionKind::Delete,
            function,
            arguments,
            barrier,
            force_post_commit_driver_shutdown,
        )
        .await?
        .into_delete()
    }
}

pub(super) async fn execute_mutation_client(
    client: &mut Client,
    operation: MutationExecutionKind,
    function: FunctionId,
    arguments: &[FunctionArgument],
    barrier: Option<&MutationTestBarrier>,
) -> Result<ServerMutationResult, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(false)
        .start()
        .await
        .map_err(|source| {
            pre_transaction_mutation_error(operation, PostgresKernelError::Database(source))
        })?;
    match execute_mutation_transaction(&transaction, operation, function, arguments, barrier).await
    {
        Ok(candidate) => commit_mutation_candidate(transaction, candidate).await,
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

pub(super) async fn commit_mutation_candidate(
    transaction: Transaction<'_>,
    candidate: ServerMutationResult,
) -> Result<ServerMutationResult, PostgresKernelError> {
    let context = candidate.context();
    match transaction.commit().await {
        Ok(()) => Ok(candidate),
        Err(source) => {
            let unique_conflict = candidate.unique_conflict(&source);
            Err(match candidate {
                ServerMutationResult::Insert { .. } if let Some(conflict) = unique_conflict => {
                    server_error(ServerMutationError::NotCommitted {
                        context,
                        source: Box::new(conflict.error(source)),
                    })
                }
                ServerMutationResult::Insert { result, .. } if source.as_db_error().is_some() => {
                    server_error(ServerMutationError::CommitRejected {
                        context,
                        target: result.target(),
                        candidate: result.object(),
                        source,
                    })
                }
                ServerMutationResult::Insert { result, .. } => {
                    server_error(ServerMutationError::CommitOutcomeUnknown {
                        context,
                        target: result.target(),
                        candidate: result.object(),
                        source,
                    })
                }
                ServerMutationResult::Update { .. } if let Some(conflict) = unique_conflict => {
                    update_error(ServerUpdateError::NotCommitted {
                        context,
                        source: Box::new(conflict.error(source)),
                    })
                }
                ServerMutationResult::Update { result, .. } if source.as_db_error().is_some() => {
                    update_error(ServerUpdateError::CommitRejected {
                        context,
                        target: result.target(),
                        selector: result.selector(),
                        matched: result.matched(),
                        source,
                    })
                }
                ServerMutationResult::Update { result, .. } => {
                    update_error(ServerUpdateError::CommitOutcomeUnknown {
                        context,
                        target: result.target(),
                        selector: result.selector(),
                        matched: result.matched(),
                        source,
                    })
                }
                ServerMutationResult::Delete(result) => match delete_commit_failure(
                    source
                        .as_db_error()
                        .map(tokio_postgres::error::DbError::code),
                ) {
                    DeleteCommitFailure::Restricted => {
                        delete_error(ServerDeleteError::DeleteRestricted {
                            context,
                            target: result.target(),
                            selector: result.selector(),
                            source,
                        })
                    }
                    DeleteCommitFailure::Rejected => {
                        delete_error(ServerDeleteError::CommitRejected {
                            context,
                            target: result.target(),
                            selector: result.selector(),
                            matched: result.matched(),
                            source,
                        })
                    }
                    DeleteCommitFailure::Unknown => {
                        delete_error(ServerDeleteError::CommitOutcomeUnknown {
                            context,
                            target: result.target(),
                            selector: result.selector(),
                            matched: result.matched(),
                            source,
                        })
                    }
                },
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DeleteCommitFailure {
    Restricted,
    Rejected,
    Unknown,
}

pub(super) fn delete_commit_failure(code: Option<&SqlState>) -> DeleteCommitFailure {
    match code {
        Some(code)
            if code == &SqlState::FOREIGN_KEY_VIOLATION
                || code == &SqlState::RESTRICT_VIOLATION =>
        {
            DeleteCommitFailure::Restricted
        }
        Some(_) => DeleteCommitFailure::Rejected,
        None => DeleteCommitFailure::Unknown,
    }
}

pub(super) async fn execute_mutation_transaction(
    transaction: &Transaction<'_>,
    operation: MutationExecutionKind,
    function_id: FunctionId,
    arguments: &[FunctionArgument],
    barrier: Option<&MutationTestBarrier>,
) -> Result<ServerMutationResult, PostgresKernelError> {
    let active = configure_and_recover(transaction)
        .await
        .map_err(|error| mutation_kernel_error(operation, error))?;
    let function =
        active
            .catalogue()
            .function_by_id(function_id)
            .ok_or_else(|| match operation {
                MutationExecutionKind::Insert => {
                    server_error(ServerMutationError::FunctionNotActive {
                        pair: active.pair(),
                        function: function_id,
                    })
                }
                MutationExecutionKind::Update => {
                    update_error(ServerUpdateError::FunctionNotActive {
                        pair: active.pair(),
                        function: function_id,
                    })
                }
                MutationExecutionKind::Delete => {
                    delete_error(ServerDeleteError::FunctionNotActive {
                        pair: active.pair(),
                        function: function_id,
                    })
                }
            })?;
    let context =
        ServerMutationContext::new(active.pair(), function_id, function.current_revision());
    pause_after_recovery(barrier).await;
    match operation {
        MutationExecutionKind::Insert => {
            execute_active_insert(transaction, &active, function, context, arguments)
                .await
                .map(
                    |(result, unique_constraints)| ServerMutationResult::Insert {
                        result,
                        unique_constraints,
                    },
                )
                .map_err(|error| not_committed(context, error))
        }
        MutationExecutionKind::Update => {
            execute_active_update(transaction, &active, function, context, arguments)
                .await
                .map(
                    |(result, unique_constraints)| ServerMutationResult::Update {
                        result,
                        unique_constraints,
                    },
                )
                .map_err(|error| update_not_committed(context, error))
        }
        MutationExecutionKind::Delete => {
            execute_active_delete(transaction, &active, function, context, arguments)
                .await
                .map(ServerMutationResult::Delete)
                .map_err(|error| delete_not_committed(context, error))
        }
    }
}

pub(super) async fn execute_active_update(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    context: ServerUpdateContext,
    arguments: &[FunctionArgument],
) -> Result<(ServerUpdateResult, UniqueConstraints), PostgresKernelError> {
    let validated =
        validate_active_mutation(active, function, arguments, MutationExecutionKind::Update)?;
    execute_validated_active_update(transaction, active, context, validated, arguments).await
}

pub(super) async fn execute_validated_active_update(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    context: ServerUpdateContext,
    validated: ValidatedActiveMutation<'_>,
    arguments: &[FunctionArgument],
) -> Result<(ServerUpdateResult, UniqueConstraints), PostgresKernelError> {
    let selector = selector_object(&validated.plan, arguments)?;
    let lowered = lower_update_with_context(
        active.catalogue_hash_context(),
        &validated.plan,
        &validated.arguments,
    )?;
    let statement = transaction
        .prepare_typed(&lowered.sql, &lowered.bind_types)
        .await
        .map_err(|source| server_error(ServerMutationError::Database { source }))?;
    validate_prepared_result(&statement, "UPDATE")?;
    let matched = execute_update(
        transaction,
        &statement,
        lowered.binds,
        selector,
        &validated.unique_constraints,
    )
    .await?;
    let result = ServerUpdateResult::new(
        context,
        validated.target.id(),
        selector,
        matched,
        validated.returned.column,
    )
    .map_err(ServerMutationError::ResultRows)
    .map_err(server_error)?;
    Ok((result, validated.unique_constraints))
}

pub(super) async fn execute_active_delete(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    context: ServerDeleteContext,
    arguments: &[FunctionArgument],
) -> Result<ServerDeleteResult, PostgresKernelError> {
    let validated = validate_active_delete(active, function, arguments)?;
    let selector = selector_argument_object(
        validated.plan.target(),
        validated.plan.selector(),
        arguments,
    )?;
    let lowered = lower_delete(&validated.plan, &validated.arguments)?;
    let statement = transaction
        .prepare_typed(&lowered.sql, &lowered.bind_types)
        .await
        .map_err(|source| server_error(ServerMutationError::Database { source }))?;
    validate_prepared_result(&statement, "DELETE")?;
    let matched = execute_delete(
        transaction,
        &statement,
        lowered.binds,
        context,
        validated.target.id(),
        selector,
    )
    .await?;
    ServerDeleteResult::new(
        context,
        validated.target.id(),
        selector,
        matched,
        validated.column,
    )
    .map_err(ServerMutationError::ResultRows)
    .map_err(server_error)
}

#[cfg(feature = "test-hooks")]
async fn pause_after_recovery(barrier: Option<&MutationTestBarrier>) {
    if let Some(barrier) = barrier {
        barrier.reached.wait().await;
        barrier.resume.wait().await;
    }
}

#[cfg(not(feature = "test-hooks"))]
async fn pause_after_recovery(_barrier: Option<&MutationTestBarrier>) {}

pub(super) async fn execute_active_insert(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    context: ServerInsertContext,
    arguments: &[FunctionArgument],
) -> Result<(ServerInsertResult, UniqueConstraints), PostgresKernelError> {
    let validated =
        validate_active_mutation(active, function, arguments, MutationExecutionKind::Insert)?;
    execute_validated_active_insert(transaction, active, context, validated).await
}

pub(super) async fn execute_validated_active_insert(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    context: ServerInsertContext,
    validated: ValidatedActiveMutation<'_>,
) -> Result<(ServerInsertResult, UniqueConstraints), PostgresKernelError> {
    let lowered = lower_insert_with_active(active, &validated.plan, &validated.arguments)?;
    let statement = transaction
        .prepare_typed(&lowered.sql, &lowered.bind_types)
        .await
        .map_err(|source| server_error(ServerInsertError::Database { source }))?;
    validate_prepared_result(&statement, "INSERT")?;

    // Object allocation is deliberately after every semantic, durable,
    // argument, lowering, and prepared-result validation above.
    let object = ObjectId::new();
    let result = ServerInsertResult::new(
        context,
        validated.target.id(),
        object,
        validated.returned.column,
    )
    .map_err(ServerInsertError::ResultRows)
    .map_err(server_error)?;
    execute_insert(
        transaction,
        &statement,
        lowered.binds,
        object,
        &validated.unique_constraints,
    )
    .await?;
    Ok((result, validated.unique_constraints))
}

#[derive(Debug)]
pub(super) struct ValidatedReturn {
    pub(super) target: TypeId,
    pub(super) column: ResultColumn,
}

pub(super) struct ValidatedMutationTarget<'a> {
    pub(super) target: &'a ObjectTypeDefinition,
    pub(super) unique_constraints: UniqueConstraints,
}

pub(super) struct ValidatedActiveMutation<'a> {
    pub(super) returned: ValidatedReturn,
    pub(super) plan: ServerMutationPlan,
    pub(super) target: &'a ObjectTypeDefinition,
    pub(super) unique_constraints: UniqueConstraints,
    pub(super) arguments: BTreeMap<ParameterId, BindValue>,
}

pub(super) struct ValidatedActiveDelete<'a> {
    pub(super) column: ResultColumn,
    pub(super) plan: ServerDeletePlan,
    pub(super) target: &'a ObjectTypeDefinition,
    pub(super) arguments: BTreeMap<ParameterId, BindValue>,
}

pub(super) fn server_error(error: ServerInsertError) -> PostgresKernelError {
    PostgresKernelError::ServerInsert(error)
}

pub(super) fn update_error(error: ServerUpdateError) -> PostgresKernelError {
    PostgresKernelError::ServerUpdate(error)
}

pub(super) fn delete_error(error: ServerDeleteError) -> PostgresKernelError {
    PostgresKernelError::ServerDelete(error)
}

pub(super) fn update_unavailable(error: PostgresKernelError) -> PostgresKernelError {
    update_error(ServerUpdateError::Unavailable {
        source: Box::new(error),
    })
}

pub(super) fn delete_unavailable(error: PostgresKernelError) -> PostgresKernelError {
    delete_error(ServerDeleteError::Unavailable {
        source: Box::new(error),
    })
}

pub(super) fn mutation_kernel_error(
    operation: MutationExecutionKind,
    error: PostgresKernelError,
) -> PostgresKernelError {
    match operation {
        MutationExecutionKind::Insert => kernel_error(error),
        MutationExecutionKind::Update => update_unavailable(error),
        MutationExecutionKind::Delete => delete_unavailable(error),
    }
}

pub(super) fn pre_transaction_mutation_error(
    operation: MutationExecutionKind,
    error: PostgresKernelError,
) -> PostgresKernelError {
    match operation {
        MutationExecutionKind::Insert => pre_transaction_error(error),
        MutationExecutionKind::Update => update_unavailable(error),
        MutationExecutionKind::Delete => delete_unavailable(error),
    }
}

pub(super) fn update_not_committed(
    context: ServerUpdateContext,
    error: PostgresKernelError,
) -> PostgresKernelError {
    let source = match error {
        PostgresKernelError::ServerInsert(source) => source,
        PostgresKernelError::Database(source) => ServerMutationError::Database { source },
        error => ServerMutationError::Kernel {
            source: Box::new(error),
        },
    };
    update_error(ServerUpdateError::NotCommitted {
        context,
        source: Box::new(source),
    })
}

pub(super) fn delete_not_committed(
    context: ServerDeleteContext,
    error: PostgresKernelError,
) -> PostgresKernelError {
    let source = match error {
        PostgresKernelError::ServerDelete(error) => {
            return PostgresKernelError::ServerDelete(error);
        }
        PostgresKernelError::ServerInsert(source) => source,
        PostgresKernelError::Database(source) => ServerMutationError::Database { source },
        error => ServerMutationError::Kernel {
            source: Box::new(error),
        },
    };
    delete_error(ServerDeleteError::NotCommitted {
        context,
        source: Box::new(source),
    })
}

pub(super) fn kernel_error(error: PostgresKernelError) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerInsert(_) => error,
        error => server_error(ServerInsertError::Kernel {
            source: Box::new(error),
        }),
    }
}

pub(super) fn pre_transaction_error(error: PostgresKernelError) -> PostgresKernelError {
    match error {
        PostgresKernelError::Database(source) => {
            server_error(ServerInsertError::Database { source })
        }
        error => kernel_error(error),
    }
}

pub(super) fn not_committed(
    context: ServerInsertContext,
    error: PostgresKernelError,
) -> PostgresKernelError {
    let source = match error {
        PostgresKernelError::ServerInsert(source) => source,
        PostgresKernelError::Database(source) => ServerInsertError::Database { source },
        error => ServerInsertError::Kernel {
            source: Box::new(error),
        },
    };
    server_error(ServerInsertError::NotCommitted {
        context,
        source: Box::new(source),
    })
}

pub(super) fn artifact_error(function: FunctionId, rule: &'static str) -> PostgresKernelError {
    server_error(ServerInsertError::Artifact { function, rule })
}

pub(super) fn plan_invariant(rule: &'static str) -> PostgresKernelError {
    server_error(ServerInsertError::PlanInvariant { rule })
}

pub(super) fn argument_error(
    parameter: Option<ParameterId>,
    rule: &'static str,
) -> PostgresKernelError {
    server_error(ServerInsertError::Argument { parameter, rule })
}
