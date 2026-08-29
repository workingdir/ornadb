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

#[path = "server_mutation_execution/lowering.rs"]
mod lowering;
#[cfg(test)]
use lowering::{
    account_record_bind_payload, lower_insert, lower_insert_with_context, lower_update,
};
use lowering::{
    lower_delete, lower_insert_with_active, lower_update_with_context, validate_prepared_result,
};

#[path = "server_mutation_execution/validation.rs"]
mod validation;
use validation::{
    BindValue, MutationExecutionKind, function_signature_error, selector_argument_object,
    selector_object, validate_active_delete, validate_active_mutation,
};
#[cfg(test)]
use validation::{
    expected_body_references, expected_delete_body_references, validate_arguments,
    validate_arguments_with_context, validate_artifact_metadata,
    validate_artifact_metadata_for_operation, validate_artifact_payload_version,
    validate_delete_function_signature, validate_delete_function_signature_with_context,
    validate_delete_plan, validate_function_signature, validate_function_signature_for_context,
    validate_function_signature_for_operation, validate_plan, validate_plan_for_context,
    validate_plan_for_operation,
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
