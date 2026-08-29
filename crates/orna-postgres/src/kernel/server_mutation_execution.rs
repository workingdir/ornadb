// Mutation execution keeps the accepted error layout across its public seam.
#![allow(clippy::result_large_err)]

//! Execution of the initial single-object SERVER mutation subset.
//!
//! This module accepts stable identities, typed runtime arguments, and one
//! recovered canonical mutation artifact. It does not resolve semantic names,
//! accept source SQL, or expose PostgreSQL details through its public seam.

use std::{collections::BTreeMap, error::Error, fmt};

use orna_artifact::server_mutation_plan::{
    self, MutationExpressionKind, MutationSelector, RecordFieldExpressionKind, ServerDeletePlan,
    ServerMutationOperation, ServerMutationPlan,
};
use orna_core::{
    FieldId, FunctionId, FunctionRevisionId, ObjectId, ParameterId, TypeId,
    catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionTransaction, FunctionVolatility, ObjectTypeDefinition, ValueTypeKind,
        ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, CatalogueHashContext, DefinitionReferenceKind,
        DefinitionReferenceTarget, ExecutableArtifactKind, RevisionPair,
    },
    security::AuthorisedInvocation,
    types::{ResolvedType, StandardScalar},
    value::{
        EnumValue, FunctionArgument, RecordValue, RecordValueError, ResultColumn, ResultRow,
        ResultRows, ResultRowsError, RuntimeType, RuntimeValue,
    },
};
use orna_protocol::{ValueCodecError, encode_active_value};
use tokio_postgres::{
    Client, IsolationLevel, Row, Statement, Transaction,
    error::SqlState,
    types::{ToSql, Type},
};

use crate::{
    PostgresKernel, PostgresKernelError,
    server_runtime::{
        ExpectedDefinitionReference, ReferenceReplayMismatch, ResolvedRuntimeType,
        configure_and_recover, postgres_type, resolve_catalogue_runtime_type, resolve_runtime_type,
        runtime_types_match, validate_function_reference_replay,
    },
    storage::{DATA_SCHEMA, OBJECT_ID_COLUMN, field_name, relation_name, unique_constraint_name},
};

#[path = "server_mutation_execution/contract.rs"]
mod contract;
pub use contract::{
    ServerDeleteCommitState, ServerDeleteContext, ServerDeleteError, ServerDeleteResult,
    ServerInsertCommitState, ServerInsertContext, ServerInsertError, ServerInsertResult,
    ServerMutationCommitState, ServerMutationContext, ServerMutationError, ServerUpdateCommitState,
    ServerUpdateContext, ServerUpdateError, ServerUpdateResult,
};

#[path = "server_mutation_execution/raw.rs"]
mod raw;
pub(crate) use raw::{
    RawServerReferenceMutation, execute_authorised_raw_server_insert,
    execute_authorised_raw_server_insert_with_arguments,
    execute_authorised_raw_server_reference_mutation, raw_server_delete_target_is_unavailable,
    raw_server_insert_target_is_selected, raw_server_insert_target_is_unavailable,
    raw_server_reference_mutation_target, raw_server_reference_value_update_target_is_selected,
    raw_server_update_target_is_unavailable,
};
#[cfg(test)]
use raw::{
    raw_reference_mutation_failure_is_unavailable, validate_raw_argument_pair_insert_parameter_use,
    validate_raw_reference_insert_parameter_use, validate_raw_scalar_insert_parameter_use,
    validate_raw_server_insert_argument_shape, validate_raw_text_insert_argument,
};

const VARIABLE_ARGUMENT_PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;
const ACTIVE_VALUE_ENVELOPE_LENGTH: usize = 25;
const SQL_LIMIT: usize = 1024 * 1024;

#[cfg(feature = "test-hooks")]
struct MutationTestBarrier {
    reached: std::sync::Arc<tokio::sync::Barrier>,
    resume: std::sync::Arc<tokio::sync::Barrier>,
}

#[cfg(not(feature = "test-hooks"))]
struct MutationTestBarrier;

