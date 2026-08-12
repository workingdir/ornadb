//! Execution of the initial immutable SERVER `SELECT` subset.
//!
//! This module accepts only a recovered active revision and a canonical server
//! plan. It never derives SQL from semantic names or accepts caller SQL.

use std::{collections::BTreeMap, error::Error, fmt};

use futures_util::TryStreamExt;
use orna_artifact::server_plan::{
    self, DistinctServerPlan, Expression, ExpressionKind, FieldStep, IdentitySelectedServerPlan,
    Ordering, ServerPlan, SortDirection,
};
use orna_core::{
    FieldId, FunctionId, FunctionRevisionId, ObjectId, ParameterId, TypeId,
    catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionTransaction, FunctionVolatility,
    },
    revision::{
        ActiveDatabaseRevision, CatalogueHashContext, DefinitionReferenceKind,
        DefinitionReferenceTarget, ExecutableArtifactKind, RevisionPair,
    },
    security::{AuthorisedInvocation, InvocationTarget},
    types::{ResolvedType, StandardScalar},
    value::{
        EnumValue, FunctionArgument, ResultColumn, ResultRow, ResultRows, ResultRowsError,
        RuntimeFloat, RuntimeValue,
    },
};
use tokio_postgres::{
    Client, IsolationLevel, Row, Statement, Transaction,
    types::{ToSql, Type},
};

use crate::{
    PostgresKernel, PostgresKernelError,
    server_runtime::{
        ExpectedDefinitionReference, ReferenceReplayMismatch, ResolvedRuntimeType,
        configure_and_recover, postgres_type, resolve_catalogue_runtime_type, resolve_runtime_type,
        runtime_types_match, validate_function_reference_replay,
    },
    storage::{DATA_SCHEMA, OBJECT_ID_COLUMN, field_name, relation_name},
};

const SERVER_PLAN_FORMAT: &str = server_plan::FORMAT_IDENTITY;
const SERVER_PLAN_VERSION: u32 = server_plan::FORMAT_VERSION;
const IDENTITY_SELECTED_SERVER_PLAN_VERSION: u32 = server_plan::IDENTITY_SELECTED_FORMAT_VERSION;
const DISTINCT_SERVER_PLAN_VERSION: u32 = server_plan::DISTINCT_FORMAT_VERSION;
const ROW_LIMIT: usize = 10_000;
const CELL_LIMIT: usize = 1_000_000;
const PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;
const FIELD_PATH_STEP_LIMIT: usize = 8_192;
const JOIN_LIMIT: usize = 1_024;
const SQL_LIMIT: usize = 1024 * 1024;
const TARGET_ENTRY_LIMIT: usize = 1_600;
const VERSION_ONE_EQUALITY_RULE: &str = "version 1 SERVER SELECT equality supports only BOOLEAN, INTEGER, BIGINT, BYTES, and references";
const PARAMETERISED_EQUALITY_RULE: &str = "parameterised SERVER SELECT equality supports only BOOLEAN, INTEGER, BIGINT, BYTES, and references";
const DISTINCT_EQUALITY_RULE: &str =
    "SELECT DISTINCT equality supports only BOOLEAN, INTEGER, BIGINT, BYTES, and references";
const DISTINCT_PROJECTION_RULE: &str =
    "projections support only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values";
const DISTINCT_REFERENCE_COUNT_RULE: &str = "its dependencies do not match its signature and query";
const DISTINCT_REFERENCE_SEQUENCE_RULE: &str =
    "its dependencies are not in the same order as its signature and query";

#[cfg(feature = "test-hooks")]
struct SelectTestBarrier {
    reached: std::sync::Arc<tokio::sync::Barrier>,
    resume: std::sync::Arc<tokio::sync::Barrier>,
}

#[cfg(not(feature = "test-hooks"))]
struct SelectTestBarrier;

/// Immutable active state pinned for one SERVER SELECT execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerSelectContext {
    pair: RevisionPair,
    function: FunctionId,
    function_revision: FunctionRevisionId,
}

impl ServerSelectContext {
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

/// The stable result of one validated SERVER `SELECT` execution.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerSelectResult {
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
    rows: ResultRows,
}

impl ServerSelectResult {
    fn new(
        pair: RevisionPair,
        function: FunctionId,
        revision: FunctionRevisionId,
        rows: ResultRows,
    ) -> Self {
        Self {
            pair,
            function,
            revision,
            rows,
        }
    }

    /// Returns the source and catalogue pair read by this execution.
    pub const fn pair(&self) -> RevisionPair {
        self.pair
    }

    /// Returns the executed function identity.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the immutable function revision that supplied the plan.
    pub const fn function_revision(&self) -> FunctionRevisionId {
        self.revision
    }

    /// Returns validated rows in declared result-column order.
    pub fn rows(&self) -> &ResultRows {
        &self.rows
    }
}

/// A typed rejection of the initial SERVER `SELECT` execution subset.
#[non_exhaustive]
#[derive(Debug)]
pub enum ServerSelectError {
    /// Authorisation evidence does not cover the recovered active revision.
    AuthorisationMismatch {
        /// The immutable target covered by the authorisation evidence.
        authorised: InvocationTarget,
        /// The recovered active revision pair.
        active: RevisionPair,
    },
    /// The requested function is not in the recovered active catalogue.
    FunctionNotActive {
        /// The recovered active revision pair.
        pair: RevisionPair,
        /// The requested stable function identity.
        function: FunctionId,
    },
    /// A failure after an active function revision was pinned.
    Execution {
        /// The immutable active execution context.
        context: ServerSelectContext,
        /// The underlying typed failure.
        source: Box<ServerSelectError>,
    },
    /// PostgreSQL failed after an active function revision was pinned.
    Database {
        /// The immutable active execution context.
        context: ServerSelectContext,
        /// The PostgreSQL failure.
        source: tokio_postgres::Error,
    },
    /// A kernel validation failure after an active function revision was pinned.
    Kernel {
        /// The kernel failure that carries its native source chain.
        source: Box<PostgresKernelError>,
    },
    /// The function does not execute in the server domain.
    FunctionDomain { function: FunctionId },
    /// The function signature is outside the supported SERVER SELECT forms.
    FunctionSignature {
        function: FunctionId,
        rule: &'static str,
    },
    /// The active function has no exact active immutable revision record.
    CurrentRevision {
        function: FunctionId,
        revision: FunctionRevisionId,
    },
    /// The current revision does not contain the supported server-plan artifact.
    Artifact {
        function: FunctionId,
        rule: &'static str,
    },
    /// The canonical server plan cannot decode.
    PlanDecode(orna_artifact::server_plan::ServerPlanError),
    /// The plan does not agree with the recovered active catalogue.
    PlanInvariant { rule: &'static str },
    /// A saved `SELECT DISTINCT` function is outside the accepted runtime form.
    Distinct {
        /// The exact human-facing rule that failed.
        rule: &'static str,
    },
    /// The ordered durable definition references do not prove this plan.
    ReferenceEvidence {
        function: FunctionId,
        rule: &'static str,
    },
    /// Supplied runtime arguments do not equal the active signature.
    Argument {
        /// The related parameter identity, when one is available.
        parameter: Option<ParameterId>,
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// A version-2 identity selection returned more than its one permitted row.
    Cardinality { rule: &'static str },
    /// The plan result cannot enter the initial runtime value subset.
    ResultRows(ResultRowsError),
    /// PostgreSQL did not prepare the exact generated result shape.
    PreparedResult { rule: &'static str },
    /// PostgreSQL returned a value that does not match the generated result shape.
    RowDecode {
        /// The zero-based result row index.
        row: usize,
        /// The zero-based result column index.
        column: usize,
        /// The PostgreSQL conversion failure.
        source: tokio_postgres::Error,
    },
    /// A decoded PostgreSQL value violates an Orna runtime value invariant.
    ValueInvariant {
        /// The zero-based result row index.
        row: usize,
        /// The zero-based result column index.
        column: usize,
        /// The exact runtime rule that failed.
        rule: &'static str,
    },
    /// A variable result value exceeded its per-cell payload bound.
    VariablePayload {
        /// The zero-based result row index.
        row: usize,
        /// The zero-based result column index.
        column: usize,
        /// The largest permitted value payload.
        maximum: usize,
    },
    /// The plan exceeded one fixed execution complexity bound.
    ComplexityLimit {
        /// The bounded plan category.
        category: &'static str,
        /// The largest accepted value.
        maximum: usize,
    },
    /// The result exceeds the fixed row bound.
    RowLimit { maximum: usize },
    /// The result exceeds the fixed cell bound.
    CellLimit { maximum: usize },
    /// The result exceeds the fixed logical payload bound.
    PayloadLimit { maximum: usize },
}

impl fmt::Display for ServerSelectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorisationMismatch { authorised, active } => write!(
                formatter,
                "authorised SERVER target {:?} does not match active pair {:?}",
                authorised, active
            ),
            Self::FunctionNotActive { pair, function } => {
                write!(
                    formatter,
                    "function {} is not active in pair {:?}",
                    function.canonical(),
                    pair
                )
            }
            Self::Execution { context, source } => {
                write!(
                    formatter,
                    "SERVER SELECT {} revision {} in pair {:?} failed: {source}",
                    context.function().canonical(),
                    context.function_revision().canonical(),
                    context.pair()
                )
            }
            Self::Database { context, source } => {
                write!(
                    formatter,
                    "SERVER SELECT {} revision {} in pair {:?} had a PostgreSQL failure: {source}",
                    context.function().canonical(),
                    context.function_revision().canonical(),
                    context.pair()
                )
            }
            Self::Kernel { source } => write!(
                formatter,
                "active SERVER SELECT kernel validation failed: {source}"
            ),
            Self::FunctionDomain { function } => {
                write!(
                    formatter,
                    "function {} is not a SERVER function",
                    function.canonical()
                )
            }
            Self::FunctionSignature { function, rule } => {
                write!(
                    formatter,
                    "function {} has an unsupported signature: {rule}",
                    function.canonical()
                )
            }
            Self::CurrentRevision { function, revision } => write!(
                formatter,
                "function {} has no active revision {}",
                function.canonical(),
                revision.canonical(),
            ),
            Self::Artifact { function, rule } => {
                write!(
                    formatter,
                    "function {} has an unsupported artifact: {rule}",
                    function.canonical()
                )
            }
            Self::PlanDecode(error) => write!(formatter, "cannot decode server plan: {error}"),
            Self::PlanInvariant { rule } => {
                write!(formatter, "server plan invariant failed: {rule}")
            }
            Self::Distinct { rule } => {
                write!(
                    formatter,
                    "saved SELECT DISTINCT function cannot run: {rule}"
                )
            }
            Self::ReferenceEvidence { function, rule } => write!(
                formatter,
                "function {} has invalid definition-reference evidence: {rule}",
                function.canonical(),
            ),
            Self::Argument { rule, .. } => {
                write!(formatter, "a supplied function argument is invalid: {rule}")
            }
            Self::Cardinality { rule } => {
                write!(formatter, "SERVER SELECT returned too many rows: {rule}")
            }
            Self::ResultRows(error) => write!(formatter, "invalid server result shape: {error}"),
            Self::PreparedResult { rule } => {
                write!(formatter, "prepared server result is invalid: {rule}")
            }
            Self::RowDecode {
                row,
                column,
                source,
            } => {
                write!(
                    formatter,
                    "cannot decode result row {row} column {column}: {source}"
                )
            }
            Self::ValueInvariant { row, column, rule } => {
                write!(
                    formatter,
                    "result row {row} column {column} violates {rule}"
                )
            }
            Self::VariablePayload {
                row,
                column,
                maximum,
            } => write!(
                formatter,
                "result row {row} column {column} exceeds {maximum} payload bytes"
            ),
            Self::ComplexityLimit { category, maximum } => {
                write!(formatter, "server plan {category} exceeds {maximum}")
            }
            Self::RowLimit { maximum } => write!(formatter, "server result exceeds {maximum} rows"),
            Self::CellLimit { maximum } => {
                write!(formatter, "server result exceeds {maximum} cells")
            }
            Self::PayloadLimit { maximum } => {
                write!(
                    formatter,
                    "server result exceeds {maximum} logical payload bytes"
                )
            }
        }
    }
}

impl Error for ServerSelectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Execution { source, .. } => Some(source),
            Self::Database { source, .. } => Some(source),
            Self::Kernel { source } => Some(source),
            Self::PlanDecode(error) => Some(error),
            Self::ResultRows(error) => Some(error),
            Self::RowDecode { source, .. } => Some(source),
            Self::FunctionNotActive { .. }
            | Self::AuthorisationMismatch { .. }
            | Self::FunctionDomain { .. }
            | Self::FunctionSignature { .. }
            | Self::CurrentRevision { .. }
            | Self::Artifact { .. }
            | Self::PlanInvariant { .. }
            | Self::Distinct { .. }
            | Self::ReferenceEvidence { .. }
            | Self::Argument { .. }
            | Self::Cardinality { .. }
            | Self::PreparedResult { .. }
            | Self::ValueInvariant { .. }
            | Self::VariablePayload { .. }
            | Self::ComplexityLimit { .. }
            | Self::RowLimit { .. }
            | Self::CellLimit { .. }
            | Self::PayloadLimit { .. } => None,
        }
    }
}

impl PostgresKernel {
    /// Executes the active no-argument SERVER `ROWS` function identified by `function`.
    ///
    /// This operation accepts no-argument version 1 and version 3 `SELECT
    /// DISTINCT` functions by stable function identity. It does not resolve
    /// names, perform authentication or authorisation, accept an invocation
    /// identity or arguments, or expose a protocol stream. It reads only the
    /// active revision and returns a bounded collected result.
    pub async fn execute_server_select(
        &self,
        function: FunctionId,
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_server_select_with_arguments(function, &[])
            .await
    }

    /// Executes an active SERVER `ROWS` function with its exact typed arguments.
    ///
    /// Version 1 and version 3 `SELECT DISTINCT` functions accept no arguments.
    /// Version 2 functions accept exactly one non-null `REF` argument,
    /// identified by the stable [`ParameterId`] from the active function
    /// signature. This method does not resolve semantic names, authenticate or
    /// authorise a caller, attach an invocation identity, or retry a failed
    /// operation automatically.
    pub async fn execute_server_select_with_arguments(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_server_select_with_options(function, arguments, None, false)
            .await
    }

    /// Pauses a live execution after it has recovered its active snapshot.
    ///
    /// This hook is compiled only for the PostgreSQL integration harness. Both
    /// barriers must have exactly two participants: the executor and the test.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_server_select_with_test_barrier(
        &self,
        function: FunctionId,
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_server_select_with_arguments_and_test_barrier(function, &[], reached, resume)
            .await
    }

    /// Pauses an argument-capable execution after active recovery.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_server_select_with_arguments_and_test_barrier(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_server_select_with_options(
            function,
            arguments,
            Some(SelectTestBarrier { reached, resume }),
            false,
        )
        .await
    }

    /// Forces a driver shutdown after PostgreSQL confirms the read-only COMMIT.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_server_select_with_forced_post_commit_driver_shutdown(
        &self,
        function: FunctionId,
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_server_select_with_arguments_and_forced_post_commit_driver_shutdown(
            function,
            &[],
        )
        .await
    }

    /// Forces a driver shutdown after PostgreSQL confirms an argument-capable read-only COMMIT.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_server_select_with_arguments_and_forced_post_commit_driver_shutdown(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_server_select_with_options(function, arguments, None, true)
            .await
    }

    async fn execute_server_select_with_options(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
        barrier: Option<SelectTestBarrier>,
        force_post_commit_driver_shutdown: bool,
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        let mut session = self.open().await?;
        let execution =
            execute_client(&mut session.client, function, arguments, barrier.as_ref()).await;
        #[cfg(feature = "test-hooks")]
        if execution.is_ok() && force_post_commit_driver_shutdown {
            session.abort_driver();
        }
        #[cfg(not(feature = "test-hooks"))]
        let _ = force_post_commit_driver_shutdown;
        let shutdown = session.shutdown().await;
        match (execution, shutdown) {
            (Ok(result), Ok(())) => Ok(result),
            (Err(error), _) => Err(error),
            (Ok(result), Err(error)) => Err(contextualize(context_from_result(&result), error)),
        }
    }
}

