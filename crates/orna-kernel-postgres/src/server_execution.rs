//! Execution of the initial immutable SERVER `SELECT` subset.
//!
//! This module accepts only a recovered active revision and a canonical server
//! plan. It never derives SQL from semantic names or accepts caller SQL.

use std::{collections::BTreeMap, error::Error, fmt};

use futures_util::TryStreamExt;
use orna_artifact::server_plan::{
    self, Expression, ExpressionKind, FieldStep, ServerPlan, SortDirection,
};
use orna_core::{
    FieldId, FunctionId, FunctionRevisionId, ObjectId, TypeId,
    catalogue::{
        FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity, FunctionTransaction,
    },
    revision::{
        ActiveDatabaseRevision, DefinitionReferenceKind, DefinitionReferenceTarget,
        ExecutableArtifactKind, RevisionPair,
    },
    types::{ResolvedType, StandardScalar},
    value::{ResultColumn, ResultRow, ResultRows, ResultRowsError, RuntimeFloat, RuntimeValue},
};
use tokio_postgres::{
    Client, IsolationLevel, Row, Statement, Transaction,
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

const SERVER_PLAN_FORMAT: &str = server_plan::FORMAT_IDENTITY;
const SERVER_PLAN_VERSION: u32 = server_plan::FORMAT_VERSION;
const ROW_LIMIT: usize = 10_000;
const CELL_LIMIT: usize = 1_000_000;
const PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;
const FIELD_PATH_STEP_LIMIT: usize = 8_192;
const JOIN_LIMIT: usize = 1_024;
const SQL_LIMIT: usize = 1024 * 1024;
const TARGET_ENTRY_LIMIT: usize = 1_600;

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
    /// The function signature is outside this no-argument ROWS subset.
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
    /// The ordered durable definition references do not prove this plan.
    ReferenceEvidence {
        function: FunctionId,
        rule: &'static str,
    },
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
            Self::ReferenceEvidence { function, rule } => write!(
                formatter,
                "function {} has invalid definition-reference evidence: {rule}",
                function.canonical(),
            ),
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
            | Self::FunctionDomain { .. }
            | Self::FunctionSignature { .. }
            | Self::CurrentRevision { .. }
            | Self::Artifact { .. }
            | Self::PlanInvariant { .. }
            | Self::ReferenceEvidence { .. }
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
    /// This operation accepts a stable function identity only. It does not
    /// resolve names, perform authentication or authorisation, accept an
    /// invocation identity or arguments, or expose a protocol stream. It
    /// reads only the active revision and returns a bounded collected result.
    pub async fn execute_server_select(
        &self,
        function: FunctionId,
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_server_select_with_barrier(function, None)
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
        self.execute_server_select_with_barrier(
            function,
            Some(SelectTestBarrier { reached, resume }),
        )
        .await
    }

    async fn execute_server_select_with_barrier(
        &self,
        function: FunctionId,
        barrier: Option<SelectTestBarrier>,
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        let mut session = self.open().await?;
        let execution = execute_client(&mut session.client, function, barrier.as_ref()).await;
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
    test_barrier: Option<&SelectTestBarrier>,
) -> Result<ServerSelectResult, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;
    let result = execute_transaction(&transaction, function, test_barrier).await;
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
    let result = execute_active_transaction(transaction, &active, function, context).await;
    result.map_err(|error| contextualize(context, error))
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
) -> Result<ServerSelectResult, PostgresKernelError> {
    validate_function_signature(function)?;
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
    if artifact.format() != SERVER_PLAN_FORMAT || artifact.version() != SERVER_PLAN_VERSION {
        return Err(server_error(ServerSelectError::Artifact {
            function: context.function(),
            rule: "current SERVER artifact must use orna.server-plan version 1",
        }));
    }
    if revision.language_version() != server_plan::LANGUAGE_VERSION_IDENTITY {
        return Err(server_error(ServerSelectError::Artifact {
            function: context.function(),
            rule: "current SERVER revision must use the server-plan language version",
        }));
    }
    let plan = ServerPlan::decode(artifact.payload())
        .map_err(ServerSelectError::PlanDecode)
        .map_err(server_error)?;
    validate_plan(active, function, &plan)?;
    validate_reference_evidence(active, function, &plan)?;
    let columns = result_columns(function, &plan)?;
    validate_target_entries(&plan, &columns)?;
    let lowered = lower_plan(active.catalogue(), &plan, &columns)?;
    let bind_types = boolean_bind_types(&lowered.binds);
    let statement = transaction
        .prepare_typed(&lowered.sql, &bind_types)
        .await
        .map_err(PostgresKernelError::Database)?;
    validate_prepared_columns(&statement, &columns, &lowered.guards)?;
    let rows = stream_rows(
        transaction,
        &statement,
        &lowered.binds,
        &columns,
        &lowered.guards,
        lowered.variable_payload_limit,
    )
    .await?;
    Ok(ServerSelectResult::new(
        context.pair(),
        context.function(),
        revision.id(),
        rows,
    ))
}