enum ServerMutationResult {
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

async fn execute_mutation_client(
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

async fn commit_mutation_candidate(
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
enum DeleteCommitFailure {
    Restricted,
    Rejected,
    Unknown,
}

fn delete_commit_failure(code: Option<&SqlState>) -> DeleteCommitFailure {
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

async fn execute_mutation_transaction(
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

async fn execute_active_update(
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

async fn execute_validated_active_update(
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

async fn execute_active_delete(
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

async fn execute_active_insert(
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

async fn execute_validated_active_insert(
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
struct ValidatedReturn {
    target: TypeId,
    column: ResultColumn,
}

struct ValidatedMutationTarget<'a> {
    target: &'a ObjectTypeDefinition,
    unique_constraints: UniqueConstraints,
}

struct ValidatedActiveMutation<'a> {
    returned: ValidatedReturn,
    plan: ServerMutationPlan,
    target: &'a ObjectTypeDefinition,
    unique_constraints: UniqueConstraints,
    arguments: BTreeMap<ParameterId, BindValue>,
}

struct ValidatedActiveDelete<'a> {
    column: ResultColumn,
    plan: ServerDeletePlan,
    target: &'a ObjectTypeDefinition,
    arguments: BTreeMap<ParameterId, BindValue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UniqueConstraint {
    Reference {
        owner: TypeId,
        field: FieldId,
        referenced_type: TypeId,
    },
    Text {
        owner: TypeId,
        field: FieldId,
    },
}

impl UniqueConstraint {
    const fn field(self) -> FieldId {
        match self {
            Self::Reference { field, .. } | Self::Text { field, .. } => field,
        }
    }

    fn error(self, source: tokio_postgres::Error) -> ServerMutationError {
        match self {
            Self::Reference {
                owner,
                field,
                referenced_type,
            } => ServerMutationError::UniqueReferenceConflict {
                owner,
                field,
                referenced_type,
                source,
            },
            Self::Text { owner, field } => ServerMutationError::UniqueTextConflict {
                owner,
                field,
                source,
            },
        }
    }
}

#[derive(Clone, Debug)]
struct UniqueConstraints {
    fields: Vec<UniqueConstraint>,
}

impl UniqueConstraints {
    fn from_target(
        context: &CatalogueHashContext,
        target: &ObjectTypeDefinition,
    ) -> Result<Self, PostgresKernelError> {
        let mut fields = Vec::new();
        for field in target.fields() {
            if !field.unique() {
                continue;
            }
            if field.is_required_unique_reference() {
                let Some(referenced_type) = field.resolved_type().reference_target() else {
                    return Err(plan_invariant(
                        "UNIQUE target fields must be exact Text or required typed references",
                    ));
                };
                fields.push(UniqueConstraint::Reference {
                    owner: target.id(),
                    field: field.id(),
                    referenced_type,
                });
                continue;
            }
            if !supports_unique_text(context, field.resolved_type()) {
                return Err(plan_invariant(
                    "UNIQUE target fields must be exact Text or required typed references",
                ));
            }
            fields.push(UniqueConstraint::Text {
                owner: target.id(),
                field: field.id(),
            });
        }
        Ok(Self { fields })
    }

    fn conflict(&self, source: &tokio_postgres::Error) -> Option<UniqueConstraint> {
        let error = source.as_db_error()?;
        unique_constraint(self, Some(error.code()), error.constraint())
    }
}

fn supports_unique_text(context: &CatalogueHashContext, resolved_type: ResolvedType) -> bool {
    match (context.standard(), resolved_type) {
        (None, ResolvedType::Scalar(StandardScalar::CharacterLargeObject)) => true,
        (Some(standard), ResolvedType::Value(type_id)) => standard
            .catalogue()
            .value_type_by_id(type_id)
            .is_some_and(|value_type| {
                value_type.kind() == ValueTypeKind::Primitive
                    && value_type.mutability() == ValueTypeMutability::Immutable
                    && value_type.persistence() == ValueTypePersistence::Persistable
                    && value_type.representation_contract()
                        == "orna.kernel.value.character-large-object@1"
            }),
        _ => false,
    }
}

fn unique_constraint(
    constraints: &UniqueConstraints,
    code: Option<&SqlState>,
    constraint: Option<&str>,
) -> Option<UniqueConstraint> {
    if code != Some(&SqlState::UNIQUE_VIOLATION) {
        return None;
    }
    let constraint = constraint?;
    constraints
        .fields
        .iter()
        .copied()
        .find(|expected| unique_constraint_name(expected.field()) == constraint)
}

fn mutation_database_error(
    source: tokio_postgres::Error,
    constraints: &UniqueConstraints,
) -> ServerMutationError {
    if let Some(constraint) = constraints.conflict(&source) {
        constraint.error(source)
    } else {
        ServerMutationError::Database { source }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MutationExecutionKind {
    Insert,
    Update,
    Delete,
}

impl MutationExecutionKind {
    const fn accepts_artifact_version(self, version: u32) -> bool {
        match self {
            Self::Insert => matches!(
                version,
                server_mutation_plan::INSERT_FORMAT_VERSION
                    | server_mutation_plan::RECORD_INSERT_FORMAT_VERSION
            ),
            Self::Update => version == server_mutation_plan::UPDATE_FORMAT_VERSION,
            Self::Delete => version == server_mutation_plan::DELETE_FORMAT_VERSION,
        }
    }
}

fn validate_active_mutation<'a>(
    active: &'a ActiveDatabaseRevision,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
    operation: MutationExecutionKind,
) -> Result<ValidatedActiveMutation<'a>, PostgresKernelError> {
    let context = active.catalogue_hash_context();
    let returned =
        validate_function_signature_for_context(context, active.catalogue(), function, operation)?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == function.id() && revision.id() == function.current_revision()
        })
        .ok_or_else(|| {
            server_error(ServerMutationError::CurrentRevision {
                function: function.id(),
                revision: function.current_revision(),
            })
        })?;
    let artifact = revision.artifact();
    validate_artifact_metadata_for_operation(
        function.id(),
        artifact.kind(),
        artifact.format(),
        artifact.version(),
        revision.language_version(),
        operation,
    )?;
    let plan = ServerMutationPlan::decode(artifact.payload())
        .map_err(ServerMutationError::PlanDecode)
        .map_err(server_error)?;
    validate_artifact_payload_version(function.id(), artifact.version(), &plan)?;
    let target = validate_plan_for_active(active, function, returned.target, &plan, operation)?;
    validate_reference_evidence(active, function, &plan)?;
    let arguments =
        validate_arguments_with_context(context, active.catalogue(), function, arguments)?;
    Ok(ValidatedActiveMutation {
        returned,
        plan,
        target: target.target,
        unique_constraints: target.unique_constraints,
        arguments,
    })
}

fn validate_active_delete<'a>(
    active: &'a ActiveDatabaseRevision,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<ValidatedActiveDelete<'a>, PostgresKernelError> {
    let context = active.catalogue_hash_context();
    let column =
        validate_delete_function_signature_with_context(context, active.catalogue(), function)?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == function.id() && revision.id() == function.current_revision()
        })
        .ok_or_else(|| {
            server_error(ServerMutationError::CurrentRevision {
                function: function.id(),
                revision: function.current_revision(),
            })
        })?;
    let artifact = revision.artifact();
    validate_artifact_metadata_for_operation(
        function.id(),
        artifact.kind(),
        artifact.format(),
        artifact.version(),
        revision.language_version(),
        MutationExecutionKind::Delete,
    )?;
    let plan = ServerDeletePlan::decode(artifact.payload())
        .map_err(ServerMutationError::PlanDecode)
        .map_err(server_error)?;
    let target = validate_delete_plan(active.catalogue(), function, &plan)?;
    validate_delete_reference_evidence(active, function, &plan)?;
    let arguments =
        validate_arguments_with_context(context, active.catalogue(), function, arguments)?;
    Ok(ValidatedActiveDelete {
        column,
        plan,
        target,
        arguments,
    })
}

#[cfg(test)]
fn validate_artifact_metadata(
    function: FunctionId,
    kind: ExecutableArtifactKind,
    format: &str,
    version: u32,
    language_version: &str,
) -> Result<(), PostgresKernelError> {
    validate_artifact_metadata_for_operation(
        function,
        kind,
        format,
        version,
        language_version,
        MutationExecutionKind::Insert,
    )
}

fn validate_artifact_metadata_for_operation(
    function: FunctionId,
    kind: ExecutableArtifactKind,
    format: &str,
    version: u32,
    language_version: &str,
    operation: MutationExecutionKind,
) -> Result<(), PostgresKernelError> {
    if kind != ExecutableArtifactKind::Server {
        return Err(artifact_error(
            function,
            "the active function must contain SERVER executable data",
        ));
    }
    if format != server_mutation_plan::FORMAT_IDENTITY
        || !operation.accepts_artifact_version(version)
    {
        return Err(artifact_error(
            function,
            match operation {
                MutationExecutionKind::Insert => {
                    "the active function must use INSERT mutation format version 1 or 4"
                }
                MutationExecutionKind::Update => {
                    "the active function must use the supported UPDATE mutation format version 2"
                }
                MutationExecutionKind::Delete => {
                    "the active function must use the supported DELETE mutation format version 3"
                }
            },
        ));
    }
    if language_version != server_mutation_plan::LANGUAGE_VERSION_IDENTITY {
        return Err(artifact_error(
            function,
            "the active function must use orna.language/1",
        ));
    }
    Ok(())
}

fn validate_artifact_payload_version(
    function: FunctionId,
    artifact_version: u32,
    plan: &ServerMutationPlan,
) -> Result<(), PostgresKernelError> {
    if artifact_version != plan.format_version() {
        return Err(artifact_error(
            function,
            "the active artifact metadata version must match its mutation payload",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn validate_function_signature(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
) -> Result<ValidatedReturn, PostgresKernelError> {
    validate_function_signature_for_operation(catalogue, function, MutationExecutionKind::Insert)
}

#[cfg(test)]
fn validate_function_signature_for_operation(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    operation: MutationExecutionKind,
) -> Result<ValidatedReturn, PostgresKernelError> {
    validate_function_signature_for_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        catalogue,
        function,
        operation,
    )
}

fn validate_function_signature_for_context(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    operation: MutationExecutionKind,
) -> Result<ValidatedReturn, PostgresKernelError> {
    validate_mutation_function_header(context, catalogue, function, operation)?;
    let reject = |rule| function_signature_error(function.id(), rule);
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => {
                "an INSERT SERVER function must return exactly one object-reference column"
            }
            MutationExecutionKind::Update => {
                "an UPDATE SERVER function must return exactly one object-reference column"
            }
            MutationExecutionKind::Delete => {
                "a DELETE SERVER function must return exactly one BOOLEAN column"
            }
        }));
    };
    let [column] = columns.as_slice() else {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => {
                "an INSERT SERVER function must return exactly one object-reference column"
            }
            MutationExecutionKind::Update => {
                "an UPDATE SERVER function must return exactly one object-reference column"
            }
            MutationExecutionKind::Delete => {
                "a DELETE SERVER function must return exactly one BOOLEAN column"
            }
        }));
    };
    let ResolvedRuntimeType::Reference(target) =
        resolve_runtime_type(context, column.resolved_type())
    else {
        return Err(reject(
            "the sole result column must be a non-null object reference",
        ));
    };
    if catalogue.object_type_by_id(target).is_none() {
        return Err(reject(
            "the result column must reference an active object type",
        ));
    }
    let column = ResultColumn::new(column.name(), ResolvedType::reference(target), false)
        .map_err(ServerInsertError::ResultRows)
        .map_err(server_error)?;
    Ok(ValidatedReturn { target, column })
}