async fn execute_client(
    client: &mut Client,
    function: FunctionId,
    arguments: &[FunctionArgument],
    test_barrier: Option<&SelectTestBarrier>,
) -> Result<ServerSelectResult, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;
    let result = execute_transaction(&transaction, function, arguments, test_barrier).await;
    match result {
        Ok(result) => {
            let context = context_from_result(&result);
            transaction
                .commit()
                .await
                .map(|()| result)
                .map_err(|source| execution_database(context, source))
        }
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => match execution_context(&error) {
                Some(context) => Err(execution_database(context, rollback)),
                None => Err(PostgresKernelError::Database(rollback)),
            },
        },
    }
}

async fn execute_transaction(
    transaction: &Transaction<'_>,
    function_id: FunctionId,
    arguments: &[FunctionArgument],
    test_barrier: Option<&SelectTestBarrier>,
) -> Result<ServerSelectResult, PostgresKernelError> {
    let active = configure_and_recover(transaction).await?;
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.id() == function_id)
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: function_id,
            })
        })?;
    let context = ServerSelectContext::new(active.pair(), function_id, function.current_revision());
    pause_after_recovery(test_barrier).await;
    let result =
        execute_active_transaction(transaction, &active, function, context, arguments).await;
    result.map_err(|error| contextualize(context, error))
}

/// Executes an exact active SERVER target from protected authorisation evidence.
///
/// This helper does not recover state or make an authorisation decision. The
/// caller must provide the active revision that produced `authorisation`.
pub(crate) async fn execute_authorised_server_select(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
) -> Result<ServerSelectResult, PostgresKernelError> {
    let target = authorisation.target();
    if target.revision() != active.pair() {
        return Err(server_error(ServerSelectError::AuthorisationMismatch {
            authorised: target,
            active: active.pair(),
        }));
    }
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.id() == target.function())
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: target.function(),
            })
        })?;
    let context = ServerSelectContext::new(
        active.pair(),
        target.function(),
        function.current_revision(),
    );
    execute_active_transaction(transaction, active, function, context, arguments)
        .await
        .map_err(|error| contextualize(context, error))
}

#[cfg(feature = "test-hooks")]
async fn pause_after_recovery(test_barrier: Option<&SelectTestBarrier>) {
    if let Some(test_barrier) = test_barrier {
        test_barrier.reached.wait().await;
        test_barrier.resume.wait().await;
    }
}

#[cfg(not(feature = "test-hooks"))]
async fn pause_after_recovery(_test_barrier: Option<&SelectTestBarrier>) {}

async fn execute_active_transaction(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    context: ServerSelectContext,
    arguments: &[FunctionArgument],
) -> Result<ServerSelectResult, PostgresKernelError> {
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == context.function()
                && revision.id() == function.current_revision()
        })
        .ok_or_else(|| {
            server_error(ServerSelectError::CurrentRevision {
                function: context.function(),
                revision: function.current_revision(),
            })
        })?;
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Server {
        return Err(server_error(ServerSelectError::Artifact {
            function: context.function(),
            rule: "current revision must contain a SERVER artifact",
        }));
    }
    if revision.language_version() != server_plan::LANGUAGE_VERSION_IDENTITY {
        return Err(server_error(ServerSelectError::Artifact {
            function: context.function(),
            rule: "current SERVER revision must use the server-plan language version",
        }));
    }
    let decoded = decode_plan(
        context.function(),
        artifact.format(),
        artifact.version(),
        artifact.payload(),
    )?;
    let (columns, lowered, cardinality) = match &decoded {
        DecodedServerPlan::V1(plan) => {
            validate_function_signature(function)?;
            validate_no_arguments(arguments)?;
            validate_plan(active, function, plan)?;
            validate_reference_evidence(active, function, plan)?;
            let columns = result_columns_for_projections(function, &plan.projections)?;
            validate_target_entries(
                active.catalogue(),
                active.catalogue_hash_context(),
                plan.projections.len(),
                &columns,
                plan.ordering.len(),
            )?;
            let lowered = lower_plan(
                active.catalogue(),
                active.catalogue_hash_context(),
                plan,
                &columns,
            )?;
            (columns, lowered, ResultCardinality::BoundedMany)
        }
        DecodedServerPlan::V2(plan) => {
            validate_identity_selected_function_signature(active.catalogue(), function)?;
            validate_identity_selected_plan(
                active.catalogue(),
                active.catalogue_hash_context(),
                function,
                plan,
            )?;
            validate_identity_selected_reference_evidence(active, function, plan)?;
            let object = validate_identity_selected_arguments(
                active.catalogue(),
                active.catalogue_hash_context(),
                function,
                plan,
                arguments,
            )?;
            let columns = result_columns_for_projections(function, plan.projections())?;
            validate_target_entries(
                active.catalogue(),
                active.catalogue_hash_context(),
                plan.projections().len(),
                &columns,
                0,
            )?;
            let lowered = lower_identity_selected_plan(
                active.catalogue(),
                active.catalogue_hash_context(),
                plan,
                &columns,
                object,
            )?;
            (columns, lowered, ResultCardinality::AtMostOne)
        }
        DecodedServerPlan::V3(plan) => {
            validate_distinct_function_signature(function)?;
            validate_no_arguments(arguments)?;
            validate_distinct_plan(
                active.catalogue(),
                active.catalogue_hash_context(),
                function,
                plan,
            )?;
            validate_distinct_reference_evidence(active, function, plan)?;
            let columns = result_columns_for_projections(function, plan.projections())?;
            validate_target_entries(
                active.catalogue(),
                active.catalogue_hash_context(),
                plan.projections().len(),
                &columns,
                0,
            )?;
            let lowered = lower_distinct_plan(
                active.catalogue(),
                active.catalogue_hash_context(),
                plan,
                &columns,
            )?;
            (columns, lowered, ResultCardinality::BoundedMany)
        }
    };
    let statement = transaction
        .prepare_typed(&lowered.sql, &lowered.bind_types)
        .await
        .map_err(PostgresKernelError::Database)?;
    validate_prepared_columns(
        active.catalogue(),
        active.catalogue_hash_context(),
        &statement,
        &columns,
        &lowered.guards,
    )?;
    let rows = stream_rows(
        transaction,
        &statement,
        &lowered.binds,
        ResultReadShape {
            catalogue: active.catalogue(),
            context: active.catalogue_hash_context(),
            columns: &columns,
            guards: &lowered.guards,
            variable_payload_limit: lowered.variable_payload_limit,
            cardinality,
        },
    )
    .await?;
    Ok(ServerSelectResult::new(
        context.pair(),
        context.function(),
        revision.id(),
        rows,
    ))
}

enum DecodedServerPlan {
    V1(ServerPlan),
    V2(IdentitySelectedServerPlan),
    V3(DistinctServerPlan),
}

fn decode_plan(
    function: FunctionId,
    format: &str,
    version: u32,
    payload: &[u8],
) -> Result<DecodedServerPlan, PostgresKernelError> {
    if format != SERVER_PLAN_FORMAT {
        return Err(artifact_error(
            function,
            "current SERVER artifact must use orna.server-plan",
        ));
    }
    match version {
        SERVER_PLAN_VERSION => ServerPlan::decode(payload)
            .map(DecodedServerPlan::V1)
            .map_err(ServerSelectError::PlanDecode)
            .map_err(server_error),
        IDENTITY_SELECTED_SERVER_PLAN_VERSION => IdentitySelectedServerPlan::decode(payload)
            .map(DecodedServerPlan::V2)
            .map_err(ServerSelectError::PlanDecode)
            .map_err(server_error),
        DISTINCT_SERVER_PLAN_VERSION => DistinctServerPlan::decode(payload)
            .map(DecodedServerPlan::V3)
            .map_err(map_distinct_plan_decode_error),
        _ => Err(artifact_error(
            function,
            "current SERVER artifact must use supported orna.server-plan version 1, version 2, or version 3",
        )),
    }
}

fn map_distinct_plan_decode_error(error: server_plan::ServerPlanError) -> PostgresKernelError {
    match error {
        server_plan::ServerPlanError::UnsupportedDistinctProjectionType { .. } => {
            distinct_error(DISTINCT_PROJECTION_RULE)
        }
        server_plan::ServerPlanError::DistinctOrderingNotAllowed { .. } => {
            distinct_error("ORDER BY is not allowed")
        }
        error => server_error(ServerSelectError::PlanDecode(error)),
    }
}

fn validate_function_signature(function: &FunctionDefinition) -> Result<(), PostgresKernelError> {
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function.id(),
        }));
    }
    if !function.parameters().is_empty() {
        return Err(server_error(ServerSelectError::FunctionSignature {
            function: function.id(),
            rule: "SERVER SELECT functions must have zero parameters",
        }));
    }
    if !matches!(function.return_type(), FunctionReturn::Rows(columns) if !columns.is_empty()) {
        return Err(server_error(ServerSelectError::FunctionSignature {
            function: function.id(),
            rule: "SERVER SELECT functions must return nonempty ROWS",
        }));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(server_error(ServerSelectError::FunctionSignature {
            function: function.id(),
            rule: "SERVER SELECT functions must use INVOKER security",
        }));
    }
    if !matches!(
        function.transaction(),
        None | Some(FunctionTransaction::Atomic | FunctionTransaction::ReadOnly)
    ) {
        return Err(server_error(ServerSelectError::FunctionSignature {
            function: function.id(),
            rule: "SERVER SELECT functions cannot use MANUAL transactions",
        }));
    }
    Ok(())
}

fn validate_identity_selected_function_signature(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    function: &FunctionDefinition,
) -> Result<(), PostgresKernelError> {
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function.id(),
        }));
    }
    if !matches!(function.return_type(), FunctionReturn::Rows(columns) if !columns.is_empty()) {
        return Err(function_signature_error(
            function.id(),
            "SERVER SELECT functions must return nonempty ROWS",
        ));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(function_signature_error(
            function.id(),
            "parameterised SERVER SELECT functions must use INVOKER security",
        ));
    }
    if function.transaction() != Some(FunctionTransaction::ReadOnly) {
        return Err(function_signature_error(
            function.id(),
            "parameterised SERVER SELECT functions must use READ ONLY transactions",
        ));
    }
    if function.volatility() != FunctionVolatility::Stable {
        return Err(function_signature_error(
            function.id(),
            "parameterised SERVER SELECT functions must use STABLE volatility",
        ));
    }
    let [parameter] = function.parameters() else {
        return Err(function_signature_error(
            function.id(),
            "parameterised SERVER SELECT functions must declare exactly one parameter",
        ));
    };
    if parameter.default_expression().is_some() {
        return Err(function_signature_error(
            function.id(),
            "the identity selector parameter cannot have a default expression",
        ));
    }
    let Some(target) = parameter.resolved_type().reference_target() else {
        return Err(function_signature_error(
            function.id(),
            "the selector parameter must use REF to an available object type",
        ));
    };
    if catalogue.object_type_by_id(target).is_none() {
        return Err(function_signature_error(
            function.id(),
            "the selector parameter must use REF to an available object type",
        ));
    }
    Ok(())
}

fn validate_distinct_function_signature(
    function: &FunctionDefinition,
) -> Result<(), PostgresKernelError> {
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function.id(),
        }));
    }
    if !function.parameters().is_empty() {
        return Err(function_signature_error(
            function.id(),
            "SELECT DISTINCT SERVER functions must have zero parameters",
        ));
    }
    if !matches!(function.return_type(), FunctionReturn::Rows(columns) if !columns.is_empty()) {
        return Err(function_signature_error(
            function.id(),
            "SELECT DISTINCT SERVER functions must return nonempty ROWS",
        ));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(function_signature_error(
            function.id(),
            "SELECT DISTINCT SERVER functions must use INVOKER security",
        ));
    }
    if function.transaction() != Some(FunctionTransaction::ReadOnly) {
        return Err(function_signature_error(
            function.id(),
            "SELECT DISTINCT SERVER functions must use READ ONLY transactions",
        ));
    }
    if function.volatility() != FunctionVolatility::Stable {
        return Err(function_signature_error(
            function.id(),
            "SELECT DISTINCT SERVER functions must use STABLE volatility",
        ));
    }
    Ok(())
}

fn validate_identity_selected_plan(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: &FunctionDefinition,
    plan: &IdentitySelectedServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_execution_complexity_for_projections(plan.projections())?;
    let scan = plan.scan();
    if scan.input != 0 || catalogue.object_type_by_id(scan.object_type).is_none() {
        return Err(plan_invariant(
            "scan must use active input zero and an active object type",
        ));
    }
    let FunctionReturn::Rows(return_columns) = function.return_type() else {
        return Err(plan_invariant("function return shape must be ROWS"));
    };
    if plan.projections().len() != return_columns.len() {
        return Err(plan_invariant(
            "projection count must equal ROWS column count",
        ));
    }
    for (projection, column) in plan.projections().iter().zip(return_columns) {
        validate_expression_with_equality_rule(
            catalogue,
            context,
            scan.object_type,
            projection,
            PARAMETERISED_EQUALITY_RULE,
        )?;
        if !runtime_types_match(
            context,
            projection.value_type.resolved_type,
            column.resolved_type(),
        ) {
            return Err(plan_invariant("projection type must equal its ROWS column"));
        }
        if !supports_result_type(catalogue, context, projection.value_type.resolved_type) {
            return Err(plan_invariant(
                "projection type is outside the initial runtime result subset",
            ));
        }
    }
    let selector = plan.selector();
    let [parameter] = function.parameters() else {
        return Err(plan_invariant(
            "parameterised SERVER SELECT function must have one declared selector parameter",
        ));
    };
    if selector.owner() != function.id() || selector.parameter() != parameter.id() {
        return Err(plan_invariant(
            "identity selector owner and parameter must equal the active function signature",
        ));
    }
    if parameter.resolved_type() != ResolvedType::reference(scan.object_type) {
        return Err(plan_invariant(
            "the selector parameter must use REF to the object type selected in FROM",
        ));
    }
    Ok(())
}

fn validate_distinct_plan(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: &FunctionDefinition,
    plan: &DistinctServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_execution_complexity_for_distinct(plan)?;
    let scan = plan.scan();
    if scan.input != 0 || catalogue.object_type_by_id(scan.object_type).is_none() {
        return Err(plan_invariant(
            "scan must use active input zero and an active object type",
        ));
    }
    let FunctionReturn::Rows(return_columns) = function.return_type() else {
        return Err(plan_invariant("function return shape must be ROWS"));
    };
    if plan.projections().len() != return_columns.len() {
        return Err(plan_invariant(
            "projection count must equal ROWS column count",
        ));
    }
    for (projection, column) in plan.projections().iter().zip(return_columns) {
        validate_expression_with_equality_rule(
            catalogue,
            context,
            scan.object_type,
            projection,
            DISTINCT_EQUALITY_RULE,
        )?;
        if !runtime_types_match(
            context,
            projection.value_type.resolved_type,
            column.resolved_type(),
        ) {
            return Err(plan_invariant("projection type must equal its ROWS column"));
        }
        if !supports_distinct_projection_type(context, projection.value_type.resolved_type) {
            return Err(distinct_error(DISTINCT_PROJECTION_RULE));
        }
        if !supports_result_type(catalogue, context, projection.value_type.resolved_type) {
            return Err(plan_invariant(
                "projection type is outside the initial runtime result subset",
            ));
        }
    }
    if let Some(selection) = plan.selection() {
        validate_expression_with_equality_rule(
            catalogue,
            context,
            scan.object_type,
            selection,
            DISTINCT_EQUALITY_RULE,
        )?;
        if selection.value_type.resolved_type != ResolvedType::scalar(StandardScalar::Boolean) {
            return Err(plan_invariant("selection must have BOOLEAN type"));
        }
    }
    Ok(())
}

