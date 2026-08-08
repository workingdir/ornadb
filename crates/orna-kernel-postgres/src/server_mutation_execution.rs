//! Execution of the initial single-row SERVER `INSERT` subset.
//!
//! This module accepts stable identities, typed runtime arguments, and one
//! recovered canonical mutation artifact. It does not resolve semantic names,
//! accept source SQL, or expose PostgreSQL details through its public seam.

use std::{collections::BTreeMap, error::Error, fmt};

use orna_artifact::server_mutation_plan::{self, MutationExpressionKind, ServerMutationPlan};
use orna_core::{
    FunctionId, FunctionRevisionId, ObjectId, ParameterId, TypeId,
    catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionTransaction, FunctionVolatility, ObjectTypeDefinition,
    },
    revision::{
        ActiveDatabaseRevision, DefinitionReferenceKind, DefinitionReferenceTarget,
        ExecutableArtifactKind, RevisionPair,
    },
    types::ResolvedType,
    value::{FunctionArgument, ResultColumn, ResultRow, ResultRows, ResultRowsError, RuntimeValue},
};
use tokio_postgres::{
    Client, IsolationLevel, Statement, Transaction,
    types::{ToSql, Type},
};

use crate::{
    PostgresKernel, PostgresKernelError,
    server_runtime::{
        ExpectedDefinitionReference, ReferenceReplayMismatch, configure_and_recover, postgres_type,
        validate_function_reference_replay,
    },
    storage::{DATA_SCHEMA, OBJECT_ID_COLUMN, field_name, relation_name},
};

const VARIABLE_ARGUMENT_PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;
const SQL_LIMIT: usize = 1024 * 1024;

#[cfg(feature = "test-hooks")]
struct InsertTestBarrier {
    reached: std::sync::Arc<tokio::sync::Barrier>,
    resume: std::sync::Arc<tokio::sync::Barrier>,
}

#[cfg(not(feature = "test-hooks"))]
struct InsertTestBarrier;

/// Immutable active state pinned for one SERVER `INSERT` execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerInsertContext {
    pair: RevisionPair,
    function: FunctionId,
    function_revision: FunctionRevisionId,
}

impl ServerInsertContext {
    const fn new(
        pair: RevisionPair,
        function: FunctionId,
        function_revision: FunctionRevisionId,
    ) -> Self {
        Self {
            pair,
            function,
            function_revision,
        }
    }

    /// Returns the source and catalogue revision pair.
    pub const fn pair(&self) -> RevisionPair {
        self.pair
    }

    /// Returns the stable function identity.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the immutable active function revision identity.
    pub const fn function_revision(&self) -> FunctionRevisionId {
        self.function_revision
    }
}

/// The committed result of one validated single-row SERVER `INSERT`.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerInsertResult {
    context: ServerInsertContext,
    target: TypeId,
    object: ObjectId,
    rows: ResultRows,
}

impl ServerInsertResult {
    fn new(
        context: ServerInsertContext,
        target: TypeId,
        object: ObjectId,
        column: ResultColumn,
    ) -> Result<Self, ResultRowsError> {
        let rows = ResultRows::new(
            [column],
            [ResultRow::new([RuntimeValue::Reference { target, object }])],
        )?;
        Ok(Self {
            context,
            target,
            object,
            rows,
        })
    }

    /// Returns the complete active execution context.
    pub const fn context(&self) -> ServerInsertContext {
        self.context
    }

    /// Returns the source and catalogue revision pair.
    pub const fn pair(&self) -> RevisionPair {
        self.context.pair()
    }

    /// Returns the executed function identity.
    pub const fn function(&self) -> FunctionId {
        self.context.function()
    }

    /// Returns the immutable function revision that supplied the plan.
    pub const fn function_revision(&self) -> FunctionRevisionId {
        self.context.function_revision()
    }

    /// Returns the inserted object type identity.
    pub const fn target(&self) -> TypeId {
        self.target
    }

    /// Returns the allocated durable object identity.
    pub const fn object(&self) -> ObjectId {
        self.object
    }

    /// Returns the declared one-column, one-row typed reference result.
    pub const fn rows(&self) -> &ResultRows {
        &self.rows
    }
}

/// The confirmed commit state attached to a SERVER `INSERT` error.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerInsertCommitState {
    /// PostgreSQL did not commit the insert.
    NotCommitted,
    /// The connection failed before PostgreSQL confirmed the commit outcome.
    Unknown,
    /// PostgreSQL confirmed the commit before a later shutdown failure.
    Committed,
}