#[cfg(test)]
fn validate_delete_function_signature(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
) -> Result<ResultColumn, PostgresKernelError> {
    validate_delete_function_signature_with_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        catalogue,
        function,
    )
}

fn validate_delete_function_signature_with_context(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
) -> Result<ResultColumn, PostgresKernelError> {
    validate_mutation_function_header(context, catalogue, function, MutationExecutionKind::Delete)?;
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(function_signature_error(
            function.id(),
            "a DELETE SERVER function must return exactly one BOOLEAN column",
        ));
    };
    let [column] = columns.as_slice() else {
        return Err(function_signature_error(
            function.id(),
            "a DELETE SERVER function must return exactly one BOOLEAN column",
        ));
    };
    if !runtime_types_match(
        context,
        column.resolved_type(),
        ResolvedType::scalar(orna_core::types::StandardScalar::Boolean),
    ) {
        return Err(function_signature_error(
            function.id(),
            "the sole DELETE result column must be BOOLEAN",
        ));
    }
    ResultColumn::new(
        column.name(),
        ResolvedType::Scalar(orna_core::types::StandardScalar::Boolean),
        false,
    )
    .map_err(ServerMutationError::ResultRows)
    .map_err(server_error)
}

fn validate_mutation_function_header(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    operation: MutationExecutionKind,
) -> Result<(), PostgresKernelError> {
    let reject = |rule| function_signature_error(function.id(), rule);
    if function.domain() != FunctionDomain::Server {
        return Err(reject("this operation requires a SERVER function"));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => "an INSERT SERVER function must use SECURITY INVOKER",
            MutationExecutionKind::Update => "an UPDATE SERVER function must use SECURITY INVOKER",
            MutationExecutionKind::Delete => "a DELETE SERVER function must use SECURITY INVOKER",
        }));
    }
    if function.transaction() != Some(FunctionTransaction::Atomic) {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => {
                "an INSERT SERVER function must use exactly TRANSACTION ATOMIC"
            }
            MutationExecutionKind::Update => {
                "an UPDATE SERVER function must use exactly TRANSACTION ATOMIC"
            }
            MutationExecutionKind::Delete => {
                "a DELETE SERVER function must use exactly TRANSACTION ATOMIC"
            }
        }));
    }
    if function.volatility() != FunctionVolatility::Volatile {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => {
                "an INSERT SERVER function must use VOLATILITY VOLATILE"
            }
            MutationExecutionKind::Update => {
                "an UPDATE SERVER function must use VOLATILITY VOLATILE"
            }
            MutationExecutionKind::Delete => {
                "a DELETE SERVER function must use VOLATILITY VOLATILE"
            }
        }));
    }
    for parameter in function.parameters() {
        if parameter.default_expression().is_some() {
            return Err(reject(match operation {
                MutationExecutionKind::Insert => {
                    "INSERT SERVER function parameters cannot have default expressions"
                }
                MutationExecutionKind::Update => {
                    "UPDATE SERVER function parameters cannot have default expressions"
                }
                MutationExecutionKind::Delete => {
                    "DELETE SERVER function parameters cannot have default expressions"
                }
            }));
        }
        if !runtime_type_is_active(context, catalogue, parameter.resolved_type()) {
            return Err(reject(match operation {
                MutationExecutionKind::Insert => {
                    "every INSERT SERVER function parameter must use a supported active type"
                }
                MutationExecutionKind::Update => {
                    "every UPDATE SERVER function parameter must use a supported active type"
                }
                MutationExecutionKind::Delete => {
                    "every DELETE SERVER function parameter must use a supported active type"
                }
            }));
        }
    }
    Ok(())
}

fn function_signature_error(function: FunctionId, rule: &'static str) -> PostgresKernelError {
    server_error(ServerMutationError::FunctionSignature { function, rule })
}

fn runtime_type_is_active(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    resolved_type: ResolvedType,
) -> bool {
    let runtime = resolve_mutation_runtime_type(context, catalogue, resolved_type);
    if postgres_type(runtime).is_none() {
        return false;
    }
    match runtime {
        ResolvedRuntimeType::Reference(target) => catalogue.object_type_by_id(target).is_some(),
        ResolvedRuntimeType::LegacyScalar(_) | ResolvedRuntimeType::VerifiedValue { .. } => true,
        ResolvedRuntimeType::CatalogueEnum(_) => true,
        ResolvedRuntimeType::Record(_) | ResolvedRuntimeType::Unsupported => false,
    }
}

fn validate_active_runtime_type(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    resolved_type: ResolvedType,
    rule: &'static str,
) -> Result<(), PostgresKernelError> {
    let runtime = resolve_mutation_runtime_type(context, catalogue, resolved_type);
    if postgres_type(runtime).is_none() {
        return Err(plan_invariant(rule));
    }
    match runtime {
        ResolvedRuntimeType::Reference(target) if catalogue.object_type_by_id(target).is_none() => {
            return Err(plan_invariant(
                "every referenced object type must be active",
            ));
        }
        ResolvedRuntimeType::LegacyScalar(_)
        | ResolvedRuntimeType::VerifiedValue { .. }
        | ResolvedRuntimeType::CatalogueEnum(_)
        | ResolvedRuntimeType::Reference(_) => {}
        ResolvedRuntimeType::Record(_) | ResolvedRuntimeType::Unsupported => {
            return Err(plan_invariant(rule));
        }
    }
    Ok(())
}

fn resolve_mutation_runtime_type(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    resolved_type: ResolvedType,
) -> ResolvedRuntimeType {
    let runtime = resolve_catalogue_runtime_type(catalogue, context, resolved_type);
    if runtime == ResolvedRuntimeType::Unsupported
        && resolved_type.named_type().is_some_and(|enum_type| {
            context
                .standard()
                .is_some_and(|standard| standard.catalogue().enum_type_by_id(enum_type).is_some())
        })
    {
        ResolvedRuntimeType::CatalogueEnum(
            resolved_type
                .named_type()
                .expect("standard enum identity was checked"),
        )
    } else {
        runtime
    }
}

#[cfg(test)]
fn validate_plan<'a>(
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
) -> Result<&'a ObjectTypeDefinition, PostgresKernelError> {
    Ok(validate_plan_for_operation(
        catalogue,
        function,
        returned_target,
        plan,
        MutationExecutionKind::Insert,
    )?
    .target)
}

#[cfg(test)]
fn validate_plan_for_context<'a>(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
    operation: MutationExecutionKind,
) -> Result<ValidatedMutationTarget<'a>, PostgresKernelError> {
    validate_plan_with_active(
        None,
        context,
        catalogue,
        function,
        returned_target,
        plan,
        operation,
    )
}

fn validate_plan_for_active<'a>(
    active: &'a ActiveDatabaseRevision,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
    operation: MutationExecutionKind,
) -> Result<ValidatedMutationTarget<'a>, PostgresKernelError> {
    validate_plan_with_active(
        Some(active),
        active.catalogue_hash_context(),
        active.catalogue(),
        function,
        returned_target,
        plan,
        operation,
    )
}