fn validate_execution_complexity_for_projections(
    projections: &[Expression],
) -> Result<(), PostgresKernelError> {
    validate_expression_complexity(projections.iter())
}

fn validate_execution_complexity_for_distinct(
    plan: &DistinctServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_expression_complexity(plan.projections().iter().chain(plan.selection()))
}

fn validate_expression_complexity<'a>(
    expressions: impl Iterator<Item = &'a Expression>,
) -> Result<(), PostgresKernelError> {
    let mut steps = 0usize;
    let mut binds = 0usize;
    for expression in expressions {
        count_expression_complexity(expression, &mut steps, &mut binds)?;
    }
    if steps > FIELD_PATH_STEP_LIMIT {
        return Err(server_error(ServerSelectError::ComplexityLimit {
            category: "field path steps",
            maximum: FIELD_PATH_STEP_LIMIT,
        }));
    }
    if binds > server_plan::MAX_EXPRESSION_NODES as usize {
        return Err(server_error(ServerSelectError::ComplexityLimit {
            category: "boolean binds",
            maximum: server_plan::MAX_EXPRESSION_NODES as usize,
        }));
    }
    Ok(())
}

fn validate_identity_selected_arguments(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: &FunctionDefinition,
    plan: &IdentitySelectedServerPlan,
    arguments: &[FunctionArgument],
) -> Result<ObjectId, PostgresKernelError> {
    let mut supplied = BTreeMap::new();
    for argument in arguments {
        let parameter_id = argument.parameter();
        if supplied.insert(parameter_id, argument.value()).is_some() {
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
        if !runtime_type_is_active(catalogue, context, value.resolved_type()) {
            return Err(argument_error(
                Some(parameter_id),
                "the argument uses an unsupported type or refers to an unavailable object type",
            ));
        }
        if !runtime_types_match(context, value.resolved_type(), parameter.resolved_type()) {
            return Err(argument_error(
                Some(parameter_id),
                "the argument type does not match the declared parameter type",
            ));
        }
    }
    let selector = plan.selector();
    let value = supplied.get(&selector.parameter()).ok_or_else(|| {
        argument_error(Some(selector.parameter()), "a required argument is missing")
    })?;
    match value {
        RuntimeValue::Reference { target, object } if *target == plan.scan().object_type => {
            Ok(*object)
        }
        _ => Err(argument_error(
            Some(selector.parameter()),
            "the selector argument must refer to the object type selected by this function",
        )),
    }
}

fn validate_no_arguments(arguments: &[FunctionArgument]) -> Result<(), PostgresKernelError> {
    if arguments.is_empty() {
        Ok(())
    } else {
        Err(argument_error(
            None,
            "this function does not accept arguments",
        ))
    }
}

fn runtime_type_is_active(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> bool {
    match resolve_catalogue_runtime_type(catalogue, context, resolved_type) {
        runtime @ ResolvedRuntimeType::LegacyScalar(_)
        | runtime @ ResolvedRuntimeType::VerifiedValue { .. } => postgres_type(runtime).is_some(),
        ResolvedRuntimeType::CatalogueEnum(_) => true,
        ResolvedRuntimeType::Reference(target) => catalogue.object_type_by_id(target).is_some(),
        ResolvedRuntimeType::Unsupported => false,
    }
}

fn function_signature_error(function: FunctionId, rule: &'static str) -> PostgresKernelError {
    server_error(ServerSelectError::FunctionSignature { function, rule })
}

fn argument_error(parameter: Option<ParameterId>, rule: &'static str) -> PostgresKernelError {
    server_error(ServerSelectError::Argument { parameter, rule })
}

fn artifact_error(function: FunctionId, rule: &'static str) -> PostgresKernelError {
    server_error(ServerSelectError::Artifact { function, rule })
}

fn distinct_error(rule: &'static str) -> PostgresKernelError {
    server_error(ServerSelectError::Distinct { rule })
}

fn validate_plan(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerPlan,
) -> Result<(), PostgresKernelError> {
    let catalogue = active.catalogue();
    let context = active.catalogue_hash_context();
    validate_execution_complexity(plan)?;
    if plan.scan.input != 0 || catalogue.object_type_by_id(plan.scan.object_type).is_none() {
        return Err(plan_invariant(
            "scan must use active input zero and an active object type",
        ));
    }
    let FunctionReturn::Rows(return_columns) = function.return_type() else {
        return Err(plan_invariant("function return shape must be ROWS"));
    };
    if plan.projections.len() != return_columns.len() {
        return Err(plan_invariant(
            "projection count must equal ROWS column count",
        ));
    }
    for (projection, column) in plan.projections.iter().zip(return_columns) {
        validate_expression(catalogue, context, plan.scan.object_type, projection)?;
        if !runtime_types_match(
            context,
            projection.value_type.resolved_type,
            column.resolved_type(),
        ) {
            return Err(plan_invariant("projection type must equal its ROWS column"));
        }
        if !supports_result_type(catalogue, context, projection.value_type.resolved_type) {
            return Err(plan_invariant(
                "projection type is outside the initial runtime result subset",
            ));
        }
    }
    if let Some(selection) = &plan.selection {
        validate_expression(catalogue, context, plan.scan.object_type, selection)?;
        if selection.value_type.resolved_type != ResolvedType::scalar(StandardScalar::Boolean) {
            return Err(plan_invariant("selection must have BOOLEAN type"));
        }
    }
    for ordering in &plan.ordering {
        validate_expression(
            catalogue,
            context,
            plan.scan.object_type,
            &ordering.expression,
        )?;
        if !supports_ordering_type(context, ordering.expression.value_type.resolved_type) {
            return Err(plan_invariant(
                "version 1 SERVER SELECT ordering supports only INTEGER and BIGINT",
            ));
        }
    }
    Ok(())
}

fn validate_expression(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    scan: TypeId,
    expression: &Expression,
) -> Result<(), PostgresKernelError> {
    validate_expression_with_equality_rule(
        catalogue,
        context,
        scan,
        expression,
        VERSION_ONE_EQUALITY_RULE,
    )
}

fn validate_expression_with_equality_rule(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    scan: TypeId,
    expression: &Expression,
    equality_rule: &'static str,
) -> Result<(), PostgresKernelError> {
    match &expression.kind {
        ExpressionKind::ObjectReference { input } => {
            if *input != 0
                || expression.value_type.resolved_type != ResolvedType::reference(scan)
                || expression.value_type.nullable
            {
                return Err(plan_invariant(
                    "object reference must be non-nullable input zero of the scan type",
                ));
            }
        }
        ExpressionKind::FieldPath { input, steps } => {
            if *input != 0 {
                return Err(plan_invariant("field path must use input zero"));
            }
            let (resolved_type, nullable) = field_path_type(catalogue, scan, steps)?;
            if !runtime_types_match(context, expression.value_type.resolved_type, resolved_type)
                || expression.value_type.nullable != nullable
            {
                return Err(plan_invariant(
                    "field path result type and nullability must match every active hop",
                ));
            }
        }
        ExpressionKind::BooleanLiteral { .. } => {
            if expression.value_type.resolved_type != ResolvedType::scalar(StandardScalar::Boolean)
                || expression.value_type.nullable
            {
                return Err(plan_invariant(
                    "BOOLEAN literal must have non-nullable BOOLEAN type",
                ));
            }
        }
        ExpressionKind::Equality { left, right } => {
            validate_expression_with_equality_rule(catalogue, context, scan, left, equality_rule)?;
            validate_expression_with_equality_rule(catalogue, context, scan, right, equality_rule)?;
            if left.value_type.resolved_type != right.value_type.resolved_type
                || expression.value_type.resolved_type
                    != ResolvedType::scalar(StandardScalar::Boolean)
                || expression.value_type.nullable
                    != (left.value_type.nullable || right.value_type.nullable)
            {
                return Err(plan_invariant(
                    "equality operands and nullable BOOLEAN result must match",
                ));
            }
            if !supports_equality_type(context, left.value_type.resolved_type) {
                return Err(plan_invariant(equality_rule));
            }
        }
    }
    Ok(())
}

fn supports_ordering_type(context: &CatalogueHashContext, resolved_type: ResolvedType) -> bool {
    matches!(
        resolve_runtime_type(context, resolved_type).compatibility_scalar(),
        Some(StandardScalar::Integer | StandardScalar::BigInt)
    )
}

fn supports_equality_type(context: &CatalogueHashContext, resolved_type: ResolvedType) -> bool {
    matches!(
        resolve_runtime_type(context, resolved_type).compatibility_scalar(),
        Some(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        )
    ) || matches!(
        resolve_runtime_type(context, resolved_type),
        ResolvedRuntimeType::Reference(_)
    )
}

fn supports_distinct_projection_type(
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> bool {
    matches!(
        resolve_runtime_type(context, resolved_type).compatibility_scalar(),
        Some(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        )
    ) || matches!(
        resolve_runtime_type(context, resolved_type),
        ResolvedRuntimeType::Reference(_)
    )
}

fn supports_result_type(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> bool {
    matches!(
        resolve_runtime_type(context, resolved_type).compatibility_scalar(),
        Some(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        )
    ) || matches!(
        resolve_catalogue_runtime_type(catalogue, context, resolved_type),
        ResolvedRuntimeType::CatalogueEnum(_) | ResolvedRuntimeType::Reference(_)
    )
}

fn validate_execution_complexity(plan: &ServerPlan) -> Result<(), PostgresKernelError> {
    validate_expression_complexity(
        plan.projections
            .iter()
            .chain(plan.selection.iter())
            .chain(plan.ordering.iter().map(|ordering| &ordering.expression)),
    )
}

fn validate_target_entries(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    projections: usize,
    columns: &[ResultColumn],
    ordering: usize,
) -> Result<(), PostgresKernelError> {
    let guards = columns
        .iter()
        .filter(|column| is_variable_type(catalogue, context, column.resolved_type()))
        .count();
    validate_target_entry_count(projections, guards, ordering)
}

fn validate_target_entry_count(
    projections: usize,
    guards: usize,
    ordering: usize,
) -> Result<(), PostgresKernelError> {
    let entries = projections
        .checked_add(guards)
        .and_then(|entries| entries.checked_add(ordering))
        .ok_or_else(|| {
            server_error(ServerSelectError::ComplexityLimit {
                category: "generated PostgreSQL target entries",
                maximum: TARGET_ENTRY_LIMIT,
            })
        })?;
    if entries > TARGET_ENTRY_LIMIT {
        return Err(server_error(ServerSelectError::ComplexityLimit {
            category: "generated PostgreSQL target entries",
            maximum: TARGET_ENTRY_LIMIT,
        }));
    }
    Ok(())
}

fn count_expression_complexity(
    expression: &Expression,
    steps: &mut usize,
    binds: &mut usize,
) -> Result<(), PostgresKernelError> {
    match &expression.kind {
        ExpressionKind::ObjectReference { .. } => {}
        ExpressionKind::FieldPath { steps: path, .. } => {
            *steps = steps.checked_add(path.len()).ok_or_else(|| {
                server_error(ServerSelectError::ComplexityLimit {
                    category: "field path steps",
                    maximum: FIELD_PATH_STEP_LIMIT,
                })
            })?;
        }
        ExpressionKind::BooleanLiteral { .. } => {
            *binds = binds.checked_add(1).ok_or_else(|| {
                server_error(ServerSelectError::ComplexityLimit {
                    category: "boolean binds",
                    maximum: server_plan::MAX_EXPRESSION_NODES as usize,
                })
            })?;
        }
        ExpressionKind::Equality { left, right } => {
            count_expression_complexity(left, steps, binds)?;
            count_expression_complexity(right, steps, binds)?;
        }
    }
    Ok(())
}

fn field_path_type(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    scan: TypeId,
    steps: &[FieldStep],
) -> Result<(ResolvedType, bool), PostgresKernelError> {
    let mut owner = scan;
    let mut nullable = false;
    for (index, step) in steps.iter().enumerate() {
        if step.owner != owner {
            return Err(plan_invariant(
                "field path owner must match the active reference hop",
            ));
        }
        let field = catalogue
            .object_type_by_id(owner)
            .and_then(|object| object.field_by_id(step.field))
            .ok_or_else(|| plan_invariant("field path field must exist on its active owner"))?;
        nullable |= field.nullable();
        if index + 1 == steps.len() {
            if let Some(target) = field.resolved_type().reference_target()
                && catalogue.object_type_by_id(target).is_none()
            {
                return Err(plan_invariant(
                    "final reference field path target must be an active object type",
                ));
            }
            return Ok((field.resolved_type(), nullable));
        }
        let Some(target) = field.resolved_type().reference_target() else {
            return Err(plan_invariant(
                "each non-final field path hop must be an object reference",
            ));
        };
        owner = target;
    }
    Err(plan_invariant("field path must contain at least one field"))
}

fn validate_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_body_reference_evidence(
        active,
        function,
        &expected_body_references(plan),
        "reference count must match signature and plan traversal",
        "references must be ordered signature evidence followed by plan traversal",
    )
}

fn validate_identity_selected_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &IdentitySelectedServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_body_reference_evidence(
        active,
        function,
        &expected_identity_selected_body_references(plan),
        "recorded dependencies must match the function signature and query",
        "recorded dependencies must appear in the same order as the function signature and query",
    )
}

fn validate_distinct_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &DistinctServerPlan,
) -> Result<(), PostgresKernelError> {
    let expected = expected_unordered_body_references(
        plan.scan().object_type,
        plan.projections(),
        plan.selection(),
    );
    validate_function_reference_replay(active, function, &expected)
        .map_err(distinct_reference_error)
}

fn distinct_reference_error(mismatch: ReferenceReplayMismatch) -> PostgresKernelError {
    distinct_error(match mismatch {
        ReferenceReplayMismatch::Count => DISTINCT_REFERENCE_COUNT_RULE,
        ReferenceReplayMismatch::Sequence => DISTINCT_REFERENCE_SEQUENCE_RULE,
    })
}

fn validate_body_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    expected: &[ExpectedDefinitionReference],
    count_rule: &'static str,
    sequence_rule: &'static str,
) -> Result<(), PostgresKernelError> {
    validate_function_reference_replay(active, function, expected).map_err(|mismatch| {
        let rule = match mismatch {
            ReferenceReplayMismatch::Count => count_rule,
            ReferenceReplayMismatch::Sequence => sequence_rule,
        };
        reference_error(function.id(), rule)
    })
}

fn expected_identity_selected_body_references(
    plan: &IdentitySelectedServerPlan,
) -> Vec<ExpectedDefinitionReference> {
    let mut expected = vec![ExpectedDefinitionReference::new(
        DefinitionReferenceKind::QueryObject,
        DefinitionReferenceTarget::ObjectType(plan.scan().object_type),
    )];
    for projection in plan.projections() {
        add_expression_references(&mut expected, plan.scan().object_type, projection);
    }
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::ObjectReference,
        DefinitionReferenceTarget::ObjectType(plan.scan().object_type),
    ));
    let selector = plan.selector();
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::ParameterRead,
        DefinitionReferenceTarget::Parameter {
            owner: selector.owner(),
            parameter: selector.parameter(),
        },
    ));
    expected
}