fn boolean_bind_types(binds: &[bool]) -> Vec<Type> {
    vec![Type::BOOL; binds.len()]
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

fn validate_plan(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerPlan,
) -> Result<(), PostgresKernelError> {
    let catalogue = active.catalogue();
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
        validate_expression(catalogue, plan.scan.object_type, projection)?;
        if projection.value_type.resolved_type != column.resolved_type() {
            return Err(plan_invariant("projection type must equal its ROWS column"));
        }
        if !supports_result_type(projection.value_type.resolved_type) {
            return Err(plan_invariant(
                "projection type is outside the initial runtime result subset",
            ));
        }
    }
    if let Some(selection) = &plan.selection {
        validate_expression(catalogue, plan.scan.object_type, selection)?;
        if selection.value_type.resolved_type != ResolvedType::scalar(StandardScalar::Boolean) {
            return Err(plan_invariant("selection must have BOOLEAN type"));
        }
    }
    for ordering in &plan.ordering {
        validate_expression(catalogue, plan.scan.object_type, &ordering.expression)?;
        if !supports_ordering_type(ordering.expression.value_type.resolved_type) {
            return Err(plan_invariant(
                "version 1 SERVER SELECT ordering supports only INTEGER and BIGINT",
            ));
        }
    }
    Ok(())
}

fn validate_expression(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    scan: TypeId,
    expression: &Expression,
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
            if expression.value_type.resolved_type != resolved_type
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
            validate_expression(catalogue, scan, left)?;
            validate_expression(catalogue, scan, right)?;
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
            if !supports_equality_type(left.value_type.resolved_type) {
                return Err(plan_invariant(
                    "version 1 SERVER SELECT equality supports only BOOLEAN, INTEGER, BIGINT, BYTES, and references",
                ));
            }
        }
    }
    Ok(())
}

const fn supports_ordering_type(resolved_type: ResolvedType) -> bool {
    matches!(
        resolved_type,
        ResolvedType::Scalar(StandardScalar::Integer | StandardScalar::BigInt)
    )
}

fn supports_equality_type(resolved_type: ResolvedType) -> bool {
    matches!(
        resolved_type,
        ResolvedType::Scalar(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        ) | ResolvedType::Reference { .. }
    )
}

fn supports_result_type(resolved_type: ResolvedType) -> bool {
    matches!(
        resolved_type,
        ResolvedType::Scalar(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        ) | ResolvedType::Reference { .. }
    )
}