fn validate_plan_with_active<'a>(
    active: Option<&'a ActiveDatabaseRevision>,
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
    operation: MutationExecutionKind,
) -> Result<ValidatedMutationTarget<'a>, PostgresKernelError> {
    let operation_matches = matches!(
        (operation, plan.operation()),
        (
            MutationExecutionKind::Insert,
            ServerMutationOperation::Insert
        ) | (
            MutationExecutionKind::Update,
            ServerMutationOperation::Update { .. }
        )
    );
    if !operation_matches || !operation.accepts_artifact_version(plan.format_version()) {
        return Err(plan_invariant(
            "the payload operation and version must match the requested mutation",
        ));
    }
    if plan.returned_object() != plan.target() || plan.target() != returned_target {
        return Err(plan_invariant(
            "plan target, returned object, and declared result REF target must match",
        ));
    }
    let target = catalogue
        .object_type_by_id(plan.target())
        .ok_or_else(|| plan_invariant("mutation target must be an active object type"))?;
    let unique_constraints = UniqueConstraints::from_target(context, target)?;
    for field in target.fields() {
        if field.default_expression().is_some() {
            return Err(plan_invariant(
                "mutation targets cannot contain field default expressions",
            ));
        }
        match resolve_runtime_type(context, field.resolved_type()) {
            ResolvedRuntimeType::Reference(target)
                if catalogue.object_type_by_id(target).is_none() =>
            {
                return Err(plan_invariant(
                    "every target-field REF type must name an active object type",
                ));
            }
            ResolvedRuntimeType::LegacyScalar(_)
            | ResolvedRuntimeType::VerifiedValue { .. }
            | ResolvedRuntimeType::Reference(_)
            | ResolvedRuntimeType::CatalogueEnum(_)
            | ResolvedRuntimeType::Record(_)
            | ResolvedRuntimeType::Unsupported => {}
        }
    }

    let mut assigned = BTreeMap::new();
    for assignment in plan.assignments() {
        if assignment.owner() != target.id() {
            return Err(plan_invariant(
                "every assignment owner must equal the mutation target",
            ));
        }
        let field = target
            .field_by_id(assignment.field())
            .ok_or_else(|| plan_invariant("every owner-qualified assigned field must be active"))?;
        if assigned.insert(field.id(), ()).is_some() {
            return Err(plan_invariant(
                "an owner-qualified field cannot be assigned more than once",
            ));
        }
        let expression = assignment.expression();
        if let MutationExpressionKind::RecordConstructor { fields } = expression.kind() {
            validate_record_constructor(
                active.ok_or_else(|| {
                    plan_invariant(
                        "record constructors require one complete active database revision",
                    )
                })?,
                function,
                field,
                expression,
                fields,
                operation,
            )?;
            continue;
        }
        validate_active_runtime_type(
            context,
            catalogue,
            expression.resolved_type(),
            "every assignment expression must use the active runtime subset",
        )?;
        if !runtime_types_match(context, expression.resolved_type(), field.resolved_type()) {
            return Err(plan_invariant(
                "assignment expression type must exactly equal its target field type",
            ));
        }
        if matches!(expression.kind(), MutationExpressionKind::TypedNull) && !field.nullable() {
            return Err(plan_invariant(
                "typed NULL can target only a nullable field",
            ));
        }
        if let MutationExpressionKind::Parameter { owner, parameter } = expression.kind() {
            if *owner != function.id() {
                return Err(plan_invariant(
                    "parameter expression owner must equal the active function",
                ));
            }
            let parameter = function.parameter_by_id(*parameter).ok_or_else(|| {
                plan_invariant("parameter expression must name an active declared parameter")
            })?;
            if parameter.default_expression().is_some()
                || !runtime_types_match(
                    context,
                    parameter.resolved_type(),
                    expression.resolved_type(),
                )
            {
                return Err(plan_invariant(
                    "parameter expression must exactly match a required active parameter",
                ));
            }
        }
    }
    if operation == MutationExecutionKind::Insert
        && target
            .fields()
            .iter()
            .any(|field| !field.nullable() && !assigned.contains_key(&field.id()))
    {
        return Err(plan_invariant(
            "every non-null target field must have an assignment",
        ));
    }
    if let ServerMutationOperation::Update { selector } = plan.operation() {
        if selector.owner() != function.id() {
            return Err(plan_invariant(
                "selector owner must equal the active function",
            ));
        }
        let parameter = function
            .parameter_by_id(selector.parameter())
            .ok_or_else(|| plan_invariant("selector must name an active declared parameter"))?;
        if parameter.default_expression().is_some()
            || parameter.resolved_type() != ResolvedType::reference(target.id())
        {
            return Err(plan_invariant(
                "selector must exactly match a required REF parameter for the target object",
            ));
        }
    }
    Ok(ValidatedMutationTarget {
        target,
        unique_constraints,
    })
}

fn validate_record_constructor(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    target_field: &orna_core::catalogue::FieldDefinition,
    expression: &server_mutation_plan::MutationExpression,
    fields: &[server_mutation_plan::RecordFieldExpression],
    operation: MutationExecutionKind,
) -> Result<(), PostgresKernelError> {
    if operation != MutationExecutionKind::Insert {
        return Err(plan_invariant(
            "record constructors are accepted only in INSERT plans",
        ));
    }
    let record_type = expression
        .resolved_type()
        .named_type()
        .ok_or_else(|| plan_invariant("record constructor must retain its nominal record type"))?;
    if target_field.nullable() || target_field.resolved_type() != ResolvedType::named(record_type) {
        return Err(plan_invariant(
            "record constructor must target a non-null field of its exact nominal type",
        ));
    }
    let definition = active
        .catalogue()
        .record_value_type_by_id(record_type)
        .ok_or_else(|| plan_invariant("record constructor type must be active"))?;
    if fields.len() != definition.fields().len() {
        return Err(plan_invariant(
            "record constructor field count must match its active definition",
        ));
    }
    for (field, declared) in fields.iter().zip(definition.fields()) {
        if field.owner() != record_type || field.field() != declared.id() {
            return Err(plan_invariant(
                "record constructor fields must retain active declaration order and identity",
            ));
        }
        let runtime_type = active
            .record_value_field_descriptor_runtime_type(declared.descriptor())
            .ok_or_else(|| plan_invariant("record constructor field type must be active"))?;
        if !runtime_types_match(
            active.catalogue_hash_context(),
            field.resolved_type(),
            runtime_type,
        ) {
            return Err(plan_invariant(
                "record constructor child type must match its active field type",
            ));
        }
        validate_active_runtime_type(
            active.catalogue_hash_context(),
            active.catalogue(),
            field.resolved_type(),
            "record constructor child type must be active",
        )?;
        match field.kind() {
            RecordFieldExpressionKind::Parameter { owner, parameter } => {
                if *owner != function.id() {
                    return Err(plan_invariant(
                        "record constructor parameter owner must equal the active function",
                    ));
                }
                let parameter = function.parameter_by_id(*parameter).ok_or_else(|| {
                    plan_invariant("record constructor parameter must be actively declared")
                })?;
                if parameter.default_expression().is_some()
                    || !runtime_types_match(
                        active.catalogue_hash_context(),
                        parameter.resolved_type(),
                        field.resolved_type(),
                    )
                {
                    return Err(plan_invariant(
                        "record constructor parameter must exactly match its artifact child",
                    ));
                }
            }
            RecordFieldExpressionKind::BooleanLiteral { .. } => {
                if !runtime_types_match(
                    active.catalogue_hash_context(),
                    field.resolved_type(),
                    ResolvedType::scalar(orna_core::types::StandardScalar::Boolean),
                ) {
                    return Err(plan_invariant(
                        "record constructor Boolean child must target a Boolean field",
                    ));
                }
            }
            _ => {
                return Err(plan_invariant(
                    "unknown future record constructor child kinds are unsupported",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
fn validate_plan_for_operation<'a>(
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
    operation: MutationExecutionKind,
) -> Result<ValidatedMutationTarget<'a>, PostgresKernelError> {
    validate_plan_for_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        catalogue,
        function,
        returned_target,
        plan,
        operation,
    )
}

fn validate_delete_plan<'a>(
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    plan: &ServerDeletePlan,
) -> Result<&'a ObjectTypeDefinition, PostgresKernelError> {
    if plan.format_version() != server_mutation_plan::DELETE_FORMAT_VERSION {
        return Err(plan_invariant(
            "the DELETE payload must use mutation format version 3",
        ));
    }
    let target = catalogue
        .object_type_by_id(plan.target())
        .ok_or_else(|| plan_invariant("DELETE target must be an active object type"))?;
    let selector = plan.selector();
    if selector.owner() != function.id() {
        return Err(plan_invariant(
            "DELETE selector owner must equal the active function",
        ));
    }
    let parameter = function
        .parameter_by_id(selector.parameter())
        .ok_or_else(|| plan_invariant("DELETE selector must name an active declared parameter"))?;
    if parameter.default_expression().is_some()
        || parameter.resolved_type() != ResolvedType::reference(target.id())
    {
        return Err(plan_invariant(
            "DELETE selector must exactly match a required REF parameter for the target object",
        ));
    }
    Ok(target)
}

fn validate_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerMutationPlan,
) -> Result<(), PostgresKernelError> {
    let expected = expected_body_references(plan);
    validate_function_reference_replay(active, function, &expected).map_err(|mismatch| {
        let rule = match mismatch {
            ReferenceReplayMismatch::Count => {
                "reference count must match the signature and mutation body"
            }
            ReferenceReplayMismatch::Sequence => {
                "references must replay the exact signature and mutation body order"
            }
        };
        server_error(ServerInsertError::ReferenceEvidence {
            function: function.id(),
            rule,
        })
    })
}