fn expected_body_references(plan: &ServerPlan) -> Vec<ExpectedDefinitionReference> {
    let mut expected = expected_unordered_body_references(
        plan.scan.object_type,
        &plan.projections,
        plan.selection.as_ref(),
    );
    for ordering in &plan.ordering {
        add_expression_references(&mut expected, plan.scan.object_type, &ordering.expression);
    }
    expected
}

fn expected_unordered_body_references(
    scan: TypeId,
    projections: &[Expression],
    selection: Option<&Expression>,
) -> Vec<ExpectedDefinitionReference> {
    let mut expected = vec![ExpectedDefinitionReference::new(
        DefinitionReferenceKind::QueryObject,
        DefinitionReferenceTarget::ObjectType(scan),
    )];
    for expression in projections {
        add_expression_references(&mut expected, scan, expression);
    }
    if let Some(selection) = selection {
        add_expression_references(&mut expected, scan, selection);
    }
    expected
}

fn add_expression_references(
    expected: &mut Vec<ExpectedDefinitionReference>,
    scan: TypeId,
    expression: &Expression,
) {
    match &expression.kind {
        ExpressionKind::ObjectReference { .. } => {
            expected.push(ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(scan),
            ));
        }
        ExpressionKind::BooleanLiteral { .. } => {}
        ExpressionKind::FieldPath { steps, .. } => {
            for step in steps {
                expected.push(ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: step.owner,
                        field: step.field,
                    },
                ));
            }
        }
        ExpressionKind::Equality { left, right } => {
            add_expression_references(expected, scan, left);
            add_expression_references(expected, scan, right);
        }
    }
}

fn result_columns_for_projections(
    function: &FunctionDefinition,
    projections: &[Expression],
) -> Result<Vec<ResultColumn>, PostgresKernelError> {
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(plan_invariant("function return must be ROWS"));
    };
    columns
        .iter()
        .zip(projections)
        .map(|(column, projection)| {
            ResultColumn::new(
                column.name(),
                projection.value_type.resolved_type,
                projection.value_type.nullable,
            )
            .map_err(ServerSelectError::ResultRows)
            .map_err(server_error)
        })
        .collect()
}

struct LoweredPlan {
    sql: String,
    bind_types: Vec<Type>,
    binds: Vec<SelectBindValue>,
    guards: Vec<VariableGuard>,
    variable_payload_limit: usize,
}

#[derive(Clone, Debug, PartialEq)]
enum SelectBindValue {
    Boolean(bool),
    Bytes(Vec<u8>),
}

impl SelectBindValue {
    fn bind_type(&self) -> Type {
        match self {
            Self::Boolean(_) => Type::BOOL,
            Self::Bytes(_) => Type::BYTEA,
        }
    }

    fn as_to_sql(&self) -> &(dyn ToSql + Sync) {
        match self {
            Self::Boolean(value) => value,
            Self::Bytes(value) => value,
        }
    }
}

struct VariableGuard {
    column: usize,
    alias: String,
}

struct PartialLoweredSelect<'a> {
    lowerer: Lowerer<'a>,
    projections: Vec<String>,
    guards: Vec<VariableGuard>,
    variable_payload_limit: usize,
}

#[derive(Clone, Copy)]
struct RuntimeResultColumns<'a> {
    context: &'a CatalogueHashContext,
    columns: &'a [ResultColumn],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DuplicatePolicy {
    Preserve,
    Distinct,
}

impl DuplicatePolicy {
    const fn select_sql(self) -> &'static str {
        match self {
            Self::Preserve => "SELECT",
            Self::Distinct => "SELECT DISTINCT",
        }
    }
}

fn lower_plan(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    plan: &ServerPlan,
    columns: &[ResultColumn],
) -> Result<LoweredPlan, PostgresKernelError> {
    let result_columns = RuntimeResultColumns { context, columns };
    lower_parameter_free_plan(
        catalogue,
        plan.scan.object_type,
        &plan.projections,
        plan.selection.as_ref(),
        &plan.ordering,
        DuplicatePolicy::Preserve,
        result_columns,
    )
}

fn lower_distinct_plan(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    plan: &DistinctServerPlan,
    columns: &[ResultColumn],
) -> Result<LoweredPlan, PostgresKernelError> {
    let result_columns = RuntimeResultColumns { context, columns };
    lower_parameter_free_plan(
        catalogue,
        plan.scan().object_type,
        plan.projections(),
        plan.selection(),
        &[],
        DuplicatePolicy::Distinct,
        result_columns,
    )
}

fn lower_parameter_free_plan(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    scan: TypeId,
    projections: &[Expression],
    selection: Option<&Expression>,
    ordering: &[Ordering],
    duplicate_policy: DuplicatePolicy,
    result_columns: RuntimeResultColumns<'_>,
) -> Result<LoweredPlan, PostgresKernelError> {
    let mut lowered = lower_select_projections(catalogue, result_columns, scan, projections)?;
    let selection = selection
        .map(|expression| lowered.lowerer.expression(expression))
        .transpose()?;
    let mut lowered_ordering = Vec::with_capacity(ordering.len());
    for item in ordering {
        let direction = ordering_sql(item.direction);
        lowered_ordering.push(format!(
            "{} {direction}",
            lowered.lowerer.expression(&item.expression)?
        ));
    }
    let mut suffix = String::new();
    if let Some(selection) = selection {
        suffix.push_str("\nWHERE ");
        suffix.push_str(&selection);
    }
    if !lowered_ordering.is_empty() {
        suffix.push_str("\nORDER BY ");
        suffix.push_str(&lowered_ordering.join(", "));
    }
    let limit = effective_query_limit(projections.len())?;
    suffix.push_str(&format!("\nLIMIT {limit}"));
    finish_lowered_select(lowered, duplicate_policy, &suffix)
}

fn lower_identity_selected_plan(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    context: &CatalogueHashContext,
    plan: &IdentitySelectedServerPlan,
    columns: &[ResultColumn],
    selector: ObjectId,
) -> Result<LoweredPlan, PostgresKernelError> {
    let scan = plan.scan();
    let result_columns = RuntimeResultColumns { context, columns };
    let mut lowered = lower_select_projections(
        catalogue,
        result_columns,
        scan.object_type,
        plan.projections(),
    )?;
    lowered
        .lowerer
        .binds
        .push(SelectBindValue::Bytes(selector.to_bytes().to_vec()));
    let selector_placeholder = lowered.lowerer.binds.len();
    let suffix = format!("\nWHERE i0.{OBJECT_ID_COLUMN} = ${selector_placeholder}\nLIMIT 2");
    finish_lowered_select(lowered, DuplicatePolicy::Preserve, &suffix)
}

fn lower_select_projections<'a>(
    catalogue: &'a orna_core::catalogue::CatalogueSnapshot,
    result_columns: RuntimeResultColumns<'_>,
    scan: TypeId,
    expressions: &[Expression],
) -> Result<PartialLoweredSelect<'a>, PostgresKernelError> {
    let context = result_columns.context;
    let columns = result_columns.columns;
    let mut lowerer = Lowerer {
        catalogue,
        scan,
        joins: BTreeMap::new(),
        join_sql: Vec::new(),
        binds: Vec::new(),
        field_path_steps: 0,
    };
    let variable_payload_limit = variable_payload_limit(catalogue, context, columns)?;
    let mut projections = Vec::with_capacity(expressions.len());
    let mut guard_projections = Vec::new();
    let mut guards = Vec::new();
    for (index, expression) in expressions.iter().enumerate() {
        let expression = lowerer.expression(expression)?;
        if is_variable_type(catalogue, context, columns[index].resolved_type()) {
            let alias = format!("g{}", guards.len());
            projections.push(format!(
                "CASE WHEN octet_length({expression}) <= {variable_payload_limit} THEN {expression} ELSE NULL END AS c{index}"
            ));
            guards.push(VariableGuard {
                column: index,
                alias: alias.clone(),
            });
            guard_projections.push(format!(
                "CASE WHEN {expression} IS NULL OR octet_length({expression}) <= {variable_payload_limit} THEN TRUE ELSE FALSE END AS {alias}"
            ));
        } else {
            projections.push(format!("{expression} AS c{index}"));
        }
    }
    projections.extend(guard_projections);
    Ok(PartialLoweredSelect {
        lowerer,
        projections,
        guards,
        variable_payload_limit,
    })
}

fn finish_lowered_select(
    lowered: PartialLoweredSelect<'_>,
    duplicate_policy: DuplicatePolicy,
    suffix: &str,
) -> Result<LoweredPlan, PostgresKernelError> {
    let mut sql = format!(
        "{} {}\nFROM {}.{} AS i0",
        duplicate_policy.select_sql(),
        lowered.projections.join(", "),
        DATA_SCHEMA,
        relation_name(lowered.lowerer.scan),
    );
    for join in &lowered.lowerer.join_sql {
        sql.push('\n');
        sql.push_str(join);
    }
    sql.push_str(suffix);
    if sql.len() > SQL_LIMIT {
        return Err(server_error(ServerSelectError::ComplexityLimit {
            category: "generated SQL bytes",
            maximum: SQL_LIMIT,
        }));
    }
    Ok(LoweredPlan {
        sql,
        bind_types: lowered
            .lowerer
            .binds
            .iter()
            .map(SelectBindValue::bind_type)
            .collect(),
        binds: lowered.lowerer.binds,
        guards: lowered.guards,
        variable_payload_limit: lowered.variable_payload_limit,
    })
}

// Version 1 fixes Orna's unspecified ordering independently of the PostgreSQL
// defaults, so every generated term names its null rule.
const fn ordering_sql(direction: SortDirection) -> &'static str {
    match direction {
        SortDirection::Unspecified | SortDirection::Ascending => "ASC NULLS LAST",
        SortDirection::Descending => "DESC NULLS FIRST",
    }
}

fn is_variable_type(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> bool {
    matches!(
        resolve_catalogue_runtime_type(catalogue, context, resolved_type),
        ResolvedRuntimeType::CatalogueEnum(_)
    ) || matches!(
        resolve_runtime_type(context, resolved_type).compatibility_scalar(),
        Some(StandardScalar::CharacterLargeObject | StandardScalar::BinaryLargeObject)
    )
}

fn variable_payload_limit(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    columns: &[ResultColumn],
) -> Result<usize, PostgresKernelError> {
    let names = initial_payload_len(columns)?;
    let fixed = columns
        .iter()
        .filter(|column| !is_variable_type(catalogue, context, column.resolved_type()))
        .try_fold(0usize, |total, column| {
            total
                .checked_add(maximum_fixed_payload_len(
                    catalogue,
                    context,
                    column.resolved_type(),
                ))
                .ok_or_else(|| {
                    server_error(ServerSelectError::PayloadLimit {
                        maximum: PAYLOAD_LIMIT,
                    })
                })
        })?;
    let available = PAYLOAD_LIMIT
        .checked_sub(names)
        .and_then(|available| available.checked_sub(fixed))
        .ok_or_else(|| {
            server_error(ServerSelectError::PayloadLimit {
                maximum: PAYLOAD_LIMIT,
            })
        })?;
    let variable_count = columns
        .iter()
        .filter(|column| is_variable_type(catalogue, context, column.resolved_type()))
        .count();
    if variable_count == 0 {
        return Ok(0);
    }
    Ok(available / variable_count)
}

fn maximum_fixed_payload_len(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> usize {
    match resolve_catalogue_runtime_type(catalogue, context, resolved_type) {
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::Boolean) => 1,
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::Integer) => 4,
        runtime
            if matches!(
                runtime.compatibility_scalar(),
                Some(StandardScalar::BigInt | StandardScalar::Float)
            ) =>
        {
            8
        }
        ResolvedRuntimeType::Reference(_) => 16,
        ResolvedRuntimeType::CatalogueEnum(_) => 0,
        ResolvedRuntimeType::LegacyScalar(_)
        | ResolvedRuntimeType::VerifiedValue { .. }
        | ResolvedRuntimeType::Unsupported => 0,
    }
}

fn effective_query_limit(projection_count: usize) -> Result<usize, PostgresKernelError> {
    let cell_rows = CELL_LIMIT
        .checked_div(projection_count)
        .ok_or_else(|| plan_invariant("server plan must contain at least one projection"))?;
    let effective = ROW_LIMIT.min(cell_rows);
    effective
        .checked_add(1)
        .ok_or_else(|| plan_invariant("effective server row limit must fit usize"))
}

struct Lowerer<'a> {
    catalogue: &'a orna_core::catalogue::CatalogueSnapshot,
    scan: TypeId,
    joins: BTreeMap<Vec<(TypeId, FieldId)>, String>,
    join_sql: Vec<String>,
    binds: Vec<SelectBindValue>,
    field_path_steps: usize,
}

impl Lowerer<'_> {
    fn expression(&mut self, expression: &Expression) -> Result<String, PostgresKernelError> {
        match &expression.kind {
            ExpressionKind::ObjectReference { .. } => Ok(format!("i0.{OBJECT_ID_COLUMN}")),
            ExpressionKind::FieldPath { steps, .. } => self.field_path(steps),
            ExpressionKind::BooleanLiteral { value } => {
                if self.binds.len() == server_plan::MAX_EXPRESSION_NODES as usize {
                    return Err(server_error(ServerSelectError::ComplexityLimit {
                        category: "boolean binds",
                        maximum: server_plan::MAX_EXPRESSION_NODES as usize,
                    }));
                }
                self.binds.push(SelectBindValue::Boolean(*value));
                Ok(format!("${}", self.binds.len()))
            }
            ExpressionKind::Equality { left, right } => Ok(format!(
                "({} = {})",
                self.expression(left)?,
                self.expression(right)?,
            )),
        }
    }

    fn field_path(&mut self, steps: &[FieldStep]) -> Result<String, PostgresKernelError> {
        let mut owner = self.scan;
        let mut alias = String::from("i0");
        let mut prefix = Vec::new();
        let mut nullable = false;
        for (index, step) in steps.iter().enumerate() {
            self.field_path_steps = self.field_path_steps.checked_add(1).ok_or_else(|| {
                server_error(ServerSelectError::ComplexityLimit {
                    category: "field path steps",
                    maximum: FIELD_PATH_STEP_LIMIT,
                })
            })?;
            if self.field_path_steps > FIELD_PATH_STEP_LIMIT {
                return Err(server_error(ServerSelectError::ComplexityLimit {
                    category: "field path steps",
                    maximum: FIELD_PATH_STEP_LIMIT,
                }));
            }
            let field = self
                .catalogue
                .object_type_by_id(owner)
                .and_then(|object| object.field_by_id(step.field))
                .ok_or_else(|| plan_invariant("field path field must exist while lowering"))?;
            if index + 1 == steps.len() {
                return Ok(format!("{alias}.{}", field_name(step.field)));
            }
            let Some(target) = field.resolved_type().reference_target() else {
                return Err(plan_invariant(
                    "non-final lowered field path hop must be a reference",
                ));
            };
            prefix.push((step.owner, step.field));
            nullable |= field.nullable();
            let prefix_alias = if let Some(alias) = self.joins.get(&prefix) {
                alias.clone()
            } else {
                if self.joins.len() == JOIN_LIMIT {
                    return Err(server_error(ServerSelectError::ComplexityLimit {
                        category: "unique joins",
                        maximum: JOIN_LIMIT,
                    }));
                }
                let joined = format!("j{}", self.joins.len());
                let join = if nullable { "LEFT JOIN" } else { "JOIN" };
                self.join_sql.push(format!(
                    "{join} {}.{} AS {joined} ON {alias}.{} = {joined}.{OBJECT_ID_COLUMN}",
                    DATA_SCHEMA,
                    relation_name(target),
                    field_name(step.field),
                ));
                self.joins.insert(prefix.clone(), joined.clone());
                joined
            };
            alias = prefix_alias;
            owner = target;
        }
        Err(plan_invariant(
            "field path must contain at least one field while lowering",
        ))
    }
}