fn validate_execution_complexity(plan: &ServerPlan) -> Result<(), PostgresKernelError> {
    let mut steps = 0usize;
    let mut binds = 0usize;
    for expression in &plan.projections {
        count_expression_complexity(expression, &mut steps, &mut binds)?;
    }
    if let Some(selection) = &plan.selection {
        count_expression_complexity(selection, &mut steps, &mut binds)?;
    }
    for ordering in &plan.ordering {
        count_expression_complexity(&ordering.expression, &mut steps, &mut binds)?;
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

fn validate_target_entries(
    plan: &ServerPlan,
    columns: &[ResultColumn],
) -> Result<(), PostgresKernelError> {
    let guards = columns
        .iter()
        .filter(|column| is_variable_type(column.resolved_type()))
        .count();
    validate_target_entry_count(plan.projections.len(), guards, plan.ordering.len())
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
            if let ResolvedType::Reference { target } = field.resolved_type()
                && catalogue.object_type_by_id(target).is_none()
            {
                return Err(plan_invariant(
                    "final reference field path target must be an active object type",
                ));
            }
            return Ok((field.resolved_type(), nullable));
        }
        let ResolvedType::Reference { target } = field.resolved_type() else {
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
    let expected = expected_body_references(plan);
    validate_function_reference_replay(active, function, &expected).map_err(|mismatch| {
        let rule = match mismatch {
            ReferenceReplayMismatch::Count => {
                "reference count must match signature and plan traversal"
            }
            ReferenceReplayMismatch::Sequence => {
                "references must be ordered signature evidence followed by plan traversal"
            }
        };
        reference_error(function.id(), rule)
    })
}

fn expected_body_references(plan: &ServerPlan) -> Vec<ExpectedDefinitionReference> {
    let mut expected = vec![ExpectedDefinitionReference::new(
        DefinitionReferenceKind::QueryObject,
        DefinitionReferenceTarget::ObjectType(plan.scan.object_type),
    )];
    for expression in &plan.projections {
        add_expression_references(&mut expected, plan.scan.object_type, expression);
    }
    if let Some(selection) = &plan.selection {
        add_expression_references(&mut expected, plan.scan.object_type, selection);
    }
    for ordering in &plan.ordering {
        add_expression_references(&mut expected, plan.scan.object_type, &ordering.expression);
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

fn result_columns(
    function: &FunctionDefinition,
    plan: &ServerPlan,
) -> Result<Vec<ResultColumn>, PostgresKernelError> {
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(plan_invariant("function return must be ROWS"));
    };
    columns
        .iter()
        .zip(&plan.projections)
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
    binds: Vec<bool>,
    guards: Vec<VariableGuard>,
    variable_payload_limit: usize,
}

struct VariableGuard {
    column: usize,
    alias: String,
}

fn lower_plan(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    plan: &ServerPlan,
    columns: &[ResultColumn],
) -> Result<LoweredPlan, PostgresKernelError> {
    let mut lowerer = Lowerer {
        catalogue,
        scan: plan.scan.object_type,
        joins: BTreeMap::new(),
        join_sql: Vec::new(),
        binds: Vec::new(),
        field_path_steps: 0,
    };
    let variable_limit = variable_payload_limit(columns)?;
    let mut projections = Vec::with_capacity(plan.projections.len());
    let mut guard_projections = Vec::new();
    let mut guards = Vec::new();
    for (index, expression) in plan.projections.iter().enumerate() {
        let expression = lowerer.expression(expression)?;
        if is_variable_type(columns[index].resolved_type()) {
            let alias = format!("g{}", guards.len());
            projections.push(format!(
                "CASE WHEN octet_length({expression}) <= {variable_limit} THEN {expression} ELSE NULL END AS c{index}"
            ));
            guards.push(VariableGuard {
                column: index,
                alias: alias.clone(),
            });
            guard_projections.push(format!(
                "CASE WHEN {expression} IS NULL OR octet_length({expression}) <= {variable_limit} THEN TRUE ELSE FALSE END AS {alias}"
            ));
        } else {
            projections.push(format!("{expression} AS c{index}"));
        }
    }
    projections.extend(guard_projections);
    let selection = plan
        .selection
        .as_ref()
        .map(|expression| lowerer.expression(expression))
        .transpose()?;
    let mut ordering = Vec::with_capacity(plan.ordering.len());
    for item in &plan.ordering {
        let direction = ordering_sql(item.direction);
        ordering.push(format!(
            "{} {direction}",
            lowerer.expression(&item.expression)?
        ));
    }
    let mut sql = format!(
        "SELECT {}\nFROM {}.{} AS i0",
        projections.join(", "),
        DATA_SCHEMA,
        relation_name(plan.scan.object_type),
    );
    for join in &lowerer.join_sql {
        sql.push('\n');
        sql.push_str(join);
    }
    if let Some(selection) = selection {
        sql.push_str("\nWHERE ");
        sql.push_str(&selection);
    }
    if !ordering.is_empty() {
        sql.push_str("\nORDER BY ");
        sql.push_str(&ordering.join(", "));
    }
    let limit = effective_query_limit(plan.projections.len())?;
    sql.push_str(&format!("\nLIMIT {limit}"));
    if sql.len() > SQL_LIMIT {
        return Err(server_error(ServerSelectError::ComplexityLimit {
            category: "generated SQL bytes",
            maximum: SQL_LIMIT,
        }));
    }
    Ok(LoweredPlan {
        sql,
        binds: lowerer.binds,
        guards,
        variable_payload_limit: variable_limit,
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

fn is_variable_type(resolved_type: ResolvedType) -> bool {
    matches!(
        resolved_type,
        ResolvedType::Scalar(
            StandardScalar::CharacterLargeObject | StandardScalar::BinaryLargeObject
        )
    )
}

fn variable_payload_limit(columns: &[ResultColumn]) -> Result<usize, PostgresKernelError> {
    let names = initial_payload_len(columns)?;
    let fixed = columns
        .iter()
        .filter(|column| !is_variable_type(column.resolved_type()))
        .try_fold(0usize, |total, column| {
            total
                .checked_add(maximum_fixed_payload_len(column.resolved_type()))
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
        .filter(|column| is_variable_type(column.resolved_type()))
        .count();
    if variable_count == 0 {
        return Ok(0);
    }
    Ok(available / variable_count)
}

const fn maximum_fixed_payload_len(resolved_type: ResolvedType) -> usize {
    match resolved_type {
        ResolvedType::Scalar(StandardScalar::Boolean) => 1,
        ResolvedType::Scalar(StandardScalar::Integer) => 4,
        ResolvedType::Scalar(StandardScalar::BigInt | StandardScalar::Float) => 8,
        ResolvedType::Reference { .. } => 16,
        ResolvedType::Scalar(_) | ResolvedType::Named(_) => 0,
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
    binds: Vec<bool>,
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
                self.binds.push(*value);
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
            let ResolvedType::Reference { target } = field.resolved_type() else {
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
            || *column.type_() != expected_postgres_type(expected.resolved_type())?
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

fn expected_postgres_type(resolved_type: ResolvedType) -> Result<Type, PostgresKernelError> {
    postgres_type(resolved_type).ok_or_else(|| {
        server_error(ServerSelectError::PreparedResult {
            rule: "result type is outside the initial runtime subset",
        })
    })
}

async fn stream_rows(
    transaction: &Transaction<'_>,
    statement: &Statement,
    binds: &[bool],
    columns: &[ResultColumn],
    guards: &[VariableGuard],
    variable_payload_limit: usize,
) -> Result<ResultRows, PostgresKernelError> {
    let parameters = binds
        .iter()
        .map(|value| value as &(dyn ToSql + Sync))
        .collect::<Vec<_>>();
    let stream = transaction
        .query_raw(statement, parameters)
        .await
        .map_err(PostgresKernelError::Database)?;
    futures_util::pin_mut!(stream);
    let mut rows = Vec::new();
    let mut cells = 0usize;
    let mut payload = initial_payload_len(columns)?;
    while let Some(row) = stream
        .try_next()
        .await
        .map_err(PostgresKernelError::Database)?
    {
        if rows.len() == ROW_LIMIT {
            return Err(server_error(ServerSelectError::RowLimit {
                maximum: ROW_LIMIT,
            }));
        }
        cells = cells.checked_add(columns.len()).ok_or_else(|| {
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
        for (guard_index, guard) in guards.iter().enumerate() {
            let accepted = row
                .try_get::<usize, bool>(columns.len() + guard_index)
                .map_err(|source| {
                    server_error(ServerSelectError::RowDecode {
                        row: row_index,
                        column: columns.len() + guard_index,
                        source,
                    })
                })?;
            if !accepted {
                return Err(server_error(ServerSelectError::VariablePayload {
                    row: row_index,
                    column: guard.column,
                    maximum: variable_payload_limit,
                }));
            }
        }
        let mut values = Vec::with_capacity(columns.len());
        for (column_index, column) in columns.iter().enumerate() {
            let value = decode_value(&row, row_index, column_index, column)?;
            payload = add_payload(payload, logical_payload_len(&value)?)?;
            values.push(value);
        }
        rows.push(ResultRow::new(values));
    }
    ResultRows::new(columns.to_vec(), rows)
        .map_err(ServerSelectError::ResultRows)
        .map_err(server_error)
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
    let value = match resolved_type {
        ResolvedType::Scalar(StandardScalar::Boolean) => {
            decode!(bool, |value| Ok(RuntimeValue::Boolean(value)))
        }
        ResolvedType::Scalar(StandardScalar::Integer) => {
            decode!(i32, |value| Ok(RuntimeValue::Integer(value)))
        }
        ResolvedType::Scalar(StandardScalar::BigInt) => {
            decode!(i64, |value| Ok(RuntimeValue::BigInt(value)))
        }
        ResolvedType::Scalar(StandardScalar::Float) => decode!(f64, |value| {
            RuntimeFloat::new(value)
                .map(RuntimeValue::Float)
                .map_err(ServerSelectError::ResultRows)
                .map_err(server_error)
        }),
        ResolvedType::Scalar(StandardScalar::CharacterLargeObject) => {
            decode!(String, |value| Ok(RuntimeValue::Text(value)))
        }
        ResolvedType::Scalar(StandardScalar::BinaryLargeObject) => {
            decode!(Vec<u8>, |value| Ok(RuntimeValue::Bytes(value)))
        }
        ResolvedType::Reference { target } => decode!(Vec<u8>, |value| {
            let object = value.try_into().map(ObjectId::from_bytes).map_err(|_| {
                server_error(ServerSelectError::ValueInvariant {
                    row: row_index,
                    column: column_index,
                    rule: "reference result values must contain exactly 16 bytes",
                })
            })?;
            Ok(RuntimeValue::Reference { target, object })
        }),
        ResolvedType::Scalar(_) | ResolvedType::Named(_) => {
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
    use orna_artifact::server_plan::{Scan, ValueType};
    use orna_core::{
        CatalogueRevisionId, FieldId, SchemaId,
        catalogue::{
            CatalogueSnapshot, FieldDefinition, FunctionReturnColumnDefinition,
            ObjectTypeDefinition, QualifiedSemanticName, SchemaDefinition,
        },
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

    fn function(
        domain: FunctionDomain,
        parameters: Vec<orna_core::catalogue::ParameterDefinition>,
        return_type: FunctionReturn,
        security: FunctionSecurity,
        transaction: Option<FunctionTransaction>,
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
            orna_core::catalogue::FunctionVolatility::Stable,
        )
    }

    fn rows_return() -> FunctionReturn {
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "value",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
        )])
    }

    #[test]
    fn lowerer_uses_identity_names_cached_nullable_joins_and_boolean_binds() {
        let (catalogue, source, reference, value) = catalogue();
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
        let lowered = lower_plan(&catalogue, &plan, &columns).unwrap();

        assert_eq!(lowered.binds, vec![true]);
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
        for resolved_type in [
            ResolvedType::scalar(StandardScalar::Boolean),
            ResolvedType::scalar(StandardScalar::Integer),
            ResolvedType::scalar(StandardScalar::BigInt),
            ResolvedType::scalar(StandardScalar::BinaryLargeObject),
            ResolvedType::reference(TypeId::from_bytes([0x55; 16])),
        ] {
            assert!(supports_equality_type(resolved_type));
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
            assert!(!supports_equality_type(ResolvedType::scalar(scalar)));
        }
        assert!(!supports_equality_type(ResolvedType::named(
            TypeId::from_bytes([0x56; 16])
        )));
        assert!(supports_ordering_type(ResolvedType::scalar(
            StandardScalar::Integer
        )));
        assert!(supports_ordering_type(ResolvedType::scalar(
            StandardScalar::BigInt
        )));
        assert!(!supports_ordering_type(ResolvedType::scalar(
            StandardScalar::Boolean
        )));
        assert!(!supports_ordering_type(ResolvedType::reference(
            TypeId::from_bytes([0x57; 16])
        )));
    }

    #[test]
    fn variable_payload_budget_reserves_names_and_fixed_values() {
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
            variable_payload_limit(&columns).unwrap(),
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
    fn boolean_binds_are_prepared_with_exact_boolean_types() {
        assert!(boolean_bind_types(&[]).is_empty());
        assert_eq!(
            boolean_bind_types(&[true, false]),
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