fn validate_delete_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerDeletePlan,
) -> Result<(), PostgresKernelError> {
    let expected = expected_delete_body_references(plan);
    validate_function_reference_replay(active, function, &expected).map_err(|mismatch| {
        let rule = match mismatch {
            ReferenceReplayMismatch::Count => {
                "reference count must match the signature and DELETE body"
            }
            ReferenceReplayMismatch::Sequence => {
                "references must replay the exact signature and DELETE body order"
            }
        };
        server_error(ServerMutationError::ReferenceEvidence {
            function: function.id(),
            rule,
        })
    })
}

fn expected_delete_body_references(plan: &ServerDeletePlan) -> [ExpectedDefinitionReference; 3] {
    [
        ExpectedDefinitionReference::new(
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(plan.target()),
        ),
        ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(plan.target()),
        ),
        ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter {
                owner: plan.selector().owner(),
                parameter: plan.selector().parameter(),
            },
        ),
    ]
}

fn expected_body_references(plan: &ServerMutationPlan) -> Vec<ExpectedDefinitionReference> {
    let mut expected = Vec::with_capacity(plan.assignments().len().saturating_mul(2) + 4);
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::WriteObject,
        DefinitionReferenceTarget::ObjectType(plan.target()),
    ));
    for assignment in plan.assignments() {
        expected.push(ExpectedDefinitionReference::new(
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Field {
                owner: assignment.owner(),
                field: assignment.field(),
            },
        ));
        match assignment.expression().kind() {
            MutationExpressionKind::Parameter { owner, parameter } => {
                expected.push(ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: *owner,
                        parameter: *parameter,
                    },
                ));
            }
            MutationExpressionKind::RecordConstructor { fields } => {
                let record_type = assignment
                    .expression()
                    .resolved_type()
                    .named_type()
                    .expect("validated record constructor must retain a named type");
                expected.push(ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::NamedType,
                    DefinitionReferenceTarget::ValueType(record_type),
                ));
                for field in fields {
                    expected.push(ExpectedDefinitionReference::new(
                        DefinitionReferenceKind::WriteField,
                        DefinitionReferenceTarget::Field {
                            owner: field.owner(),
                            field: field.field(),
                        },
                    ));
                    if let RecordFieldExpressionKind::Parameter { owner, parameter } = field.kind()
                    {
                        expected.push(ExpectedDefinitionReference::new(
                            DefinitionReferenceKind::ParameterRead,
                            DefinitionReferenceTarget::Parameter {
                                owner: *owner,
                                parameter: *parameter,
                            },
                        ));
                    }
                }
            }
            MutationExpressionKind::BooleanLiteral { .. } | MutationExpressionKind::TypedNull => {}
            _ => {}
        }
    }
    if let ServerMutationOperation::Update { selector } = plan.operation() {
        expected.push(ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(plan.target()),
        ));
        expected.push(ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter {
                owner: selector.owner(),
                parameter: selector.parameter(),
            },
        ));
    }
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::ObjectReference,
        DefinitionReferenceTarget::ObjectType(plan.returned_object()),
    ));
    expected
}