fn validate_prepared_columns(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    statement: &Statement,
    expected: &[ResultColumn],
    guards: &[VariableGuard],
) -> Result<(), PostgresKernelError> {
    if statement.columns().len() != expected.len() + guards.len() {
        return Err(server_error(ServerSelectError::PreparedResult {
            rule: "prepared result column count must equal declared ROWS shape",
        }));
    }
    for (index, (column, expected)) in statement.columns().iter().zip(expected).enumerate() {
        if column.name() != format!("c{index}")
            || *column.type_()
                != expected_postgres_type(catalogue, context, expected.resolved_type())?
        {
            return Err(server_error(ServerSelectError::PreparedResult {
                rule: "prepared result column name and PostgreSQL type must match generated shape",
            }));
        }
    }
    for (column, guard) in statement.columns()[expected.len()..].iter().zip(guards) {
        if column.name() != guard.alias || *column.type_() != Type::BOOL {
            return Err(server_error(ServerSelectError::PreparedResult {
                rule: "prepared variable payload guards must use generated BOOLEAN columns",
            }));
        }
    }
    Ok(())
}

fn expected_postgres_type(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> Result<Type, PostgresKernelError> {
    postgres_type(resolve_catalogue_runtime_type(
        catalogue,
        context,
        resolved_type,
    ))
    .ok_or_else(|| {
        server_error(ServerSelectError::PreparedResult {
            rule: "result type is outside the initial runtime subset",
        })
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResultCardinality {
    BoundedMany,
    AtMostOne,
}

impl ResultCardinality {
    fn validate(self, row_count: usize) -> Result<(), PostgresKernelError> {
        match self {
            Self::BoundedMany => Ok(()),
            Self::AtMostOne => validate_identity_selected_cardinality(row_count),
        }
    }
}

struct ResultReadShape<'a> {
    catalogue: &'a CatalogueSnapshot,
    context: &'a CatalogueHashContext,
    columns: &'a [ResultColumn],
    guards: &'a [VariableGuard],
    variable_payload_limit: usize,
    cardinality: ResultCardinality,
}

async fn stream_rows(
    transaction: &Transaction<'_>,
    statement: &Statement,
    binds: &[SelectBindValue],
    shape: ResultReadShape<'_>,
) -> Result<ResultRows, PostgresKernelError> {
    let parameters = binds
        .iter()
        .map(SelectBindValue::as_to_sql)
        .collect::<Vec<_>>();
    let stream = transaction
        .query_raw(statement, parameters)
        .await
        .map_err(PostgresKernelError::Database)?;
    futures_util::pin_mut!(stream);
    let mut rows = Vec::new();
    let mut cells = 0usize;
    let mut payload = initial_payload_len(shape.columns)?;
    while let Some(row) = stream
        .try_next()
        .await
        .map_err(PostgresKernelError::Database)?
    {
        shape.cardinality.validate(rows.len().saturating_add(1))?;
        if rows.len() == ROW_LIMIT {
            return Err(server_error(ServerSelectError::RowLimit {
                maximum: ROW_LIMIT,
            }));
        }
        cells = cells.checked_add(shape.columns.len()).ok_or_else(|| {
            server_error(ServerSelectError::CellLimit {
                maximum: CELL_LIMIT,
            })
        })?;
        if cells > CELL_LIMIT {
            return Err(server_error(ServerSelectError::CellLimit {
                maximum: CELL_LIMIT,
            }));
        }
        let row_index = rows.len();
        for (guard_index, guard) in shape.guards.iter().enumerate() {
            let accepted = row
                .try_get::<usize, bool>(shape.columns.len() + guard_index)
                .map_err(|source| {
                    server_error(ServerSelectError::RowDecode {
                        row: row_index,
                        column: shape.columns.len() + guard_index,
                        source,
                    })
                })?;
            if !accepted {
                return Err(server_error(ServerSelectError::VariablePayload {
                    row: row_index,
                    column: guard.column,
                    maximum: shape.variable_payload_limit,
                }));
            }
        }
        let mut values = Vec::with_capacity(shape.columns.len());
        for (column_index, column) in shape.columns.iter().enumerate() {
            let value = decode_value(
                shape.catalogue,
                shape.context,
                &row,
                row_index,
                column_index,
                column,
            )?;
            payload = add_payload(payload, logical_payload_len(&value)?)?;
            values.push(value);
        }
        rows.push(ResultRow::new(values));
    }
    ResultRows::new(shape.columns.to_vec(), rows)
        .map_err(ServerSelectError::ResultRows)
        .map_err(server_error)
}

fn validate_identity_selected_cardinality(row_count: usize) -> Result<(), PostgresKernelError> {
    if row_count > 1 {
        return Err(server_error(ServerSelectError::Cardinality {
            rule: "more than one row was returned for the requested object",
        }));
    }
    Ok(())
}

fn initial_payload_len(columns: &[ResultColumn]) -> Result<usize, PostgresKernelError> {
    columns.iter().try_fold(0usize, |payload, column| {
        add_payload(payload, column.name().len())
    })
}

fn add_payload(payload: usize, additional: usize) -> Result<usize, PostgresKernelError> {
    let payload = payload.checked_add(additional).ok_or_else(|| {
        server_error(ServerSelectError::PayloadLimit {
            maximum: PAYLOAD_LIMIT,
        })
    })?;
    if payload > PAYLOAD_LIMIT {
        return Err(server_error(ServerSelectError::PayloadLimit {
            maximum: PAYLOAD_LIMIT,
        }));
    }
    Ok(payload)
}

fn decode_value(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    row: &Row,
    row_index: usize,
    column_index: usize,
    column: &ResultColumn,
) -> Result<RuntimeValue, PostgresKernelError> {
    macro_rules! decode {
        ($type:ty, $value:expr) => {
            row.try_get::<usize, Option<$type>>(column_index)
                .map_err(|source| {
                    server_error(ServerSelectError::RowDecode {
                        row: row_index,
                        column: column_index,
                        source,
                    })
                })?
                .map($value)
                .transpose()?
        };
    }
    let resolved_type = column.resolved_type();
    let value = match resolve_catalogue_runtime_type(catalogue, context, resolved_type) {
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::Boolean) => {
            decode!(bool, |value| Ok(RuntimeValue::Boolean(value)))
        }
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::Integer) => {
            decode!(i32, |value| Ok(RuntimeValue::Integer(value)))
        }
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::BigInt) => {
            decode!(i64, |value| Ok(RuntimeValue::BigInt(value)))
        }
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::Float) => {
            decode!(f64, |value| {
                RuntimeFloat::new(value)
                    .map(RuntimeValue::Float)
                    .map_err(ServerSelectError::ResultRows)
                    .map_err(server_error)
            })
        }
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::CharacterLargeObject) => {
            decode!(String, |value| Ok(RuntimeValue::Text(value)))
        }
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::BinaryLargeObject) => {
            decode!(Vec<u8>, |value| Ok(RuntimeValue::Bytes(value)))
        }
        ResolvedRuntimeType::Reference(target) => decode!(Vec<u8>, |value| {
            let object = value.try_into().map(ObjectId::from_bytes).map_err(|_| {
                server_error(ServerSelectError::ValueInvariant {
                    row: row_index,
                    column: column_index,
                    rule: "reference result values must contain exactly 16 bytes",
                })
            })?;
            Ok(RuntimeValue::Reference { target, object })
        }),
        ResolvedRuntimeType::CatalogueEnum(enum_type) => decode!(String, |value| {
            EnumValue::new(catalogue, enum_type, value)
                .map(RuntimeValue::Enum)
                .map_err(|_| {
                    server_error(ServerSelectError::ValueInvariant {
                        row: row_index,
                        column: column_index,
                        rule: "enum result must contain one exact label declared by the active enum type",
                    })
                })
        }),
        ResolvedRuntimeType::LegacyScalar(_)
        | ResolvedRuntimeType::VerifiedValue { .. }
        | ResolvedRuntimeType::Unsupported => {
            return Err(server_error(ServerSelectError::PreparedResult {
                rule: "result value type is outside the initial runtime subset",
            }));
        }
    };
    match value {
        Some(value) => Ok(value),
        None => RuntimeValue::null(resolved_type)
            .map_err(ServerSelectError::ResultRows)
            .map_err(server_error),
    }
}

fn logical_payload_len(value: &RuntimeValue) -> Result<usize, PostgresKernelError> {
    Ok(match value {
        RuntimeValue::Null(_) => 0,
        RuntimeValue::Boolean(_) => 1,
        RuntimeValue::Integer(_) => 4,
        RuntimeValue::BigInt(_) | RuntimeValue::Float(_) => 8,
        RuntimeValue::Text(value) => value.len(),
        RuntimeValue::Bytes(value) => value.len(),
        RuntimeValue::Reference { .. } => 16,
        RuntimeValue::Enum(value) => value.label().len(),
        _ => {
            return Err(server_error(ServerSelectError::ValueInvariant {
                row: 0,
                column: 0,
                rule: "unknown future RuntimeValue variants cannot contribute zero payload",
            }));
        }
    })
}

fn server_error(error: ServerSelectError) -> PostgresKernelError {
    PostgresKernelError::ServerSelect(error)
}

fn contextualize(context: ServerSelectContext, error: PostgresKernelError) -> PostgresKernelError {
    match error {
        PostgresKernelError::Database(source) => execution_database(context, source),
        PostgresKernelError::ServerSelect(source) => server_error(ServerSelectError::Execution {
            context,
            source: Box::new(source),
        }),
        error => server_error(ServerSelectError::Execution {
            context,
            source: Box::new(ServerSelectError::Kernel {
                source: Box::new(error),
            }),
        }),
    }
}

fn execution_database(
    context: ServerSelectContext,
    source: tokio_postgres::Error,
) -> PostgresKernelError {
    server_error(ServerSelectError::Database { context, source })
}

fn context_from_result(result: &ServerSelectResult) -> ServerSelectContext {
    ServerSelectContext::new(result.pair(), result.function(), result.function_revision())
}

fn execution_context(error: &PostgresKernelError) -> Option<ServerSelectContext> {
    let PostgresKernelError::ServerSelect(error) = error else {
        return None;
    };
    match error {
        ServerSelectError::Execution { context, .. }
        | ServerSelectError::Database { context, .. } => Some(*context),
        _ => None,
    }
}

fn plan_invariant(rule: &'static str) -> PostgresKernelError {
    server_error(ServerSelectError::PlanInvariant { rule })
}

fn reference_error(function: FunctionId, rule: &'static str) -> PostgresKernelError {
    server_error(ServerSelectError::ReferenceEvidence { function, rule })
}

#[cfg(test)]
mod tests {
    use orna_artifact::server_plan::{IdentitySelector, Scan, ValueType};
    use orna_core::{
        CatalogueRevisionId, FieldId, ParameterId, SchemaId,
        catalogue::{
            CatalogueSnapshot, FieldDefinition, FunctionReturnColumnDefinition,
            ObjectTypeDefinition, ParameterDefinition, QualifiedSemanticName, SchemaDefinition,
        },
        revision::CatalogueHashContext,
    };