/// A typed failure from the initial single-row SERVER `INSERT` subset.
#[non_exhaustive]
#[derive(Debug)]
pub enum ServerInsertError {
    /// The requested function is not in the recovered active catalogue.
    FunctionNotActive {
        /// The recovered active revision pair.
        pair: RevisionPair,
        /// The requested stable function identity.
        function: FunctionId,
    },
    /// A failure after an active function revision was pinned and rolled back.
    NotCommitted {
        /// The immutable active execution context.
        context: ServerInsertContext,
        /// The underlying typed validation or execution failure.
        source: Box<ServerInsertError>,
    },
    /// A kernel failure occurred before the insert could commit.
    Kernel {
        /// The kernel failure with its native source chain.
        source: Box<PostgresKernelError>,
    },
    /// PostgreSQL failed before the commit attempt.
    Database {
        /// The PostgreSQL failure.
        source: tokio_postgres::Error,
    },
    /// The function declaration is outside the accepted INSERT subset.
    FunctionSignature {
        /// The stable function identity.
        function: FunctionId,
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// The active function has no exact active immutable revision record.
    CurrentRevision {
        /// The stable function identity.
        function: FunctionId,
        /// The required immutable revision identity.
        revision: FunctionRevisionId,
    },
    /// The current revision does not contain the accepted mutation artifact.
    Artifact {
        /// The stable function identity.
        function: FunctionId,
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// The canonical mutation plan cannot decode.
    PlanDecode(server_mutation_plan::ServerMutationPlanError),
    /// The mutation plan disagrees with the recovered active catalogue.
    PlanInvariant {
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// Durable definition references do not prove the mutation body.
    ReferenceEvidence {
        /// The stable function identity.
        function: FunctionId,
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// Supplied runtime arguments do not equal the active signature.
    Argument {
        /// The related parameter identity, when one is available.
        parameter: Option<ParameterId>,
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// A fixed execution or lowering limit was exceeded.
    ComplexityLimit {
        /// The bounded category.
        category: &'static str,
        /// The largest accepted value.
        maximum: usize,
    },
    /// PostgreSQL did not prepare the exact generated return shape.
    PreparedResult {
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// PostgreSQL returned a value that could not be decoded.
    RowDecode {
        /// The PostgreSQL conversion failure.
        source: tokio_postgres::Error,
    },
    /// PostgreSQL returned a value that violates the generated result contract.
    ValueInvariant {
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// The declared one-row typed reference result could not be built.
    ResultRows(ResultRowsError),
    /// PostgreSQL rejected COMMIT and confirmed that the transaction did not commit.
    CommitRejected {
        /// The immutable active execution context.
        context: ServerInsertContext,
        /// The insert target identity.
        target: TypeId,
        /// The candidate object identity that did not commit.
        candidate: ObjectId,
        /// The PostgreSQL commit rejection.
        source: tokio_postgres::Error,
    },
    /// The connection failed while the commit outcome was unknown.
    CommitOutcomeUnknown {
        /// The immutable active execution context.
        context: ServerInsertContext,
        /// The insert target identity.
        target: TypeId,
        /// The candidate object identity whose commit outcome is unknown.
        candidate: ObjectId,
        /// The driver or transport failure.
        source: tokio_postgres::Error,
    },
    /// COMMIT succeeded, but the connection driver then failed to shut down.
    CommittedButShutdownFailed {
        /// The complete confirmed committed result.
        result: Box<ServerInsertResult>,
        /// The connection shutdown failure.
        source: Box<PostgresKernelError>,
    },
}

impl ServerInsertError {
    /// Returns the commit state that callers must use for retry decisions.
    pub const fn commit_state(&self) -> ServerInsertCommitState {
        match self {
            Self::CommitOutcomeUnknown { .. } => ServerInsertCommitState::Unknown,
            Self::CommittedButShutdownFailed { .. } => ServerInsertCommitState::Committed,
            Self::FunctionNotActive { .. }
            | Self::NotCommitted { .. }
            | Self::Kernel { .. }
            | Self::Database { .. }
            | Self::FunctionSignature { .. }
            | Self::CurrentRevision { .. }
            | Self::Artifact { .. }
            | Self::PlanDecode(_)
            | Self::PlanInvariant { .. }
            | Self::ReferenceEvidence { .. }
            | Self::Argument { .. }
            | Self::ComplexityLimit { .. }
            | Self::PreparedResult { .. }
            | Self::RowDecode { .. }
            | Self::ValueInvariant { .. }
            | Self::ResultRows(_)
            | Self::CommitRejected { .. } => ServerInsertCommitState::NotCommitted,
        }
    }
}

impl fmt::Display for ServerInsertError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FunctionNotActive { .. } => {
                formatter.write_str("the requested function is not active; no row was added")
            }
            Self::NotCommitted { source, .. } => {
                write!(formatter, "the row was not added: {source}")
            }
            Self::Kernel { .. } => {
                formatter.write_str("the database could not check the active function")
            }
            Self::Database { .. } => {
                formatter.write_str("the database operation failed before the row was added")
            }
            Self::FunctionSignature { rule, .. } => {
                write!(formatter, "the function cannot add this row: {rule}")
            }
            Self::CurrentRevision { .. } => {
                formatter.write_str("the active function definition is incomplete")
            }
            Self::Artifact { .. } => formatter.write_str(
                "the saved function is unsupported; redeploy it or contact the database administrator",
            ),
            Self::PlanDecode(_) => formatter.write_str(
                "the saved function cannot be read; redeploy it or contact the database administrator",
            ),
            Self::PlanInvariant { .. } | Self::ReferenceEvidence { .. } => formatter.write_str(
                "the saved function is inconsistent with the active database; redeploy it or contact the database administrator",
            ),
            Self::Argument { rule, .. } => {
                write!(formatter, "a supplied function argument is invalid: {rule}")
            }
            Self::ComplexityLimit { category, maximum } => {
                write!(
                    formatter,
                    "the request is too large: {category} limit is {maximum}"
                )
            }
            Self::PreparedResult { .. } => formatter.write_str(
                "the database prepared an unexpected result; redeploy the function or contact the database administrator",
            ),
            Self::RowDecode { .. } => {
                formatter.write_str("the database returned an unreadable object identity")
            }
            Self::ValueInvariant { .. } => formatter.write_str(
                "the database returned an unexpected object identity; contact the database administrator",
            ),
            Self::ResultRows(_) => formatter.write_str("the function result is invalid"),
            Self::CommitRejected { candidate, .. } => write!(
                formatter,
                "the database rejected the final save for object {}; no row was added",
                candidate.canonical(),
            ),
            Self::CommitOutcomeUnknown { candidate, .. } => write!(
                formatter,
                "the connection failed while saving object {}; it is not known whether the row was added; do not retry automatically",
                candidate.canonical(),
            ),
            Self::CommittedButShutdownFailed { result, .. } => write!(
                formatter,
                "object {} was added, but the database connection did not close cleanly",
                result.object().canonical(),
            ),
        }
    }
}

impl Error for ServerInsertError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NotCommitted { source, .. } => Some(source),
            Self::Kernel { source } => Some(source),
            Self::Database { source }
            | Self::RowDecode { source }
            | Self::CommitRejected { source, .. }
            | Self::CommitOutcomeUnknown { source, .. } => Some(source),
            Self::PlanDecode(error) => Some(error),
            Self::ResultRows(error) => Some(error),
            Self::CommittedButShutdownFailed { source, .. } => Some(source),
            Self::FunctionNotActive { .. }
            | Self::FunctionSignature { .. }
            | Self::CurrentRevision { .. }
            | Self::Artifact { .. }
            | Self::PlanInvariant { .. }
            | Self::ReferenceEvidence { .. }
            | Self::Argument { .. }
            | Self::ComplexityLimit { .. }
            | Self::PreparedResult { .. }
            | Self::ValueInvariant { .. } => None,
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
            Some(InsertTestBarrier { reached, resume }),
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
        barrier: Option<InsertTestBarrier>,
        force_post_commit_driver_shutdown: bool,
    ) -> Result<ServerInsertResult, PostgresKernelError> {
        let mut session = self.open().await.map_err(pre_transaction_error)?;
        let execution =
            execute_client(&mut session.client, function, arguments, barrier.as_ref()).await;
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
            (Ok(result), Err(source)) => Err(server_error(
                ServerInsertError::CommittedButShutdownFailed {
                    result: Box::new(result),
                    source: Box::new(source),
                },
            )),
        }
    }
}

async fn execute_client(
    client: &mut Client,
    function: FunctionId,
    arguments: &[FunctionArgument],
    barrier: Option<&InsertTestBarrier>,
) -> Result<ServerInsertResult, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(false)
        .start()
        .await
        .map_err(|source| server_error(ServerInsertError::Database { source }))?;
    match execute_transaction(&transaction, function, arguments, barrier).await {
        Ok(candidate) => commit_candidate(transaction, candidate).await,
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

async fn commit_candidate(
    transaction: Transaction<'_>,
    candidate: ServerInsertResult,
) -> Result<ServerInsertResult, PostgresKernelError> {
    let context = candidate.context();
    let target = candidate.target();
    let object = candidate.object();
    match transaction.commit().await {
        Ok(()) => Ok(candidate),
        Err(source) if source.as_db_error().is_some() => {
            Err(server_error(ServerInsertError::CommitRejected {
                context,
                target,
                candidate: object,
                source,
            }))
        }
        Err(source) => Err(server_error(ServerInsertError::CommitOutcomeUnknown {
            context,
            target,
            candidate: object,
            source,
        })),
    }
}

async fn execute_transaction(
    transaction: &Transaction<'_>,
    function_id: FunctionId,
    arguments: &[FunctionArgument],
    barrier: Option<&InsertTestBarrier>,
) -> Result<ServerInsertResult, PostgresKernelError> {
    let active = configure_and_recover(transaction)
        .await
        .map_err(kernel_error)?;
    let function = active
        .catalogue()
        .function_by_id(function_id)
        .ok_or_else(|| {
            server_error(ServerInsertError::FunctionNotActive {
                pair: active.pair(),
                function: function_id,
            })
        })?;
    let context = ServerInsertContext::new(active.pair(), function_id, function.current_revision());
    pause_after_recovery(barrier).await;
    execute_active_transaction(transaction, &active, function, context, arguments)
        .await
        .map_err(|error| not_committed(context, error))
}

#[cfg(feature = "test-hooks")]
async fn pause_after_recovery(barrier: Option<&InsertTestBarrier>) {
    if let Some(barrier) = barrier {
        barrier.reached.wait().await;
        barrier.resume.wait().await;
    }
}

#[cfg(not(feature = "test-hooks"))]
async fn pause_after_recovery(_barrier: Option<&InsertTestBarrier>) {}

async fn execute_active_transaction(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    context: ServerInsertContext,
    arguments: &[FunctionArgument],
) -> Result<ServerInsertResult, PostgresKernelError> {
    let returned = validate_function_signature(active.catalogue(), function)?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == function.id() && revision.id() == function.current_revision()
        })
        .ok_or_else(|| {
            server_error(ServerInsertError::CurrentRevision {
                function: function.id(),
                revision: function.current_revision(),
            })
        })?;
    let artifact = revision.artifact();
    validate_artifact_metadata(
        function.id(),
        artifact.kind(),
        artifact.format(),
        artifact.version(),
        revision.language_version(),
    )?;
    let plan = ServerMutationPlan::decode(artifact.payload())
        .map_err(ServerInsertError::PlanDecode)
        .map_err(server_error)?;
    let target = validate_plan(active.catalogue(), function, returned.target, &plan)?;
    validate_reference_evidence(active, function, &plan)?;
    let arguments = validate_arguments(active.catalogue(), function, arguments)?;
    let lowered = lower_insert(&plan, &arguments)?;
    let statement = transaction
        .prepare_typed(&lowered.sql, &lowered.bind_types)
        .await
        .map_err(|source| server_error(ServerInsertError::Database { source }))?;
    validate_prepared_result(&statement)?;