fn validate_arguments_with_context(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<BTreeMap<ParameterId, BindValue>, PostgresKernelError> {
    let mut validated = BTreeMap::new();
    let mut variable_payload = 0usize;
    for argument in arguments {
        let parameter_id = argument.parameter();
        if validated.contains_key(&parameter_id) {
            return Err(argument_error(
                Some(parameter_id),
                "the same parameter was supplied twice",
            ));
        }
        let parameter = function.parameter_by_id(parameter_id).ok_or_else(|| {
            argument_error(
                Some(parameter_id),
                "an argument was supplied for a parameter that this function does not declare",
            )
        })?;
        let value = argument.value();
        if value.is_null() {
            return Err(argument_error(
                Some(parameter_id),
                "function arguments cannot be NULL",
            ));
        }
        let RuntimeType::Flat(value_type) = value.runtime_type() else {
            return Err(argument_error(
                Some(parameter_id),
                "the argument type is unsupported or its referenced object type is inactive",
            ));
        };
        if !runtime_type_is_active(context, catalogue, value_type) {
            return Err(argument_error(
                Some(parameter_id),
                "the argument type is unsupported or its referenced object type is inactive",
            ));
        }
        if !runtime_types_match(context, value_type, parameter.resolved_type()) {
            return Err(argument_error(
                Some(parameter_id),
                "the argument type does not match the declared parameter type",
            ));
        }
        if let RuntimeValue::Enum(value) = value
            && !enum_value_is_active(context, catalogue, value)
        {
            return Err(argument_error(
                Some(parameter_id),
                "the enum argument label is not active in the pinned catalogue",
            ));
        }
        variable_payload = variable_payload
            .checked_add(variable_payload_len(value)?)
            .ok_or_else(payload_limit_error)?;
        if variable_payload > VARIABLE_ARGUMENT_PAYLOAD_LIMIT {
            return Err(payload_limit_error());
        }
        validated.insert(parameter_id, BindValue::from_runtime(value, parameter_id)?);
    }
    for parameter in function.parameters() {
        if !validated.contains_key(&parameter.id()) {
            return Err(argument_error(
                Some(parameter.id()),
                "a required argument is missing",
            ));
        }
    }
    Ok(validated)
}

#[cfg(test)]
fn validate_arguments(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<BTreeMap<ParameterId, BindValue>, PostgresKernelError> {
    validate_arguments_with_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        catalogue,
        function,
        arguments,
    )
}

fn selector_object(
    plan: &ServerMutationPlan,
    arguments: &[FunctionArgument],
) -> Result<ObjectId, PostgresKernelError> {
    let selector = plan
        .selector()
        .ok_or_else(|| plan_invariant("UPDATE plan must contain one selector parameter"))?;
    selector_argument_object(plan.target(), selector, arguments)
}

fn selector_argument_object(
    target: TypeId,
    selector: MutationSelector,
    arguments: &[FunctionArgument],
) -> Result<ObjectId, PostgresKernelError> {
    let argument = arguments
        .iter()
        .find(|argument| argument.parameter() == selector.parameter())
        .ok_or_else(|| plan_invariant("validated selector argument must be present"))?;
    match argument.value() {
        RuntimeValue::Reference {
            target: actual,
            object,
        } if *actual == target => Ok(*object),
        _ => Err(plan_invariant(
            "validated selector argument must be an exact target object reference",
        )),
    }
}

fn variable_payload_len(value: &RuntimeValue) -> Result<usize, PostgresKernelError> {
    match value {
        RuntimeValue::Text(value) => Ok(value.len()),
        RuntimeValue::Bytes(value) => Ok(value.len()),
        RuntimeValue::Enum(value) => Ok(value.label().len()),
        RuntimeValue::Null(_)
        | RuntimeValue::Boolean(_)
        | RuntimeValue::Integer(_)
        | RuntimeValue::BigInt(_)
        | RuntimeValue::Float(_)
        | RuntimeValue::Reference { .. } => Ok(0),
        _ => Err(argument_error(None, "the argument type is unsupported")),
    }
}

fn payload_limit_error() -> PostgresKernelError {
    server_error(ServerInsertError::ComplexityLimit {
        category: "total size of text and binary arguments",
        maximum: VARIABLE_ARGUMENT_PAYLOAD_LIMIT,
    })
}

#[derive(Clone, Debug, PartialEq)]
enum BindValue {
    Boolean(bool),
    Integer(i32),
    BigInt(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Enum { value: EnumValue, label: String },
}

impl BindValue {
    fn from_runtime(
        value: &RuntimeValue,
        parameter: ParameterId,
    ) -> Result<Self, PostgresKernelError> {
        match value {
            RuntimeValue::Boolean(value) => Ok(Self::Boolean(*value)),
            RuntimeValue::Integer(value) => Ok(Self::Integer(*value)),
            RuntimeValue::BigInt(value) => Ok(Self::BigInt(*value)),
            RuntimeValue::Float(value) => Ok(Self::Float(value.value())),
            RuntimeValue::Text(value) => Ok(Self::Text(value.clone())),
            RuntimeValue::Bytes(value) => Ok(Self::Bytes(value.clone())),
            RuntimeValue::Reference { object, .. } => Ok(Self::Bytes(object.to_bytes().to_vec())),
            RuntimeValue::Enum(value) => Ok(Self::Enum {
                value: value.clone(),
                label: value.label().to_owned(),
            }),
            RuntimeValue::Null(_) => Err(argument_error(
                Some(parameter),
                "function arguments cannot be NULL",
            )),
            _ => Err(argument_error(
                Some(parameter),
                "the argument type is unsupported",
            )),
        }
    }

    fn as_to_sql(&self) -> &(dyn ToSql + Sync) {
        match self {
            Self::Boolean(value) => value,
            Self::Integer(value) => value,
            Self::BigInt(value) => value,
            Self::Float(value) => value,
            Self::Text(value) => value,
            Self::Bytes(value) => value,
            Self::Enum { label, .. } => label,
        }
    }

    fn to_runtime(&self) -> RuntimeValue {
        match self {
            Self::Boolean(value) => RuntimeValue::Boolean(*value),
            Self::Integer(value) => RuntimeValue::Integer(*value),
            Self::BigInt(value) => RuntimeValue::BigInt(*value),
            Self::Float(value) => RuntimeValue::Float(
                orna_core::value::RuntimeFloat::new(*value)
                    .expect("validated bind float must remain finite"),
            ),
            Self::Text(value) => RuntimeValue::Text(value.clone()),
            Self::Bytes(value) => RuntimeValue::Bytes(value.clone()),
            Self::Enum { value, .. } => RuntimeValue::Enum(value.clone()),
        }
    }
}

fn enum_value_is_active(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    value: &EnumValue,
) -> bool {
    catalogue
        .enum_type_by_id(value.enum_type())
        .or_else(|| {
            context
                .standard()
                .and_then(|standard| standard.catalogue().enum_type_by_id(value.enum_type()))
        })
        .is_some_and(|definition| {
            definition
                .labels()
                .iter()
                .any(|label| label == value.label())
        })
}

struct LoweredMutation {
    sql: String,
    bind_types: Vec<Type>,
    binds: Vec<BindValue>,
}

struct MutationBindState {
    bind_types: Vec<Type>,
    binds: Vec<BindValue>,
    parameter_placeholders: BTreeMap<ParameterId, usize>,
    record_payload: usize,
}

#[cfg(test)]
fn lower_insert_with_context(
    context: &orna_core::revision::CatalogueHashContext,
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    lower_insert_inner(None, context, plan, arguments)
}

fn lower_insert_with_active(
    active: &ActiveDatabaseRevision,
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    lower_insert_inner(
        Some(active),
        active.catalogue_hash_context(),
        plan,
        arguments,
    )
}

fn lower_insert_inner(
    active: Option<&ActiveDatabaseRevision>,
    context: &orna_core::revision::CatalogueHashContext,
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    let mut columns = vec![String::from(OBJECT_ID_COLUMN)];
    let mut values = vec![String::from("$1")];
    let mut bind_state = MutationBindState {
        bind_types: vec![Type::BYTEA],
        binds: Vec::new(),
        parameter_placeholders: BTreeMap::new(),
        record_payload: 0,
    };
    for assignment in plan.assignments() {
        columns.push(field_name(assignment.field()));
        values.push(lower_assignment_expression(
            active,
            context,
            assignment.expression(),
            arguments,
            &mut bind_state,
        )?);
    }
    let sql = format!(
        "INSERT INTO {DATA_SCHEMA}.{} ({}) VALUES ({}) RETURNING {OBJECT_ID_COLUMN} AS c0",
        relation_name(plan.target()),
        columns.join(", "),
        values.join(", "),
    );
    if sql.len() > SQL_LIMIT {
        return Err(server_error(ServerInsertError::ComplexityLimit {
            category: "saved function complexity",
            maximum: SQL_LIMIT,
        }));
    }
    Ok(LoweredMutation {
        sql,
        bind_types: bind_state.bind_types,
        binds: bind_state.binds,
    })
}

#[cfg(test)]
fn lower_insert(
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    lower_insert_with_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        plan,
        arguments,
    )
}

fn lower_assignment_expression(
    active: Option<&ActiveDatabaseRevision>,
    context: &orna_core::revision::CatalogueHashContext,
    expression: &server_mutation_plan::MutationExpression,
    arguments: &BTreeMap<ParameterId, BindValue>,
    bind_state: &mut MutationBindState,
) -> Result<String, PostgresKernelError> {
    match expression.kind() {
        MutationExpressionKind::Parameter { parameter, .. } => {
            let value_type = assignment_postgres_type(context, expression)?;
            parameter_placeholder(*parameter, value_type, arguments, bind_state)
        }
        MutationExpressionKind::BooleanLiteral { value } => {
            let value_type = assignment_postgres_type(context, expression)?;
            bind_state.binds.push(BindValue::Boolean(*value));
            bind_state.bind_types.push(value_type);
            Ok(format!("${}", bind_state.bind_types.len()))
        }
        MutationExpressionKind::TypedNull => {
            let value_type = assignment_postgres_type(context, expression)?;
            Ok(format!("CAST(NULL AS {})", value_type.name()))
        }
        MutationExpressionKind::RecordConstructor { fields } => lower_record_constructor(
            active.ok_or_else(|| {
                plan_invariant("record constructor lowering requires an active revision")
            })?,
            expression,
            fields,
            arguments,
            bind_state,
        ),
        _ => Err(plan_invariant(
            "unknown future mutation expression kinds are unsupported",
        )),
    }
}

fn assignment_postgres_type(
    context: &orna_core::revision::CatalogueHashContext,
    expression: &server_mutation_plan::MutationExpression,
) -> Result<Type, PostgresKernelError> {
    postgres_type(resolve_runtime_type(context, expression.resolved_type())).ok_or_else(|| {
        plan_invariant("the assignment type cannot be stored by the initial runtime")
    })
}

fn lower_record_constructor(
    active: &ActiveDatabaseRevision,
    expression: &server_mutation_plan::MutationExpression,
    fields: &[server_mutation_plan::RecordFieldExpression],
    arguments: &BTreeMap<ParameterId, BindValue>,
    bind_state: &mut MutationBindState,
) -> Result<String, PostgresKernelError> {
    let record_type = expression
        .resolved_type()
        .named_type()
        .ok_or_else(|| plan_invariant("validated record constructor must have a named type"))?;
    let record_definition = active
        .catalogue()
        .record_value_type_by_id(record_type)
        .ok_or_else(|| plan_invariant("validated record constructor type must be active"))?;
    if fields.len() != record_definition.fields().len() {
        return Err(plan_invariant(
            "validated record constructor field count must remain exact",
        ));
    }
    let values = fields
        .iter()
        .zip(record_definition.fields())
        .map(|(field, declared)| {
            let value = match field.kind() {
                RecordFieldExpressionKind::Parameter { parameter, .. } => arguments
                    .get(parameter)
                    .ok_or_else(|| {
                        plan_invariant(
                            "validated record constructor parameter must have one argument",
                        )
                    })?
                    .to_runtime(),
                RecordFieldExpressionKind::BooleanLiteral { value } => {
                    RuntimeValue::Boolean(*value)
                }
                _ => {
                    return Err(plan_invariant(
                        "unknown future record constructor child kinds are unsupported",
                    ));
                }
            };
            Ok((declared.name().to_owned(), value))
        })
        .collect::<Result<Vec<_>, PostgresKernelError>>()?;
    let record = RecordValue::new(active, record_type, values)
        .map_err(ServerMutationError::RecordValue)
        .map_err(server_error)?;
    let encoded = encode_active_value(active, &RuntimeValue::Record(record))
        .map_err(ServerMutationError::ValueCodec)
        .map_err(server_error)?;
    account_record_bind_payload(&mut bind_state.record_payload, encoded.len())?;
    bind_state.binds.push(BindValue::Bytes(encoded));
    bind_state.bind_types.push(Type::BYTEA);
    Ok(format!("${}", bind_state.bind_types.len()))
}

fn account_record_bind_payload(
    total: &mut usize,
    encoded_length: usize,
) -> Result<(), PostgresKernelError> {
    let payload_length = encoded_length
        .checked_sub(ACTIVE_VALUE_ENVELOPE_LENGTH)
        .ok_or_else(|| {
            plan_invariant("canonical record bind must contain one complete ORV3 envelope")
        })?;
    let next = total
        .checked_add(payload_length)
        .ok_or_else(record_bind_payload_limit_error)?;
    if next > VARIABLE_ARGUMENT_PAYLOAD_LIMIT {
        return Err(record_bind_payload_limit_error());
    }
    *total = next;
    Ok(())
}

fn record_bind_payload_limit_error() -> PostgresKernelError {
    server_error(ServerInsertError::ComplexityLimit {
        category: "total size of canonical record payloads",
        maximum: VARIABLE_ARGUMENT_PAYLOAD_LIMIT,
    })
}

fn parameter_placeholder(
    parameter: ParameterId,
    value_type: Type,
    arguments: &BTreeMap<ParameterId, BindValue>,
    bind_state: &mut MutationBindState,
) -> Result<String, PostgresKernelError> {
    if let Some(placeholder) = bind_state.parameter_placeholders.get(&parameter).copied() {
        return Ok(format!("${placeholder}"));
    }
    let value = arguments.get(&parameter).ok_or_else(|| {
        plan_invariant("validated parameter expression must have one runtime argument")
    })?;
    bind_state.binds.push(value.clone());
    bind_state.bind_types.push(value_type);
    let placeholder = bind_state.bind_types.len();
    bind_state
        .parameter_placeholders
        .insert(parameter, placeholder);
    Ok(format!("${placeholder}"))
}

fn lower_update_with_context(
    context: &orna_core::revision::CatalogueHashContext,
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    let ServerMutationOperation::Update { selector } = plan.operation() else {
        return Err(plan_invariant("UPDATE execution requires an UPDATE plan"));
    };
    let mut assignments = Vec::with_capacity(plan.assignments().len());
    let mut bind_state = MutationBindState {
        bind_types: Vec::new(),
        binds: Vec::new(),
        parameter_placeholders: BTreeMap::new(),
        record_payload: 0,
    };
    for assignment in plan.assignments() {
        let value = lower_assignment_expression(
            None,
            context,
            assignment.expression(),
            arguments,
            &mut bind_state,
        )?;
        assignments.push(format!("{} = {value}", field_name(assignment.field())));
    }
    let selector_placeholder = parameter_placeholder(
        selector.parameter(),
        Type::BYTEA,
        arguments,
        &mut bind_state,
    )?;
    let sql = format!(
        "UPDATE {DATA_SCHEMA}.{} SET {} WHERE {OBJECT_ID_COLUMN} = {selector_placeholder} RETURNING {OBJECT_ID_COLUMN} AS c0",
        relation_name(plan.target()),
        assignments.join(", "),
    );
    if sql.len() > SQL_LIMIT {
        return Err(server_error(ServerInsertError::ComplexityLimit {
            category: "saved function complexity",
            maximum: SQL_LIMIT,
        }));
    }
    Ok(LoweredMutation {
        sql,
        bind_types: bind_state.bind_types,
        binds: bind_state.binds,
    })
}

#[cfg(test)]
fn lower_update(
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    lower_update_with_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        plan,
        arguments,
    )
}