    use super::*;

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }

    fn catalogue() -> (CatalogueSnapshot, TypeId, FieldId, FieldId) {
        let source = TypeId::from_bytes([0x10; 16]);
        let target = TypeId::from_bytes([0x20; 16]);
        let reference = FieldId::from_bytes([0x11; 16]);
        let value = FieldId::from_bytes([0x21; 16]);
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x01; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x02; 16]),
                name(&["test"]),
            )],
            vec![
                ObjectTypeDefinition::new(
                    source,
                    name(&["test", "semantic_source"]),
                    vec![FieldDefinition::new(
                        reference,
                        "semantic_reference",
                        0,
                        ResolvedType::reference(target),
                        true,
                        false,
                        None,
                        None,
                    )],
                ),
                ObjectTypeDefinition::new(
                    target,
                    name(&["test", "semantic_target"]),
                    vec![FieldDefinition::new(
                        value,
                        "semantic_value",
                        0,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                        false,
                        false,
                        None,
                        None,
                    )],
                ),
            ],
        )
        .unwrap();
        (catalogue, source, reference, value)
    }

    fn nullable_text_path(
        source: TypeId,
        reference: FieldId,
        target: TypeId,
        value: FieldId,
    ) -> Expression {
        Expression {
            kind: ExpressionKind::FieldPath {
                input: 0,
                steps: vec![
                    FieldStep {
                        owner: source,
                        field: reference,
                    },
                    FieldStep {
                        owner: target,
                        field: value,
                    },
                ],
            },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                nullable: true,
            },
        }
    }

    fn retained_value_context(contract: &str) -> (CatalogueHashContext, TypeId) {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot()
                .expect("retained standard-library snapshot"),
        )
        .expect("verified standard-library snapshot");
        let value_type = standard
            .catalogue()
            .value_types()
            .iter()
            .find(|definition| definition.representation_contract() == contract)
            .expect("retained value type")
            .id();
        (CatalogueHashContext::version_two(standard), value_type)
    }

    fn catalogue_with_value_field(value_type: TypeId) -> (CatalogueSnapshot, TypeId, FieldId) {
        let source = TypeId::from_bytes([0x70; 16]);
        let field = FieldId::from_bytes([0x71; 16]);
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x72; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x73; 16]),
                name(&["value_test"]),
            )],
            vec![ObjectTypeDefinition::new(
                source,
                name(&["value_test", "source"]),
                vec![FieldDefinition::new(
                    field,
                    "value",
                    0,
                    ResolvedType::value(value_type),
                    false,
                    false,
                    None,
                    None,
                )],
            )],
        )
        .expect("value catalogue");
        (catalogue, source, field)
    }

    fn field_projection(source: TypeId, field: FieldId, scalar: StandardScalar) -> Expression {
        Expression {
            kind: ExpressionKind::FieldPath {
                input: 0,
                steps: vec![FieldStep {
                    owner: source,
                    field,
                }],
            },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(scalar),
                nullable: false,
            },
        }
    }

    fn function(
        domain: FunctionDomain,
        parameters: Vec<orna_core::catalogue::ParameterDefinition>,
        return_type: FunctionReturn,
        security: FunctionSecurity,
        transaction: Option<FunctionTransaction>,
    ) -> FunctionDefinition {
        function_with_volatility(
            domain,
            parameters,
            return_type,
            security,
            transaction,
            FunctionVolatility::Stable,
        )
    }

    fn function_with_volatility(
        domain: FunctionDomain,
        parameters: Vec<orna_core::catalogue::ParameterDefinition>,
        return_type: FunctionReturn,
        security: FunctionSecurity,
        transaction: Option<FunctionTransaction>,
        volatility: FunctionVolatility,
    ) -> FunctionDefinition {
        FunctionDefinition::new(
            FunctionId::from_bytes([0x31; 16]),
            name(&["test", "function"]),
            domain,
            parameters,
            return_type,
            FunctionRevisionId::from_bytes([0x32; 16]),
            security,
            transaction,
            volatility,
        )
    }

    fn rows_return() -> FunctionReturn {
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "value",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
        )])
    }

    fn boolean_rows_return() -> FunctionReturn {
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "selected",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
        )])
    }

    fn assert_signature_rule(
        result: Result<(), PostgresKernelError>,
        function: FunctionId,
        expected: &'static str,
    ) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::FunctionSignature {
            function: actual_function,
            rule,
        })) = result
        else {
            panic!("expected a function-signature rejection");
        };
        assert_eq!(actual_function, function);
        assert_eq!(rule, expected);
    }

    fn assert_plan_rule(result: Result<(), PostgresKernelError>, expected: &'static str) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::PlanInvariant { rule })) =
            result
        else {
            panic!("expected a saved-query rejection");
        };
        assert_eq!(rule, expected);
    }

    fn assert_distinct_rule<T>(result: Result<T, PostgresKernelError>, expected: &'static str) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::Distinct { rule })) = result
        else {
            panic!("expected a SELECT DISTINCT rejection");
        };
        assert_eq!(rule, expected);
    }

    fn assert_argument_rule<T>(
        result: Result<T, PostgresKernelError>,
        parameter: Option<ParameterId>,
        expected: &'static str,
    ) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::Argument {
            parameter: actual_parameter,
            rule,
        })) = result
        else {
            panic!("expected a function-argument rejection");
        };
        assert_eq!(actual_parameter, parameter);
        assert_eq!(rule, expected);
    }

    #[test]
    fn lowerer_uses_identity_names_cached_nullable_joins_and_boolean_binds() {
        let (catalogue, source, reference, value) = catalogue();
        let context = CatalogueHashContext::version_one();
        let target = TypeId::from_bytes([0x20; 16]);
        let path = nullable_text_path(source, reference, target, value);
        let plan = ServerPlan {
            scan: Scan {
                input: 0,
                object_type: source,
            },
            projections: vec![path.clone(), path.clone()],
            selection: Some(Expression {
                kind: ExpressionKind::BooleanLiteral { value: true },
                value_type: ValueType {
                    resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                    nullable: false,
                },
            }),
            ordering: vec![server_plan::Ordering {
                expression: path,
                direction: SortDirection::Descending,
                null_order: server_plan::NullOrder::Unspecified,
            }],
        };

        let columns = [
            ResultColumn::new(
                "first",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                true,
            )
            .unwrap(),
            ResultColumn::new(
                "second",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                true,
            )
            .unwrap(),
        ];
        let lowered = lower_plan(&catalogue, &context, &plan, &columns).unwrap();

        assert_eq!(lowered.binds, vec![SelectBindValue::Boolean(true)]);
        assert_eq!(lowered.sql.matches("LEFT JOIN").count(), 1);
        assert!(
            lowered
                .sql
                .contains("CASE WHEN octet_length(j0.f_21212121212121212121212121212121) <=")
        );
        assert!(lowered.sql.contains("AS c0, CASE WHEN octet_length"));
        assert!(lowered.sql.contains("AS c1, CASE WHEN"));
        assert!(lowered.sql.contains("AS g0, CASE WHEN"));
        assert!(lowered.sql.contains("AS g1"));
        assert_eq!(lowered.guards.len(), 2);
        assert!(lowered.sql.contains("WHERE $1"));
        assert!(
            lowered
                .sql
                .contains("ORDER BY j0.f_21212121212121212121212121212121 DESC NULLS FIRST")
        );
        assert!(lowered.sql.ends_with("LIMIT 10001"));
        assert!(!lowered.sql.contains("semantic_source"));
        assert!(!lowered.sql.contains("semantic_reference"));
        assert!(!lowered.sql.contains("semantic_target"));
        assert!(!lowered.sql.contains("semantic_value"));
    }

    #[test]
    fn identity_selected_lowering_keeps_projection_bind_order_and_appends_selector() {
        let (catalogue, source, _, _) = catalogue();
        let context = CatalogueHashContext::version_one();
        let function = FunctionId::from_bytes([0x31; 16]);
        let parameter = ParameterId::from_bytes([0x33; 16]);
        let plan = IdentitySelectedServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [Expression {
                kind: ExpressionKind::BooleanLiteral { value: true },
                value_type: ValueType {
                    resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                    nullable: false,
                },
            }],
            IdentitySelector::new(function, parameter),
        )
        .unwrap();
        let columns = [ResultColumn::new(
            "selected",
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
        )
        .unwrap()];
        let object = ObjectId::from_bytes([0x41; 16]);
        let lowered =
            lower_identity_selected_plan(&catalogue, &context, &plan, &columns, object).unwrap();

        assert_eq!(
            lowered.binds,
            vec![
                SelectBindValue::Boolean(true),
                SelectBindValue::Bytes(object.to_bytes().to_vec()),
            ]
        );
        assert_eq!(lowered.bind_types, vec![Type::BOOL, Type::BYTEA]);
        assert!(lowered.sql.contains("SELECT $1 AS c0"));
        assert!(lowered.sql.contains("WHERE i0._orna_object_id = $2"));
        assert!(lowered.sql.ends_with("LIMIT 2"));
        assert!(!lowered.sql.contains("semantic_source"));
    }

    #[test]
    fn distinct_lowering_changes_only_the_select_policy_and_adds_no_bind() {
        let (catalogue, source, _, _) = catalogue();
        let context = CatalogueHashContext::version_one();
        let projection = Expression {
            kind: ExpressionKind::BooleanLiteral { value: true },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: false,
            },
        };
        let selection = Expression {
            kind: ExpressionKind::BooleanLiteral { value: false },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: false,
            },
        };
        let plan = DistinctServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [projection.clone()],
            Some(selection.clone()),
        )
        .unwrap();
        let version_one = ServerPlan {
            scan: plan.scan(),
            projections: vec![projection],
            selection: Some(selection),
            ordering: Vec::new(),
        };
        let columns = [ResultColumn::new(
            "selected",
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
        )
        .unwrap()];

        let distinct = lower_distinct_plan(&catalogue, &context, &plan, &columns).unwrap();
        let preserving = lower_plan(&catalogue, &context, &version_one, &columns).unwrap();
        assert_eq!(
            distinct.sql,
            format!(
                "SELECT DISTINCT $1 AS c0\nFROM {}.{} AS i0\nWHERE $2\nLIMIT 10001",
                DATA_SCHEMA,
                relation_name(source),
            )
        );
        assert_eq!(
            distinct.sql,
            preserving.sql.replacen("SELECT ", "SELECT DISTINCT ", 1)
        );
        assert_eq!(
            distinct.binds,
            vec![
                SelectBindValue::Boolean(true),
                SelectBindValue::Boolean(false),
            ]
        );
        assert_eq!(distinct.bind_types, vec![Type::BOOL, Type::BOOL]);
    }

    #[test]
    fn artifact_versions_decode_only_their_matching_plan_model() {
        let (_, source, _, _) = catalogue();
        let projection = Expression {
            kind: ExpressionKind::BooleanLiteral { value: true },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: false,
            },
        };
        let v1 = ServerPlan {
            scan: Scan {
                input: 0,
                object_type: source,
            },
            projections: vec![projection.clone()],
            selection: None,
            ordering: Vec::new(),
        }
        .encode()
        .unwrap();
        let v2 = IdentitySelectedServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [projection.clone()],
            IdentitySelector::new(
                FunctionId::from_bytes([0x31; 16]),
                ParameterId::from_bytes([0x33; 16]),
            ),
        )
        .unwrap()
        .encode()
        .unwrap();
        let v3 = DistinctServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [projection],
            None,
        )
        .unwrap()
        .encode()
        .unwrap();
        let function = FunctionId::from_bytes([0x31; 16]);

        assert!(matches!(
            decode_plan(function, SERVER_PLAN_FORMAT, SERVER_PLAN_VERSION, &v1),
            Ok(DecodedServerPlan::V1(_))
        ));
        assert!(matches!(
            decode_plan(
                function,
                SERVER_PLAN_FORMAT,
                IDENTITY_SELECTED_SERVER_PLAN_VERSION,
                &v2,
            ),
            Ok(DecodedServerPlan::V2(_))
        ));
        assert!(matches!(
            decode_plan(
                function,
                SERVER_PLAN_FORMAT,
                DISTINCT_SERVER_PLAN_VERSION,
                &v3,
            ),
            Ok(DecodedServerPlan::V3(_))
        ));
        assert!(matches!(
            decode_plan(
                function,
                SERVER_PLAN_FORMAT,
                IDENTITY_SELECTED_SERVER_PLAN_VERSION,
                &v1,
            ),
            Err(PostgresKernelError::ServerSelect(
                ServerSelectError::PlanDecode(server_plan::ServerPlanError::UnsupportedVersion(
                    SERVER_PLAN_VERSION
                ))
            ))
        ));
        assert!(matches!(
            decode_plan(function, SERVER_PLAN_FORMAT, SERVER_PLAN_VERSION, &v2),
            Err(PostgresKernelError::ServerSelect(
                ServerSelectError::PlanDecode(server_plan::ServerPlanError::UnsupportedVersion(
                    IDENTITY_SELECTED_SERVER_PLAN_VERSION
                ))
            ))
        ));
        assert!(matches!(
            decode_plan(
                function,
                SERVER_PLAN_FORMAT,
                DISTINCT_SERVER_PLAN_VERSION,
                &v1,
            ),
            Err(PostgresKernelError::ServerSelect(
                ServerSelectError::PlanDecode(server_plan::ServerPlanError::UnsupportedVersion(
                    SERVER_PLAN_VERSION
                ))
            ))
        ));
        assert!(matches!(
            decode_plan(
                function,
                SERVER_PLAN_FORMAT,
                DISTINCT_SERVER_PLAN_VERSION,
                &v2,
            ),
            Err(PostgresKernelError::ServerSelect(
                ServerSelectError::PlanDecode(server_plan::ServerPlanError::UnsupportedVersion(
                    IDENTITY_SELECTED_SERVER_PLAN_VERSION
                ))
            ))
        ));
        assert!(matches!(
            decode_plan(function, SERVER_PLAN_FORMAT, SERVER_PLAN_VERSION, &v3),
            Err(PostgresKernelError::ServerSelect(
                ServerSelectError::PlanDecode(server_plan::ServerPlanError::UnsupportedVersion(
                    DISTINCT_SERVER_PLAN_VERSION
                ))
            ))
        ));
        assert!(matches!(
            decode_plan(
                function,
                SERVER_PLAN_FORMAT,
                IDENTITY_SELECTED_SERVER_PLAN_VERSION,
                &v3,
            ),
            Err(PostgresKernelError::ServerSelect(
                ServerSelectError::PlanDecode(server_plan::ServerPlanError::UnsupportedVersion(
                    DISTINCT_SERVER_PLAN_VERSION
                ))
            ))
        ));
        assert!(matches!(
            decode_plan(function, "unknown", SERVER_PLAN_VERSION, &v1),
            Err(PostgresKernelError::ServerSelect(ServerSelectError::Artifact {
                function: actual,
                rule: "current SERVER artifact must use orna.server-plan",
            })) if actual == function
        ));
        assert!(matches!(
            decode_plan(function, SERVER_PLAN_FORMAT, 99, &v1),
            Err(PostgresKernelError::ServerSelect(ServerSelectError::Artifact {
                function: actual,
                rule: "current SERVER artifact must use supported orna.server-plan version 1, version 2, or version 3",
            })) if actual == function
        ));
    }

    #[test]
    fn distinct_decode_maps_only_human_actionable_v3_failures() {
        let (_, source, reference, value) = catalogue();
        let target = TypeId::from_bytes([0x20; 16]);
        let function = FunctionId::from_bytes([0x31; 16]);
        let mut unsupported_projection = ServerPlan {
            scan: Scan {
                input: 0,
                object_type: source,
            },
            projections: vec![nullable_text_path(source, reference, target, value)],
            selection: None,
            ordering: Vec::new(),
        }
        .encode()
        .unwrap();
        unsupported_projection[8..12].copy_from_slice(&DISTINCT_SERVER_PLAN_VERSION.to_be_bytes());
        assert_distinct_rule(
            decode_plan(
                function,
                SERVER_PLAN_FORMAT,
                DISTINCT_SERVER_PLAN_VERSION,
                &unsupported_projection,
            ),
            DISTINCT_PROJECTION_RULE,
        );

        let projection = Expression {
            kind: ExpressionKind::BooleanLiteral { value: true },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: false,
            },
        };
        let mut ordering = ServerPlan {
            scan: Scan {
                input: 0,
                object_type: source,
            },
            projections: vec![projection.clone()],
            selection: None,
            ordering: vec![Ordering {
                expression: projection,
                direction: SortDirection::Unspecified,
                null_order: server_plan::NullOrder::Unspecified,
            }],
        }
        .encode()
        .unwrap();
        ordering[8..12].copy_from_slice(&DISTINCT_SERVER_PLAN_VERSION.to_be_bytes());
        assert_distinct_rule(
            decode_plan(
                function,
                SERVER_PLAN_FORMAT,
                DISTINCT_SERVER_PLAN_VERSION,
                &ordering,
            ),
            "ORDER BY is not allowed",
        );

        assert!(matches!(
            decode_plan(
                function,
                SERVER_PLAN_FORMAT,
                DISTINCT_SERVER_PLAN_VERSION,
                b"not a server plan",
            ),
            Err(PostgresKernelError::ServerSelect(
                ServerSelectError::PlanDecode(server_plan::ServerPlanError::InvalidMagic)
            ))
        ));
    }

    #[test]
    fn distinct_error_display_is_human_facing_without_changing_existing_copy() {
        let function = FunctionId::from_bytes([0x31; 16]);
        assert_eq!(
            ServerSelectError::Distinct {
                rule: DISTINCT_PROJECTION_RULE,
            }
            .to_string(),
            "saved SELECT DISTINCT function cannot run: projections support only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values"
        );
        assert_eq!(
            ServerSelectError::PlanInvariant { rule: "test" }.to_string(),
            "server plan invariant failed: test"
        );
        assert_eq!(
            ServerSelectError::ReferenceEvidence {
                function,
                rule: "test",
            }
            .to_string(),
            "function function:64rk2c9h64rk2c9h64rk2c9h64 has invalid definition-reference evidence: test"
        );
    }

    #[test]
    fn distinct_signature_rejects_each_unsupported_shape_exactly() {
        let function_id = FunctionId::from_bytes([0x31; 16]);
        let valid = function(
            FunctionDomain::Server,
            Vec::new(),
            rows_return(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
        );
        assert!(validate_distinct_function_signature(&valid).is_ok());

        let wrong_domain = function(
            FunctionDomain::Client,
            Vec::new(),
            rows_return(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
        );
        assert!(matches!(
            validate_distinct_function_signature(&wrong_domain),
            Err(PostgresKernelError::ServerSelect(
                ServerSelectError::FunctionDomain { function }
            )) if function == function_id
        ));

        assert_signature_rule(
            validate_distinct_function_signature(&function(
                FunctionDomain::Server,
                vec![ParameterDefinition::new(
                    ParameterId::from_bytes([0x33; 16]),
                    "value",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    None,
                )],
                rows_return(),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
            )),
            function_id,
            "SELECT DISTINCT SERVER functions must have zero parameters",
        );
        for return_type in [
            FunctionReturn::Rows(Vec::new()),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
        ] {
            assert_signature_rule(
                validate_distinct_function_signature(&function(
                    FunctionDomain::Server,
                    Vec::new(),
                    return_type,
                    FunctionSecurity::Invoker,
                    Some(FunctionTransaction::ReadOnly),
                )),
                function_id,
                "SELECT DISTINCT SERVER functions must return nonempty ROWS",
            );
        }
        assert_signature_rule(
            validate_distinct_function_signature(&function(
                FunctionDomain::Server,
                Vec::new(),
                rows_return(),
                FunctionSecurity::Definer,
                Some(FunctionTransaction::ReadOnly),
            )),
            function_id,
            "SELECT DISTINCT SERVER functions must use INVOKER security",
        );
        for transaction in [
            None,
            Some(FunctionTransaction::Atomic),
            Some(FunctionTransaction::Manual),
        ] {
            assert_signature_rule(
                validate_distinct_function_signature(&function(
                    FunctionDomain::Server,
                    Vec::new(),
                    rows_return(),
                    FunctionSecurity::Invoker,
                    transaction,
                )),
                function_id,
                "SELECT DISTINCT SERVER functions must use READ ONLY transactions",
            );
        }
        for volatility in [FunctionVolatility::Immutable, FunctionVolatility::Volatile] {
            assert_signature_rule(
                validate_distinct_function_signature(&function_with_volatility(
                    FunctionDomain::Server,
                    Vec::new(),
                    rows_return(),
                    FunctionSecurity::Invoker,
                    Some(FunctionTransaction::ReadOnly),
                    volatility,
                )),
                function_id,
                "SELECT DISTINCT SERVER functions must use STABLE volatility",
            );
        }
    }

    #[test]
    fn parameter_free_versions_accept_only_an_empty_argument_slice() {
        assert!(validate_no_arguments(&[]).is_ok());
        let argument = FunctionArgument::new(
            ParameterId::from_bytes([0x33; 16]),
            RuntimeValue::Integer(7),
        )
        .unwrap();
        assert_argument_rule(
            validate_no_arguments(&[argument]),
            None,
            "this function does not accept arguments",
        );
    }

    #[test]
    fn identity_selected_signature_rejects_each_unsupported_shape_exactly() {
        let (catalogue, source, _, _) = catalogue();
        let function_id = FunctionId::from_bytes([0x31; 16]);
        let parameter_id = ParameterId::from_bytes([0x33; 16]);
        let selector_parameter = |resolved_type, default_expression| {
            ParameterDefinition::new(
                parameter_id,
                "selected",
                0,
                resolved_type,
                default_expression,
            )
        };
        let valid_parameter = || selector_parameter(ResolvedType::reference(source), None);

        let valid = function(
            FunctionDomain::Server,
            vec![valid_parameter()],
            boolean_rows_return(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
        );
        assert!(validate_identity_selected_function_signature(&catalogue, &valid).is_ok());

        let wrong_domain = function(
            FunctionDomain::Client,
            vec![valid_parameter()],
            boolean_rows_return(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
        );
        assert!(matches!(
            validate_identity_selected_function_signature(&catalogue, &wrong_domain),
            Err(PostgresKernelError::ServerSelect(
                ServerSelectError::FunctionDomain { function }
            )) if function == function_id
        ));

        assert_signature_rule(
            validate_identity_selected_function_signature(
                &catalogue,
                &function(
                    FunctionDomain::Server,
                    vec![valid_parameter()],
                    FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
                    FunctionSecurity::Invoker,
                    Some(FunctionTransaction::ReadOnly),
                ),
            ),
            function_id,
            "SERVER SELECT functions must return nonempty ROWS",
        );
        assert_signature_rule(
            validate_identity_selected_function_signature(
                &catalogue,
                &function(
                    FunctionDomain::Server,
                    vec![valid_parameter()],
                    boolean_rows_return(),
                    FunctionSecurity::Definer,
                    Some(FunctionTransaction::ReadOnly),
                ),
            ),
            function_id,
            "parameterised SERVER SELECT functions must use INVOKER security",
        );
        for transaction in [
            None,
            Some(FunctionTransaction::Atomic),
            Some(FunctionTransaction::Manual),
        ] {
            assert_signature_rule(
                validate_identity_selected_function_signature(
                    &catalogue,
                    &function(
                        FunctionDomain::Server,
                        vec![valid_parameter()],
                        boolean_rows_return(),
                        FunctionSecurity::Invoker,
                        transaction,
                    ),
                ),
                function_id,
                "parameterised SERVER SELECT functions must use READ ONLY transactions",
            );
        }
        for volatility in [FunctionVolatility::Immutable, FunctionVolatility::Volatile] {
            assert_signature_rule(
                validate_identity_selected_function_signature(
                    &catalogue,
                    &function_with_volatility(
                        FunctionDomain::Server,
                        vec![valid_parameter()],
                        boolean_rows_return(),
                        FunctionSecurity::Invoker,
                        Some(FunctionTransaction::ReadOnly),
                        volatility,
                    ),
                ),
                function_id,
                "parameterised SERVER SELECT functions must use STABLE volatility",
            );
        }
        for parameters in [
            Vec::new(),
            vec![
                valid_parameter(),
                ParameterDefinition::new(
                    ParameterId::from_bytes([0x34; 16]),
                    "other",
                    1,
                    ResolvedType::reference(source),
                    None,
                ),
            ],
        ] {
            assert_signature_rule(
                validate_identity_selected_function_signature(
                    &catalogue,
                    &function(
                        FunctionDomain::Server,
                        parameters,
                        boolean_rows_return(),
                        FunctionSecurity::Invoker,
                        Some(FunctionTransaction::ReadOnly),
                    ),
                ),
                function_id,
                "parameterised SERVER SELECT functions must declare exactly one parameter",
            );
        }
        assert_signature_rule(
            validate_identity_selected_function_signature(
                &catalogue,
                &function(
                    FunctionDomain::Server,
                    vec![selector_parameter(
                        ResolvedType::reference(source),
                        Some(orna_core::ExpressionId::from_bytes([0x35; 16])),
                    )],
                    boolean_rows_return(),
                    FunctionSecurity::Invoker,
                    Some(FunctionTransaction::ReadOnly),
                ),
            ),
            function_id,
            "the identity selector parameter cannot have a default expression",
        );
        for unsupported in [
            ResolvedType::scalar(StandardScalar::Integer),
            ResolvedType::reference(TypeId::from_bytes([0x99; 16])),
        ] {
            assert_signature_rule(
                validate_identity_selected_function_signature(
                    &catalogue,
                    &function(
                        FunctionDomain::Server,
                        vec![selector_parameter(unsupported, None)],
                        boolean_rows_return(),
                        FunctionSecurity::Invoker,
                        Some(FunctionTransaction::ReadOnly),
                    ),
                ),
                function_id,
                "the selector parameter must use REF to an available object type",
            );
        }
    }

    #[test]
    fn identity_selected_plan_requires_the_exact_selector_owner_parameter_and_target() {
        let (catalogue, source, _, _) = catalogue();
        let context = CatalogueHashContext::version_one();
        let other_active = TypeId::from_bytes([0x20; 16]);
        let function_id = FunctionId::from_bytes([0x31; 16]);
        let parameter_id = ParameterId::from_bytes([0x33; 16]);
        let projection = Expression {
            kind: ExpressionKind::BooleanLiteral { value: true },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: false,
            },
        };
        let plan = |owner, parameter| {
            IdentitySelectedServerPlan::new(
                Scan {
                    input: 0,
                    object_type: source,
                },
                [projection.clone()],
                IdentitySelector::new(owner, parameter),
            )
            .unwrap()
        };
        let function_for_target = |target| {
            function(
                FunctionDomain::Server,
                vec![ParameterDefinition::new(
                    parameter_id,
                    "selected",
                    0,
                    ResolvedType::reference(target),
                    None,
                )],
                boolean_rows_return(),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
            )
        };
        let valid = function_for_target(source);
        assert!(
            validate_identity_selected_plan(
                &catalogue,
                &context,
                &valid,
                &plan(function_id, parameter_id),
            )
            .is_ok()
        );

        for invalid in [
            plan(FunctionId::from_bytes([0x98; 16]), parameter_id),
            plan(function_id, ParameterId::from_bytes([0x97; 16])),
        ] {
            assert_plan_rule(
                validate_identity_selected_plan(&catalogue, &context, &valid, &invalid),
                "identity selector owner and parameter must equal the active function signature",
            );
        }
        assert_plan_rule(
            validate_identity_selected_plan(
                &catalogue,
                &context,
                &function_for_target(other_active),
                &plan(function_id, parameter_id),
            ),
            "the selector parameter must use REF to the object type selected in FROM",
        );
    }

    #[test]
    fn version_two_value_rows_accept_compatibility_plan_scalars() {
        let (context, integer) = retained_value_context("orna.kernel.value.integer@1");
        let (catalogue, source, field) = catalogue_with_value_field(integer);
        let function_id = FunctionId::from_bytes([0x31; 16]);
        let parameter_id = ParameterId::from_bytes([0x75; 16]);
        let function = function(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter_id,
                "selected",
                0,
                ResolvedType::reference(source),
                None,
            )],
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "value",
                0,
                ResolvedType::value(integer),
            )]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
        );
        let plan = IdentitySelectedServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [field_projection(source, field, StandardScalar::Integer)],
            IdentitySelector::new(function_id, parameter_id),
        )
        .expect("identity selected plan");

        assert!(validate_identity_selected_plan(&catalogue, &context, &function, &plan).is_ok());
        let columns = result_columns_for_projections(&function, plan.projections()).unwrap();
        assert_eq!(
            columns[0].resolved_type(),
            ResolvedType::scalar(StandardScalar::Integer)
        );
        let lowered = lower_identity_selected_plan(
            &catalogue,
            &context,
            &plan,
            &columns,
            ObjectId::from_bytes([0x76; 16]),
        )
        .unwrap();
        assert_eq!(lowered.bind_types, vec![Type::BYTEA]);
    }

    #[test]
    fn version_two_value_contracts_keep_the_existing_runtime_allowlist_error() {
        let (context, decimal) = retained_value_context("orna.kernel.value.decimal@1");
        let (catalogue, source, field) = catalogue_with_value_field(decimal);
        let function_id = FunctionId::from_bytes([0x31; 16]);
        let parameter_id = ParameterId::from_bytes([0x78; 16]);
        let function = function(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter_id,
                "selected",
                0,
                ResolvedType::reference(source),
                None,
            )],
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "value",
                0,
                ResolvedType::value(decimal),
            )]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
        );
        let plan = IdentitySelectedServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [field_projection(source, field, StandardScalar::Decimal)],
            IdentitySelector::new(function_id, parameter_id),
        )
        .expect("identity selected plan");

        assert_plan_rule(
            validate_identity_selected_plan(&catalogue, &context, &function, &plan),
            "projection type is outside the initial runtime result subset",
        );
    }

    #[test]
    fn identity_selected_equality_rejection_names_the_parameterised_query() {
        let (catalogue, source, reference, value) = catalogue();
        let context = CatalogueHashContext::version_one();
        let target = TypeId::from_bytes([0x20; 16]);
        let function_id = FunctionId::from_bytes([0x31; 16]);
        let parameter_id = ParameterId::from_bytes([0x33; 16]);
        let text = nullable_text_path(source, reference, target, value);
        let plan = IdentitySelectedServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [Expression {
                kind: ExpressionKind::Equality {
                    left: Box::new(text.clone()),
                    right: Box::new(text),
                },
                value_type: ValueType {
                    resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                    nullable: true,
                },
            }],
            IdentitySelector::new(function_id, parameter_id),
        )
        .unwrap();
        let function = function(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter_id,
                "selected",
                0,
                ResolvedType::reference(source),
                None,
            )],
            boolean_rows_return(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
        );

        assert_plan_rule(
            validate_identity_selected_plan(&catalogue, &context, &function, &plan),
            PARAMETERISED_EQUALITY_RULE,
        );
    }

    #[test]
    fn identity_selector_arguments_are_exact_complete_and_target_typed() {
        let (catalogue, source, _, _) = catalogue();
        let context = CatalogueHashContext::version_one();
        let function_id = FunctionId::from_bytes([0x31; 16]);
        let parameter_id = ParameterId::from_bytes([0x33; 16]);
        let function = function(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter_id,
                "selected",
                0,
                ResolvedType::reference(source),
                None,
            )],
            rows_return(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
        );
        let plan = IdentitySelectedServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [Expression {
                kind: ExpressionKind::BooleanLiteral { value: true },
                value_type: ValueType {
                    resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                    nullable: false,
                },
            }],
            IdentitySelector::new(function_id, parameter_id),
        )
        .unwrap();
        let object = ObjectId::from_bytes([0x42; 16]);
        let argument = FunctionArgument::new(
            parameter_id,
            RuntimeValue::Reference {
                target: source,
                object,
            },
        )
        .unwrap();

        assert!(validate_identity_selected_function_signature(&catalogue, &function).is_ok());
        assert_eq!(
            validate_identity_selected_arguments(
                &catalogue,
                &context,
                &function,
                &plan,
                std::slice::from_ref(&argument),
            )
            .unwrap(),
            object
        );
        assert_argument_rule(
            validate_identity_selected_arguments(&catalogue, &context, &function, &plan, &[]),
            Some(parameter_id),
            "a required argument is missing",
        );
        assert_argument_rule(
            validate_identity_selected_arguments(
                &catalogue,
                &context,
                &function,
                &plan,
                &[argument.clone(), argument],
            ),
            Some(parameter_id),
            "the same parameter was supplied twice",
        );
        let unknown_parameter = ParameterId::from_bytes([0x98; 16]);
        let unknown = FunctionArgument::new(
            unknown_parameter,
            RuntimeValue::Reference {
                target: source,
                object,
            },
        )
        .unwrap();
        assert_argument_rule(
            validate_identity_selected_arguments(
                &catalogue,
                &context,
                &function,
                &plan,
                &[unknown],
            ),
            Some(unknown_parameter),
            "an argument was supplied for a parameter that this function does not declare",
        );
        let wrong_scalar = FunctionArgument::new(parameter_id, RuntimeValue::Integer(7)).unwrap();
        assert_argument_rule(
            validate_identity_selected_arguments(
                &catalogue,
                &context,
                &function,
                &plan,
                &[wrong_scalar],
            ),
            Some(parameter_id),
            "the argument type does not match the declared parameter type",
        );
        let wrong_active_target = FunctionArgument::new(
            parameter_id,
            RuntimeValue::Reference {
                target: TypeId::from_bytes([0x20; 16]),
                object,
            },
        )
        .unwrap();
        assert_argument_rule(
            validate_identity_selected_arguments(
                &catalogue,
                &context,
                &function,
                &plan,
                &[wrong_active_target],
            ),
            Some(parameter_id),
            "the argument type does not match the declared parameter type",
        );
        let wrong_inactive_target = FunctionArgument::new(
            parameter_id,
            RuntimeValue::Reference {
                target: TypeId::from_bytes([0x99; 16]),
                object,
            },
        )
        .unwrap();
        assert_argument_rule(
            validate_identity_selected_arguments(
                &catalogue,
                &context,
                &function,
                &plan,
                &[wrong_inactive_target],
            ),
            Some(parameter_id),
            "the argument uses an unsupported type or refers to an unavailable object type",
        );
    }

    #[test]
    fn identity_selected_evidence_orders_query_projection_selector_and_parameter() {
        let (_, source, _, _) = catalogue();
        let owner = FunctionId::from_bytes([0x31; 16]);
        let parameter = ParameterId::from_bytes([0x33; 16]);
        let plan = IdentitySelectedServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [Expression {
                kind: ExpressionKind::ObjectReference { input: 0 },
                value_type: ValueType {
                    resolved_type: ResolvedType::reference(source),
                    nullable: false,
                },
            }],
            IdentitySelector::new(owner, parameter),
        )
        .unwrap();

        assert_eq!(
            expected_identity_selected_body_references(&plan),
            vec![
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::QueryObject,
                    DefinitionReferenceTarget::ObjectType(source),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(source),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(source),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter { owner, parameter },
                ),
            ]
        );
    }

    #[test]
    fn distinct_evidence_orders_source_projections_then_optional_selection() {
        let (_, source, reference, _) = catalogue();
        let target = TypeId::from_bytes([0x20; 16]);
        let projection = Expression {
            kind: ExpressionKind::FieldPath {
                input: 0,
                steps: vec![FieldStep {
                    owner: source,
                    field: reference,
                }],
            },
            value_type: ValueType {
                resolved_type: ResolvedType::reference(target),
                nullable: true,
            },
        };
        let object_reference = || Expression {
            kind: ExpressionKind::ObjectReference { input: 0 },
            value_type: ValueType {
                resolved_type: ResolvedType::reference(source),
                nullable: false,
            },
        };
        let selection = Expression {
            kind: ExpressionKind::Equality {
                left: Box::new(object_reference()),
                right: Box::new(object_reference()),
            },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: false,
            },
        };

        assert_eq!(
            expected_unordered_body_references(source, &[projection], Some(&selection)),
            vec![
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::QueryObject,
                    DefinitionReferenceTarget::ObjectType(source),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::QueryField,
                    DefinitionReferenceTarget::Field {
                        owner: source,
                        field: reference,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(source),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(source),
                ),
            ]
        );
    }

    #[test]
    fn distinct_evidence_mismatches_use_the_exact_human_rules() {
        assert_distinct_rule::<()>(
            Err(distinct_reference_error(ReferenceReplayMismatch::Count)),
            DISTINCT_REFERENCE_COUNT_RULE,
        );
        assert_distinct_rule::<()>(
            Err(distinct_reference_error(ReferenceReplayMismatch::Sequence)),
            DISTINCT_REFERENCE_SEQUENCE_RULE,
        );
    }

    #[test]
    fn identity_selected_cardinality_accepts_zero_or_one_and_rejects_two() {
        assert!(ResultCardinality::BoundedMany.validate(2).is_ok());
        assert!(validate_identity_selected_cardinality(0).is_ok());
        assert!(validate_identity_selected_cardinality(1).is_ok());
        assert!(ResultCardinality::AtMostOne.validate(1).is_ok());
        let error = ResultCardinality::AtMostOne.validate(2).unwrap_err();
        assert_eq!(
            error.to_string(),
            "server SELECT failed: SERVER SELECT returned too many rows: more than one row was returned for the requested object"
        );
        assert!(matches!(
            error,
            PostgresKernelError::ServerSelect(ServerSelectError::Cardinality { .. })
        ));
    }

    #[test]
    fn field_path_validation_rejects_a_wrong_owner_or_final_type() {
        let (catalogue, source, reference, value) = catalogue();
        let target = TypeId::from_bytes([0x20; 16]);
        let path = nullable_text_path(source, reference, target, value);
        assert_eq!(
            field_path_type(
                &catalogue,
                source,
                match &path.kind {
                    ExpressionKind::FieldPath { steps, .. } => steps,
                    _ => unreachable!(),
                }
            )
            .unwrap(),
            (
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                true
            ),
        );
        let wrong = [FieldStep {
            owner: target,
            field: reference,
        }];
        assert!(field_path_type(&catalogue, source, &wrong).is_err());
    }

    #[test]
    fn object_reference_emits_ordered_query_object_evidence() {
        let source = TypeId::from_bytes([0x10; 16]);
        let expression = Expression {
            kind: ExpressionKind::ObjectReference { input: 0 },
            value_type: ValueType {
                resolved_type: ResolvedType::reference(source),
                nullable: false,
            },
        };
        let mut references = Vec::new();
        add_expression_references(&mut references, source, &expression);
        assert_eq!(
            references,
            vec![ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(source),
            )]
        );
    }

    #[test]
    fn signature_matrix_accepts_only_active_server_rows_invoker_modes() {
        for transaction in [
            None,
            Some(FunctionTransaction::Atomic),
            Some(FunctionTransaction::ReadOnly),
        ] {
            assert!(
                validate_function_signature(&function(
                    FunctionDomain::Server,
                    Vec::new(),
                    rows_return(),
                    FunctionSecurity::Invoker,
                    transaction,
                ))
                .is_ok()
            );
        }
        assert!(
            validate_function_signature(&function(
                FunctionDomain::Client,
                Vec::new(),
                rows_return(),
                FunctionSecurity::Invoker,
                None,
            ))
            .is_err()
        );
        assert!(
            validate_function_signature(&function(
                FunctionDomain::Server,
                Vec::new(),
                FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
                FunctionSecurity::Invoker,
                None,
            ))
            .is_err()
        );
        assert!(
            validate_function_signature(&function(
                FunctionDomain::Server,
                Vec::new(),
                rows_return(),
                FunctionSecurity::Definer,
                None,
            ))
            .is_err()
        );
        assert!(
            validate_function_signature(&function(
                FunctionDomain::Server,
                Vec::new(),
                rows_return(),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::Manual),
            ))
            .is_err()
        );
    }

    #[test]
    fn operation_matrix_is_closed_for_equality_and_ordering() {
        let context = CatalogueHashContext::version_one();
        for resolved_type in [
            ResolvedType::scalar(StandardScalar::Boolean),
            ResolvedType::scalar(StandardScalar::Integer),
            ResolvedType::scalar(StandardScalar::BigInt),
            ResolvedType::scalar(StandardScalar::BinaryLargeObject),
            ResolvedType::reference(TypeId::from_bytes([0x55; 16])),
        ] {
            assert!(supports_equality_type(&context, resolved_type));
        }
        for scalar in [
            StandardScalar::Float,
            StandardScalar::CharacterLargeObject,
            StandardScalar::Decimal,
            StandardScalar::Uuid,
            StandardScalar::Date,
            StandardScalar::Time,
            StandardScalar::Timestamp,
            StandardScalar::Duration,
            StandardScalar::Void,
        ] {
            assert!(!supports_equality_type(
                &context,
                ResolvedType::scalar(scalar)
            ));
        }
        assert!(!supports_equality_type(
            &context,
            ResolvedType::named(TypeId::from_bytes([0x56; 16]))
        ));
        assert!(supports_ordering_type(
            &context,
            ResolvedType::scalar(StandardScalar::Integer)
        ));
        assert!(supports_ordering_type(
            &context,
            ResolvedType::scalar(StandardScalar::BigInt)
        ));
        assert!(!supports_ordering_type(
            &context,
            ResolvedType::scalar(StandardScalar::Boolean)
        ));
        assert!(!supports_ordering_type(
            &context,
            ResolvedType::reference(TypeId::from_bytes([0x57; 16]))
        ));
    }

    #[test]
    fn distinct_projection_domain_is_exhaustive_and_independent() {
        let context = CatalogueHashContext::version_one();
        let mut accepted_scalars = 0usize;
        for scalar in StandardScalar::ALL {
            let expected = matches!(
                scalar,
                StandardScalar::Boolean
                    | StandardScalar::Integer
                    | StandardScalar::BigInt
                    | StandardScalar::BinaryLargeObject
            );
            assert_eq!(
                supports_distinct_projection_type(&context, ResolvedType::scalar(scalar)),
                expected,
                "unexpected SELECT DISTINCT support for {scalar:?}",
            );
            accepted_scalars += usize::from(expected);
        }
        assert_eq!(accepted_scalars, 4);
        assert!(supports_distinct_projection_type(
            &context,
            ResolvedType::reference(TypeId::from_bytes([0x55; 16]))
        ));
        assert!(!supports_distinct_projection_type(
            &context,
            ResolvedType::named(TypeId::from_bytes([0x56; 16]))
        ));
    }

    #[test]
    fn distinct_plan_revalidates_catalogue_shape_and_uses_its_own_equality_copy() {
        let (catalogue, source, reference, value) = catalogue();
        let context = CatalogueHashContext::version_one();
        let target = TypeId::from_bytes([0x20; 16]);
        let reference_projection = |scan| Expression {
            kind: ExpressionKind::ObjectReference { input: 0 },
            value_type: ValueType {
                resolved_type: ResolvedType::reference(scan),
                nullable: false,
            },
        };
        let plan = DistinctServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [reference_projection(source)],
            None,
        )
        .unwrap();
        let reference_rows = FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "value",
            0,
            ResolvedType::reference(source),
        )]);
        let reference_function = function(
            FunctionDomain::Server,
            Vec::new(),
            reference_rows,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
        );
        assert!(validate_distinct_plan(&catalogue, &context, &reference_function, &plan).is_ok());

        let inactive = TypeId::from_bytes([0x99; 16]);
        let inactive_plan = DistinctServerPlan::new(
            Scan {
                input: 0,
                object_type: inactive,
            },
            [reference_projection(inactive)],
            None,
        )
        .unwrap();
        assert_plan_rule(
            validate_distinct_plan(&catalogue, &context, &reference_function, &inactive_plan),
            "scan must use active input zero and an active object type",
        );
        assert_plan_rule(
            validate_distinct_plan(
                &catalogue,
                &context,
                &function(
                    FunctionDomain::Server,
                    Vec::new(),
                    boolean_rows_return(),
                    FunctionSecurity::Invoker,
                    Some(FunctionTransaction::ReadOnly),
                ),
                &plan,
            ),
            "projection type must equal its ROWS column",
        );

        let unknown_field = DistinctServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [Expression {
                kind: ExpressionKind::FieldPath {
                    input: 0,
                    steps: vec![FieldStep {
                        owner: source,
                        field: FieldId::from_bytes([0x99; 16]),
                    }],
                },
                value_type: ValueType {
                    resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                    nullable: false,
                },
            }],
            None,
        )
        .unwrap();
        assert_plan_rule(
            validate_distinct_plan(
                &catalogue,
                &context,
                &function(
                    FunctionDomain::Server,
                    Vec::new(),
                    boolean_rows_return(),
                    FunctionSecurity::Invoker,
                    Some(FunctionTransaction::ReadOnly),
                ),
                &unknown_field,
            ),
            "field path field must exist on its active owner",
        );

        let text = nullable_text_path(source, reference, target, value);
        let unsupported_equality = DistinctServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [Expression {
                kind: ExpressionKind::Equality {
                    left: Box::new(text.clone()),
                    right: Box::new(text),
                },
                value_type: ValueType {
                    resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                    nullable: true,
                },
            }],
            None,
        )
        .unwrap();
        assert_plan_rule(
            validate_distinct_plan(
                &catalogue,
                &context,
                &function(
                    FunctionDomain::Server,
                    Vec::new(),
                    boolean_rows_return(),
                    FunctionSecurity::Invoker,
                    Some(FunctionTransaction::ReadOnly),
                ),
                &unsupported_equality,
            ),
            DISTINCT_EQUALITY_RULE,
        );
    }

    #[test]
    fn variable_payload_budget_reserves_names_and_fixed_values() {
        let context = CatalogueHashContext::version_one();
        let catalogue =
            CatalogueSnapshot::new(CatalogueRevisionId::new(), Vec::new(), Vec::new()).unwrap();
        let columns = [
            ResultColumn::new(
                "integer",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .unwrap(),
            ResultColumn::new(
                "left",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                true,
            )
            .unwrap(),
            ResultColumn::new(
                "right",
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                true,
            )
            .unwrap(),
        ];
        assert_eq!(
            variable_payload_limit(&catalogue, &context, &columns).unwrap(),
            (PAYLOAD_LIMIT - "integerleftright".len() - 4) / 2
        );
    }

    #[test]
    fn contextualized_kernel_failures_keep_the_pinned_execution_context_and_source() {
        let context = ServerSelectContext::new(
            RevisionPair::new(
                orna_core::SourceRevisionId::from_bytes([0x61; 16]),
                CatalogueRevisionId::from_bytes([0x62; 16]),
            ),
            FunctionId::from_bytes([0x63; 16]),
            FunctionRevisionId::from_bytes([0x64; 16]),
        );
        let error = contextualize(
            context,
            PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.test",
                record: String::from("record"),
                rule: "test",
            },
        );
        let PostgresKernelError::ServerSelect(ServerSelectError::Execution {
            context: actual,
            source,
        }) = error
        else {
            panic!("active failures must retain context");
        };
        assert_eq!(actual, context);
        assert!(source.source().is_some());
    }

    #[test]
    fn successful_result_reconstructs_the_shutdown_error_context() {
        let pair = RevisionPair::new(
            orna_core::SourceRevisionId::from_bytes([0x65; 16]),
            CatalogueRevisionId::from_bytes([0x66; 16]),
        );
        let result = ServerSelectResult::new(
            pair,
            FunctionId::from_bytes([0x67; 16]),
            FunctionRevisionId::from_bytes([0x68; 16]),
            ResultRows::new(
                [ResultColumn::new(
                    "value",
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                )
                .unwrap()],
                [ResultRow::new([RuntimeValue::Boolean(true)])],
            )
            .unwrap(),
        );
        assert_eq!(
            context_from_result(&result),
            ServerSelectContext::new(pair, result.function(), result.function_revision())
        );
    }

    #[test]
    fn select_binds_are_prepared_with_exact_types() {
        assert!(Vec::<SelectBindValue>::new().is_empty());
        assert_eq!(
            [
                SelectBindValue::Boolean(true),
                SelectBindValue::Boolean(false)
            ]
            .iter()
            .map(SelectBindValue::bind_type)
            .collect::<Vec<_>>(),
            vec![Type::BOOL, Type::BOOL]
        );
    }

    #[test]
    fn payload_accounting_has_stable_fixed_width_values() {
        assert_eq!(
            logical_payload_len(&RuntimeValue::Boolean(true)).unwrap(),
            1
        );
        assert_eq!(logical_payload_len(&RuntimeValue::Integer(1)).unwrap(), 4);
        assert_eq!(logical_payload_len(&RuntimeValue::BigInt(1)).unwrap(), 8);
        assert_eq!(
            logical_payload_len(&RuntimeValue::Float(RuntimeFloat::new(1.0).unwrap())).unwrap(),
            8
        );
        assert_eq!(
            logical_payload_len(&RuntimeValue::Text(String::from("abc"))).unwrap(),
            3
        );
        assert_eq!(
            logical_payload_len(&RuntimeValue::Bytes(vec![1, 2])).unwrap(),
            2
        );
        assert_eq!(
            logical_payload_len(
                &RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap()
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn query_limit_uses_the_stricter_row_or_cell_bound() {
        assert_eq!(effective_query_limit(1).unwrap(), ROW_LIMIT + 1);
        assert_eq!(effective_query_limit(1_024).unwrap(), 977);
        assert!(effective_query_limit(0).is_err());
    }

    #[test]
    fn target_entry_limit_reserves_postgres_headroom() {
        assert!(validate_target_entry_count(1_000, 400, 200).is_ok());
        assert!(validate_target_entry_count(1_000, 400, 201).is_err());
        assert!(validate_target_entry_count(usize::MAX, 1, 0).is_err());
    }

    #[test]
    fn ordering_rules_are_explicit_and_independent_of_postgres_defaults() {
        assert_eq!(ordering_sql(SortDirection::Unspecified), "ASC NULLS LAST");
        assert_eq!(ordering_sql(SortDirection::Ascending), "ASC NULLS LAST");
        assert_eq!(ordering_sql(SortDirection::Descending), "DESC NULLS FIRST");
    }

    #[test]
    fn payload_accounting_includes_column_names_and_fails_closed() {
        let columns = [
            ResultColumn::new("one", ResolvedType::scalar(StandardScalar::Boolean), false).unwrap(),
            ResultColumn::new("two", ResolvedType::scalar(StandardScalar::Integer), false).unwrap(),
        ];
        assert_eq!(initial_payload_len(&columns).unwrap(), 6);
        assert!(add_payload(PAYLOAD_LIMIT, 1).is_err());
    }

    #[test]
    fn server_error_sources_remain_typed() {
        let error = ServerSelectError::ResultRows(ResultRowsError::EmptyColumns);
        assert!(error.source().is_some());
        assert!(
            ServerSelectError::PlanInvariant { rule: "test" }
                .source()
                .is_none()
        );
    }
}