    // Object allocation is deliberately after every semantic, durable,
    // argument, lowering, and prepared-result validation above.
    let object = ObjectId::new();
    let result = ServerInsertResult::new(context, target.id(), object, returned.column)
        .map_err(ServerInsertError::ResultRows)
        .map_err(server_error)?;
    execute_insert(transaction, &statement, lowered.binds, object).await?;
    Ok(result)
}

#[derive(Debug)]
struct ValidatedReturn {
    target: TypeId,
    column: ResultColumn,
}

fn validate_artifact_metadata(
    function: FunctionId,
    kind: ExecutableArtifactKind,
    format: &str,
    version: u32,
    language_version: &str,
) -> Result<(), PostgresKernelError> {
    if kind != ExecutableArtifactKind::Server {
        return Err(artifact_error(
            function,
            "the active function must contain SERVER executable data",
        ));
    }
    if format != server_mutation_plan::FORMAT_IDENTITY
        || version != server_mutation_plan::FORMAT_VERSION
    {
        return Err(artifact_error(
            function,
            "the active function must use the supported single-row mutation format version 1",
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

fn validate_function_signature(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
) -> Result<ValidatedReturn, PostgresKernelError> {
    let reject = |rule| {
        server_error(ServerInsertError::FunctionSignature {
            function: function.id(),
            rule,
        })
    };
    if function.domain() != FunctionDomain::Server {
        return Err(reject("this operation requires an INSERT SERVER function"));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(reject(
            "an INSERT SERVER function must use SECURITY INVOKER",
        ));
    }
    if function.transaction() != Some(FunctionTransaction::Atomic) {
        return Err(reject(
            "an INSERT SERVER function must use exactly TRANSACTION ATOMIC",
        ));
    }
    if function.volatility() != FunctionVolatility::Volatile {
        return Err(reject(
            "an INSERT SERVER function must use VOLATILITY VOLATILE",
        ));
    }
    for parameter in function.parameters() {
        if parameter.default_expression().is_some() {
            return Err(reject(
                "INSERT SERVER function parameters cannot have default expressions",
            ));
        }
        if !runtime_type_is_active(catalogue, parameter.resolved_type()) {
            return Err(reject(
                "every INSERT SERVER function parameter must use a supported active type",
            ));
        }
    }
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(reject(
            "an INSERT SERVER function must return exactly one object-reference column",
        ));
    };
    let [column] = columns.as_slice() else {
        return Err(reject(
            "an INSERT SERVER function must return exactly one object-reference column",
        ));
    };
    let ResolvedType::Reference { target } = column.resolved_type() else {
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

fn runtime_type_is_active(catalogue: &CatalogueSnapshot, resolved_type: ResolvedType) -> bool {
    postgres_type(resolved_type).is_some()
        && !matches!(
            resolved_type,
            ResolvedType::Reference { target } if catalogue.object_type_by_id(target).is_none()
        )
}

fn validate_active_runtime_type(
    catalogue: &CatalogueSnapshot,
    resolved_type: ResolvedType,
    rule: &'static str,
) -> Result<(), PostgresKernelError> {
    if postgres_type(resolved_type).is_none() {
        return Err(plan_invariant(rule));
    }
    if let ResolvedType::Reference { target } = resolved_type
        && catalogue.object_type_by_id(target).is_none()
    {
        return Err(plan_invariant(
            "every referenced object type must be active",
        ));
    }
    Ok(())
}

fn validate_plan<'a>(
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
) -> Result<&'a ObjectTypeDefinition, PostgresKernelError> {
    if plan.returned_object() != plan.target() || plan.target() != returned_target {
        return Err(plan_invariant(
            "plan target, returned object, and declared result REF target must match",
        ));
    }
    let target = catalogue
        .object_type_by_id(plan.target())
        .ok_or_else(|| plan_invariant("mutation target must be an active object type"))?;
    for field in target.fields() {
        if field.unique() {
            return Err(plan_invariant(
                "mutation targets cannot contain UNIQUE fields",
            ));
        }
        if field.default_expression().is_some() {
            return Err(plan_invariant(
                "mutation targets cannot contain field default expressions",
            ));
        }
        if let ResolvedType::Reference { target } = field.resolved_type()
            && catalogue.object_type_by_id(target).is_none()
        {
            return Err(plan_invariant(
                "every target-field REF type must name an active object type",
            ));
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
        validate_active_runtime_type(
            catalogue,
            expression.resolved_type(),
            "every assignment expression must use the active runtime subset",
        )?;
        if expression.resolved_type() != field.resolved_type() {
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
                || parameter.resolved_type() != expression.resolved_type()
            {
                return Err(plan_invariant(
                    "parameter expression must exactly match a required active parameter",
                ));
            }
        }
    }
    if target
        .fields()
        .iter()
        .any(|field| !field.nullable() && !assigned.contains_key(&field.id()))
    {
        return Err(plan_invariant(
            "every non-null target field must have an assignment",
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

fn expected_body_references(plan: &ServerMutationPlan) -> Vec<ExpectedDefinitionReference> {
    let mut expected = Vec::with_capacity(plan.assignments().len().saturating_mul(2) + 2);
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
        if let MutationExpressionKind::Parameter { owner, parameter } =
            assignment.expression().kind()
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
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::ObjectReference,
        DefinitionReferenceTarget::ObjectType(plan.returned_object()),
    ));
    expected
}

fn validate_arguments(
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
        if !runtime_type_is_active(catalogue, value.resolved_type()) {
            return Err(argument_error(
                Some(parameter_id),
                "the argument type is unsupported or its referenced object type is inactive",
            ));
        }
        if value.resolved_type() != parameter.resolved_type() {
            return Err(argument_error(
                Some(parameter_id),
                "the argument type does not match the declared parameter type",
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

fn variable_payload_len(value: &RuntimeValue) -> Result<usize, PostgresKernelError> {
    match value {
        RuntimeValue::Text(value) => Ok(value.len()),
        RuntimeValue::Bytes(value) => Ok(value.len()),
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
        }
    }
}

struct LoweredInsert {
    sql: String,
    bind_types: Vec<Type>,
    binds: Vec<BindValue>,
}

fn lower_insert(
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredInsert, PostgresKernelError> {
    let mut columns = vec![String::from(OBJECT_ID_COLUMN)];
    let mut values = vec![String::from("$1")];
    let mut bind_types = vec![Type::BYTEA];
    let mut binds = Vec::new();
    let mut parameter_placeholders = BTreeMap::new();
    for assignment in plan.assignments() {
        columns.push(field_name(assignment.field()));
        let expression = assignment.expression();
        let postgres_type = postgres_type(expression.resolved_type()).ok_or_else(|| {
            plan_invariant("the assignment type cannot be stored by the initial runtime")
        })?;
        match expression.kind() {
            MutationExpressionKind::Parameter { parameter, .. } => {
                let placeholder =
                    if let Some(placeholder) = parameter_placeholders.get(parameter).copied() {
                        placeholder
                    } else {
                        let value = arguments.get(parameter).ok_or_else(|| {
                            plan_invariant(
                                "validated parameter expression must have one runtime argument",
                            )
                        })?;
                        binds.push(value.clone());
                        bind_types.push(postgres_type);
                        let placeholder = bind_types.len();
                        parameter_placeholders.insert(*parameter, placeholder);
                        placeholder
                    };
                values.push(format!("${placeholder}"));
            }
            MutationExpressionKind::BooleanLiteral { value } => {
                binds.push(BindValue::Boolean(*value));
                bind_types.push(postgres_type);
                values.push(format!("${}", binds.len() + 1));
            }
            MutationExpressionKind::TypedNull => {
                values.push(format!("CAST(NULL AS {})", postgres_type.name()));
            }
            _ => {
                return Err(plan_invariant(
                    "unknown future mutation expression kinds are unsupported",
                ));
            }
        }
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
    Ok(LoweredInsert {
        sql,
        bind_types,
        binds,
    })
}

fn validate_prepared_result(statement: &Statement) -> Result<(), PostgresKernelError> {
    let [column] = statement.columns() else {
        return Err(server_error(ServerInsertError::PreparedResult {
            rule: "prepared INSERT must return exactly one column",
        }));
    };
    if column.name() != "c0" || *column.type_() != Type::BYTEA {
        return Err(server_error(ServerInsertError::PreparedResult {
            rule: "prepared INSERT must return one BYTEA column named c0",
        }));
    }
    Ok(())
}

async fn execute_insert(
    transaction: &Transaction<'_>,
    statement: &Statement,
    binds: Vec<BindValue>,
    object: ObjectId,
) -> Result<(), PostgresKernelError> {
    let object_bytes = object.to_bytes().to_vec();
    let mut parameters = Vec::<&(dyn ToSql + Sync)>::with_capacity(binds.len() + 1);
    parameters.push(&object_bytes);
    parameters.extend(binds.iter().map(BindValue::as_to_sql));
    let rows = transaction
        .query(statement, &parameters)
        .await
        .map_err(|source| server_error(ServerInsertError::Database { source }))?;
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

fn server_error(error: ServerInsertError) -> PostgresKernelError {
    PostgresKernelError::ServerInsert(error)
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
mod tests {
    use orna_artifact::server_mutation_plan::{FieldAssignment, MutationExpression};
    use orna_core::{
        CatalogueRevisionId, ExpressionId, FieldId, SchemaId, SourceRevisionId,
        catalogue::{
            FieldDefinition, FunctionReturnColumnDefinition, ParameterDefinition,
            QualifiedSemanticName, SchemaDefinition,
        },
        types::StandardScalar,
        value::RuntimeFloat,
    };

    use super::*;

    const TARGET: TypeId = TypeId::from_bytes([0x10; 16]);
    const OTHER: TypeId = TypeId::from_bytes([0x20; 16]);
    const MISSING: TypeId = TypeId::from_bytes([0x21; 16]);
    const FUNCTION: FunctionId = FunctionId::from_bytes([0x30; 16]);
    const OTHER_FUNCTION: FunctionId = FunctionId::from_bytes([0x31; 16]);
    const REVISION: FunctionRevisionId = FunctionRevisionId::from_bytes([0x32; 16]);
    const FIELD_TITLE: FieldId = FieldId::from_bytes([0x41; 16]);
    const FIELD_ENABLED: FieldId = FieldId::from_bytes([0x42; 16]);
    const FIELD_COUNT: FieldId = FieldId::from_bytes([0x43; 16]);
    const FIELD_OWNER: FieldId = FieldId::from_bytes([0x44; 16]);
    const FIELD_NOTE: FieldId = FieldId::from_bytes([0x45; 16]);
    const PARAMETER_TITLE: ParameterId = ParameterId::from_bytes([0x51; 16]);
    const PARAMETER_OWNER: ParameterId = ParameterId::from_bytes([0x52; 16]);
    const OBJECT: ObjectId = ObjectId::from_bytes([0x61; 16]);

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }

    fn field(
        id: FieldId,
        semantic_name: &str,
        ordinal: u32,
        resolved_type: ResolvedType,
        nullable: bool,
    ) -> FieldDefinition {
        FieldDefinition::new(
            id,
            semantic_name,
            ordinal,
            resolved_type,
            nullable,
            false,
            None,
            None,
        )
    }

    fn target_fields(reference_target: TypeId) -> Vec<FieldDefinition> {
        vec![
            field(
                FIELD_TITLE,
                "semantic_title",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                false,
            ),
            field(
                FIELD_ENABLED,
                "semantic_enabled",
                1,
                ResolvedType::scalar(StandardScalar::Boolean),
                false,
            ),
            field(
                FIELD_COUNT,
                "semantic_count",
                2,
                ResolvedType::scalar(StandardScalar::Integer),
                true,
            ),
            field(
                FIELD_OWNER,
                "semantic_owner",
                3,
                ResolvedType::reference(reference_target),
                true,
            ),
            field(
                FIELD_NOTE,
                "semantic_note",
                4,
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                true,
            ),
        ]
    }

    fn object_types(
        fields: Vec<FieldDefinition>,
        include_other: bool,
    ) -> Vec<ObjectTypeDefinition> {
        let mut objects = vec![ObjectTypeDefinition::new(
            TARGET,
            name(&["test", "semantic_target"]),
            fields,
        )];
        if include_other {
            objects.push(ObjectTypeDefinition::new(
                OTHER,
                name(&["test", "semantic_other"]),
                Vec::new(),
            ));
        }
        objects
    }

    fn catalogue(
        fields: Vec<FieldDefinition>,
        include_other: bool,
        functions: Vec<FunctionDefinition>,
    ) -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([0x01; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x02; 16]),
                name(&["test"]),
            )],
            object_types(fields, include_other),
            functions,
        )
        .unwrap()
    }

    fn parameters(reference_target: TypeId) -> Vec<ParameterDefinition> {
        vec![
            ParameterDefinition::new(
                PARAMETER_TITLE,
                "semantic_title_parameter",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                None,
            ),
            ParameterDefinition::new(
                PARAMETER_OWNER,
                "semantic_owner_parameter",
                1,
                ResolvedType::reference(reference_target),
                None,
            ),
        ]
    }

    fn function(
        domain: FunctionDomain,
        parameters: Vec<ParameterDefinition>,
        return_type: FunctionReturn,
        security: FunctionSecurity,
        transaction: Option<FunctionTransaction>,
        volatility: FunctionVolatility,
    ) -> FunctionDefinition {
        FunctionDefinition::new(
            FUNCTION,
            name(&["test", "semantic_insert"]),
            domain,
            parameters,
            return_type,
            REVISION,
            security,
            transaction,
            volatility,
        )
    }

    fn rows_reference(target: TypeId) -> FunctionReturn {
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "semantic_created",
            0,
            ResolvedType::reference(target),
        )])
    }

    fn valid_function() -> FunctionDefinition {
        function(
            FunctionDomain::Server,
            parameters(OTHER),
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        )
    }

    fn valid_plan() -> ServerMutationPlan {
        ServerMutationPlan::new_insert(
            TARGET,
            [
                FieldAssignment::new(
                    TARGET,
                    FIELD_TITLE,
                    MutationExpression::parameter(
                        FUNCTION,
                        PARAMETER_TITLE,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    )
                    .unwrap(),
                ),
                FieldAssignment::new(
                    TARGET,
                    FIELD_ENABLED,
                    MutationExpression::boolean_literal(true),
                ),
                FieldAssignment::new(
                    TARGET,
                    FIELD_COUNT,
                    MutationExpression::typed_null(ResolvedType::scalar(StandardScalar::Integer))
                        .unwrap(),
                ),
                FieldAssignment::new(
                    TARGET,
                    FIELD_OWNER,
                    MutationExpression::parameter(
                        FUNCTION,
                        PARAMETER_OWNER,
                        ResolvedType::reference(OTHER),
                    )
                    .unwrap(),
                ),
            ],
            TARGET,
        )
        .unwrap()
    }

    fn valid_arguments() -> Vec<FunctionArgument> {
        vec![
            FunctionArgument::new(
                PARAMETER_OWNER,
                RuntimeValue::Reference {
                    target: OTHER,
                    object: OBJECT,
                },
            )
            .unwrap(),
            FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Text(String::from("title")))
                .unwrap(),
        ]
    }

    fn expect_insert_error(error: PostgresKernelError) -> ServerInsertError {
        let PostgresKernelError::ServerInsert(error) = error else {
            panic!("expected typed SERVER INSERT error");
        };
        error
    }

    #[test]
    fn context_and_result_expose_only_stable_execution_facts() {
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x71; 16]),
            CatalogueRevisionId::from_bytes([0x72; 16]),
        );
        let context = ServerInsertContext::new(pair, FUNCTION, REVISION);
        let result = ServerInsertResult::new(
            context,
            TARGET,
            OBJECT,
            ResultColumn::new("semantic_created", ResolvedType::reference(TARGET), false).unwrap(),
        )
        .unwrap();

        assert_eq!(context.pair(), pair);
        assert_eq!(context.function(), FUNCTION);
        assert_eq!(context.function_revision(), REVISION);
        assert_eq!(result.context(), context);
        assert_eq!(result.pair(), pair);
        assert_eq!(result.function(), FUNCTION);
        assert_eq!(result.function_revision(), REVISION);
        assert_eq!(result.target(), TARGET);
        assert_eq!(result.object(), OBJECT);
        assert_eq!(result.rows().columns().len(), 1);
        assert_eq!(result.rows().columns()[0].name(), "semantic_created");
        assert_eq!(
            result.rows().columns()[0].resolved_type(),
            ResolvedType::reference(TARGET),
        );
        assert!(!result.rows().columns()[0].nullable());
        assert_eq!(
            result.rows().rows()[0].values(),
            &[RuntimeValue::Reference {
                target: TARGET,
                object: OBJECT,
            }],
        );
    }

    #[test]
    fn signature_accepts_only_server_invoker_atomic_volatile_rows_ref() {
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let returned = validate_function_signature(&catalogue, &valid_function()).unwrap();
        assert_eq!(returned.target, TARGET);
        assert_eq!(returned.column.name(), "semantic_created");
        assert_eq!(
            returned.column.resolved_type(),
            ResolvedType::reference(TARGET),
        );
        assert!(!returned.column.nullable());

        let invalid = [
            function(
                FunctionDomain::Client,
                parameters(OTHER),
                rows_reference(TARGET),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Volatile,
            ),
            function(
                FunctionDomain::Server,
                parameters(OTHER),
                rows_reference(TARGET),
                FunctionSecurity::Definer,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            ),
            function(
                FunctionDomain::Server,
                parameters(OTHER),
                rows_reference(TARGET),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Volatile,
            ),
            function(
                FunctionDomain::Server,
                parameters(OTHER),
                rows_reference(TARGET),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Stable,
            ),
            function(
                FunctionDomain::Server,
                parameters(OTHER),
                FunctionReturn::Single(ResolvedType::reference(TARGET)),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            ),
            function(
                FunctionDomain::Server,
                parameters(OTHER),
                FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                    "value",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                )]),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            ),
        ];
        for function in invalid {
            assert!(matches!(
                expect_insert_error(
                    validate_function_signature(&catalogue, &function).unwrap_err()
                ),
                ServerInsertError::FunctionSignature { .. },
            ));
        }
    }

    #[test]
    fn signature_rejects_defaults_unsupported_types_and_inactive_references() {
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let cases = [
            vec![ParameterDefinition::new(
                PARAMETER_TITLE,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                Some(ExpressionId::from_bytes([0x73; 16])),
            )],
            vec![ParameterDefinition::new(
                PARAMETER_TITLE,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Date),
                None,
            )],
            vec![ParameterDefinition::new(
                PARAMETER_TITLE,
                "value",
                0,
                ResolvedType::reference(MISSING),
                None,
            )],
        ];
        for parameters in cases {
            let candidate = function(
                FunctionDomain::Server,
                parameters,
                rows_reference(TARGET),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            );
            assert!(matches!(
                expect_insert_error(
                    validate_function_signature(&catalogue, &candidate).unwrap_err()
                ),
                ServerInsertError::FunctionSignature { .. },
            ));
        }

        let missing_result = function(
            FunctionDomain::Server,
            Vec::new(),
            rows_reference(MISSING),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        assert!(validate_function_signature(&catalogue, &missing_result).is_err());
    }

    #[test]
    fn artifact_metadata_accepts_only_server_mutation_v1_and_language_v1() {
        assert!(
            validate_artifact_metadata(
                FUNCTION,
                ExecutableArtifactKind::Server,
                server_mutation_plan::FORMAT_IDENTITY,
                server_mutation_plan::FORMAT_VERSION,
                server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
            )
            .is_ok()
        );
        for (kind, format, version, language) in [
            (
                ExecutableArtifactKind::Client,
                server_mutation_plan::FORMAT_IDENTITY,
                1,
                server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
            ),
            (
                ExecutableArtifactKind::Server,
                "orna.server-plan",
                1,
                server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
            ),
            (
                ExecutableArtifactKind::Server,
                server_mutation_plan::FORMAT_IDENTITY,
                2,
                server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
            ),
            (
                ExecutableArtifactKind::Server,
                server_mutation_plan::FORMAT_IDENTITY,
                1,
                "orna.language/2",
            ),
        ] {
            assert!(matches!(
                expect_insert_error(
                    validate_artifact_metadata(FUNCTION, kind, format, version, language)
                        .unwrap_err()
                ),
                ServerInsertError::Artifact { .. },
            ));
        }
        assert!(matches!(
            ServerMutationPlan::decode(b"not a mutation plan"),
            Err(server_mutation_plan::ServerMutationPlanError::InvalidMagic),
        ));
    }

    #[test]
    fn plan_matches_the_active_catalogue_and_allows_omitted_nullable_fields() {
        let function = valid_function();
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let target = validate_plan(&catalogue, &function, TARGET, &valid_plan()).unwrap();

        assert_eq!(target.id(), TARGET);
        assert!(
            valid_plan()
                .assignments()
                .iter()
                .all(|assignment| assignment.field() != FIELD_NOTE)
        );
    }

    #[test]
    fn plan_rejects_unknown_fields_type_mismatches_nullability_and_omissions() {
        let function = valid_function();
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let cases = [
            ServerMutationPlan::new_insert(
                TARGET,
                [FieldAssignment::new(
                    TARGET,
                    FieldId::from_bytes([0x7a; 16]),
                    MutationExpression::boolean_literal(true),
                )],
                TARGET,
            )
            .unwrap(),
            ServerMutationPlan::new_insert(
                TARGET,
                [FieldAssignment::new(
                    TARGET,
                    FIELD_TITLE,
                    MutationExpression::boolean_literal(true),
                )],
                TARGET,
            )
            .unwrap(),
            ServerMutationPlan::new_insert(
                TARGET,
                [FieldAssignment::new(
                    TARGET,
                    FIELD_TITLE,
                    MutationExpression::typed_null(ResolvedType::scalar(
                        StandardScalar::CharacterLargeObject,
                    ))
                    .unwrap(),
                )],
                TARGET,
            )
            .unwrap(),
            ServerMutationPlan::new_insert(
                TARGET,
                [FieldAssignment::new(
                    TARGET,
                    FIELD_TITLE,
                    MutationExpression::parameter(
                        OTHER_FUNCTION,
                        PARAMETER_TITLE,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    )
                    .unwrap(),
                )],
                TARGET,
            )
            .unwrap(),
        ];
        for plan in cases {
            assert!(matches!(
                expect_insert_error(
                    validate_plan(&catalogue, &function, TARGET, &plan).unwrap_err()
                ),
                ServerInsertError::PlanInvariant { .. },
            ));
        }
    }

    #[test]
    fn plan_rejects_unique_defaults_inactive_references_and_result_mismatch() {
        let function = valid_function();
        let mut unique_fields = target_fields(OTHER);
        unique_fields[0] = FieldDefinition::new(
            FIELD_TITLE,
            "semantic_title",
            0,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            false,
            true,
            None,
            None,
        );
        let unique_catalogue = catalogue(unique_fields, true, Vec::new());
        assert!(validate_plan(&unique_catalogue, &function, TARGET, &valid_plan()).is_err());

        let mut default_fields = target_fields(OTHER);
        default_fields[4] = FieldDefinition::new(
            FIELD_NOTE,
            "semantic_note",
            4,
            ResolvedType::scalar(StandardScalar::BinaryLargeObject),
            true,
            false,
            Some(ExpressionId::from_bytes([0x74; 16])),
            None,
        );
        let default_catalogue = catalogue(default_fields, true, Vec::new());
        assert!(validate_plan(&default_catalogue, &function, TARGET, &valid_plan()).is_err());

        let inactive_catalogue = catalogue(target_fields(MISSING), false, Vec::new());
        assert!(validate_plan(&inactive_catalogue, &function, TARGET, &valid_plan()).is_err());
        assert!(
            validate_plan(
                &catalogue(target_fields(OTHER), true, Vec::new()),
                &function,
                OTHER,
                &valid_plan()
            )
            .is_err()
        );
    }

    #[test]
    fn reference_replay_body_is_write_object_fields_parameter_reads_then_returned_ref() {
        let expected = expected_body_references(&valid_plan());
        assert_eq!(
            expected,
            vec![
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteObject,
                    DefinitionReferenceTarget::ObjectType(TARGET),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: TARGET,
                        field: FIELD_TITLE,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: FUNCTION,
                        parameter: PARAMETER_TITLE,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: TARGET,
                        field: FIELD_ENABLED,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: TARGET,
                        field: FIELD_COUNT,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: TARGET,
                        field: FIELD_OWNER,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: FUNCTION,
                        parameter: PARAMETER_OWNER,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(TARGET),
                ),
            ],
        );
    }

    #[test]
    fn arguments_are_unordered_exact_typed_and_reference_target_checked() {
        let function = valid_function();
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let validated = validate_arguments(&catalogue, &function, &valid_arguments()).unwrap();
        assert_eq!(validated.len(), 2);
        assert_eq!(validated[&PARAMETER_TITLE], BindValue::Text("title".into()));
        assert_eq!(
            validated[&PARAMETER_OWNER],
            BindValue::Bytes(OBJECT.to_bytes().to_vec()),
        );

        let duplicate = [
            valid_arguments()[1].clone(),
            valid_arguments()[1].clone(),
            valid_arguments()[0].clone(),
        ];
        assert!(validate_arguments(&catalogue, &function, &duplicate).is_err());
        assert!(validate_arguments(&catalogue, &function, &valid_arguments()[..1]).is_err());
        let unknown = [
            FunctionArgument::new(
                ParameterId::from_bytes([0x75; 16]),
                RuntimeValue::Integer(1),
            )
            .unwrap(),
            valid_arguments()[0].clone(),
            valid_arguments()[1].clone(),
        ];
        assert!(validate_arguments(&catalogue, &function, &unknown).is_err());
        let wrong_scalar = [
            FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Integer(1)).unwrap(),
            valid_arguments()[0].clone(),
        ];
        assert!(validate_arguments(&catalogue, &function, &wrong_scalar).is_err());
        let wrong_reference = [
            FunctionArgument::new(
                PARAMETER_OWNER,
                RuntimeValue::Reference {
                    target: TARGET,
                    object: OBJECT,
                },
            )
            .unwrap(),
            valid_arguments()[1].clone(),
        ];
        assert!(validate_arguments(&catalogue, &function, &wrong_reference).is_err());
    }

    #[test]
    fn total_variable_argument_payload_is_bounded() {
        let function = valid_function();
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let oversized = [
            FunctionArgument::new(
                PARAMETER_TITLE,
                RuntimeValue::Text("x".repeat(VARIABLE_ARGUMENT_PAYLOAD_LIMIT + 1)),
            )
            .unwrap(),
            valid_arguments()[0].clone(),
        ];
        assert!(matches!(
            expect_insert_error(validate_arguments(&catalogue, &function, &oversized).unwrap_err()),
            ServerInsertError::ComplexityLimit {
                category: "total size of text and binary arguments",
                maximum: VARIABLE_ARGUMENT_PAYLOAD_LIMIT,
            },
        ));
    }

    #[test]
    fn exact_argument_validation_accepts_more_parameters_than_the_assignment_limit() {
        let parameter_count = server_mutation_plan::MAX_ASSIGNMENTS as usize + 1;
        let integer_type = ResolvedType::scalar(StandardScalar::Integer);
        let parameters = (0..parameter_count)
            .map(|index| {
                ParameterDefinition::new(
                    ParameterId::from_bytes((index as u128 + 1).to_be_bytes()),
                    format!("parameter_{index}"),
                    u32::try_from(index).unwrap(),
                    integer_type,
                    None,
                )
            })
            .collect();
        let function = function(
            FunctionDomain::Server,
            parameters,
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let arguments = (0..parameter_count)
            .map(|index| {
                FunctionArgument::new(
                    ParameterId::from_bytes((index as u128 + 1).to_be_bytes()),
                    RuntimeValue::Integer(i32::try_from(index).unwrap()),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();

        validate_function_signature(&catalogue, &function).unwrap();
        let validated = validate_arguments(&catalogue, &function, &arguments).unwrap();

        assert_eq!(validated.len(), parameter_count);
    }

    #[test]
    fn lowering_uses_exact_stable_ids_typed_binds_and_an_unbound_typed_null() {
        let function = valid_function();
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let arguments = validate_arguments(&catalogue, &function, &valid_arguments()).unwrap();
        let lowered = lower_insert(&valid_plan(), &arguments).unwrap();

        assert_eq!(
            lowered.sql,
            "INSERT INTO _orna_data.t_10101010101010101010101010101010 (_orna_object_id, f_41414141414141414141414141414141, f_42424242424242424242424242424242, f_43434343434343434343434343434343, f_44444444444444444444444444444444) VALUES ($1, $2, $3, CAST(NULL AS int4), $4) RETURNING _orna_object_id AS c0",
        );
        assert_eq!(
            lowered.bind_types,
            vec![Type::BYTEA, Type::TEXT, Type::BOOL, Type::BYTEA],
        );
        assert_eq!(
            lowered.binds,
            vec![
                BindValue::Text(String::from("title")),
                BindValue::Boolean(true),
                BindValue::Bytes(OBJECT.to_bytes().to_vec()),
            ],
        );
        assert_eq!(lowered.sql.matches('$').count(), 4);
        for forbidden in [
            "semantic_target",
            "semantic_title",
            "semantic_insert",
            "semantic_created",
            "semantic_owner_parameter",
        ] {
            assert!(!lowered.sql.contains(forbidden));
        }
        assert!(!lowered.sql.contains("f_45454545454545454545454545454545"));
        assert!(lowered.sql.len() < SQL_LIMIT);
    }

    #[test]
    fn lowering_reuses_one_owned_bind_for_repeated_parameter_assignments() {
        let text_type = ResolvedType::scalar(StandardScalar::CharacterLargeObject);
        let function = function(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                PARAMETER_TITLE,
                "semantic_title_parameter",
                0,
                text_type,
                None,
            )],
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        let catalogue = catalogue(
            vec![
                field(FIELD_TITLE, "first", 0, text_type, false),
                field(FIELD_ENABLED, "second", 1, text_type, false),
                field(FIELD_COUNT, "third", 2, text_type, false),
            ],
            false,
            Vec::new(),
        );
        let parameter =
            || MutationExpression::parameter(FUNCTION, PARAMETER_TITLE, text_type).unwrap();
        let plan = ServerMutationPlan::new_insert(
            TARGET,
            [
                FieldAssignment::new(TARGET, FIELD_TITLE, parameter()),
                FieldAssignment::new(TARGET, FIELD_ENABLED, parameter()),
                FieldAssignment::new(TARGET, FIELD_COUNT, parameter()),
            ],
            TARGET,
        )
        .unwrap();
        validate_plan(&catalogue, &function, TARGET, &plan).unwrap();
        let arguments = validate_arguments(
            &catalogue,
            &function,
            &[FunctionArgument::new(
                PARAMETER_TITLE,
                RuntimeValue::Text(String::from("one owned payload")),
            )
            .unwrap()],
        )
        .unwrap();

        let lowered = lower_insert(&plan, &arguments).unwrap();

        assert_eq!(
            lowered.sql,
            "INSERT INTO _orna_data.t_10101010101010101010101010101010 (_orna_object_id, f_41414141414141414141414141414141, f_42424242424242424242424242424242, f_43434343434343434343434343434343) VALUES ($1, $2, $2, $2) RETURNING _orna_object_id AS c0",
        );
        assert_eq!(lowered.bind_types, vec![Type::BYTEA, Type::TEXT]);
        assert_eq!(
            lowered.binds,
            vec![BindValue::Text(String::from("one owned payload"))],
        );
    }

    #[test]
    fn bind_ownership_covers_every_runtime_storage_type() {
        let values = [
            (RuntimeValue::Boolean(true), BindValue::Boolean(true)),
            (RuntimeValue::Integer(-1), BindValue::Integer(-1)),
            (RuntimeValue::BigInt(2), BindValue::BigInt(2)),
            (
                RuntimeValue::Float(RuntimeFloat::new(3.5).unwrap()),
                BindValue::Float(3.5),
            ),
            (
                RuntimeValue::Text(String::from("text")),
                BindValue::Text(String::from("text")),
            ),
            (
                RuntimeValue::Bytes(vec![1, 2]),
                BindValue::Bytes(vec![1, 2]),
            ),
            (
                RuntimeValue::Reference {
                    target: OTHER,
                    object: OBJECT,
                },
                BindValue::Bytes(OBJECT.to_bytes().to_vec()),
            ),
        ];
        for (value, expected) in values {
            assert_eq!(
                BindValue::from_runtime(&value, PARAMETER_TITLE).unwrap(),
                expected,
            );
        }
    }

    #[test]
    fn public_errors_preserve_context_source_and_commit_state() {
        let context = ServerInsertContext::new(
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x76; 16]),
                CatalogueRevisionId::from_bytes([0x77; 16]),
            ),
            FUNCTION,
            REVISION,
        );
        let not_committed = ServerInsertError::NotCommitted {
            context,
            source: Box::new(ServerInsertError::Argument {
                parameter: Some(PARAMETER_TITLE),
                rule: "the argument type does not match the declared parameter type",
            }),
        };
        assert_eq!(
            not_committed.commit_state(),
            ServerInsertCommitState::NotCommitted,
        );
        assert!(not_committed.source().is_some());
        assert_eq!(
            not_committed.to_string(),
            "the row was not added: a supplied function argument is invalid: the argument type does not match the declared parameter type",
        );

        let rejected = ServerInsertError::CommitRejected {
            context,
            target: TARGET,
            candidate: OBJECT,
            source: "port=invalid"
                .parse::<tokio_postgres::Config>()
                .unwrap_err(),
        };
        assert_eq!(
            rejected.to_string(),
            format!(
                "the database rejected the final save for object {}; no row was added",
                OBJECT.canonical(),
            ),
        );

        let unknown = ServerInsertError::CommitOutcomeUnknown {
            context,
            target: TARGET,
            candidate: OBJECT,
            source: "port=invalid"
                .parse::<tokio_postgres::Config>()
                .unwrap_err(),
        };
        assert_eq!(unknown.commit_state(), ServerInsertCommitState::Unknown);
        assert!(unknown.source().is_some());
        assert_eq!(
            unknown.to_string(),
            format!(
                "the connection failed while saving object {}; it is not known whether the row was added; do not retry automatically",
                OBJECT.canonical(),
            ),
        );

        let result = ServerInsertResult::new(
            context,
            TARGET,
            OBJECT,
            ResultColumn::new("created", ResolvedType::reference(TARGET), false).unwrap(),
        )
        .unwrap();
        let committed = ServerInsertError::CommittedButShutdownFailed {
            result: Box::new(result.clone()),
            source: Box::new(PostgresKernelError::CatalogueInvariant("shutdown test")),
        };
        assert_eq!(committed.commit_state(), ServerInsertCommitState::Committed);
        let ServerInsertError::CommittedButShutdownFailed {
            result: retained, ..
        } = committed
        else {
            unreachable!();
        };
        assert_eq!(*retained, result);
        assert_eq!(
            ServerInsertError::CommittedButShutdownFailed {
                result: Box::new(result),
                source: Box::new(PostgresKernelError::CatalogueInvariant("shutdown test")),
            }
            .to_string(),
            format!(
                "object {} was added, but the database connection did not close cleanly",
                OBJECT.canonical(),
            ),
        );
    }

    #[test]
    fn saved_function_errors_hide_internal_rules_and_give_one_recovery_action() {
        assert_eq!(
            ServerInsertError::Artifact {
                function: FUNCTION,
                rule: "internal artifact detail",
            }
            .to_string(),
            "the saved function is unsupported; redeploy it or contact the database administrator",
        );
        assert_eq!(
            ServerInsertError::PlanDecode(ServerMutationPlan::decode(&[]).unwrap_err()).to_string(),
            "the saved function cannot be read; redeploy it or contact the database administrator",
        );
        assert_eq!(
            ServerInsertError::PlanInvariant {
                rule: "internal invariant detail",
            }
            .to_string(),
            "the saved function is inconsistent with the active database; redeploy it or contact the database administrator",
        );
        assert_eq!(
            ServerInsertError::ReferenceEvidence {
                function: FUNCTION,
                rule: "internal evidence detail",
            }
            .to_string(),
            "the saved function is inconsistent with the active database; redeploy it or contact the database administrator",
        );
        assert_eq!(
            ServerInsertError::PreparedResult {
                rule: "one BYTEA column named c0",
            }
            .to_string(),
            "the database prepared an unexpected result; redeploy the function or contact the database administrator",
        );
        assert_eq!(
            ServerInsertError::ValueInvariant {
                rule: "identity must contain 16 bytes",
            }
            .to_string(),
            "the database returned an unexpected object identity; contact the database administrator",
        );
        assert_eq!(
            ServerInsertError::ComplexityLimit {
                category: "total size of text and binary arguments",
                maximum: VARIABLE_ARGUMENT_PAYLOAD_LIMIT,
            }
            .to_string(),
            format!(
                "the request is too large: total size of text and binary arguments limit is {VARIABLE_ARGUMENT_PAYLOAD_LIMIT}"
            ),
        );
        assert_eq!(
            ServerInsertError::ComplexityLimit {
                category: "saved function complexity",
                maximum: SQL_LIMIT,
            }
            .to_string(),
            format!("the request is too large: saved function complexity limit is {SQL_LIMIT}"),
        );
    }

    #[test]
    fn outer_kernel_error_keeps_the_public_server_insert_source() {
        let error = PostgresKernelError::ServerInsert(ServerInsertError::FunctionNotActive {
            pair: RevisionPair::new(
                SourceRevisionId::from_bytes([0x78; 16]),
                CatalogueRevisionId::from_bytes([0x79; 16]),
            ),
            function: FUNCTION,
        });
        assert!(error.source().is_some());
        assert_eq!(
            error.to_string(),
            "row creation failed: the requested function is not active; no row was added",
        );
    }
}