fn lower_delete(
    plan: &ServerDeletePlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    let selector = plan.selector();
    let value = arguments.get(&selector.parameter()).ok_or_else(|| {
        plan_invariant("validated DELETE selector must have one runtime argument")
    })?;
    let sql = format!(
        "DELETE FROM {DATA_SCHEMA}.{} WHERE {OBJECT_ID_COLUMN} = $1 RETURNING {OBJECT_ID_COLUMN} AS c0",
        relation_name(plan.target()),
    );
    if sql.len() > SQL_LIMIT {
        return Err(server_error(ServerMutationError::ComplexityLimit {
            category: "saved function complexity",
            maximum: SQL_LIMIT,
        }));
    }
    Ok(LoweredMutation {
        sql,
        bind_types: vec![Type::BYTEA],
        binds: vec![value.clone()],
    })
}

fn validate_prepared_result(
    statement: &Statement,
    operation: &'static str,
) -> Result<(), PostgresKernelError> {
    let [column] = statement.columns() else {
        return Err(server_error(ServerInsertError::PreparedResult {
            rule: match operation {
                "INSERT" => "prepared INSERT must return exactly one column",
                "UPDATE" => "prepared UPDATE must return exactly one column",
                "DELETE" => "prepared DELETE must return exactly one column",
                _ => "prepared mutation must return exactly one column",
            },
        }));
    };
    if column.name() != "c0" || *column.type_() != Type::BYTEA {
        return Err(server_error(ServerInsertError::PreparedResult {
            rule: match operation {
                "INSERT" => "prepared INSERT must return one BYTEA column named c0",
                "UPDATE" => "prepared UPDATE must return one BYTEA column named c0",
                "DELETE" => "prepared DELETE must return one BYTEA column named c0",
                _ => "prepared mutation must return one BYTEA column named c0",
            },
        }));
    }
    Ok(())
}

async fn execute_insert(
    transaction: &Transaction<'_>,
    statement: &Statement,
    binds: Vec<BindValue>,
    object: ObjectId,
    unique_constraints: &UniqueConstraints,
) -> Result<(), PostgresKernelError> {
    let object_bytes = object.to_bytes().to_vec();
    let mut parameters = Vec::<&(dyn ToSql + Sync)>::with_capacity(binds.len() + 1);
    parameters.push(&object_bytes);
    parameters.extend(binds.iter().map(BindValue::as_to_sql));
    let rows = transaction
        .query(statement, &parameters)
        .await
        .map_err(|source| server_error(mutation_database_error(source, unique_constraints)))?;
    let [row] = rows.as_slice() else {
        return Err(server_error(ServerInsertError::ValueInvariant {
            rule: "INSERT must return exactly one row",
        }));
    };
    let returned = row
        .try_get::<usize, Vec<u8>>(0)
        .map_err(|source| server_error(ServerInsertError::RowDecode { source }))?;
    let returned: [u8; 16] = returned.try_into().map_err(|_| {
        server_error(ServerInsertError::ValueInvariant {
            rule: "returned object identity must contain exactly 16 bytes",
        })
    })?;
    if ObjectId::from_bytes(returned) != object {
        return Err(server_error(ServerInsertError::ValueInvariant {
            rule: "returned object identity must equal the allocated identity",
        }));
    }
    Ok(())
}

async fn execute_update(
    transaction: &Transaction<'_>,
    statement: &Statement,
    binds: Vec<BindValue>,
    selector: ObjectId,
    unique_constraints: &UniqueConstraints,
) -> Result<bool, PostgresKernelError> {
    let parameters = binds.iter().map(BindValue::as_to_sql).collect::<Vec<_>>();
    let rows = transaction
        .query(statement, &parameters)
        .await
        .map_err(|source| server_error(mutation_database_error(source, unique_constraints)))?;
    decode_selected_result(&rows, selector, "UPDATE")
}

async fn execute_delete(
    transaction: &Transaction<'_>,
    statement: &Statement,
    binds: Vec<BindValue>,
    context: ServerDeleteContext,
    target: TypeId,
    selector: ObjectId,
) -> Result<bool, PostgresKernelError> {
    let parameters = binds.iter().map(BindValue::as_to_sql).collect::<Vec<_>>();
    let rows = transaction
        .query(statement, &parameters)
        .await
        .map_err(|source| {
            if delete_commit_failure(
                source
                    .as_db_error()
                    .map(tokio_postgres::error::DbError::code),
            ) == DeleteCommitFailure::Restricted
            {
                delete_error(ServerDeleteError::DeleteRestricted {
                    context,
                    target,
                    selector,
                    source,
                })
            } else {
                server_error(ServerMutationError::Database { source })
            }
        })?;
    decode_selected_result(&rows, selector, "DELETE")
}

fn decode_selected_result(
    rows: &[Row],
    selector: ObjectId,
    operation: &'static str,
) -> Result<bool, PostgresKernelError> {
    let [row] = rows else {
        if rows.is_empty() {
            return Ok(false);
        }
        return Err(server_error(ServerInsertError::ValueInvariant {
            rule: match operation {
                "UPDATE" => "UPDATE must return at most one row",
                "DELETE" => "DELETE must return at most one row",
                _ => "identity-selected mutation must return at most one row",
            },
        }));
    };
    let returned = row
        .try_get::<usize, Vec<u8>>(0)
        .map_err(|source| server_error(ServerInsertError::RowDecode { source }))?;
    let returned: [u8; 16] = returned.try_into().map_err(|_| {
        server_error(ServerInsertError::ValueInvariant {
            rule: "returned object identity must contain exactly 16 bytes",
        })
    })?;
    if ObjectId::from_bytes(returned) != selector {
        return Err(server_error(ServerInsertError::ValueInvariant {
            rule: match operation {
                "UPDATE" => "updated object identity must equal the selected identity",
                "DELETE" => "deleted object identity must equal the selected identity",
                _ => "returned object identity must equal the selected identity",
            },
        }));
    }
    Ok(true)
}

fn server_error(error: ServerInsertError) -> PostgresKernelError {
    PostgresKernelError::ServerInsert(error)
}

fn update_error(error: ServerUpdateError) -> PostgresKernelError {
    PostgresKernelError::ServerUpdate(error)
}

fn delete_error(error: ServerDeleteError) -> PostgresKernelError {
    PostgresKernelError::ServerDelete(error)
}

fn update_unavailable(error: PostgresKernelError) -> PostgresKernelError {
    update_error(ServerUpdateError::Unavailable {
        source: Box::new(error),
    })
}

fn delete_unavailable(error: PostgresKernelError) -> PostgresKernelError {
    delete_error(ServerDeleteError::Unavailable {
        source: Box::new(error),
    })
}

fn mutation_kernel_error(
    operation: MutationExecutionKind,
    error: PostgresKernelError,
) -> PostgresKernelError {
    match operation {
        MutationExecutionKind::Insert => kernel_error(error),
        MutationExecutionKind::Update => update_unavailable(error),
        MutationExecutionKind::Delete => delete_unavailable(error),
    }
}

fn pre_transaction_mutation_error(
    operation: MutationExecutionKind,
    error: PostgresKernelError,
) -> PostgresKernelError {
    match operation {
        MutationExecutionKind::Insert => pre_transaction_error(error),
        MutationExecutionKind::Update => update_unavailable(error),
        MutationExecutionKind::Delete => delete_unavailable(error),
    }
}

fn update_not_committed(
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

fn delete_not_committed(
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

fn kernel_error(error: PostgresKernelError) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerInsert(_) => error,
        error => server_error(ServerInsertError::Kernel {
            source: Box::new(error),
        }),
    }
}

fn pre_transaction_error(error: PostgresKernelError) -> PostgresKernelError {
    match error {
        PostgresKernelError::Database(source) => {
            server_error(ServerInsertError::Database { source })
        }
        error => kernel_error(error),
    }
}

fn not_committed(context: ServerInsertContext, error: PostgresKernelError) -> PostgresKernelError {
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

fn artifact_error(function: FunctionId, rule: &'static str) -> PostgresKernelError {
    server_error(ServerInsertError::Artifact { function, rule })
}

fn plan_invariant(rule: &'static str) -> PostgresKernelError {
    server_error(ServerInsertError::PlanInvariant { rule })
}

fn argument_error(parameter: Option<ParameterId>, rule: &'static str) -> PostgresKernelError {
    server_error(ServerInsertError::Argument { parameter, rule })
}

#[cfg(test)]
#[path = "server_mutation_execution/tests.rs"]
mod tests;
