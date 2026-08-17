//! Execution of the initial immutable SERVER `SELECT` subset.
//!
//! This module accepts only a recovered active revision and a canonical server
//! plan. It never derives SQL from semantic names or accepts caller SQL.

use std::{collections::BTreeMap, error::Error, fmt, sync::OnceLock};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures_util::TryStreamExt;
use orna_artifact::server_csv_encode::{self, CsvEncodePlan, CsvEncodePlanError};
use orna_artifact::server_json_encode::{self, JsonEncodePlan, JsonEncodePlanError};
use orna_artifact::server_parameter_echo::{self, ServerParameterEcho, ServerParameterEchoError};
use orna_artifact::server_plan::{
    self, DistinctServerPlan, Expression, ExpressionKind, FieldStep, IdentitySelectedServerPlan,
    Ordering, SelectBindValue as UniqueTextSelectBindValue, ServerPlan, SortDirection,
    UniqueTextSelectedServerPlan,
};
use orna_artifact::server_terminal_table::{self, TerminalTablePlan, TerminalTablePlanError};
use orna_core::{
    FieldId, FunctionId, FunctionRevisionId, ObjectId, ParameterId, SourceUnitId, TypeId,
    canonical_hash::artifact_payload_digest,
    catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionTransaction, FunctionVolatility, ParameterDefinition, QualifiedSemanticName,
        TypeLookupName, ValueTypeKind, ValueTypeMutability, ValueTypePersistence,
    },
    invocation::InvocationOutputRequirement,
    presenter::{OutputResolutionError, PresenterEntry, PresenterRegistry},
    revision::{
        ActiveDatabaseRevision, CatalogueHashContext, DefinitionReferenceKind,
        DefinitionReferenceTarget, ExecutableArtifact, ExecutableArtifactKind,
        FunctionRevisionRecord, RevisionPair, Sha256Digest, SourceOrigin,
    },
    security::{AuthorisedInvocation, InvocationTarget},
    types::{ResolvedType, StandardScalar},
    value::{
        ConstructedValueKind, EnumValue, FunctionArgument, OpaqueCodecRegistry, OpaqueValue,
        OpaqueValueError, RecordValue, ResultColumn, ResultRow, ResultRows, ResultRowsError,
        RuntimeFloat, RuntimeType, RuntimeValue,
    },
};
use orna_protocol::{ValueCodecError, decode_active_value, encode_active_value};
use orna_standard::{
    BYTE_STREAM_MAGIC, INTEGER_TYPE_ID, STD_IO_BYTE_STREAM_TYPE_ID, STD_TERMINAL_DOCUMENT_TYPE_ID,
    TERMINAL_DOCUMENT_MAGIC,
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
const UNIQUE_TEXT_SELECTED_SERVER_PLAN_VERSION: u32 =
    server_plan::UNIQUE_TEXT_SELECTED_FORMAT_VERSION;
const ROW_LIMIT: usize = 10_000;
const CELL_LIMIT: usize = 1_000_000;
const PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;
const FIELD_PATH_STEP_LIMIT: usize = 8_192;
const JOIN_LIMIT: usize = 1_024;
const SQL_LIMIT: usize = 1024 * 1024;
const TARGET_ENTRY_LIMIT: usize = 1_600;
const ACTIVE_VALUE_ENVELOPE_LENGTH: usize = 25;
const VERSION_ONE_EQUALITY_RULE: &str = "version 1 SERVER SELECT equality supports only BOOLEAN, INTEGER, BIGINT, BYTES, and references";
const PARAMETERISED_EQUALITY_RULE: &str = "parameterised SERVER SELECT equality supports only BOOLEAN, INTEGER, BIGINT, BYTES, and references";
const DISTINCT_EQUALITY_RULE: &str =
    "SELECT DISTINCT equality supports only BOOLEAN, INTEGER, BIGINT, BYTES, and references";
const DISTINCT_PROJECTION_RULE: &str =
    "projections support only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values";
const DISTINCT_REFERENCE_COUNT_RULE: &str = "its dependencies do not match its signature and query";
const DISTINCT_REFERENCE_SEQUENCE_RULE: &str =
    "its dependencies are not in the same order as its signature and query";

/// The fixed ADR 0057 `std.json.Value` value-type identity: `...11` (ADR 0058).
const STD_JSON_VALUE_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);
/// The fixed ADR 0057 `std.data.Rows` value-type identity: `...12` (ADR 0058).
const STD_DATA_ROWS_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]);
/// The fixed ADR 0057 `std.json.encode` function identity: `...11`.
const STD_JSON_ENCODE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);
/// The fixed ADR 0057 `std.json.encode.p_value` parameter identity: `...11`.
const STD_JSON_ENCODE_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);
/// The fixed ADR 0057 `std.json.encode` function-revision identity: `...11`.
const STD_JSON_ENCODE_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);
/// The fixed ADR 0057 `std.terminal.present_table` function identity: `...12`.
const STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]);
/// The fixed ADR 0057 `std.terminal.present_table.p_rows` parameter identity: `...12`.
const STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]);
/// The fixed ADR 0057 `std.terminal.present_table` function-revision identity: `...12`.
const STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]);
/// The fixed ADR 0067 `std.csv.encode` function identity: `...13`.
const STD_CSV_ENCODE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x13]);
/// The fixed ADR 0067 `std.csv.encode.p_rows` parameter identity: `...13`.
const STD_CSV_ENCODE_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x13]);
/// The fixed ADR 0067 `std.csv.encode` function-revision identity: `...13`.
const STD_CSV_ENCODE_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x13]);

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

    /// Transfers validated rows without cloning their value payloads.
    pub(crate) fn into_rows(self) -> ResultRows {
        self.rows
    }
}

/// A typed rejection of the initial SERVER `SELECT` execution subset.
#[non_exhaustive]
#[derive(Debug)]
pub enum ServerSelectError {
    /// Authorisation evidence does not cover the recovered active revision.
    AuthorisationMismatch {
        /// The immutable target covered by the authorisation evidence.
        authorised: Box<InvocationTarget>,
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
    /// The function or its result is outside the raw-call SERVER boundary.
    RawTarget {
        /// The requested function identity.
        function: FunctionId,
        /// The exact rejected raw-call rule.
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
    /// The pinned standard parameter-echo artifact cannot decode.
    ParameterEchoDecode(ServerParameterEchoError),
    /// The pinned standard json-encode artifact cannot decode.
    JsonEncodeDecode(JsonEncodePlanError),
    /// The pinned standard terminal-table artifact cannot decode.
    TerminalTableDecode(TerminalTablePlanError),
    /// The pinned standard csv-encode artifact cannot decode.
    CsvEncodeDecode(CsvEncodePlanError),
    /// A standard presenter cannot convert the bound value without loss.
    Presenter {
        /// The exact rejected presenter rule.
        rule: &'static str,
    },
    /// A standard presenter built an invalid opaque value payload.
    PresenterOpaque(OpaqueValueError),
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
    /// Returned values did not reconstruct the already validated result shape.
    ReturnedRows(ResultRowsError),
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
    /// Canonical record bytes did not decode under the pinned active revision.
    ValueCodec {
        /// The zero-based result row index.
        row: usize,
        /// The zero-based result column index.
        column: usize,
        /// The canonical value-codec failure.
        source: ValueCodecError,
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
            Self::RawTarget { function, rule } => write!(
                formatter,
                "function {} is not an available raw SERVER target: {rule}",
                function.canonical()
            ),
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
            Self::ParameterEchoDecode(error) => {
                write!(
                    formatter,
                    "cannot decode server parameter-echo artifact: {error}"
                )
            }
            Self::JsonEncodeDecode(error) => {
                write!(
                    formatter,
                    "cannot decode server json-encode artifact: {error}"
                )
            }
            Self::TerminalTableDecode(error) => {
                write!(
                    formatter,
                    "cannot decode server terminal-table artifact: {error}"
                )
            }
            Self::CsvEncodeDecode(error) => {
                write!(
                    formatter,
                    "cannot decode server csv-encode artifact: {error}"
                )
            }
            Self::Presenter { rule } => {
                write!(formatter, "standard presenter execution failed: {rule}")
            }
            Self::PresenterOpaque(error) => {
                write!(
                    formatter,
                    "standard presenter built an invalid opaque value: {error}"
                )
            }
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
            Self::ReturnedRows(error) => {
                write!(formatter, "returned server rows are invalid: {error}")
            }
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
            Self::ValueCodec {
                row,
                column,
                source,
            } => write!(
                formatter,
                "cannot decode canonical result row {row} column {column}: {source}",
            ),
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
            Self::ParameterEchoDecode(error) => Some(error),
            Self::JsonEncodeDecode(error) => Some(error),
            Self::TerminalTableDecode(error) => Some(error),
            Self::CsvEncodeDecode(error) => Some(error),
            Self::PresenterOpaque(error) => Some(error),
            Self::ResultRows(error) => Some(error),
            Self::ReturnedRows(error) => Some(error),
            Self::RowDecode { source, .. } => Some(source),
            Self::ValueCodec { source, .. } => Some(source),
            Self::FunctionNotActive { .. }
            | Self::AuthorisationMismatch { .. }
            | Self::FunctionDomain { .. }
            | Self::FunctionSignature { .. }
            | Self::RawTarget { .. }
            | Self::CurrentRevision { .. }
            | Self::Artifact { .. }
            | Self::PlanInvariant { .. }
            | Self::Distinct { .. }
            | Self::Presenter { .. }
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
    execute_recovered_server_select(transaction, &active, function_id, arguments, test_barrier)
        .await
}

async fn execute_recovered_server_select(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
    arguments: &[FunctionArgument],
    test_barrier: Option<&SelectTestBarrier>,
) -> Result<ServerSelectResult, PostgresKernelError> {
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
        execute_active_transaction(transaction, active, function, context, arguments).await;
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
            authorised: Box::new(target),
            active: active.pair(),
        }));
    }
    execute_recovered_server_select(transaction, active, target.function(), arguments, None).await
}

/// Executes one raw-compatible SERVER SELECT through its existing authorised entry.
///
/// Parameter-free calls retain the one-column, many-row boundary. A call with
/// one Reference uses the version-2 identity-selected boundary and one Text
/// value uses the version-4 unique-Text-selected boundary. Both flatten only
/// their zero-or-one result row. The caller owns the savepoint and outer
/// transaction.
pub(crate) async fn execute_authorised_raw_server_select(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
) -> Result<Vec<RuntimeValue>, PostgresKernelError> {
    let function = authorisation.target().function();
    if raw_unique_text_selected_server_select_target_is_selected(active, function) {
        validate_raw_unique_text_selected_server_select_target(active, function)?;
    } else if arguments.is_empty() {
        validate_raw_server_select_target(active, function)?;
    } else {
        validate_raw_identity_selected_server_select_target(active, function)?;
    }
    let result =
        execute_authorised_server_select(transaction, active, authorisation, arguments).await?;
    if arguments.is_empty() {
        into_raw_server_values(active, function, result)
    } else {
        into_raw_selected_server_values(active, function, result)
    }
}

/// Reports whether an active artifact is a superficial version-4 raw SELECT candidate.
///
/// The check deliberately stops before decoding or validating the target. An
/// authorised caller uses it only to select the existing SELECT savepoint;
/// complete validation remains in [`execute_authorised_raw_server_select`].
pub(crate) fn raw_unique_text_selected_server_select_target_is_selected(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
) -> bool {
    let Some(function) = active.catalogue().function_by_id(function_id) else {
        return false;
    };
    let Some(revision) = active.function_revisions().iter().find(|revision| {
        revision.function() == function_id && revision.id() == function.current_revision()
    }) else {
        return false;
    };
    let artifact = revision.artifact();
    function.domain() == FunctionDomain::Server
        && artifact.kind() == ExecutableArtifactKind::Server
        && artifact.format() == SERVER_PLAN_FORMAT
        && artifact.version() == UNIQUE_TEXT_SELECTED_SERVER_PLAN_VERSION
}

/// Reports whether an active artifact is a superficial version-2 raw SELECT candidate.
///
/// The check deliberately stops before decoding or validating the target. An
/// authorised caller uses it only to select the existing SELECT savepoint;
/// complete validation remains in [`execute_authorised_raw_server_select`].
pub(crate) fn raw_identity_selected_server_select_target_is_selected(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
) -> bool {
    let Some(function) = active.catalogue().function_by_id(function_id) else {
        return false;
    };
    let Some(revision) = active.function_revisions().iter().find(|revision| {
        revision.function() == function_id && revision.id() == function.current_revision()
    }) else {
        return false;
    };
    let artifact = revision.artifact();
    function.domain() == FunctionDomain::Server
        && artifact.kind() == ExecutableArtifactKind::Server
        && artifact.format() == SERVER_PLAN_FORMAT
        && artifact.version() == IDENTITY_SELECTED_SERVER_PLAN_VERSION
}

fn validate_raw_identity_selected_server_select_target(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
) -> Result<(), PostgresKernelError> {
    let function = active
        .catalogue()
        .function_by_id(function_id)
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: function_id,
            })
        })?;
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function_id,
        }));
    }
    if function.parameters().len() != 1 {
        return Err(raw_target_error(
            function_id,
            "raw identity-selected SERVER calls must declare exactly one parameter",
        ));
    }
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(raw_target_error(
            function_id,
            "raw identity-selected SERVER calls must return nonempty ROWS",
        ));
    };
    if columns.is_empty() {
        return Err(raw_target_error(
            function_id,
            "raw identity-selected SERVER calls must return nonempty ROWS",
        ));
    }
    if columns.iter().any(|column| {
        !raw_result_type_is_supported(
            active.catalogue(),
            active.catalogue_hash_context(),
            column.resolved_type(),
        )
    }) {
        return Err(raw_target_error(
            function_id,
            "raw identity-selected SERVER results support only protocol-1 scalar and reference values",
        ));
    }
    Ok(())
}

fn validate_raw_unique_text_selected_server_select_target(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
) -> Result<(), PostgresKernelError> {
    let function = active
        .catalogue()
        .function_by_id(function_id)
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: function_id,
            })
        })?;
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function_id,
        }));
    }
    if function.parameters().len() != 1 {
        return Err(raw_target_error(
            function_id,
            "raw unique-Text-selected SERVER calls must declare exactly one parameter",
        ));
    }
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(raw_target_error(
            function_id,
            "raw unique-Text-selected SERVER calls must return nonempty ROWS",
        ));
    };
    if columns.is_empty() {
        return Err(raw_target_error(
            function_id,
            "raw unique-Text-selected SERVER calls must return nonempty ROWS",
        ));
    }
    if columns.iter().any(|column| {
        !raw_result_type_is_supported(
            active.catalogue(),
            active.catalogue_hash_context(),
            column.resolved_type(),
        )
    }) {
        return Err(raw_target_error(
            function_id,
            "raw unique-Text-selected SERVER results support only protocol-1 scalar and reference values",
        ));
    }
    Ok(())
}

/// Validates the closed parameter-free, one-column raw SERVER target shape.
pub(crate) fn validate_raw_server_select_target(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
) -> Result<(), PostgresKernelError> {
    let function = active
        .catalogue()
        .function_by_id(function_id)
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: function_id,
            })
        })?;
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function_id,
        }));
    }
    if !function.parameters().is_empty() {
        return Err(raw_target_error(
            function_id,
            "raw SERVER calls must have zero parameters",
        ));
    }
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(raw_target_error(
            function_id,
            "raw SERVER calls must return ROWS with exactly one column",
        ));
    };
    let [column] = columns.as_slice() else {
        return Err(raw_target_error(
            function_id,
            "raw SERVER calls must return ROWS with exactly one column",
        ));
    };
    if !raw_result_type_is_supported(
        active.catalogue(),
        active.catalogue_hash_context(),
        column.resolved_type(),
    ) {
        return Err(raw_target_error(
            function_id,
            "raw SERVER results support only protocol-1 scalar and reference values",
        ));
    }
    Ok(())
}

/// Transfers one-column raw SERVER results without cloning value payloads.
pub(crate) fn into_raw_server_values(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
    result: ServerSelectResult,
) -> Result<Vec<RuntimeValue>, PostgresKernelError> {
    into_raw_server_values_for_context(
        active.catalogue(),
        active.catalogue_hash_context(),
        function,
        result,
    )
}

fn into_raw_server_values_for_context(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: FunctionId,
    result: ServerSelectResult,
) -> Result<Vec<RuntimeValue>, PostgresKernelError> {
    result
        .into_rows()
        .into_rows()
        .into_iter()
        .map(|row| {
            let [value] =
                <Vec<RuntimeValue> as TryInto<[RuntimeValue; 1]>>::try_into(row.into_values())
                    .map_err(|_| {
                        raw_target_error(
                            function,
                            "raw SERVER execution must produce exactly one value per row",
                        )
                    })?;
            normalise_raw_runtime_value(catalogue, context, function, value)
        })
        .collect()
}

/// Transfers one zero-or-one-row raw identity result in projection order.
fn into_raw_selected_server_values(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
    result: ServerSelectResult,
) -> Result<Vec<RuntimeValue>, PostgresKernelError> {
    let mut rows = result.into_rows().into_rows().into_iter();
    let Some(row) = rows.next() else {
        return Ok(Vec::new());
    };
    if rows.next().is_some() {
        return Err(raw_target_error(
            function,
            "raw selected SERVER execution must produce at most one row",
        ));
    }
    row.into_values()
        .into_iter()
        .map(|value| {
            normalise_raw_runtime_value(
                active.catalogue(),
                active.catalogue_hash_context(),
                function,
                value,
            )
        })
        .collect()
}

fn normalise_raw_runtime_value(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: FunctionId,
    value: RuntimeValue,
) -> Result<RuntimeValue, PostgresKernelError> {
    if let RuntimeValue::Null(value) = value {
        normalise_raw_null(catalogue, context, function, value.resolved_type())
    } else if raw_runtime_value_is_supported(&value) {
        Ok(value)
    } else {
        Err(raw_target_error(
            function,
            "raw SERVER execution produced a value outside the protocol-1 subset",
        ))
    }
}

/// Reports whether a SERVER failure is an unavailable raw target, not an operational failure.
pub(crate) const fn raw_server_target_is_unavailable(error: &ServerSelectError) -> bool {
    match error {
        ServerSelectError::Execution { source, .. } => raw_server_target_is_unavailable(source),
        ServerSelectError::FunctionNotActive { .. }
        | ServerSelectError::FunctionDomain { .. }
        | ServerSelectError::FunctionSignature { .. }
        | ServerSelectError::RawTarget { .. }
        | ServerSelectError::Artifact { .. }
        | ServerSelectError::PlanDecode(_)
        | ServerSelectError::ParameterEchoDecode(_)
        | ServerSelectError::JsonEncodeDecode(_)
        | ServerSelectError::TerminalTableDecode(_)
        | ServerSelectError::CsvEncodeDecode(_)
        | ServerSelectError::PlanInvariant { .. }
        | ServerSelectError::Distinct { .. }
        | ServerSelectError::ReferenceEvidence { .. }
        | ServerSelectError::Argument { .. }
        | ServerSelectError::Cardinality { .. }
        | ServerSelectError::ResultRows(_)
        | ServerSelectError::VariablePayload { .. }
        | ServerSelectError::ComplexityLimit { .. }
        | ServerSelectError::RowLimit { .. }
        | ServerSelectError::CellLimit { .. }
        | ServerSelectError::PayloadLimit { .. } => true,
        ServerSelectError::AuthorisationMismatch { .. }
        | ServerSelectError::Database { .. }
        | ServerSelectError::Kernel { .. }
        | ServerSelectError::CurrentRevision { .. }
        | ServerSelectError::PreparedResult { .. }
        | ServerSelectError::ReturnedRows(_)
        | ServerSelectError::Presenter { .. }
        | ServerSelectError::PresenterOpaque(_)
        | ServerSelectError::RowDecode { .. }
        | ServerSelectError::ValueInvariant { .. }
        | ServerSelectError::ValueCodec { .. } => false,
    }
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
        DecodedServerPlan::V4(plan) => {
            validate_unique_text_selected_function_signature(function)?;
            validate_unique_text_selected_plan(
                active.catalogue(),
                active.catalogue_hash_context(),
                function,
                plan,
            )?;
            validate_unique_text_selected_reference_evidence(active, function, plan)?;
            let selector = validate_unique_text_selected_arguments(
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
            let lowered = lower_unique_text_selected_plan(
                active.catalogue(),
                active.catalogue_hash_context(),
                plan,
                &columns,
                selector,
            )?;
            (columns, lowered, ResultCardinality::AtMostOne)
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
            active,
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
    V4(UniqueTextSelectedServerPlan),
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
        UNIQUE_TEXT_SELECTED_SERVER_PLAN_VERSION => UniqueTextSelectedServerPlan::decode(payload)
            .map(DecodedServerPlan::V4)
            .map_err(ServerSelectError::PlanDecode)
            .map_err(server_error),
        _ => Err(artifact_error(
            function,
            "current SERVER artifact must use supported orna.server-plan version 1, version 2, version 3, or version 4",
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

/// Executes one closed standard `orna.server-parameter-echo` artifact.
///
/// This engine is reachable only from a pinned standard
/// [`FunctionRevisionRecord`] and its already bound [`FunctionArgument`]. It
/// dispatches purely by checked artifact kind, format, and version, then
/// validates the artifact against the pinned standard function signature:
/// decode pins the function's parameter identity and the resolved INTEGER
/// value type, and the signature validator requires the fixed ADR 0055 echo
/// shape. It never matches a function by Rust name or [`FunctionId`], executes
/// SQL, or opens a PostgreSQL row. The result is the already bound typed
/// integer.
///
/// The sealed `sys.invoke` execution step (ADR 0055 implementation order item
/// 11) is the sole caller (`dispatch_sealed_sys_invoke`).
pub(crate) fn execute_standard_parameter_echo(
    function: &FunctionDefinition,
    revision: &FunctionRevisionRecord,
    arguments: &[FunctionArgument],
) -> Result<RuntimeValue, PostgresKernelError> {
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Server {
        return Err(artifact_error(
            function.id(),
            "current revision must contain a SERVER artifact",
        ));
    }
    if artifact.format() != server_parameter_echo::FORMAT_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-parameter-echo",
        ));
    }
    if artifact.version() != server_parameter_echo::FORMAT_VERSION {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-parameter-echo version 1",
        ));
    }
    if revision.language_version() != server_parameter_echo::LANGUAGE_VERSION_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER revision must use the parameter-echo language version",
        ));
    }
    let parameter = validate_standard_parameter_echo_signature(function)?;
    ServerParameterEcho::decode(artifact.payload(), parameter, INTEGER_TYPE_ID)
        .map_err(ServerSelectError::ParameterEchoDecode)
        .map_err(server_error)?;
    validate_standard_parameter_echo_argument(parameter, arguments)
}

/// Validates one pinned function against the fixed ADR 0055 echo signature.
///
/// The accepted shape is exactly: SERVER domain, one required non-null
/// `INTEGER` parameter with no default expression, one single `INTEGER`
/// result, `SECURITY INVOKER`, `TRANSACTION READ ONLY`, and `VOLATILITY
/// STABLE`. Both the parameter and the result must resolve to the durable
/// INTEGER value type. Returns the pinned parameter identity the artifact must
/// carry.
fn validate_standard_parameter_echo_signature(
    function: &FunctionDefinition,
) -> Result<ParameterId, PostgresKernelError> {
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function.id(),
        }));
    }
    let [parameter] = function.parameters() else {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must declare exactly one required non-null INTEGER parameter",
        ));
    };
    if parameter.default_expression().is_some() {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must declare exactly one required non-null INTEGER parameter",
        ));
    }
    let FunctionReturn::Single(result_type) = function.return_type() else {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must return a single INTEGER value",
        ));
    };
    if !is_standard_integer_type(&parameter.resolved_type())
        || !is_standard_integer_type(result_type)
    {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must declare one INTEGER parameter and one INTEGER result",
        ));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must use INVOKER security",
        ));
    }
    if function.transaction() != Some(FunctionTransaction::ReadOnly) {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must use READ ONLY transactions",
        ));
    }
    if function.volatility() != FunctionVolatility::Stable {
        return Err(function_signature_error(
            function.id(),
            "standard parameter echo functions must use STABLE volatility",
        ));
    }
    Ok(parameter.id())
}

/// Returns whether one resolved type is the standard INTEGER of the pinned
/// V2 context.
///
/// The retained standard catalogue declares the echo parameter and result as
/// the primitive `Scalar(Integer)` form, while the pinned echo artifact
/// carries the durable `Value(INTEGER_TYPE_ID)` identity. Both denote the
/// same standard INTEGER (`orna.std/2` value type `...02`), so the closed
/// signature validator admits exactly these two forms and nothing else.
fn is_standard_integer_type(resolved_type: &ResolvedType) -> bool {
    *resolved_type == ResolvedType::value(INTEGER_TYPE_ID)
        || *resolved_type == ResolvedType::scalar(StandardScalar::Integer)
}

/// Validates the exact bound argument of one standard parameter-echo call.
///
/// The engine accepts exactly one argument bound to the pinned parameter that
/// carries one non-null `RuntimeValue::Integer`, and returns that typed
/// integer. A typed null cannot cross the [`FunctionArgument`] boundary; the
/// explicit null arm keeps the closed-engine invariant independent of that
/// boundary.
fn validate_standard_parameter_echo_argument(
    parameter: ParameterId,
    arguments: &[FunctionArgument],
) -> Result<RuntimeValue, PostgresKernelError> {
    let [argument] = arguments else {
        return Err(argument_error(
            None,
            "standard parameter echo calls require exactly one argument",
        ));
    };
    if argument.parameter() != parameter {
        return Err(argument_error(
            Some(argument.parameter()),
            "standard parameter echo arguments must bind the pinned parameter identity",
        ));
    }
    match argument.value() {
        RuntimeValue::Integer(value) => Ok(RuntimeValue::Integer(*value)),
        RuntimeValue::Null(_) => Err(argument_error(
            Some(parameter),
            "standard parameter echo arguments cannot be NULL",
        )),
        _ => Err(argument_error(
            Some(parameter),
            "standard parameter echo arguments must be one non-null INTEGER value",
        )),
    }
}

/// Executes one closed standard `orna.server-json-encode` artifact.
///
/// This engine is reachable only from a pinned standard
/// [`FunctionRevisionRecord`], its already bound [`FunctionArgument`], the
/// active revision it executes against, and the opaque codec registry of the
/// active verified standard. It dispatches purely by checked artifact kind,
/// format, and version, then validates the artifact against the pinned
/// standard presenter signature: decode pins the function's parameter
/// identity and the resolved `std.json.Value` value type, and the signature
/// validator requires the fixed ADR 0057 `std.json.encode` shape. It never
/// matches a function by Rust name or [`FunctionId`], executes SQL, or opens
/// a PostgreSQL row.
///
/// The bound value converts to JSON without loss (integers, bigints, floats,
/// booleans, text, bytes as base64, references as an explicit
/// `$ref`/`$type` object, lists, maps, and null), and the result is one
/// `std.io.ByteStream` opaque value whose payload follows the ADR 0058 codec
/// framing (`ORNA-BYTE-STREAM/1 <media-type-len:u32 be> <media-type>
/// <len:u32 be> <bytes>`) with media type `application/json`.
///
/// ADR 0057 step 7 wires the presenter engines into the sealed output
/// resolution; the sealed route (`dispatch_sealed_sys_invoke`) is the sole
/// caller of this engine.
pub(crate) fn execute_standard_json_encode(
    function: &FunctionDefinition,
    revision: &FunctionRevisionRecord,
    arguments: &[FunctionArgument],
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<RuntimeValue, PostgresKernelError> {
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Server {
        return Err(artifact_error(
            function.id(),
            "current revision must contain a SERVER artifact",
        ));
    }
    if artifact.format() != server_json_encode::FORMAT_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-json-encode",
        ));
    }
    if artifact.version() != server_json_encode::FORMAT_VERSION {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-json-encode version 1",
        ));
    }
    if revision.language_version() != server_json_encode::LANGUAGE_VERSION_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER revision must use the json-encode language version",
        ));
    }
    let parameter = validate_standard_json_encode_signature(function)?;
    JsonEncodePlan::decode(artifact.payload(), parameter, STD_JSON_VALUE_TYPE_ID)
        .map_err(ServerSelectError::JsonEncodeDecode)
        .map_err(server_error)?;
    let value = validate_standard_json_encode_argument(parameter, arguments)?;
    let json = encode_json_value(active, value)
        .map_err(|rule| ServerSelectError::Presenter { rule })
        .map_err(server_error)?;
    let json_bytes = serde_json::to_vec(&json).map_err(|_| {
        server_error(ServerSelectError::Presenter {
            rule: "std.json.encode produced an unrepresentable JSON document",
        })
    })?;
    let payload = frame_byte_stream(b"application/json", &json_bytes);
    let opaque = OpaqueValue::new(active, registry, STD_IO_BYTE_STREAM_TYPE_ID, &payload)
        .map_err(ServerSelectError::PresenterOpaque)
        .map_err(server_error)?;
    Ok(RuntimeValue::Opaque(opaque))
}

/// Executes one closed standard `orna.server-terminal-table` artifact.
///
/// This engine is reachable only from a pinned standard
/// [`FunctionRevisionRecord`], the bound `std.data.Rows` input (the validated
/// [`ResultRows`] result set itself, which cannot ride the value channel),
/// the active revision it executes against, and the opaque codec registry of
/// the active verified standard. It dispatches purely by checked artifact
/// kind, format, and version, then validates the artifact against the pinned
/// standard presenter signature: decode pins the function's parameter
/// identity and the resolved `std.data.Rows` value type, and the signature
/// validator requires the fixed ADR 0057 `std.terminal.present_table` shape.
/// It never matches a function by Rust name or [`FunctionId`], executes SQL,
/// or opens a PostgreSQL row.
///
/// The bound rows render as the fixed plain-text table (column headers,
/// aligned values, and a trailing row count), and the result is one
/// `std.terminal.Document` opaque value whose payload follows the ADR 0058
/// codec framing (`ORNA-TERMINAL-DOCUMENT/1 <len:u32 be> <utf-8>`).
///
/// ADR 0057 step 7 wires the presenter engines into the sealed output
/// resolution; the sealed route (`dispatch_sealed_sys_invoke`) is the sole
/// caller of this engine.
pub(crate) fn execute_standard_terminal_table(
    function: &FunctionDefinition,
    revision: &FunctionRevisionRecord,
    rows: &ResultRows,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<RuntimeValue, PostgresKernelError> {
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Server {
        return Err(artifact_error(
            function.id(),
            "current revision must contain a SERVER artifact",
        ));
    }
    if artifact.format() != server_terminal_table::FORMAT_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-terminal-table",
        ));
    }
    if artifact.version() != server_terminal_table::FORMAT_VERSION {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-terminal-table version 1",
        ));
    }
    if revision.language_version() != server_terminal_table::LANGUAGE_VERSION_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER revision must use the terminal-table language version",
        ));
    }
    let parameter = validate_standard_terminal_present_table_signature(function)?;
    TerminalTablePlan::decode(artifact.payload(), parameter, STD_DATA_ROWS_TYPE_ID)
        .map_err(ServerSelectError::TerminalTableDecode)
        .map_err(server_error)?;
    let document = render_terminal_table(active, rows)
        .map_err(|rule| ServerSelectError::Presenter { rule })
        .map_err(server_error)?;
    let payload = frame_terminal_document(&document);
    let opaque = OpaqueValue::new(active, registry, STD_TERMINAL_DOCUMENT_TYPE_ID, &payload)
        .map_err(ServerSelectError::PresenterOpaque)
        .map_err(server_error)?;
    Ok(RuntimeValue::Opaque(opaque))
}

/// Executes one closed standard `orna.server-csv-encode` artifact.
///
/// This engine is reachable only from a pinned standard
/// [`FunctionRevisionRecord`], the bound `std.data.Rows` input (the validated
/// [`ResultRows`] result set itself, which cannot ride the value channel),
/// the active revision it executes against, and the opaque codec registry of
/// the active verified standard. It dispatches purely by checked artifact
/// kind, format, and version, then validates the artifact against the pinned
/// standard presenter signature: decode pins the function's parameter
/// identity and the resolved `std.data.Rows` value type, and the signature
/// validator requires the fixed ADR 0067 `std.csv.encode` shape.
///
/// The bound rows render as one CSV document (header row of column names,
/// one row per result row, RFC-4180-style quoting), and the result is one
/// `std.io.ByteStream` opaque value whose payload follows the ADR 0058 codec
/// framing (`ORNA-BYTE-STREAM/1 <media-type:u32 be> <media-type>
/// <len:u32 be> <bytes>`) with media type `text/csv`.
pub(crate) fn execute_standard_csv_encode(
    function: &FunctionDefinition,
    revision: &FunctionRevisionRecord,
    rows: &ResultRows,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<RuntimeValue, PostgresKernelError> {
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Server {
        return Err(artifact_error(
            function.id(),
            "current revision must contain a SERVER artifact",
        ));
    }
    if artifact.format() != server_csv_encode::FORMAT_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-csv-encode",
        ));
    }
    if artifact.version() != server_csv_encode::FORMAT_VERSION {
        return Err(artifact_error(
            function.id(),
            "current SERVER artifact must use orna.server-csv-encode version 1",
        ));
    }
    if revision.language_version() != server_csv_encode::LANGUAGE_VERSION_IDENTITY {
        return Err(artifact_error(
            function.id(),
            "current SERVER revision must use the csv-encode language version",
        ));
    }
    let parameter = validate_standard_csv_encode_signature(function)?;
    CsvEncodePlan::decode(artifact.payload(), parameter, STD_DATA_ROWS_TYPE_ID)
        .map_err(ServerSelectError::CsvEncodeDecode)
        .map_err(server_error)?;
    let document = render_csv_document(active, rows)
        .map_err(|rule| ServerSelectError::Presenter { rule })
        .map_err(server_error)?;
    let payload = frame_byte_stream(b"text/csv", document.as_bytes());
    let opaque = OpaqueValue::new(active, registry, STD_IO_BYTE_STREAM_TYPE_ID, &payload)
        .map_err(ServerSelectError::PresenterOpaque)
        .map_err(server_error)?;
    Ok(RuntimeValue::Opaque(opaque))
}

/// One closed presentation failure from the sealed output route (ADR 0057
/// step 7).
///
/// Both the unresolved-requirement and the no-path failures are presentation
/// errors (spec exit 5); the sealed dispatch discloses neither variant in its
/// public result. The `Kernel` variant carries only closed engine or
/// invariant failures.
#[derive(Debug)]
pub(crate) enum SealedPresentationError {
    /// The output requirement did not resolve against the presenter registry:
    /// `ORNA0702` (spec exit 5).
    OutputResolution(OutputResolutionError),
    /// The resolved presenter's input pattern does not accept the canonical
    /// result: `ORNA0701` (spec exit 5).
    NoPath,
    /// A closed presenter-engine or registry-invariant failure.
    Kernel(PostgresKernelError),
}

impl SealedPresentationError {
    /// Returns the stable spec code for this presentation failure.
    #[cfg(test)]
    pub(crate) const fn spec_code(&self) -> &'static str {
        match self {
            Self::OutputResolution(error) => error.spec_code(),
            Self::NoPath => "ORNA0701",
            Self::Kernel(_) => "ORNA0702",
        }
    }
    /// Returns the spec exit code for a presentation error.
    #[cfg(test)]
    pub(crate) const fn exit_code(&self) -> u8 {
        5
    }
}

impl fmt::Display for SealedPresentationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputResolution(error) => write!(formatter, "{error}"),
            Self::NoPath => formatter.write_str("no presenter accepts the canonical result type"),
            Self::Kernel(error) => write!(formatter, "{error}"),
        }
    }
}

/// The immutable sealed presenter registry (ADR 0057 step 7, ADR 0067).
///
/// The standard snapshot does not yet declare the ADR 0057/0067 presenter
/// functions as standard-library objects, so the sealed route constructs the
/// known presenter records here: alias `json` -> `std.json.encode` (input
/// `std.json.Value`, output `std.io.ByteStream` with media type
/// `application/json`), alias `table` -> `std.terminal.present_table`
/// (input `std.data.Rows`, output `std.terminal.Document`, no media type),
/// and alias `csv` -> `std.csv.encode` (input `std.data.Rows`, output
/// `std.io.ByteStream` with media type `text/csv`).
/// All entries stream nothing and carry the default priority.
fn sealed_presenter_registry() -> &'static PresenterRegistry {
    static REGISTRY: OnceLock<PresenterRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let json = PresenterEntry::new(
            String::from("json"),
            STD_JSON_ENCODE_FUNCTION_ID,
            STD_JSON_VALUE_TYPE_ID,
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            Some(String::from("application/json")),
            false,
            0,
        )
        .expect("the fixed json presenter entry is valid");
        let table = PresenterEntry::new(
            String::from("table"),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            STD_DATA_ROWS_TYPE_ID,
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
            None,
            false,
            0,
        )
        .expect("the fixed table presenter entry is valid");
        let csv = PresenterEntry::new(
            String::from("csv"),
            STD_CSV_ENCODE_FUNCTION_ID,
            STD_DATA_ROWS_TYPE_ID,
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            Some(String::from("text/csv")),
            false,
            0,
        )
        .expect("the fixed csv presenter entry is valid");
        PresenterRegistry::new(vec![json, table, csv])
            .expect("the fixed presenter registry is valid")
    })
}

/// Resolves one sealed output requirement and presents the canonical result
/// through the matched presenter engine (ADR 0057 step 7).
///
/// The requirement resolves against the sealed presenter registry with the
/// alias > media-type > type-name precedence, then the matched presenter's
/// input pattern is checked against the canonical result: `std.json.encode`
/// accepts every argument the closed value channel can carry (any
/// json-convertible flat value), while `std.terminal.present_table` and
/// `std.csv.encode` accept the canonical result only when it converts to a
/// bounded `ResultRows` (the one-column, one-row `result` set this step
/// builds). An unresolved alias,
/// media type, or type name is [`SealedPresentationError::OutputResolution`]
/// (`ORNA0702`); a result the matched presenter cannot accept is
/// [`SealedPresentationError::NoPath`] (`ORNA0701`). The presented opaque
/// value replaces the canonical value in the final `ValueBatch`.
pub(crate) fn present_sealed_standard_output(
    requirement: &InvocationOutputRequirement,
    value: RuntimeValue,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<RuntimeValue, SealedPresentationError> {
    let entry = sealed_presenter_registry()
        .resolve_requirement(requirement, |name| {
            active
                .catalogue()
                .type_id_by_name(&TypeLookupName::qualified(name.clone()))
        })
        .map_err(SealedPresentationError::OutputResolution)?;
    match entry.function() {
        STD_JSON_ENCODE_FUNCTION_ID => {
            let argument = FunctionArgument::new(STD_JSON_ENCODE_PARAMETER_ID, value)
                .map_err(|_| SealedPresentationError::NoPath)?;
            execute_standard_json_encode(
                &sealed_json_encode_definition(),
                &sealed_json_encode_revision(),
                std::slice::from_ref(&argument),
                active,
                registry,
            )
            .map_err(sealed_presenter_engine_error)
        }
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID => {
            let rows = sealed_result_rows(value)?;
            execute_standard_terminal_table(
                &sealed_terminal_table_definition(),
                &sealed_terminal_table_revision(),
                &rows,
                active,
                registry,
            )
            .map_err(sealed_presenter_engine_error)
        }
        STD_CSV_ENCODE_FUNCTION_ID => {
            let rows = sealed_result_rows(value)?;
            execute_standard_csv_encode(
                &sealed_csv_encode_definition(),
                &sealed_csv_encode_revision(),
                &rows,
                active,
                registry,
            )
            .map_err(sealed_presenter_engine_error)
        }
        other => Err(SealedPresentationError::Kernel(
            PostgresKernelError::DurableInvariant {
                relation: "sealed presenter registry",
                record: other.canonical(),
                rule: "the sealed presenter registry must name only the ADR 0057/0067 presenters",
            },
        )),
    }
}

/// Converts the canonical sealed result to the bounded `ResultRows` model the
/// terminal-table engine accepts.
///
/// The canonical result cannot ride the value channel as rows, so this step
/// wraps it as the one-column, one-row `result` set. Only flat runtime forms
/// convert; opaque, constructed, and invocation-carrier values have no path
/// to the terminal-document sink (`ORNA0701`).
fn sealed_result_rows(value: RuntimeValue) -> Result<ResultRows, SealedPresentationError> {
    let RuntimeType::Flat(resolved_type) = value.runtime_type() else {
        return Err(SealedPresentationError::NoPath);
    };
    let column = ResultColumn::new("result", resolved_type, value.is_null())
        .map_err(|_| SealedPresentationError::NoPath)?;
    ResultRows::new(vec![column], vec![ResultRow::new([value])])
        .map_err(|_| SealedPresentationError::NoPath)
}

/// Classifies one closed presenter-engine failure for the sealed route.
///
/// A conversion failure inside a presenter engine means the canonical result
/// has no path to the matched sink (`ORNA0701`); every other engine failure
/// is a closed kernel or registry invariant.
fn sealed_presenter_engine_error(error: PostgresKernelError) -> SealedPresentationError {
    match error {
        PostgresKernelError::ServerSelect(ServerSelectError::Presenter { .. }) => {
            SealedPresentationError::NoPath
        }
        other => SealedPresentationError::Kernel(other),
    }
}

/// Builds the closed ADR 0057 `std.json.encode` definition the sealed route
/// executes.
///
/// The exact shape matches the engine's signature validator: SERVER domain,
/// one required non-null `std.json.Value` parameter, one single
/// `std.io.ByteStream` result, INVOKER security, READ ONLY transaction, and
/// STABLE volatility.
fn sealed_json_encode_definition() -> FunctionDefinition {
    FunctionDefinition::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        QualifiedSemanticName::new(["std", "json", "encode"])
            .expect("the fixed json-encode name is qualified"),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_JSON_ENCODE_PARAMETER_ID,
            "p_value",
            0,
            ResolvedType::named(STD_JSON_VALUE_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    )
}

/// Builds the closed ADR 0057 `std.json.encode` revision the sealed route
/// executes: the canonical `orna.server-json-encode` version 1 artifact.
fn sealed_json_encode_revision() -> FunctionRevisionRecord {
    sealed_presenter_revision(
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        sealed_presenter_artifact(
            server_json_encode::FORMAT_IDENTITY,
            server_json_encode::FORMAT_VERSION,
            JsonEncodePlan::new(STD_JSON_ENCODE_PARAMETER_ID, STD_JSON_VALUE_TYPE_ID)
                .expect("the fixed json-encode plan is valid")
                .encode()
                .expect("the fixed json-encode plan encodes"),
        ),
    )
}

/// Builds the closed ADR 0057 `std.terminal.present_table` definition the
/// sealed route executes.
///
/// The exact shape matches the engine's signature validator: SERVER domain,
/// one required non-null `std.data.Rows` parameter, one single
/// `std.terminal.Document` result, INVOKER security, READ ONLY transaction,
/// and STABLE volatility.
fn sealed_terminal_table_definition() -> FunctionDefinition {
    FunctionDefinition::new(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        QualifiedSemanticName::new(["std", "terminal", "present_table"])
            .expect("the fixed present-table name is qualified"),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            "p_rows",
            0,
            ResolvedType::named(STD_DATA_ROWS_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
        )),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    )
}

/// Builds the closed ADR 0057 `std.terminal.present_table` revision the
/// sealed route executes: the canonical `orna.server-terminal-table` version
/// 1 artifact.
fn sealed_terminal_table_revision() -> FunctionRevisionRecord {
    sealed_presenter_revision(
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        sealed_presenter_artifact(
            server_terminal_table::FORMAT_IDENTITY,
            server_terminal_table::FORMAT_VERSION,
            TerminalTablePlan::new(
                STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
                STD_DATA_ROWS_TYPE_ID,
            )
            .expect("the fixed terminal-table plan is valid")
            .encode()
            .expect("the fixed terminal-table plan encodes"),
        ),
    )
}

/// Builds the closed ADR 0067 `std.csv.encode` definition the sealed route
/// executes.
///
/// The exact shape matches the engine's signature validator: SERVER domain,
/// one required non-null `std.data.Rows` parameter, one single
/// `std.io.ByteStream` result, INVOKER security, READ ONLY transaction, and
/// STABLE volatility.
fn sealed_csv_encode_definition() -> FunctionDefinition {
    FunctionDefinition::new(
        STD_CSV_ENCODE_FUNCTION_ID,
        QualifiedSemanticName::new(["std", "csv", "encode"])
            .expect("the fixed csv-encode name is qualified"),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_CSV_ENCODE_PARAMETER_ID,
            "p_rows",
            0,
            ResolvedType::named(STD_DATA_ROWS_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    )
}

/// Builds the closed ADR 0067 `std.csv.encode` revision the sealed route
/// executes: the canonical `orna.server-csv-encode` version 1 artifact.
fn sealed_csv_encode_revision() -> FunctionRevisionRecord {
    sealed_presenter_revision(
        STD_CSV_ENCODE_FUNCTION_ID,
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        server_csv_encode::LANGUAGE_VERSION_IDENTITY,
        sealed_presenter_artifact(
            server_csv_encode::FORMAT_IDENTITY,
            server_csv_encode::FORMAT_VERSION,
            CsvEncodePlan::new(STD_CSV_ENCODE_PARAMETER_ID, STD_DATA_ROWS_TYPE_ID)
                .expect("the fixed csv-encode plan is valid")
                .encode()
                .expect("the fixed csv-encode plan encodes"),
        ),
    )
}

/// Frames one closed presenter artifact payload as a canonical executable
/// artifact.
fn sealed_presenter_artifact(format: &str, version: u32, payload: Vec<u8>) -> ExecutableArtifact {
    let content_hash =
        artifact_payload_digest(&payload).expect("the fixed presenter artifact digests");
    ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        format,
        version,
        payload,
        content_hash,
    )
    .expect("the fixed presenter artifact is valid")
}

/// Builds one closed presenter revision record carrying the exact language
/// version and canonical artifact of the pinned ADR 0057 presenter.
fn sealed_presenter_revision(
    function: FunctionId,
    revision: FunctionRevisionId,
    language_version: &str,
    artifact: ExecutableArtifact,
) -> FunctionRevisionRecord {
    FunctionRevisionRecord::new(
        function,
        revision,
        1,
        SourceOrigin::new(SourceUnitId::from_bytes([0x91; 16]), 0, 1)
            .expect("the fixed presenter source origin is valid"),
        Sha256Digest::from_bytes([0x42; 32]),
        Sha256Digest::from_bytes([0x43; 32]),
        language_version,
        artifact,
    )
    .expect("the fixed presenter revision is valid")
}

/// Validates one pinned function against the fixed ADR 0057
/// `std.json.encode` presenter signature.
///
/// The accepted shape is exactly: SERVER domain, one required non-null
/// `std.json.Value` parameter with no default expression, one single
/// `std.io.ByteStream` result, `SECURITY INVOKER`, `TRANSACTION READ ONLY`,
/// and `VOLATILITY STABLE`. Both the parameter and the result must resolve to
/// the fixed value types. Returns the pinned parameter identity the artifact
/// must carry.
fn validate_standard_json_encode_signature(
    function: &FunctionDefinition,
) -> Result<ParameterId, PostgresKernelError> {
    validate_standard_presenter_signature(
        function,
        STD_JSON_VALUE_TYPE_ID,
        STD_IO_BYTE_STREAM_TYPE_ID,
        "standard json-encode presenters must declare exactly one required non-null std.json.Value parameter",
        "standard json-encode presenters must return a single std.io.ByteStream value",
        "standard json-encode presenters must declare one std.json.Value parameter and one std.io.ByteStream result",
    )
}

/// Validates one pinned function against the fixed ADR 0057
/// `std.terminal.present_table` presenter signature.
///
/// The accepted shape is exactly: SERVER domain, one required non-null
/// `std.data.Rows` parameter with no default expression, one single
/// `std.terminal.Document` result, `SECURITY INVOKER`, `TRANSACTION READ
/// ONLY`, and `VOLATILITY STABLE`. Both the parameter and the result must
/// resolve to the fixed value types. Returns the pinned parameter identity
/// the artifact must carry.
fn validate_standard_terminal_present_table_signature(
    function: &FunctionDefinition,
) -> Result<ParameterId, PostgresKernelError> {
    validate_standard_presenter_signature(
        function,
        STD_DATA_ROWS_TYPE_ID,
        STD_TERMINAL_DOCUMENT_TYPE_ID,
        "standard terminal-table presenters must declare exactly one required non-null std.data.Rows parameter",
        "standard terminal-table presenters must return a single std.terminal.Document value",
        "standard terminal-table presenters must declare one std.data.Rows parameter and one std.terminal.Document result",
    )
}

/// Validates one pinned function against the fixed ADR 0067
/// `std.csv.encode` presenter signature.
///
/// The accepted shape is exactly: SERVER domain, one required non-null
/// `std.data.Rows` parameter with no default expression, one single
/// `std.io.ByteStream` result, `SECURITY INVOKER`, `TRANSACTION READ
/// ONLY`, and `VOLATILITY STABLE`. Both the parameter and the result must
/// resolve to the fixed value types. Returns the pinned parameter identity
/// the artifact must carry.
fn validate_standard_csv_encode_signature(
    function: &FunctionDefinition,
) -> Result<ParameterId, PostgresKernelError> {
    validate_standard_presenter_signature(
        function,
        STD_DATA_ROWS_TYPE_ID,
        STD_IO_BYTE_STREAM_TYPE_ID,
        "standard csv-encode presenters must declare exactly one required non-null std.data.Rows parameter",
        "standard csv-encode presenters must return a single std.io.ByteStream value",
        "standard csv-encode presenters must declare one std.data.Rows parameter and one std.io.ByteStream result",
    )
}

/// Validates one pinned function against the fixed ADR 0057 presenter shape.
///
/// The accepted shape is exactly: SERVER domain, one required non-null
/// parameter with no default expression, one single result, `SECURITY
/// INVOKER`, `TRANSACTION READ ONLY`, and `VOLATILITY STABLE`. The parameter
/// must resolve to `parameter_type` and the result to `result_type`; both
/// the retained named spelling and the durable value-type identity are
/// admitted (the retained standard catalogue spells the parameter and result
/// as resolved named types, while the pinned artifacts carry the durable
/// value-type identities). Returns the pinned parameter identity the artifact
/// must carry.
fn validate_standard_presenter_signature(
    function: &FunctionDefinition,
    parameter_type: TypeId,
    result_type: TypeId,
    parameter_rule: &'static str,
    result_rule: &'static str,
    types_rule: &'static str,
) -> Result<ParameterId, PostgresKernelError> {
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function.id(),
        }));
    }
    let [parameter] = function.parameters() else {
        return Err(function_signature_error(function.id(), parameter_rule));
    };
    if parameter.default_expression().is_some() {
        return Err(function_signature_error(function.id(), parameter_rule));
    }
    let FunctionReturn::Single(result) = function.return_type() else {
        return Err(function_signature_error(function.id(), result_rule));
    };
    if !is_standard_presenter_type(&parameter.resolved_type(), parameter_type)
        || !is_standard_presenter_type(result, result_type)
    {
        return Err(function_signature_error(function.id(), types_rule));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(function_signature_error(
            function.id(),
            "standard presenter functions must use INVOKER security",
        ));
    }
    if function.transaction() != Some(FunctionTransaction::ReadOnly) {
        return Err(function_signature_error(
            function.id(),
            "standard presenter functions must use READ ONLY transactions",
        ));
    }
    if function.volatility() != FunctionVolatility::Stable {
        return Err(function_signature_error(
            function.id(),
            "standard presenter functions must use STABLE volatility",
        ));
    }
    Ok(parameter.id())
}

/// Returns whether one resolved type is the fixed ADR 0057 presenter type.
///
/// The retained standard catalogue spells presenter parameters and results as
/// resolved named types, while the pinned presenter artifacts carry the
/// durable `Value(type_id)` identities; both denote the same fixed value
/// type, so the closed signature validator admits exactly these two forms and
/// nothing else.
fn is_standard_presenter_type(resolved_type: &ResolvedType, type_id: TypeId) -> bool {
    *resolved_type == ResolvedType::value(type_id) || *resolved_type == ResolvedType::named(type_id)
}

/// Validates the exact bound argument of one standard json-encode call.
///
/// The engine accepts exactly one argument bound to the pinned parameter. A
/// typed null cannot cross the [`FunctionArgument`] boundary; the explicit
/// null arm keeps the closed-engine invariant independent of that boundary.
/// The returned value is the already bound typed value, whose conversion to
/// JSON is the presenter's closed lossless rule.
fn validate_standard_json_encode_argument(
    parameter: ParameterId,
    arguments: &[FunctionArgument],
) -> Result<&RuntimeValue, PostgresKernelError> {
    let [argument] = arguments else {
        return Err(argument_error(
            None,
            "standard json-encode calls require exactly one argument",
        ));
    };
    if argument.parameter() != parameter {
        return Err(argument_error(
            Some(argument.parameter()),
            "standard json-encode arguments must bind the pinned parameter identity",
        ));
    }
    match argument.value() {
        RuntimeValue::Null(_) => Err(argument_error(
            Some(parameter),
            "standard json-encode arguments cannot be NULL",
        )),
        value => Ok(value),
    }
}

/// Converts one bound runtime value to JSON without loss.
///
/// The closed ADR 0057 conversion matrix accepts exactly: null, booleans,
/// integers, bigints, floats, text, bytes (base64), references (an explicit
/// `{"$ref": "orna://<type-name>/<object-id>", "$type": "<type-name>"}`
/// object), lists (arrays), and maps (objects). Every other runtime form
/// (enums, records, opaque values, options, and invocation carriers) cannot
/// be represented without loss and is rejected.
fn encode_json_value(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
) -> Result<serde_json::Value, &'static str> {
    match value {
        RuntimeValue::Null(_) => Ok(serde_json::Value::Null),
        RuntimeValue::Boolean(value) => Ok(serde_json::Value::Bool(*value)),
        RuntimeValue::Integer(value) => Ok(serde_json::Value::from(*value)),
        RuntimeValue::BigInt(value) => Ok(serde_json::Value::from(*value)),
        RuntimeValue::Float(value) => serde_json::Number::from_f64(value.value())
            .map(serde_json::Value::Number)
            .ok_or("std.json.encode cannot represent a non-finite FLOAT value"),
        RuntimeValue::Text(value) => Ok(serde_json::Value::String(value.clone())),
        RuntimeValue::Bytes(value) => Ok(serde_json::Value::String(BASE64_STANDARD.encode(value))),
        RuntimeValue::Reference { target, object } => {
            let Some(definition) = active.catalogue().object_type_by_id(*target) else {
                return Err(
                    "std.json.encode cannot encode a reference outside the active catalogue",
                );
            };
            let type_name = definition.name().to_string();
            Ok(serde_json::json!({
                "$ref": format!("orna://{type_name}/{}", object.canonical()),
                "$type": type_name,
            }))
        }
        RuntimeValue::Constructed(value) => match value.kind() {
            ConstructedValueKind::List(values) => values
                .iter()
                .map(|value| encode_json_value(active, value))
                .collect::<Result<Vec<_>, _>>()
                .map(serde_json::Value::Array),
            ConstructedValueKind::Map(entries) => entries
                .iter()
                .map(|(key, value)| {
                    Ok((
                        encode_json_object_key(active, key)?,
                        encode_json_value(active, value)?,
                    ))
                })
                .collect::<Result<serde_json::Map<String, serde_json::Value>, _>>()
                .map(serde_json::Value::Object),
            ConstructedValueKind::Option(_) => {
                Err("std.json.encode cannot convert an OPTION value to JSON without loss")
            }
            _ => Err(
                "std.json.encode cannot convert an unknown constructed value to JSON without loss",
            ),
        },
        RuntimeValue::Enum(_) => {
            Err("std.json.encode cannot convert an ENUM value to JSON without loss")
        }
        RuntimeValue::Record(_) => {
            Err("std.json.encode cannot convert a RECORD value to JSON without loss")
        }
        RuntimeValue::Opaque(_) => {
            Err("std.json.encode cannot convert an OPAQUE value to JSON without loss")
        }
        RuntimeValue::InvokeValue(_)
        | RuntimeValue::InvokeRequest(_)
        | RuntimeValue::InvokeEvent(_) => {
            Err("std.json.encode cannot convert an invocation carrier to JSON without loss")
        }
        _ => Err("std.json.encode cannot convert an unknown runtime value to JSON without loss"),
    }
}

/// Converts one map key to its canonical JSON object-key text.
///
/// JSON object keys are strings, so each lossless scalar form renders in its
/// canonical text: text verbatim, booleans and numbers in decimal, bytes as
/// base64, enums as their declared label, and references as their canonical
/// `orna://` URI. Every other form cannot be reduced to a JSON string key
/// without loss and is rejected.
fn encode_json_object_key(
    active: &ActiveDatabaseRevision,
    key: &RuntimeValue,
) -> Result<String, &'static str> {
    match key {
        RuntimeValue::Text(value) => Ok(value.clone()),
        RuntimeValue::Boolean(value) => Ok(value.to_string()),
        RuntimeValue::Integer(value) => Ok(value.to_string()),
        RuntimeValue::BigInt(value) => Ok(value.to_string()),
        RuntimeValue::Float(value) => Ok(value.value().to_string()),
        RuntimeValue::Bytes(value) => Ok(BASE64_STANDARD.encode(value)),
        RuntimeValue::Enum(value) => Ok(value.label().to_owned()),
        RuntimeValue::Reference { target, object } => {
            let Some(definition) = active.catalogue().object_type_by_id(*target) else {
                return Err(
                    "std.json.encode cannot encode a reference outside the active catalogue",
                );
            };
            Ok(format!(
                "orna://{}/{}",
                definition.name(),
                object.canonical()
            ))
        }
        _ => Err("std.json.encode map keys must be losslessly encodable JSON strings"),
    }
}

/// Frames one media-typed byte payload as the canonical ADR 0058
/// `std.io.ByteStream` payload: `ORNA-BYTE-STREAM/1 <media-type-len:u32 be>
/// <media-type> <len:u32 be> <bytes>`.
fn frame_byte_stream(media_type: &[u8], bytes: &[u8]) -> Vec<u8> {
    let mut payload =
        Vec::with_capacity(BYTE_STREAM_MAGIC.len() + 4 + media_type.len() + 4 + bytes.len());
    payload.extend_from_slice(BYTE_STREAM_MAGIC.as_bytes());
    payload.extend_from_slice(
        &u32::try_from(media_type.len())
            .expect("the presenter media type length fits u32")
            .to_be_bytes(),
    );
    payload.extend_from_slice(media_type);
    payload.extend_from_slice(
        &u32::try_from(bytes.len())
            .expect("the presenter byte payload length fits u32")
            .to_be_bytes(),
    );
    payload.extend_from_slice(bytes);
    payload
}

/// Frames one UTF-8 document as the canonical ADR 0058 `std.terminal.Document`
/// payload: `ORNA-TERMINAL-DOCUMENT/1 <len:u32 be> <utf-8 bytes>`.
fn frame_terminal_document(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut payload = Vec::with_capacity(TERMINAL_DOCUMENT_MAGIC.len() + 4 + bytes.len());
    payload.extend_from_slice(TERMINAL_DOCUMENT_MAGIC.as_bytes());
    payload.extend_from_slice(
        &u32::try_from(bytes.len())
            .expect("the presenter document length fits u32")
            .to_be_bytes(),
    );
    payload.extend_from_slice(bytes);
    payload
}

/// Renders one validated [`ResultRows`] as the fixed plain-text terminal
/// table.
///
/// The fixed layout is one header line (column names padded to their column
/// width), one separator line (`-` repeated to the width of each column), one
/// line per row (cells padded to their column width), and a trailing row
/// count line. Columns are joined by a single space, every line ends with
/// `\n`, and the document carries no control characters: any rendered cell or
/// column name containing a control character is rejected.
fn render_terminal_table(
    active: &ActiveDatabaseRevision,
    rows: &ResultRows,
) -> Result<String, &'static str> {
    let columns = rows.columns();
    let mut widths = Vec::with_capacity(columns.len());
    let mut header = Vec::with_capacity(columns.len());
    for column in columns {
        reject_control_characters(
            column.name(),
            "terminal table column names cannot contain control characters",
        )?;
        widths.push(column.name().chars().count());
        header.push(column.name().to_owned());
    }
    let mut body = Vec::with_capacity(rows.rows().len());
    for row in rows.rows() {
        let mut cells = Vec::with_capacity(columns.len());
        for (index, value) in row.values().iter().enumerate() {
            let cell = render_terminal_cell(active, value)?;
            let width = cell.chars().count();
            if width > widths[index] {
                widths[index] = width;
            }
            cells.push(cell);
        }
        body.push(cells);
    }
    let mut document = String::new();
    push_table_line(&mut document, &header, &widths, false);
    push_table_line(&mut document, &header, &widths, true);
    for cells in &body {
        push_table_line(&mut document, cells, &widths, false);
    }
    let count = rows.rows().len();
    if count == 1 {
        document.push_str("(1 row)\n");
    } else {
        document.push_str(&format!("({count} rows)\n"));
    }
    Ok(document)
}

/// Renders one validated [`ResultRows`] as one CSV document.
///
/// The fixed layout is one header row of column names followed by one row
/// per result row; every row ends with `\n` and the document ends with a
/// trailing newline. Cells render with the same closed rules as the terminal
/// table, then receive RFC-4180-style quoting: a cell containing a comma,
/// double quote, CR, or LF is quoted and embedded quotes are doubled. Column
/// names and cells cannot carry control characters; any rendered fragment
/// containing one is rejected.
fn render_csv_document(
    active: &ActiveDatabaseRevision,
    rows: &ResultRows,
) -> Result<String, &'static str> {
    let columns = rows.columns();
    let mut document = String::new();
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            document.push(',');
        }
        reject_control_characters(
            column.name(),
            "csv column names cannot contain control characters",
        )?;
        push_csv_field(&mut document, column.name());
    }
    document.push('\n');
    for row in rows.rows() {
        for (index, value) in row.values().iter().enumerate() {
            if index > 0 {
                document.push(',');
            }
            let cell = render_terminal_cell(active, value)?;
            push_csv_field(&mut document, &cell);
        }
        document.push('\n');
    }
    Ok(document)
}

/// Appends one CSV field to the document with RFC-4180-style quoting.
///
/// A field containing a comma, double quote, CR, or LF is wrapped in double
/// quotes and every embedded double quote is doubled. A field free of those
/// four characters is appended verbatim.
fn push_csv_field(document: &mut String, field: &str) {
    let needs_quoting = field
        .chars()
        .any(|character| matches!(character, ',' | '"' | '\r' | '\n'));
    if !needs_quoting {
        document.push_str(field);
        return;
    }
    document.push('"');
    for character in field.chars() {
        if character == '"' {
            document.push('"');
        }
        document.push(character);
    }
    document.push('"');
}

/// Appends one aligned table line to the document.
///
/// Data lines left-pad every cell to its column width (the final column is
/// not padded, so lines carry no trailing whitespace); the separator line
/// repeats `-` to the width of each column. Columns are joined by one space.
fn push_table_line(document: &mut String, cells: &[String], widths: &[usize], separator: bool) {
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            document.push(' ');
        }
        if separator {
            document.extend(std::iter::repeat_n('-', widths[index]));
        } else {
            document.push_str(cell);
            if index + 1 < cells.len() {
                let width = cell.chars().count();
                document.extend(std::iter::repeat_n(' ', widths[index] - width));
            }
        }
    }
    document.push('\n');
}

/// Renders one terminal-table cell as plain text.
///
/// Nulls render as `NULL`, scalars in their canonical text, bytes as base64,
/// references as their canonical object id, enums as their declared label,
/// and records as `type-name{field=value, ...}` in declaration order. Opaque
/// values, constructed values, and invocation carriers cannot appear in a
/// validated [`ResultRows`]; the explicit arms keep the renderer closed.
fn render_terminal_cell(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
) -> Result<String, &'static str> {
    let cell = match value {
        RuntimeValue::Null(_) => "NULL".to_owned(),
        RuntimeValue::Boolean(value) => value.to_string(),
        RuntimeValue::Integer(value) => value.to_string(),
        RuntimeValue::BigInt(value) => value.to_string(),
        RuntimeValue::Float(value) => value.value().to_string(),
        RuntimeValue::Text(value) => value.clone(),
        RuntimeValue::Bytes(value) => BASE64_STANDARD.encode(value),
        RuntimeValue::Reference { object, .. } => object.canonical(),
        RuntimeValue::Enum(value) => value.label().to_owned(),
        RuntimeValue::Record(value) => render_record_cell(active, value)?,
        RuntimeValue::Opaque(_) => {
            return Err("terminal tables cannot render OPAQUE values");
        }
        RuntimeValue::Constructed(_) => {
            return Err("terminal tables cannot render constructed values");
        }
        RuntimeValue::InvokeValue(_)
        | RuntimeValue::InvokeRequest(_)
        | RuntimeValue::InvokeEvent(_) => {
            return Err("terminal tables cannot render invocation carriers");
        }
        _ => return Err("terminal tables cannot render an unknown runtime value"),
    };
    reject_control_characters(
        &cell,
        "terminal table cells cannot contain control characters",
    )?;
    Ok(cell)
}

/// Renders one record cell as `type-name{field=value, ...}`.
///
/// Field names and the record type name come from the active catalogue;
/// field values render with the same closed cell rules and are never null
/// (the record constructor rejects null fields).
fn render_record_cell(
    active: &ActiveDatabaseRevision,
    record: &RecordValue,
) -> Result<String, &'static str> {
    let Some(definition) = active
        .catalogue()
        .record_value_type_by_id(record.record_type())
    else {
        return Err("terminal tables cannot render a record outside the active catalogue");
    };
    let mut cell = definition.name().to_string();
    cell.push('{');
    for (index, (field, value)) in definition.fields().iter().zip(record.fields()).enumerate() {
        if index > 0 {
            cell.push_str(", ");
        }
        cell.push_str(field.name());
        cell.push('=');
        cell.push_str(&render_terminal_cell(active, value)?);
    }
    cell.push('}');
    Ok(cell)
}

/// Rejects any control character in one rendered table text fragment.
fn reject_control_characters(text: &str, rule: &'static str) -> Result<(), &'static str> {
    if text.chars().any(char::is_control) {
        Err(rule)
    } else {
        Ok(())
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

fn validate_unique_text_selected_function_signature(
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
            "unique-Text-selected SERVER functions must return nonempty ROWS",
        ));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(function_signature_error(
            function.id(),
            "unique-Text-selected SERVER functions must use INVOKER security",
        ));
    }
    if function.transaction() != Some(FunctionTransaction::ReadOnly) {
        return Err(function_signature_error(
            function.id(),
            "unique-Text-selected SERVER functions must use READ ONLY transactions",
        ));
    }
    if function.volatility() != FunctionVolatility::Stable {
        return Err(function_signature_error(
            function.id(),
            "unique-Text-selected SERVER functions must use STABLE volatility",
        ));
    }
    let [parameter] = function.parameters() else {
        return Err(function_signature_error(
            function.id(),
            "unique-Text-selected SERVER functions must declare exactly one parameter",
        ));
    };
    if parameter.default_expression().is_some() {
        return Err(function_signature_error(
            function.id(),
            "the unique-Text selector parameter cannot have a default expression",
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
        if !supports_result_type(
            catalogue,
            context,
            projection.value_type.resolved_type,
            projection.value_type.nullable,
        ) {
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

fn validate_unique_text_selected_plan(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: &FunctionDefinition,
    plan: &UniqueTextSelectedServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_execution_complexity_for_projections(plan.projections())?;
    let scan = plan.scan();
    let Some(object_type) = catalogue.object_type_by_id(scan.object_type) else {
        return Err(plan_invariant(
            "unique-Text selector scan must use active input zero and an active object type",
        ));
    };
    if scan.input != 0 {
        return Err(plan_invariant(
            "unique-Text selector scan must use active input zero and an active object type",
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
        if !supports_result_type(
            catalogue,
            context,
            projection.value_type.resolved_type,
            projection.value_type.nullable,
        ) {
            return Err(plan_invariant(
                "projection type is outside the initial runtime result subset",
            ));
        }
    }
    let UniqueTextSelectBindValue::Text {
        scan_object_type,
        field_owner,
        field,
        parameter_owner,
        parameter,
        resolved_type,
        field_nullable,
        parameter_required_non_null,
    } = plan.selector();
    if *scan_object_type != scan.object_type || *field_owner != scan.object_type {
        return Err(plan_invariant(
            "unique-Text selector scan and direct field owner must match the active scan",
        ));
    }
    let field = object_type.field_by_id(*field).ok_or_else(|| {
        plan_invariant("unique-Text selector field must exist on the active scanned object type")
    })?;
    if !field.unique()
        || field.nullable() != *field_nullable
        || field.resolved_type() != *resolved_type
        || !supports_unique_text(context, field.resolved_type())
    {
        return Err(plan_invariant(
            "unique-Text selector field must be an exact active nullable or required unique Text field",
        ));
    }
    let [declared_parameter] = function.parameters() else {
        return Err(plan_invariant(
            "unique-Text-selected SERVER function must have one declared selector parameter",
        ));
    };
    if *parameter_owner != function.id()
        || *parameter != declared_parameter.id()
        || !*parameter_required_non_null
        || declared_parameter.default_expression().is_some()
        || declared_parameter.resolved_type() != *resolved_type
        || !supports_unique_text(context, declared_parameter.resolved_type())
    {
        return Err(plan_invariant(
            "unique-Text selector owner, parameter, required fact, and exact Text authority must match the active function signature",
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
        if !supports_result_type(
            catalogue,
            context,
            projection.value_type.resolved_type,
            projection.value_type.nullable,
        ) {
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
        let RuntimeType::Flat(value_type) = value.runtime_type() else {
            return Err(argument_error(
                Some(parameter_id),
                "the argument uses an unsupported type or refers to an unavailable object type",
            ));
        };
        if !runtime_type_is_active(catalogue, context, value_type) {
            return Err(argument_error(
                Some(parameter_id),
                "the argument uses an unsupported type or refers to an unavailable object type",
            ));
        }
        if !runtime_types_match(context, value_type, parameter.resolved_type()) {
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

fn validate_unique_text_selected_arguments(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: &FunctionDefinition,
    plan: &UniqueTextSelectedServerPlan,
    arguments: &[FunctionArgument],
) -> Result<String, PostgresKernelError> {
    let [argument] = arguments else {
        return Err(argument_error(
            None,
            "unique-Text-selected SERVER calls require exactly one Text argument",
        ));
    };
    let UniqueTextSelectBindValue::Text {
        parameter,
        resolved_type,
        ..
    } = plan.selector();
    if argument.parameter() != *parameter {
        return Err(argument_error(
            Some(argument.parameter()),
            "the supplied argument must name the unique-Text selector parameter",
        ));
    }
    let parameter = function
        .parameter_by_id(argument.parameter())
        .ok_or_else(|| {
            argument_error(
                Some(argument.parameter()),
                "an argument was supplied for a parameter that this function does not declare",
            )
        })?;
    if parameter.resolved_type() != *resolved_type
        || !supports_unique_text(context, parameter.resolved_type())
    {
        return Err(argument_error(
            Some(argument.parameter()),
            "the unique-Text selector parameter must retain exact active Text authority",
        ));
    }
    let RuntimeType::Flat(value_type) = argument.value().runtime_type() else {
        return Err(argument_error(
            Some(argument.parameter()),
            "the unique-Text selector argument must be one non-null Text value",
        ));
    };
    if !runtime_type_is_active(catalogue, context, value_type)
        || !runtime_types_match(context, value_type, parameter.resolved_type())
    {
        return Err(argument_error(
            Some(argument.parameter()),
            "the unique-Text selector argument type does not match the declared parameter type",
        ));
    }
    let RuntimeValue::Text(value) = argument.value() else {
        return Err(argument_error(
            Some(argument.parameter()),
            "the unique-Text selector argument must be one non-null Text value",
        ));
    };
    if value.contains('\0') {
        return Err(argument_error(
            Some(argument.parameter()),
            "unique-Text selector arguments cannot contain U+0000",
        ));
    }
    Ok(value.clone())
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
        ResolvedRuntimeType::Record(_) => false,
        ResolvedRuntimeType::Reference(target) => catalogue.object_type_by_id(target).is_some(),
        ResolvedRuntimeType::Unsupported => false,
    }
}

fn function_signature_error(function: FunctionId, rule: &'static str) -> PostgresKernelError {
    server_error(ServerSelectError::FunctionSignature { function, rule })
}

fn raw_target_error(function: FunctionId, rule: &'static str) -> PostgresKernelError {
    server_error(ServerSelectError::RawTarget { function, rule })
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
        if !supports_result_type(
            catalogue,
            context,
            projection.value_type.resolved_type,
            projection.value_type.nullable,
        ) {
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
    nullable: bool,
) -> bool {
    if nullable
        && matches!(
            resolve_catalogue_runtime_type(catalogue, context, resolved_type),
            ResolvedRuntimeType::Record(_)
        )
    {
        return false;
    }
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
        ResolvedRuntimeType::CatalogueEnum(_)
            | ResolvedRuntimeType::Record(_)
            | ResolvedRuntimeType::Reference(_)
    )
}

fn raw_result_type_is_supported(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> bool {
    match resolve_catalogue_runtime_type(catalogue, context, resolved_type) {
        ResolvedRuntimeType::LegacyScalar(scalar)
        | ResolvedRuntimeType::VerifiedValue {
            compatibility: scalar,
            ..
        } => raw_scalar_is_supported(scalar),
        ResolvedRuntimeType::Reference(target) => catalogue.object_type_by_id(target).is_some(),
        ResolvedRuntimeType::CatalogueEnum(_)
        | ResolvedRuntimeType::Record(_)
        | ResolvedRuntimeType::Unsupported => false,
    }
}

fn raw_runtime_value_is_supported(value: &RuntimeValue) -> bool {
    match value {
        RuntimeValue::Null(_) => false,
        RuntimeValue::Boolean(_)
        | RuntimeValue::Integer(_)
        | RuntimeValue::BigInt(_)
        | RuntimeValue::Float(_)
        | RuntimeValue::Text(_)
        | RuntimeValue::Bytes(_)
        | RuntimeValue::Reference { .. } => true,
        RuntimeValue::Enum(_) | RuntimeValue::Record(_) => false,
        _ => false,
    }
}

fn normalise_raw_null(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: FunctionId,
    resolved_type: ResolvedType,
) -> Result<RuntimeValue, PostgresKernelError> {
    let normalised = match resolve_catalogue_runtime_type(catalogue, context, resolved_type) {
        ResolvedRuntimeType::LegacyScalar(scalar)
        | ResolvedRuntimeType::VerifiedValue {
            compatibility: scalar,
            ..
        } if raw_scalar_is_supported(scalar) => ResolvedType::scalar(scalar),
        ResolvedRuntimeType::Reference(target) if catalogue.object_type_by_id(target).is_some() => {
            ResolvedType::reference(target)
        }
        _ => {
            return Err(raw_target_error(
                function,
                "raw SERVER execution produced a null outside the protocol-1 subset",
            ));
        }
    };
    RuntimeValue::null(normalised)
        .map_err(ServerSelectError::ReturnedRows)
        .map_err(server_error)
}

const fn raw_scalar_is_supported(scalar: StandardScalar) -> bool {
    matches!(
        scalar,
        StandardScalar::Boolean
            | StandardScalar::Integer
            | StandardScalar::BigInt
            | StandardScalar::Float
            | StandardScalar::CharacterLargeObject
            | StandardScalar::BinaryLargeObject
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

fn validate_unique_text_selected_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &UniqueTextSelectedServerPlan,
) -> Result<(), PostgresKernelError> {
    validate_body_reference_evidence(
        active,
        function,
        &expected_unique_text_selected_body_references(plan),
        "recorded dependencies must match the unique-Text-selected function signature and query",
        "recorded dependencies must appear in the same order as the unique-Text-selected function signature and query",
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

fn expected_unique_text_selected_body_references(
    plan: &UniqueTextSelectedServerPlan,
) -> Vec<ExpectedDefinitionReference> {
    let mut expected = vec![ExpectedDefinitionReference::new(
        DefinitionReferenceKind::QueryObject,
        DefinitionReferenceTarget::ObjectType(plan.scan().object_type),
    )];
    for projection in plan.projections() {
        add_expression_references(&mut expected, plan.scan().object_type, projection);
    }
    let UniqueTextSelectBindValue::Text {
        field_owner,
        field,
        parameter_owner,
        parameter,
        ..
    } = plan.selector();
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::QueryField,
        DefinitionReferenceTarget::Field {
            owner: *field_owner,
            field: *field,
        },
    ));
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::ParameterRead,
        DefinitionReferenceTarget::Parameter {
            owner: *parameter_owner,
            parameter: *parameter,
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
    Text(String),
}

impl SelectBindValue {
    fn bind_type(&self) -> Type {
        match self {
            Self::Boolean(_) => Type::BOOL,
            Self::Bytes(_) => Type::BYTEA,
            Self::Text(_) => Type::TEXT,
        }
    }

    fn as_to_sql(&self) -> &(dyn ToSql + Sync) {
        match self {
            Self::Boolean(value) => value,
            Self::Bytes(value) => value,
            Self::Text(value) => value,
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

fn lower_unique_text_selected_plan(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    plan: &UniqueTextSelectedServerPlan,
    columns: &[ResultColumn],
    selector: String,
) -> Result<LoweredPlan, PostgresKernelError> {
    let scan = plan.scan();
    let result_columns = RuntimeResultColumns { context, columns };
    let mut lowered = lower_select_projections(
        catalogue,
        result_columns,
        scan.object_type,
        plan.projections(),
    )?;
    let UniqueTextSelectBindValue::Text { field, .. } = plan.selector();
    lowered.lowerer.binds.push(SelectBindValue::Text(selector));
    let selector_placeholder = lowered.lowerer.binds.len();
    let suffix = format!(
        "\nWHERE i0.{} = ${selector_placeholder}\nLIMIT 2",
        field_name(*field),
    );
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
            let guarded_payload_limit = if matches!(
                resolve_catalogue_runtime_type(catalogue, context, columns[index].resolved_type(),),
                ResolvedRuntimeType::Record(_)
            ) {
                variable_payload_limit
                    .checked_add(ACTIVE_VALUE_ENVELOPE_LENGTH)
                    .ok_or_else(|| {
                        server_error(ServerSelectError::PayloadLimit {
                            maximum: PAYLOAD_LIMIT,
                        })
                    })?
            } else {
                variable_payload_limit
            };
            let alias = format!("g{}", guards.len());
            projections.push(format!(
                "CASE WHEN octet_length({expression}) <= {guarded_payload_limit} THEN {expression} ELSE NULL END AS c{index}"
            ));
            guards.push(VariableGuard {
                column: index,
                alias: alias.clone(),
            });
            guard_projections.push(format!(
                "CASE WHEN {expression} IS NULL OR octet_length({expression}) <= {guarded_payload_limit} THEN TRUE ELSE FALSE END AS {alias}"
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
        ResolvedRuntimeType::CatalogueEnum(_) | ResolvedRuntimeType::Record(_)
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
        ResolvedRuntimeType::CatalogueEnum(_) | ResolvedRuntimeType::Record(_) => 0,
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
    active: &'a ActiveDatabaseRevision,
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
            let value = decode_value(shape.active, &row, row_index, column_index, column)?;
            let value_payload = match &value {
                RuntimeValue::Record(_) => {
                    canonical_record_payload_len(shape.active, &value, row_index, column_index)?
                }
                _ => logical_payload_len(&value)?,
            };
            payload = add_payload(payload, value_payload)?;
            values.push(value);
        }
        rows.push(ResultRow::new(values));
    }
    ResultRows::new(shape.columns.to_vec(), rows)
        .map_err(ServerSelectError::ReturnedRows)
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
    active: &ActiveDatabaseRevision,
    row: &Row,
    row_index: usize,
    column_index: usize,
    column: &ResultColumn,
) -> Result<RuntimeValue, PostgresKernelError> {
    let catalogue = active.catalogue();
    let context = active.catalogue_hash_context();
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
                    .map_err(ServerSelectError::ReturnedRows)
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
        ResolvedRuntimeType::Record(record_type) => decode!(Vec<u8>, |encoded| {
            match decode_active_value(active, &encoded) {
                Ok(value) => match &value {
                    RuntimeValue::Record(record) if record.record_type() == record_type => {
                        Ok(value)
                    }
                    _ => Err(server_error(ServerSelectError::ValueInvariant {
                        row: row_index,
                        column: column_index,
                        rule: "canonical record result type must equal its declared active type",
                    })),
                },
                Err(source) => Err(server_error(ServerSelectError::ValueCodec {
                    row: row_index,
                    column: column_index,
                    source,
                })),
            }
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
            .map_err(ServerSelectError::ReturnedRows)
            .map_err(server_error),
    }
}

fn canonical_record_payload_len(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    row: usize,
    column: usize,
) -> Result<usize, PostgresKernelError> {
    encode_active_value(active, value)
        .map_err(|source| {
            server_error(ServerSelectError::ValueCodec {
                row,
                column,
                source,
            })
        })?
        .len()
        .checked_sub(ACTIVE_VALUE_ENVELOPE_LENGTH)
        .ok_or_else(|| {
            server_error(ServerSelectError::ValueInvariant {
                row,
                column,
                rule: "canonical record result must contain one complete ORV3 envelope",
            })
        })
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
        RuntimeValue::Record(_) => {
            return Err(server_error(ServerSelectError::ValueInvariant {
                row: 0,
                column: 0,
                rule: "record payload accounting requires an active revision",
            }));
        }
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
    use orna_artifact::server_json_encode::{self, JsonEncodePlan, JsonEncodePlanError};
    use orna_artifact::server_parameter_echo::{
        self, ServerParameterEcho, ServerParameterEchoError,
    };
    use orna_artifact::server_plan::{IdentitySelector, Scan, ValueType};
    use orna_artifact::server_terminal_table::{self, TerminalTablePlan, TerminalTablePlanError};
    use orna_core::{
        CatalogueRevisionId, ExpressionId, FieldId, InvocationId, ParameterId, PrincipalId,
        SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
        canonical_hash::{
            artifact_payload_digest, catalogue_digest_with_context, source_bundle_digest,
            source_revision_record_digest, source_unit_content_digest,
        },
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, FieldDefinition, FunctionReturnColumnDefinition,
            ObjectTypeDefinition, ParameterDefinition, QualifiedSemanticName,
            RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition,
        },
        invocation::{
            InvocationEventBody, InvocationOutputTypeSelector, InvocationStreamingRequirement,
            InvokeValue,
        },
        revision::{
            ActiveDatabaseRevisionInput, ActiveRevisionContent, CatalogueHashContext,
            DefinitionIdentity, DefinitionOrigin, ExecutableArtifact, ExecutableArtifactKind,
            FunctionRevisionRecord, Sha256Digest, SourceOrigin, StoredSourceRevision,
            StoredSourceUnit, VerifiedStandardLibrarySnapshot,
        },
        types::TypeDescriptor,
    };

    use super::*;

    /// The fixed ADR 0055 `std.invoke.echo` function identity: `...10`.
    const STD_INVOKE_ECHO_FUNCTION_ID: FunctionId =
        FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]);
    /// The fixed ADR 0055 `std.invoke.echo.p_value` parameter identity: `...10`.
    const STD_INVOKE_ECHO_PARAMETER_ID: ParameterId =
        ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]);
    /// The fixed ADR 0055 `std.invoke.echo` function-revision identity: `...10`.
    const STD_INVOKE_ECHO_FUNCTION_REVISION_ID: FunctionRevisionId =
        FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]);

    fn echo_parameter(parameter: ParameterId) -> ParameterDefinition {
        ParameterDefinition::new(
            parameter,
            "p_value",
            0,
            ResolvedType::value(orna_standard::INTEGER_TYPE_ID),
            None,
        )
    }

    fn echo_function(function: FunctionId, parameter: ParameterId) -> FunctionDefinition {
        FunctionDefinition::new(
            function,
            name(&["std", "invoke", "echo"]),
            FunctionDomain::Server,
            vec![echo_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )
    }

    fn echo_payload(parameter: ParameterId) -> Vec<u8> {
        ServerParameterEcho::new(parameter, orna_standard::INTEGER_TYPE_ID)
            .expect("any identities form a valid echo model")
            .encode()
            .expect("the canonical echo model encodes")
    }

    fn artifact(
        kind: ExecutableArtifactKind,
        format: &str,
        version: u32,
        payload: Vec<u8>,
    ) -> ExecutableArtifact {
        let content_hash = artifact_payload_digest(&payload).expect("the payload digests");
        ExecutableArtifact::new(kind, format, version, payload, content_hash)
            .expect("the artifact is valid")
    }

    fn echo_artifact(parameter: ParameterId) -> ExecutableArtifact {
        artifact(
            ExecutableArtifactKind::Server,
            server_parameter_echo::FORMAT_IDENTITY,
            server_parameter_echo::FORMAT_VERSION,
            echo_payload(parameter),
        )
    }

    fn revision_with_artifact(
        function: FunctionId,
        artifact: ExecutableArtifact,
    ) -> FunctionRevisionRecord {
        FunctionRevisionRecord::new(
            function,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            1,
            SourceOrigin::new(SourceUnitId::from_bytes([0x91; 16]), 0, 1)
                .expect("a test source origin is valid"),
            Sha256Digest::from_bytes([0x42; 32]),
            Sha256Digest::from_bytes([0x43; 32]),
            server_parameter_echo::LANGUAGE_VERSION_IDENTITY,
            artifact,
        )
        .expect("the test revision is valid")
    }

    fn echo_revision(function: FunctionId, parameter: ParameterId) -> FunctionRevisionRecord {
        revision_with_artifact(function, echo_artifact(parameter))
    }

    /// The active-catalogue object type targeted by presenter reference tests.
    const PRESENTER_OBJECT_TYPE: TypeId = TypeId::from_bytes([0x91; 16]);
    /// The active-catalogue enum type rendered by table cells.
    const PRESENTER_ENUM_TYPE: TypeId = TypeId::from_bytes([0x92; 16]);
    /// The active-catalogue record type rendered by table cells.
    const PRESENTER_RECORD_TYPE: TypeId = TypeId::from_bytes([0x93; 16]);
    const PRESENTER_RECORD_X_FIELD: FieldId = FieldId::from_bytes([0x94; 16]);
    const PRESENTER_RECORD_Y_FIELD: FieldId = FieldId::from_bytes([0x95; 16]);

    /// Verifies the retained `orna.std/3` standard snapshot.
    fn presenter_standard() -> VerifiedStandardLibrarySnapshot {
        orna_standard::verify_standard_library_v3_snapshot(
            orna_standard::retained_standard_library_v3_snapshot()
                .expect("the retained V3 standard source is valid"),
        )
        .expect("the retained V3 standard source verifies")
    }

    /// Builds the active revision the presenter tests execute against: an
    /// application catalogue holding one object type, one enum type, and one
    /// record type, pinned to the verified V3 standard snapshot.
    fn presenter_active(standard: &VerifiedStandardLibrarySnapshot) -> ActiveDatabaseRevision {
        let schema = SchemaId::from_bytes([0x81; 16]);
        let catalogue_revision = CatalogueRevisionId::from_bytes([0x82; 16]);
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            catalogue_revision,
            vec![SchemaDefinition::new(schema, name(&["app"]))],
            vec![ObjectTypeDefinition::new(
                PRESENTER_OBJECT_TYPE,
                name(&["app", "item"]),
                vec![],
            )],
            vec![],
            vec![EnumTypeDefinition::new(
                PRESENTER_ENUM_TYPE,
                name(&["app", "stage"]),
                ["lead", "qualified"],
            )],
            vec![RecordValueTypeDefinition::new(
                PRESENTER_RECORD_TYPE,
                name(&["app", "status"]),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        PRESENTER_RECORD_X_FIELD,
                        "x",
                        0,
                        TypeDescriptor::named(orna_standard::INTEGER_TYPE_ID),
                    )
                    .expect("the record field descriptor is valid"),
                    RecordValueFieldDefinition::try_new_descriptor(
                        PRESENTER_RECORD_Y_FIELD,
                        "y",
                        1,
                        TypeDescriptor::named(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
                    )
                    .expect("the record field descriptor is valid"),
                ],
            )],
            vec![],
        )
        .expect("the presenter test catalogue is valid");
        let context = CatalogueHashContext::version_two(standard.clone());
        let source_content = "abcdef";
        let source_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x83; 16]),
            0,
            "app/types.orna",
            source_content,
            source_unit_content_digest(source_content).expect("the source unit digests"),
        )
        .expect("the source unit is valid");
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit))
            .expect("the source bundle digests");
        let source_revision = SourceRevisionId::from_bytes([0x84; 16]);
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x85; 16]),
            source_revision,
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x85; 16]),
                None,
                bundle_hash,
            )
            .expect("the source revision record digests"),
        )
        .expect("the stored source revision is valid");
        let source_unit = SourceUnitId::from_bytes([0x83; 16]);
        let origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(schema),
                SourceOrigin::new(source_unit, 0, 1).expect("the test origin is valid"),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(PRESENTER_OBJECT_TYPE),
                SourceOrigin::new(source_unit, 1, 2).expect("the test origin is valid"),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(PRESENTER_ENUM_TYPE),
                SourceOrigin::new(source_unit, 2, 3).expect("the test origin is valid"),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(PRESENTER_RECORD_TYPE),
                SourceOrigin::new(source_unit, 3, 4).expect("the test origin is valid"),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: PRESENTER_RECORD_TYPE,
                    field: PRESENTER_RECORD_X_FIELD,
                },
                SourceOrigin::new(source_unit, 4, 5).expect("the test origin is valid"),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: PRESENTER_RECORD_TYPE,
                    field: PRESENTER_RECORD_Y_FIELD,
                },
                SourceOrigin::new(source_unit, 5, 6).expect("the test origin is valid"),
            ),
        ];
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[])
                .expect("the active catalogue digests");
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(source_revision, catalogue_revision),
                source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
            ),
            context,
        )
        .expect("the presenter active revision is valid")
    }

    fn json_encode_parameter(parameter: ParameterId) -> ParameterDefinition {
        ParameterDefinition::new(
            parameter,
            "p_value",
            0,
            ResolvedType::named(STD_JSON_VALUE_TYPE_ID),
            None,
        )
    }

    fn json_encode_function(
        function: FunctionId,
        parameter: ParameterId,
        revision: FunctionRevisionId,
    ) -> FunctionDefinition {
        FunctionDefinition::new(
            function,
            name(&["std", "json", "encode"]),
            FunctionDomain::Server,
            vec![json_encode_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            revision,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )
    }

    fn terminal_table_parameter(parameter: ParameterId) -> ParameterDefinition {
        ParameterDefinition::new(
            parameter,
            "p_rows",
            0,
            ResolvedType::named(STD_DATA_ROWS_TYPE_ID),
            None,
        )
    }

    fn terminal_table_function(
        function: FunctionId,
        parameter: ParameterId,
        revision: FunctionRevisionId,
    ) -> FunctionDefinition {
        FunctionDefinition::new(
            function,
            name(&["std", "terminal", "present_table"]),
            FunctionDomain::Server,
            vec![terminal_table_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
            )),
            revision,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )
    }

    fn json_encode_payload(parameter: ParameterId) -> Vec<u8> {
        JsonEncodePlan::new(parameter, STD_JSON_VALUE_TYPE_ID)
            .expect("any identities form a valid json-encode model")
            .encode()
            .expect("the canonical json-encode model encodes")
    }

    fn terminal_table_payload(parameter: ParameterId) -> Vec<u8> {
        TerminalTablePlan::new(parameter, STD_DATA_ROWS_TYPE_ID)
            .expect("any identities form a valid terminal-table model")
            .encode()
            .expect("the canonical terminal-table model encodes")
    }

    fn json_encode_artifact(parameter: ParameterId) -> ExecutableArtifact {
        artifact(
            ExecutableArtifactKind::Server,
            server_json_encode::FORMAT_IDENTITY,
            server_json_encode::FORMAT_VERSION,
            json_encode_payload(parameter),
        )
    }

    fn terminal_table_artifact(parameter: ParameterId) -> ExecutableArtifact {
        artifact(
            ExecutableArtifactKind::Server,
            server_terminal_table::FORMAT_IDENTITY,
            server_terminal_table::FORMAT_VERSION,
            terminal_table_payload(parameter),
        )
    }

    fn presenter_revision(
        function: FunctionId,
        revision_id: FunctionRevisionId,
        language_version: &str,
        artifact: ExecutableArtifact,
    ) -> FunctionRevisionRecord {
        FunctionRevisionRecord::new(
            function,
            revision_id,
            1,
            SourceOrigin::new(SourceUnitId::from_bytes([0x91; 16]), 0, 1)
                .expect("a test source origin is valid"),
            Sha256Digest::from_bytes([0x42; 32]),
            Sha256Digest::from_bytes([0x43; 32]),
            language_version,
            artifact,
        )
        .expect("the test revision is valid")
    }

    fn json_encode_revision(
        function: FunctionId,
        parameter: ParameterId,
    ) -> FunctionRevisionRecord {
        presenter_revision(
            function,
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            server_json_encode::LANGUAGE_VERSION_IDENTITY,
            json_encode_artifact(parameter),
        )
    }

    fn terminal_table_revision(
        function: FunctionId,
        parameter: ParameterId,
    ) -> FunctionRevisionRecord {
        presenter_revision(
            function,
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            server_terminal_table::LANGUAGE_VERSION_IDENTITY,
            terminal_table_artifact(parameter),
        )
    }

    fn csv_encode_parameter(parameter: ParameterId) -> ParameterDefinition {
        ParameterDefinition::new(
            parameter,
            "p_rows",
            0,
            ResolvedType::named(STD_DATA_ROWS_TYPE_ID),
            None,
        )
    }

    fn csv_encode_function(
        function: FunctionId,
        parameter: ParameterId,
        revision: FunctionRevisionId,
    ) -> FunctionDefinition {
        FunctionDefinition::new(
            function,
            name(&["std", "csv", "encode"]),
            FunctionDomain::Server,
            vec![csv_encode_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            revision,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )
    }

    fn csv_encode_payload(parameter: ParameterId) -> Vec<u8> {
        CsvEncodePlan::new(parameter, STD_DATA_ROWS_TYPE_ID)
            .expect("any identities form a valid csv-encode model")
            .encode()
            .expect("the canonical csv-encode model encodes")
    }

    fn csv_encode_artifact(parameter: ParameterId) -> ExecutableArtifact {
        artifact(
            ExecutableArtifactKind::Server,
            server_csv_encode::FORMAT_IDENTITY,
            server_csv_encode::FORMAT_VERSION,
            csv_encode_payload(parameter),
        )
    }

    fn csv_encode_revision(function: FunctionId, parameter: ParameterId) -> FunctionRevisionRecord {
        presenter_revision(
            function,
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            server_csv_encode::LANGUAGE_VERSION_IDENTITY,
            csv_encode_artifact(parameter),
        )
    }

    fn assert_csv_encode_decode_rule(
        result: Result<RuntimeValue, PostgresKernelError>,
        expected: CsvEncodePlanError,
    ) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::CsvEncodeDecode(actual))) =
            result
        else {
            panic!("expected a csv-encode decode rejection");
        };
        assert_eq!(actual, expected);
    }

    fn json_encode_argument(parameter: ParameterId, value: RuntimeValue) -> FunctionArgument {
        FunctionArgument::new(parameter, value).expect("the bound json argument is valid")
    }

    fn assert_presenter_artifact_rule(
        result: Result<RuntimeValue, PostgresKernelError>,
        function: FunctionId,
        expected: &'static str,
    ) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::Artifact {
            function: actual_function,
            rule,
        })) = result
        else {
            panic!("expected an artifact rejection");
        };
        assert_eq!(actual_function, function);
        assert_eq!(rule, expected);
    }

    fn assert_json_encode_decode_rule(
        result: Result<RuntimeValue, PostgresKernelError>,
        expected: JsonEncodePlanError,
    ) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::JsonEncodeDecode(actual))) =
            result
        else {
            panic!("expected a json-encode decode rejection");
        };
        assert_eq!(actual, expected);
    }

    fn assert_terminal_table_decode_rule(
        result: Result<RuntimeValue, PostgresKernelError>,
        expected: TerminalTablePlanError,
    ) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::TerminalTableDecode(actual))) =
            result
        else {
            panic!("expected a terminal-table decode rejection");
        };
        assert_eq!(actual, expected);
    }

    fn assert_presenter_rule<T>(result: Result<T, PostgresKernelError>, expected: &'static str) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::Presenter { rule })) = result
        else {
            panic!("expected a presenter conversion rejection");
        };
        assert_eq!(rule, expected);
    }

    fn assert_presenter_domain_rule(result: Result<RuntimeValue, PostgresKernelError>) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::FunctionDomain { .. })) =
            result
        else {
            panic!("expected a function-domain rejection");
        };
    }

    fn assert_presenter_opaque_rule(result: Result<RuntimeValue, PostgresKernelError>) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::PresenterOpaque(_))) = result
        else {
            panic!("expected an opaque-value rejection");
        };
    }

    fn assert_echo_artifact_rule(
        result: Result<RuntimeValue, PostgresKernelError>,
        function: FunctionId,
        expected: &'static str,
    ) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::Artifact {
            function: actual_function,
            rule,
        })) = result
        else {
            panic!("expected an artifact rejection");
        };
        assert_eq!(actual_function, function);
        assert_eq!(rule, expected);
    }

    fn assert_echo_decode_rule(
        result: Result<RuntimeValue, PostgresKernelError>,
        expected: ServerParameterEchoError,
    ) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::ParameterEchoDecode(actual))) =
            result
        else {
            panic!("expected a parameter-echo decode rejection");
        };
        assert_eq!(actual, expected);
    }

    fn assert_echo_domain_rule(result: Result<RuntimeValue, PostgresKernelError>) {
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::FunctionDomain { .. })) =
            result
        else {
            panic!("expected a function-domain rejection");
        };
    }

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

    fn catalogue_with_record_field() -> (CatalogueSnapshot, TypeId, FieldId, TypeId) {
        let object = TypeId::from_bytes([0x74; 16]);
        let field = FieldId::from_bytes([0x75; 16]);
        let record = TypeId::from_bytes([0x76; 16]);
        let enum_type = TypeId::from_bytes([0x77; 16]);
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes([0x78; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x79; 16]),
                name(&["record_test"]),
            )],
            vec![ObjectTypeDefinition::new(
                object,
                name(&["record_test", "object"]),
                vec![FieldDefinition::new(
                    field,
                    "status",
                    0,
                    ResolvedType::named(record),
                    false,
                    false,
                    None,
                    None,
                )],
            )],
            vec![],
            vec![EnumTypeDefinition::new(
                enum_type,
                name(&["record_test", "stage"]),
                ["lead"],
            )],
            vec![RecordValueTypeDefinition::new(
                record,
                name(&["record_test", "status"]),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        FieldId::from_bytes([0x7a; 16]),
                        "stage",
                        0,
                        TypeDescriptor::named(enum_type),
                    )
                    .expect("record field"),
                ],
            )],
            vec![],
        )
        .expect("record catalogue");
        (catalogue, object, field, record)
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

    fn assert_signature_rule<T>(
        result: Result<T, PostgresKernelError>,
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
            [projection.clone()],
            None,
        )
        .unwrap()
        .encode()
        .unwrap();
        let v4 = UniqueTextSelectedServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [projection],
            UniqueTextSelectBindValue::Text {
                scan_object_type: source,
                field_owner: source,
                field: FieldId::from_bytes([0x34; 16]),
                parameter_owner: FunctionId::from_bytes([0x31; 16]),
                parameter: ParameterId::from_bytes([0x33; 16]),
                resolved_type: ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                field_nullable: true,
                parameter_required_non_null: true,
            },
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
                UNIQUE_TEXT_SELECTED_SERVER_PLAN_VERSION,
                &v4,
            ),
            Ok(DecodedServerPlan::V4(_))
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
                rule: "current SERVER artifact must use supported orna.server-plan version 1, version 2, version 3, or version 4",
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
                SelectBindValue::Bytes(vec![0]),
                SelectBindValue::Text(String::from("selector")),
            ]
            .iter()
            .map(SelectBindValue::bind_type)
            .collect::<Vec<_>>(),
            vec![Type::BOOL, Type::BYTEA, Type::TEXT]
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
    fn record_results_require_non_null_bytea_and_guard_the_outer_envelope() {
        let (catalogue, object, field, record) = catalogue_with_record_field();
        let context = CatalogueHashContext::version_one();
        assert!(supports_result_type(
            &catalogue,
            &context,
            ResolvedType::named(record),
            false,
        ));
        assert!(!supports_result_type(
            &catalogue,
            &context,
            ResolvedType::named(record),
            true,
        ));
        assert_eq!(
            expected_postgres_type(&catalogue, &context, ResolvedType::named(record)).unwrap(),
            Type::BYTEA,
        );

        let columns = [ResultColumn::new("status", ResolvedType::named(record), false).unwrap()];
        let expression = Expression {
            kind: ExpressionKind::FieldPath {
                input: 0,
                steps: vec![FieldStep {
                    owner: object,
                    field,
                }],
            },
            value_type: ValueType {
                resolved_type: ResolvedType::named(record),
                nullable: false,
            },
        };
        let logical_limit = variable_payload_limit(&catalogue, &context, &columns).unwrap();
        let lowered = lower_select_projections(
            &catalogue,
            RuntimeResultColumns {
                context: &context,
                columns: &columns,
            },
            object,
            &[expression],
        )
        .unwrap();
        assert_eq!(lowered.variable_payload_limit, logical_limit);
        let guarded_limit = logical_limit + ACTIVE_VALUE_ENVELOPE_LENGTH;
        assert!(
            lowered
                .projections
                .iter()
                .all(|projection| projection.contains(&format!("<= {guarded_limit}")))
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
    fn raw_result_boundary_accepts_only_protocol_one_types() {
        let (catalogue, active_object, _, _) = catalogue();
        let context = CatalogueHashContext::version_one();
        for scalar in StandardScalar::ALL {
            let expected = matches!(
                scalar,
                StandardScalar::Boolean
                    | StandardScalar::Integer
                    | StandardScalar::BigInt
                    | StandardScalar::Float
                    | StandardScalar::CharacterLargeObject
                    | StandardScalar::BinaryLargeObject
            );
            assert_eq!(
                raw_result_type_is_supported(&catalogue, &context, ResolvedType::scalar(scalar)),
                expected,
                "unexpected raw support for {scalar:?}",
            );
        }
        assert!(raw_result_type_is_supported(
            &catalogue,
            &context,
            ResolvedType::reference(active_object),
        ));
        assert!(!raw_result_type_is_supported(
            &catalogue,
            &context,
            ResolvedType::reference(TypeId::from_bytes([0xfe; 16])),
        ));
        assert!(!raw_result_type_is_supported(
            &catalogue,
            &context,
            ResolvedType::named(TypeId::from_bytes([0xfd; 16])),
        ));
    }

    #[test]
    fn raw_result_transfer_preserves_rows_and_reference_nulls() {
        let (catalogue, active_object, _, _) = catalogue();
        let context = CatalogueHashContext::version_one();
        let pair = RevisionPair::new(
            orna_core::SourceRevisionId::from_bytes([0xa1; 16]),
            CatalogueRevisionId::from_bytes([0xa2; 16]),
        );
        let function = FunctionId::from_bytes([0xa3; 16]);
        let revision = FunctionRevisionId::from_bytes([0xa4; 16]);
        let rows = ResultRows::new(
            [ResultColumn::new(
                "value",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .unwrap()],
            [
                ResultRow::new([RuntimeValue::Integer(1)]),
                ResultRow::new([RuntimeValue::Integer(2)]),
            ],
        )
        .unwrap();
        assert_eq!(
            into_raw_server_values_for_context(
                &catalogue,
                &context,
                function,
                ServerSelectResult::new(pair, function, revision, rows),
            )
            .unwrap(),
            vec![RuntimeValue::Integer(1), RuntimeValue::Integer(2)],
        );

        let reference = ResolvedType::reference(active_object);
        let rows = ResultRows::new(
            [ResultColumn::new("value", reference, true).unwrap()],
            [ResultRow::new([RuntimeValue::null(reference).unwrap()])],
        )
        .unwrap();
        assert_eq!(
            into_raw_server_values_for_context(
                &catalogue,
                &context,
                function,
                ServerSelectResult::new(pair, function, revision, rows),
            )
            .unwrap(),
            vec![RuntimeValue::null(reference).unwrap()]
        );
    }

    #[test]
    fn raw_result_transfer_normalises_standard_value_nulls_to_protocol_one_scalars() {
        let (context, boolean) = retained_value_context("orna.kernel.value.boolean@1");
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0xc1; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let pair = RevisionPair::new(
            orna_core::SourceRevisionId::from_bytes([0xc2; 16]),
            CatalogueRevisionId::from_bytes([0xc1; 16]),
        );
        let function = FunctionId::from_bytes([0xc3; 16]);
        let revision = FunctionRevisionId::from_bytes([0xc4; 16]);
        let value_type = ResolvedType::value(boolean);
        let rows = ResultRows::new(
            [ResultColumn::new("value", value_type, true).unwrap()],
            [ResultRow::new([RuntimeValue::null(value_type).unwrap()])],
        )
        .unwrap();

        assert_eq!(
            into_raw_server_values_for_context(
                &catalogue,
                &context,
                function,
                ServerSelectResult::new(pair, function, revision, rows),
            )
            .unwrap(),
            vec![RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap()]
        );
    }

    #[test]
    fn raw_target_classification_separates_validation_from_operations() {
        let function = FunctionId::from_bytes([0xb1; 16]);
        assert!(raw_server_target_is_unavailable(
            &ServerSelectError::RawTarget {
                function,
                rule: "test",
            }
        ));
        assert!(raw_server_target_is_unavailable(
            &ServerSelectError::Execution {
                context: ServerSelectContext::new(
                    RevisionPair::new(
                        orna_core::SourceRevisionId::from_bytes([0xb2; 16]),
                        CatalogueRevisionId::from_bytes([0xb3; 16]),
                    ),
                    function,
                    FunctionRevisionId::from_bytes([0xb4; 16]),
                ),
                source: Box::new(ServerSelectError::PayloadLimit {
                    maximum: PAYLOAD_LIMIT,
                }),
            }
        ));
        assert!(!raw_server_target_is_unavailable(
            &ServerSelectError::PreparedResult { rule: "test" }
        ));
        assert!(!raw_server_target_is_unavailable(
            &ServerSelectError::ReturnedRows(ResultRowsError::NonFiniteFloat)
        ));
        assert!(!raw_server_target_is_unavailable(
            &ServerSelectError::CurrentRevision {
                function,
                revision: FunctionRevisionId::from_bytes([0xb5; 16]),
            }
        ));
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
        assert!(
            ServerSelectError::ParameterEchoDecode(ServerParameterEchoError::InvalidMagic)
                .source()
                .is_some()
        );
        assert_eq!(
            ServerSelectError::ParameterEchoDecode(ServerParameterEchoError::Truncated).to_string(),
            "cannot decode server parameter-echo artifact: truncated orna.server-parameter-echo artifact"
        );
    }

    #[test]
    fn standard_parameter_echo_executes_and_returns_the_bound_integer() {
        let function = echo_function(STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_PARAMETER_ID);
        let revision = echo_revision(STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_PARAMETER_ID);
        let argument =
            FunctionArgument::new(STD_INVOKE_ECHO_PARAMETER_ID, RuntimeValue::Integer(7))
                .expect("the bound integer argument is valid");
        assert_eq!(
            execute_standard_parameter_echo(&function, &revision, &[argument])
                .expect("the exact standard artifact must execute"),
            RuntimeValue::Integer(7)
        );
        let negative =
            FunctionArgument::new(STD_INVOKE_ECHO_PARAMETER_ID, RuntimeValue::Integer(-41))
                .expect("a negative bound integer argument is valid");
        assert_eq!(
            execute_standard_parameter_echo(&function, &revision, &[negative])
                .expect("a negative bound integer must echo unchanged"),
            RuntimeValue::Integer(-41)
        );
    }

    #[test]
    fn standard_parameter_echo_dispatches_without_function_name_or_id_matching() {
        // A different function identity, revision, parameter identity, and name
        // with the same closed echo shape executes identically: the engine
        // dispatches only on artifact kind, format, and version, then validates
        // against the pinned signature.
        let other_function = FunctionId::from_bytes([0x41; 16]);
        let other_parameter = ParameterId::from_bytes([0x42; 16]);
        let function = FunctionDefinition::new(
            other_function,
            name(&["other", "echo"]),
            FunctionDomain::Server,
            vec![echo_parameter(other_parameter)],
            FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
            FunctionRevisionId::from_bytes([0x44; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        let revision = echo_revision(other_function, other_parameter);
        let argument = FunctionArgument::new(other_parameter, RuntimeValue::Integer(3))
            .expect("the bound integer argument is valid");
        assert_eq!(
            execute_standard_parameter_echo(&function, &revision, &[argument])
                .expect("the same artifact shape must execute identically"),
            RuntimeValue::Integer(3)
        );
    }

    #[test]
    fn standard_parameter_echo_rejects_wrong_kind_format_and_version() {
        let function = echo_function(STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_PARAMETER_ID);
        let parameter = STD_INVOKE_ECHO_PARAMETER_ID;
        let argument = || {
            FunctionArgument::new(parameter, RuntimeValue::Integer(5))
                .expect("the bound integer argument is valid")
        };

        // Wrong artifact kind: a CLIENT artifact with the exact echo payload.
        let revision = revision_with_artifact(
            STD_INVOKE_ECHO_FUNCTION_ID,
            artifact(
                ExecutableArtifactKind::Client,
                server_parameter_echo::FORMAT_IDENTITY,
                server_parameter_echo::FORMAT_VERSION,
                echo_payload(parameter),
            ),
        );
        assert_echo_artifact_rule(
            execute_standard_parameter_echo(&function, &revision, &[argument()]),
            STD_INVOKE_ECHO_FUNCTION_ID,
            "current revision must contain a SERVER artifact",
        );

        // Wrong format: a different SERVER artifact format with the exact payload.
        let revision = revision_with_artifact(
            STD_INVOKE_ECHO_FUNCTION_ID,
            artifact(
                ExecutableArtifactKind::Server,
                "orna.server-plan",
                server_parameter_echo::FORMAT_VERSION,
                echo_payload(parameter),
            ),
        );
        assert_echo_artifact_rule(
            execute_standard_parameter_echo(&function, &revision, &[argument()]),
            STD_INVOKE_ECHO_FUNCTION_ID,
            "current SERVER artifact must use orna.server-parameter-echo",
        );

        // Wrong version.
        let revision = revision_with_artifact(
            STD_INVOKE_ECHO_FUNCTION_ID,
            artifact(
                ExecutableArtifactKind::Server,
                server_parameter_echo::FORMAT_IDENTITY,
                2,
                echo_payload(parameter),
            ),
        );
        assert_echo_artifact_rule(
            execute_standard_parameter_echo(&function, &revision, &[argument()]),
            STD_INVOKE_ECHO_FUNCTION_ID,
            "current SERVER artifact must use orna.server-parameter-echo version 1",
        );

        // Wrong revision language version.
        let revision = FunctionRevisionRecord::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            1,
            SourceOrigin::new(SourceUnitId::from_bytes([0x91; 16]), 0, 1)
                .expect("a test source origin is valid"),
            Sha256Digest::from_bytes([0x42; 32]),
            Sha256Digest::from_bytes([0x43; 32]),
            "orna.language/2",
            echo_artifact(parameter),
        )
        .expect("the test revision is valid");
        assert_echo_artifact_rule(
            execute_standard_parameter_echo(&function, &revision, &[argument()]),
            STD_INVOKE_ECHO_FUNCTION_ID,
            "current SERVER revision must use the parameter-echo language version",
        );
    }

    #[test]
    fn standard_parameter_echo_rejects_each_artifact_payload_deviation() {
        let function = echo_function(STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_PARAMETER_ID);
        let parameter = STD_INVOKE_ECHO_PARAMETER_ID;
        let argument = || {
            FunctionArgument::new(parameter, RuntimeValue::Integer(5))
                .expect("the bound integer argument is valid")
        };
        let canonical = echo_payload(parameter);

        // Wrong magic.
        let mut bytes = canonical.clone();
        bytes[0] = b'X';
        let revision = revision_with_artifact(
            STD_INVOKE_ECHO_FUNCTION_ID,
            artifact(
                ExecutableArtifactKind::Server,
                server_parameter_echo::FORMAT_IDENTITY,
                server_parameter_echo::FORMAT_VERSION,
                bytes,
            ),
        );
        assert_echo_decode_rule(
            execute_standard_parameter_echo(&function, &revision, &[argument()]),
            ServerParameterEchoError::InvalidMagic,
        );

        // Wrong parameter identity: the artifact pins a parameter the pinned
        // function does not declare.
        let other_parameter = ParameterId::from_bytes([0x45; 16]);
        let revision = echo_revision(STD_INVOKE_ECHO_FUNCTION_ID, other_parameter);
        assert_echo_decode_rule(
            execute_standard_parameter_echo(&function, &revision, &[argument()]),
            ServerParameterEchoError::UnexpectedParameter {
                actual: other_parameter,
                expected: parameter,
            },
        );

        // Wrong type identity: the artifact pins a non-INTEGER value type.
        let mut bytes = canonical.clone();
        bytes[43] = 0x03;
        let other_type = TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03]);
        let revision = revision_with_artifact(
            STD_INVOKE_ECHO_FUNCTION_ID,
            artifact(
                ExecutableArtifactKind::Server,
                server_parameter_echo::FORMAT_IDENTITY,
                server_parameter_echo::FORMAT_VERSION,
                bytes,
            ),
        );
        assert_echo_decode_rule(
            execute_standard_parameter_echo(&function, &revision, &[argument()]),
            ServerParameterEchoError::UnexpectedType {
                actual: other_type,
                expected: orna_standard::INTEGER_TYPE_ID,
            },
        );

        // Truncated payload.
        let revision = revision_with_artifact(
            STD_INVOKE_ECHO_FUNCTION_ID,
            artifact(
                ExecutableArtifactKind::Server,
                server_parameter_echo::FORMAT_IDENTITY,
                server_parameter_echo::FORMAT_VERSION,
                canonical[..43].to_vec(),
            ),
        );
        assert_echo_decode_rule(
            execute_standard_parameter_echo(&function, &revision, &[argument()]),
            ServerParameterEchoError::Truncated,
        );

        // Excess bytes after the canonical payload.
        let mut excess = canonical;
        excess.push(0);
        let revision = revision_with_artifact(
            STD_INVOKE_ECHO_FUNCTION_ID,
            artifact(
                ExecutableArtifactKind::Server,
                server_parameter_echo::FORMAT_IDENTITY,
                server_parameter_echo::FORMAT_VERSION,
                excess,
            ),
        );
        assert_echo_decode_rule(
            execute_standard_parameter_echo(&function, &revision, &[argument()]),
            ServerParameterEchoError::TrailingBytes,
        );
    }

    #[test]
    fn standard_parameter_echo_signature_rejects_each_shape_deviation() {
        let parameter = STD_INVOKE_ECHO_PARAMETER_ID;
        let valid = || {
            FunctionDefinition::new(
                STD_INVOKE_ECHO_FUNCTION_ID,
                name(&["std", "invoke", "echo"]),
                FunctionDomain::Server,
                vec![echo_parameter(parameter)],
                FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
                STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Stable,
            )
        };
        let revision = || echo_revision(STD_INVOKE_ECHO_FUNCTION_ID, parameter);
        let argument = || {
            FunctionArgument::new(parameter, RuntimeValue::Integer(5))
                .expect("the bound integer argument is valid")
        };
        let run = |function: &FunctionDefinition| {
            execute_standard_parameter_echo(function, &revision(), &[argument()])
        };

        // Wrong domain.
        let client = FunctionDefinition::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            name(&["std", "invoke", "echo"]),
            FunctionDomain::Client,
            vec![echo_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_echo_domain_rule(run(&client));

        // Parameter count and default deviations.
        let none = FunctionDefinition::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            name(&["std", "invoke", "echo"]),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&none),
            STD_INVOKE_ECHO_FUNCTION_ID,
            "standard parameter echo functions must declare exactly one required non-null INTEGER parameter",
        );

        let extra = FunctionDefinition::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            name(&["std", "invoke", "echo"]),
            FunctionDomain::Server,
            vec![
                echo_parameter(parameter),
                echo_parameter(ParameterId::from_bytes([0x47; 16])),
            ],
            FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&extra),
            STD_INVOKE_ECHO_FUNCTION_ID,
            "standard parameter echo functions must declare exactly one required non-null INTEGER parameter",
        );

        let defaulted = FunctionDefinition::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            name(&["std", "invoke", "echo"]),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter,
                "p_value",
                0,
                ResolvedType::value(orna_standard::INTEGER_TYPE_ID),
                Some(ExpressionId::from_bytes([0x48; 16])),
            )],
            FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&defaulted),
            STD_INVOKE_ECHO_FUNCTION_ID,
            "standard parameter echo functions must declare exactly one required non-null INTEGER parameter",
        );

        // Result shape deviations.
        let rows = FunctionDefinition::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            name(&["std", "invoke", "echo"]),
            FunctionDomain::Server,
            vec![echo_parameter(parameter)],
            rows_return(),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&rows),
            STD_INVOKE_ECHO_FUNCTION_ID,
            "standard parameter echo functions must return a single INTEGER value",
        );

        let boolean = FunctionDefinition::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            name(&["std", "invoke", "echo"]),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter,
                "p_value",
                0,
                ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID),
                None,
            )],
            FunctionReturn::Single(ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID)),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&boolean),
            STD_INVOKE_ECHO_FUNCTION_ID,
            "standard parameter echo functions must declare one INTEGER parameter and one INTEGER result",
        );

        let mismatched = FunctionDefinition::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            name(&["std", "invoke", "echo"]),
            FunctionDomain::Server,
            vec![echo_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID)),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&mismatched),
            STD_INVOKE_ECHO_FUNCTION_ID,
            "standard parameter echo functions must declare one INTEGER parameter and one INTEGER result",
        );

        // Security, transaction, and volatility deviations.
        let owner = FunctionDefinition::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            name(&["std", "invoke", "echo"]),
            FunctionDomain::Server,
            vec![echo_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            FunctionSecurity::Definer,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&owner),
            STD_INVOKE_ECHO_FUNCTION_ID,
            "standard parameter echo functions must use INVOKER security",
        );

        for transaction in [None, Some(FunctionTransaction::Atomic)] {
            let wrong_transaction = FunctionDefinition::new(
                STD_INVOKE_ECHO_FUNCTION_ID,
                name(&["std", "invoke", "echo"]),
                FunctionDomain::Server,
                vec![echo_parameter(parameter)],
                FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
                STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
                FunctionSecurity::Invoker,
                transaction,
                FunctionVolatility::Stable,
            );
            assert_signature_rule(
                run(&wrong_transaction),
                STD_INVOKE_ECHO_FUNCTION_ID,
                "standard parameter echo functions must use READ ONLY transactions",
            );
        }

        let volatile = FunctionDefinition::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            name(&["std", "invoke", "echo"]),
            FunctionDomain::Server,
            vec![echo_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Immutable,
        );
        assert_signature_rule(
            run(&volatile),
            STD_INVOKE_ECHO_FUNCTION_ID,
            "standard parameter echo functions must use STABLE volatility",
        );

        // The exact pinned shape still executes after every rejection.
        assert_eq!(
            run(&valid()).expect("the pinned shape must execute"),
            RuntimeValue::Integer(5)
        );
    }

    #[test]
    fn standard_parameter_echo_arguments_are_exact_complete_and_typed() {
        let function = echo_function(STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_PARAMETER_ID);
        let revision = echo_revision(STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_PARAMETER_ID);
        let parameter = STD_INVOKE_ECHO_PARAMETER_ID;

        // Missing argument.
        assert_argument_rule(
            execute_standard_parameter_echo(&function, &revision, &[]),
            None,
            "standard parameter echo calls require exactly one argument",
        );

        // Extra argument.
        let first = FunctionArgument::new(parameter, RuntimeValue::Integer(1))
            .expect("the bound integer argument is valid");
        let second = FunctionArgument::new(parameter, RuntimeValue::Integer(2))
            .expect("the bound integer argument is valid");
        assert_argument_rule(
            execute_standard_parameter_echo(&function, &revision, &[first, second]),
            None,
            "standard parameter echo calls require exactly one argument",
        );

        // Argument bound to a different parameter identity.
        let other = ParameterId::from_bytes([0x46; 16]);
        let wrong = FunctionArgument::new(other, RuntimeValue::Integer(1))
            .expect("the bound integer argument is valid");
        assert_argument_rule(
            execute_standard_parameter_echo(&function, &revision, &[wrong]),
            Some(other),
            "standard parameter echo arguments must bind the pinned parameter identity",
        );

        // Non-INTEGER runtime value.
        let boolean = FunctionArgument::new(parameter, RuntimeValue::Boolean(true))
            .expect("a Boolean argument binds");
        assert_argument_rule(
            execute_standard_parameter_echo(&function, &revision, &[boolean]),
            Some(parameter),
            "standard parameter echo arguments must be one non-null INTEGER value",
        );

        // A typed null cannot cross the bound-argument boundary, so the engine
        // can never receive one: FunctionArgument::new rejects it.
        let null = RuntimeValue::null(ResolvedType::value(orna_standard::INTEGER_TYPE_ID))
            .expect("a typed INTEGER null is valid");
        assert!(matches!(
            FunctionArgument::new(parameter, null),
            Err(orna_core::value::FunctionArgumentError::NullValue {
                parameter: actual,
                ..
            }) if actual == parameter
        ));
    }

    #[test]
    fn raw_server_execution_never_reaches_the_parameter_echo_engine() {
        // A direct raw request for a standard target is denied at raw dispatch
        // because the target is not in the active application catalogue. Even
        // if an echo-formatted artifact sat in an active application revision,
        // the raw SERVER executor's format gate rejects it before any plan
        // decoding: decode_plan accepts only orna.server-plan formats, so the
        // raw path can never reach execute_standard_parameter_echo.
        let parameter = STD_INVOKE_ECHO_PARAMETER_ID;
        let payload = echo_payload(parameter);
        let Err(PostgresKernelError::ServerSelect(ServerSelectError::Artifact { function, rule })) =
            decode_plan(
                STD_INVOKE_ECHO_FUNCTION_ID,
                server_parameter_echo::FORMAT_IDENTITY,
                server_parameter_echo::FORMAT_VERSION,
                &payload,
            )
        else {
            panic!("raw SERVER plan decoding must reject the parameter-echo format");
        };
        assert_eq!(function, STD_INVOKE_ECHO_FUNCTION_ID);
        assert_eq!(rule, "current SERVER artifact must use orna.server-plan");
    }

    #[test]
    fn standard_json_encode_executes_and_returns_the_framed_byte_stream() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let function = json_encode_function(
            STD_JSON_ENCODE_FUNCTION_ID,
            STD_JSON_ENCODE_PARAMETER_ID,
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        );
        let revision =
            json_encode_revision(STD_JSON_ENCODE_FUNCTION_ID, STD_JSON_ENCODE_PARAMETER_ID);
        let argument = json_encode_argument(
            STD_JSON_ENCODE_PARAMETER_ID,
            RuntimeValue::Text("hello".to_owned()),
        );
        let RuntimeValue::Opaque(value) =
            execute_standard_json_encode(&function, &revision, &[argument], &active, &registry)
                .expect("the exact standard artifact must execute")
        else {
            panic!("the json-encode presenter must return one opaque value");
        };
        assert_eq!(
            value.opaque_type(),
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
        );
        let mut expected = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        expected.extend_from_slice(&16_u32.to_be_bytes());
        expected.extend_from_slice(b"application/json");
        expected.extend_from_slice(&7_u32.to_be_bytes());
        expected.extend_from_slice(b"\"hello\"");
        assert_eq!(value.canonical_payload(), expected);
    }

    #[test]
    fn standard_json_encode_dispatches_without_function_name_or_id_matching() {
        // A different function identity, revision identity, and name with the
        // same closed artifact shape executes identically: the engine
        // dispatches only on artifact kind, format, and version, then
        // validates the pinned signature and decodes the artifact.
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let other_function = FunctionId::from_bytes([0x41; 16]);
        let other_revision = FunctionRevisionId::from_bytes([0x43; 16]);
        let function = FunctionDefinition::new(
            other_function,
            name(&["other", "encode"]),
            FunctionDomain::Server,
            vec![json_encode_parameter(STD_JSON_ENCODE_PARAMETER_ID)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            other_revision,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        let revision = json_encode_revision(other_function, STD_JSON_ENCODE_PARAMETER_ID);
        let argument = json_encode_argument(STD_JSON_ENCODE_PARAMETER_ID, RuntimeValue::Integer(3));
        let RuntimeValue::Opaque(value) =
            execute_standard_json_encode(&function, &revision, &[argument], &active, &registry)
                .expect("the same artifact shape must execute identically")
        else {
            panic!("the json-encode presenter must return one opaque value");
        };
        assert_eq!(
            value.opaque_type(),
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
        );
        let mut expected = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        expected.extend_from_slice(&16_u32.to_be_bytes());
        expected.extend_from_slice(b"application/json");
        expected.extend_from_slice(&1_u32.to_be_bytes());
        expected.extend_from_slice(b"3");
        assert_eq!(value.canonical_payload(), expected);
    }

    #[test]
    fn json_encoding_converts_each_scalar_and_reference_form_without_loss() {
        let standard = presenter_standard();
        let active = presenter_active(&standard);

        assert_eq!(
            encode_json_value(
                &active,
                &RuntimeValue::null(ResolvedType::scalar(StandardScalar::Integer))
                    .expect("a typed INTEGER null is valid"),
            )
            .expect("a null encodes"),
            serde_json::json!(null)
        );
        assert_eq!(
            encode_json_value(&active, &RuntimeValue::Boolean(true)).expect("a boolean encodes"),
            serde_json::json!(true)
        );
        assert_eq!(
            encode_json_value(&active, &RuntimeValue::Integer(-41)).expect("an integer encodes"),
            serde_json::json!(-41)
        );
        assert_eq!(
            encode_json_value(&active, &RuntimeValue::BigInt(i64::MAX)).expect("a bigint encodes"),
            serde_json::json!(i64::MAX)
        );
        assert_eq!(
            encode_json_value(
                &active,
                &RuntimeValue::Float(RuntimeFloat::new(1.5).expect("1.5 is finite")),
            )
            .expect("a float encodes"),
            serde_json::json!(1.5)
        );
        assert_eq!(
            encode_json_value(&active, &RuntimeValue::Text("a\"b\\c\n".to_owned()))
                .expect("text encodes"),
            serde_json::json!("a\"b\\c\n")
        );
        assert_eq!(
            encode_json_value(&active, &RuntimeValue::Bytes(vec![0x00, 0xff, 0x10]))
                .expect("bytes encode as base64"),
            serde_json::json!("AP8Q")
        );

        let object = ObjectId::from_bytes([0x55; 16]);
        assert_eq!(
            encode_json_value(
                &active,
                &RuntimeValue::Reference {
                    target: PRESENTER_OBJECT_TYPE,
                    object,
                },
            )
            .expect("a reference encodes"),
            serde_json::json!({
                "$ref": format!("orna://app.item/{}", object.canonical()),
                "$type": "app.item",
            })
        );
    }

    #[test]
    fn json_encoding_converts_lists_and_maps_without_loss() {
        let standard = presenter_standard();
        let active = presenter_active(&standard);

        let integer = TypeDescriptor::named(orna_standard::INTEGER_TYPE_ID);
        let list = RuntimeValue::list(
            &active,
            TypeDescriptor::list(integer.clone()).expect("a list descriptor is valid"),
            vec![
                RuntimeValue::Integer(1),
                RuntimeValue::Integer(2),
                RuntimeValue::Integer(3),
            ],
        )
        .expect("the integer list is valid");
        assert_eq!(
            encode_json_value(&active, &list).expect("a list encodes"),
            serde_json::json!([1, 2, 3])
        );

        let map = RuntimeValue::map(
            &active,
            TypeDescriptor::map(integer.clone(), integer.clone())
                .expect("a map descriptor is valid"),
            vec![
                (RuntimeValue::Integer(2), RuntimeValue::Integer(20)),
                (RuntimeValue::Integer(1), RuntimeValue::Integer(10)),
            ],
        )
        .expect("the integer map is valid");
        assert_eq!(
            encode_json_value(&active, &map).expect("a map encodes"),
            serde_json::json!({ "1": 10, "2": 20 })
        );

        let nested = RuntimeValue::list(
            &active,
            TypeDescriptor::list(
                TypeDescriptor::list(integer).expect("a list descriptor is valid"),
            )
            .expect("a list descriptor is valid"),
            vec![list],
        )
        .expect("the nested list is valid");
        assert_eq!(
            encode_json_value(&active, &nested).expect("a nested list encodes"),
            serde_json::json!([[1, 2, 3]])
        );
    }

    #[test]
    fn json_encoding_rejects_every_non_lossless_runtime_form() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);

        let enum_value = RuntimeValue::Enum(
            EnumValue::new(active.catalogue(), PRESENTER_ENUM_TYPE, "lead")
                .expect("the enum label is declared"),
        );
        assert_presenter_conversion_rule(&active, enum_value, "ENUM");

        let record_value = RuntimeValue::Record(
            RecordValue::new(
                &active,
                PRESENTER_RECORD_TYPE,
                vec![
                    ("x".to_owned(), RuntimeValue::Integer(1)),
                    ("y".to_owned(), RuntimeValue::Text("a".to_owned())),
                ],
            )
            .expect("the record value is valid"),
        );
        assert_presenter_conversion_rule(&active, record_value, "RECORD");

        let mut byte_stream_payload = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        byte_stream_payload.extend_from_slice(&16_u32.to_be_bytes());
        byte_stream_payload.extend_from_slice(b"application/json");
        byte_stream_payload.extend_from_slice(&2_u32.to_be_bytes());
        byte_stream_payload.extend_from_slice(b"{}");
        let opaque_value = RuntimeValue::Opaque(
            OpaqueValue::new(
                &active,
                &registry,
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
                &byte_stream_payload,
            )
            .expect("the byte-stream payload constructs"),
        );
        assert_presenter_conversion_rule(&active, opaque_value, "OPAQUE");

        let option_value = RuntimeValue::option(
            &active,
            TypeDescriptor::option(TypeDescriptor::named(orna_standard::INTEGER_TYPE_ID))
                .expect("an option descriptor is valid"),
            Some(RuntimeValue::Integer(1)),
        )
        .expect("the option value is valid");
        assert_presenter_conversion_rule(&active, option_value, "OPTION");

        let carrier = RuntimeValue::InvokeValue(
            InvokeValue::new(RuntimeValue::Integer(1)).expect("the invoke value is valid"),
        );
        assert_presenter_conversion_rule(&active, carrier, "invocation carrier");

        let foreign_reference = RuntimeValue::Reference {
            target: TypeId::from_bytes([0x61; 16]),
            object: ObjectId::from_bytes([0x62; 16]),
        };
        assert_presenter_conversion_rule(
            &active,
            foreign_reference,
            "outside the active catalogue",
        );
    }

    #[test]
    fn standard_json_encode_rejects_wrong_kind_format_and_version() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let function = json_encode_function(
            STD_JSON_ENCODE_FUNCTION_ID,
            STD_JSON_ENCODE_PARAMETER_ID,
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        );
        let parameter = STD_JSON_ENCODE_PARAMETER_ID;
        let argument = || json_encode_argument(parameter, RuntimeValue::Integer(1));

        let wrong_kind = presenter_revision(
            function.id(),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            server_json_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Client,
                server_json_encode::FORMAT_IDENTITY,
                server_json_encode::FORMAT_VERSION,
                json_encode_payload(parameter),
            ),
        );
        assert_presenter_artifact_rule(
            execute_standard_json_encode(&function, &wrong_kind, &[argument()], &active, &registry),
            function.id(),
            "current revision must contain a SERVER artifact",
        );

        let wrong_format = presenter_revision(
            function.id(),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            server_json_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_parameter_echo::FORMAT_IDENTITY,
                server_json_encode::FORMAT_VERSION,
                json_encode_payload(parameter),
            ),
        );
        assert_presenter_artifact_rule(
            execute_standard_json_encode(
                &function,
                &wrong_format,
                &[argument()],
                &active,
                &registry,
            ),
            function.id(),
            "current SERVER artifact must use orna.server-json-encode",
        );

        let wrong_version = presenter_revision(
            function.id(),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            server_json_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_json_encode::FORMAT_IDENTITY,
                server_json_encode::FORMAT_VERSION + 1,
                json_encode_payload(parameter),
            ),
        );
        assert_presenter_artifact_rule(
            execute_standard_json_encode(
                &function,
                &wrong_version,
                &[argument()],
                &active,
                &registry,
            ),
            function.id(),
            "current SERVER artifact must use orna.server-json-encode version 1",
        );

        let wrong_language = presenter_revision(
            function.id(),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            "orna.language/9",
            json_encode_artifact(parameter),
        );
        assert_presenter_artifact_rule(
            execute_standard_json_encode(
                &function,
                &wrong_language,
                &[argument()],
                &active,
                &registry,
            ),
            function.id(),
            "current SERVER revision must use the json-encode language version",
        );

        assert_eq!(
            execute_standard_json_encode(
                &function,
                &json_encode_revision(function.id(), parameter),
                &[argument()],
                &active,
                &registry
            )
            .expect("the exact artifact must execute"),
            RuntimeValue::Opaque(
                OpaqueValue::new(
                    &active,
                    &registry,
                    orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
                    frame_byte_stream(b"application/json", b"1"),
                )
                .expect("the framed byte stream constructs"),
            )
        );
    }

    #[test]
    fn standard_json_encode_artifacts_reject_each_decode_deviation() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let function = json_encode_function(
            STD_JSON_ENCODE_FUNCTION_ID,
            STD_JSON_ENCODE_PARAMETER_ID,
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        );
        let parameter = STD_JSON_ENCODE_PARAMETER_ID;
        let argument = || json_encode_argument(parameter, RuntimeValue::Integer(1));

        let mut invalid_magic = json_encode_payload(parameter);
        invalid_magic[0] = b'X';
        let revision = presenter_revision(
            function.id(),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            server_json_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_json_encode::FORMAT_IDENTITY,
                server_json_encode::FORMAT_VERSION,
                invalid_magic,
            ),
        );
        assert_json_encode_decode_rule(
            execute_standard_json_encode(&function, &revision, &[argument()], &active, &registry),
            JsonEncodePlanError::InvalidMagic,
        );

        let other_parameter = ParameterId::from_bytes([0x51; 16]);
        let revision = presenter_revision(
            function.id(),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            server_json_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_json_encode::FORMAT_IDENTITY,
                server_json_encode::FORMAT_VERSION,
                json_encode_payload(other_parameter),
            ),
        );
        assert_json_encode_decode_rule(
            execute_standard_json_encode(&function, &revision, &[argument()], &active, &registry),
            JsonEncodePlanError::UnexpectedParameter {
                actual: other_parameter,
                expected: parameter,
            },
        );

        let other_type = orna_standard::BIGINT_TYPE_ID;
        let revision = presenter_revision(
            function.id(),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            server_json_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_json_encode::FORMAT_IDENTITY,
                server_json_encode::FORMAT_VERSION,
                JsonEncodePlan::new(parameter, other_type)
                    .expect("any identities form a valid json-encode model")
                    .encode()
                    .expect("the canonical json-encode model encodes"),
            ),
        );
        assert_json_encode_decode_rule(
            execute_standard_json_encode(&function, &revision, &[argument()], &active, &registry),
            JsonEncodePlanError::UnexpectedType {
                actual: other_type,
                expected: STD_JSON_VALUE_TYPE_ID,
            },
        );

        let truncated = json_encode_payload(parameter)[..40].to_vec();
        let revision = presenter_revision(
            function.id(),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            server_json_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_json_encode::FORMAT_IDENTITY,
                server_json_encode::FORMAT_VERSION,
                truncated,
            ),
        );
        assert_json_encode_decode_rule(
            execute_standard_json_encode(&function, &revision, &[argument()], &active, &registry),
            JsonEncodePlanError::Truncated,
        );

        let mut trailing = json_encode_payload(parameter);
        trailing.push(0);
        let revision = presenter_revision(
            function.id(),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            server_json_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_json_encode::FORMAT_IDENTITY,
                server_json_encode::FORMAT_VERSION,
                trailing,
            ),
        );
        assert_json_encode_decode_rule(
            execute_standard_json_encode(&function, &revision, &[argument()], &active, &registry),
            JsonEncodePlanError::TrailingBytes,
        );
    }

    #[test]
    fn standard_json_encode_signature_rejects_each_shape_deviation() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let parameter = STD_JSON_ENCODE_PARAMETER_ID;
        let revision = json_encode_revision(STD_JSON_ENCODE_FUNCTION_ID, parameter);
        let argument = || json_encode_argument(parameter, RuntimeValue::Integer(1));
        let run = |function: &FunctionDefinition| {
            execute_standard_json_encode(function, &revision, &[argument()], &active, &registry)
        };

        let client = FunctionDefinition::new(
            STD_JSON_ENCODE_FUNCTION_ID,
            name(&["std", "json", "encode"]),
            FunctionDomain::Client,
            vec![json_encode_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_presenter_domain_rule(run(&client));

        let mut missing = json_encode_function(
            STD_JSON_ENCODE_FUNCTION_ID,
            parameter,
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        );
        missing = FunctionDefinition::new(
            missing.id(),
            name(&["std", "json", "encode"]),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&missing),
            STD_JSON_ENCODE_FUNCTION_ID,
            "standard json-encode presenters must declare exactly one required non-null std.json.Value parameter",
        );

        let defaulted = FunctionDefinition::new(
            STD_JSON_ENCODE_FUNCTION_ID,
            name(&["std", "json", "encode"]),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter,
                "p_value",
                0,
                ResolvedType::named(STD_JSON_VALUE_TYPE_ID),
                Some(ExpressionId::from_bytes([0x72; 16])),
            )],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&defaulted),
            STD_JSON_ENCODE_FUNCTION_ID,
            "standard json-encode presenters must declare exactly one required non-null std.json.Value parameter",
        );

        let rows_result = FunctionDefinition::new(
            STD_JSON_ENCODE_FUNCTION_ID,
            name(&["std", "json", "encode"]),
            FunctionDomain::Server,
            vec![json_encode_parameter(parameter)],
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
            )]),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&rows_result),
            STD_JSON_ENCODE_FUNCTION_ID,
            "standard json-encode presenters must return a single std.io.ByteStream value",
        );

        let wrong_parameter_type = FunctionDefinition::new(
            STD_JSON_ENCODE_FUNCTION_ID,
            name(&["std", "json", "encode"]),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter,
                "p_value",
                0,
                ResolvedType::named(orna_standard::BIGINT_TYPE_ID),
                None,
            )],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&wrong_parameter_type),
            STD_JSON_ENCODE_FUNCTION_ID,
            "standard json-encode presenters must declare one std.json.Value parameter and one std.io.ByteStream result",
        );

        let wrong_result_type = FunctionDefinition::new(
            STD_JSON_ENCODE_FUNCTION_ID,
            name(&["std", "json", "encode"]),
            FunctionDomain::Server,
            vec![json_encode_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
            )),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&wrong_result_type),
            STD_JSON_ENCODE_FUNCTION_ID,
            "standard json-encode presenters must declare one std.json.Value parameter and one std.io.ByteStream result",
        );

        let definer = FunctionDefinition::new(
            STD_JSON_ENCODE_FUNCTION_ID,
            name(&["std", "json", "encode"]),
            FunctionDomain::Server,
            vec![json_encode_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Definer,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&definer),
            STD_JSON_ENCODE_FUNCTION_ID,
            "standard presenter functions must use INVOKER security",
        );

        let manual = FunctionDefinition::new(
            STD_JSON_ENCODE_FUNCTION_ID,
            name(&["std", "json", "encode"]),
            FunctionDomain::Server,
            vec![json_encode_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&manual),
            STD_JSON_ENCODE_FUNCTION_ID,
            "standard presenter functions must use READ ONLY transactions",
        );

        let volatile = FunctionDefinition::new(
            STD_JSON_ENCODE_FUNCTION_ID,
            name(&["std", "json", "encode"]),
            FunctionDomain::Server,
            vec![json_encode_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Immutable,
        );
        assert_signature_rule(
            run(&volatile),
            STD_JSON_ENCODE_FUNCTION_ID,
            "standard presenter functions must use STABLE volatility",
        );

        // The exact pinned shape still executes after every rejection.
        assert_eq!(
            execute_standard_json_encode(
                &json_encode_function(
                    STD_JSON_ENCODE_FUNCTION_ID,
                    parameter,
                    STD_JSON_ENCODE_FUNCTION_REVISION_ID
                ),
                &revision,
                &[argument()],
                &active,
                &registry,
            )
            .expect("the pinned shape must execute"),
            RuntimeValue::Opaque(
                OpaqueValue::new(
                    &active,
                    &registry,
                    orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
                    frame_byte_stream(b"application/json", b"1"),
                )
                .expect("the framed byte stream constructs"),
            )
        );
    }

    #[test]
    fn standard_json_encode_rejects_a_mismatched_opaque_codec_registry() {
        // The engine constructs its ByteStream against the codec registry of
        // the active verified standard. A registry bound to a different
        // standard snapshot (here the version-one registry, which registers
        // only the opaque-token codec) cannot validate the presented opaque
        // value and is rejected without producing a value.
        let standard = presenter_standard();
        let active = presenter_active(&standard);
        let version_one = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot()
                .expect("the retained V1 standard source is valid"),
        )
        .expect("the retained V1 standard source verifies");
        let mismatched_registry = orna_standard::registered_opaque_codecs(&version_one)
            .expect("the V1 opaque codecs register");
        let function = json_encode_function(
            STD_JSON_ENCODE_FUNCTION_ID,
            STD_JSON_ENCODE_PARAMETER_ID,
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        );
        let revision =
            json_encode_revision(STD_JSON_ENCODE_FUNCTION_ID, STD_JSON_ENCODE_PARAMETER_ID);
        let argument = json_encode_argument(STD_JSON_ENCODE_PARAMETER_ID, RuntimeValue::Integer(1));
        assert_presenter_opaque_rule(execute_standard_json_encode(
            &function,
            &revision,
            &[argument],
            &active,
            &mismatched_registry,
        ));
    }

    #[test]
    fn standard_json_encode_arguments_are_exact_complete_and_typed() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let function = json_encode_function(
            STD_JSON_ENCODE_FUNCTION_ID,
            STD_JSON_ENCODE_PARAMETER_ID,
            STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        );
        let revision =
            json_encode_revision(STD_JSON_ENCODE_FUNCTION_ID, STD_JSON_ENCODE_PARAMETER_ID);
        let parameter = STD_JSON_ENCODE_PARAMETER_ID;

        // Missing argument.
        assert_argument_rule(
            execute_standard_json_encode(&function, &revision, &[], &active, &registry),
            None,
            "standard json-encode calls require exactly one argument",
        );

        // Extra argument.
        let first = json_encode_argument(parameter, RuntimeValue::Integer(1));
        let second = json_encode_argument(parameter, RuntimeValue::Integer(2));
        assert_argument_rule(
            execute_standard_json_encode(
                &function,
                &revision,
                &[first, second],
                &active,
                &registry,
            ),
            None,
            "standard json-encode calls require exactly one argument",
        );

        // Argument bound to a different parameter identity.
        let other = ParameterId::from_bytes([0x46; 16]);
        let wrong = json_encode_argument(other, RuntimeValue::Integer(1));
        assert_argument_rule(
            execute_standard_json_encode(&function, &revision, &[wrong], &active, &registry),
            Some(other),
            "standard json-encode arguments must bind the pinned parameter identity",
        );

        // A typed null cannot cross the bound-argument boundary, so the engine
        // can never receive one: FunctionArgument::new rejects it.
        let null = RuntimeValue::null(ResolvedType::scalar(StandardScalar::Integer))
            .expect("a typed INTEGER null is valid");
        assert!(matches!(
            FunctionArgument::new(parameter, null),
            Err(orna_core::value::FunctionArgumentError::NullValue {
                parameter: actual,
                ..
            }) if actual == parameter
        ));
    }

    #[test]
    fn standard_terminal_table_executes_and_returns_the_framed_document() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let function = terminal_table_function(
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        );
        let revision = terminal_table_revision(
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
        );
        let rows = ResultRows::new(
            [
                ResultColumn::new("id", ResolvedType::scalar(StandardScalar::Integer), false)
                    .expect("the id column is valid"),
                ResultColumn::new(
                    "name",
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    true,
                )
                .expect("the name column is valid"),
            ],
            [
                ResultRow::new([
                    RuntimeValue::Integer(1),
                    RuntimeValue::Text("alpha".to_owned()),
                ]),
                ResultRow::new([
                    RuntimeValue::Integer(2),
                    RuntimeValue::null(ResolvedType::scalar(StandardScalar::CharacterLargeObject))
                        .expect("a typed TEXT null is valid"),
                ]),
            ],
        )
        .expect("the presenter rows are valid");
        let RuntimeValue::Opaque(value) =
            execute_standard_terminal_table(&function, &revision, &rows, &active, &registry)
                .expect("the exact standard artifact must execute")
        else {
            panic!("the terminal-table presenter must return one opaque value");
        };
        assert_eq!(
            value.opaque_type(),
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID
        );
        let document = "id name\n-- -----\n1  alpha\n2  NULL\n(2 rows)\n";
        assert_eq!(value.canonical_payload(), frame_terminal_document(document));
    }

    #[test]
    fn standard_terminal_table_dispatches_without_function_name_or_id_matching() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let other_function = FunctionId::from_bytes([0x41; 16]);
        let other_revision = FunctionRevisionId::from_bytes([0x43; 16]);
        let function = FunctionDefinition::new(
            other_function,
            name(&["other", "table"]),
            FunctionDomain::Server,
            vec![terminal_table_parameter(
                STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            )],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
            )),
            other_revision,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        let revision =
            terminal_table_revision(other_function, STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID);
        let rows = ResultRows::new(
            [ResultColumn::new(
                "value",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .expect("the value column is valid")],
            [ResultRow::new([RuntimeValue::Integer(3)])],
        )
        .expect("the presenter rows are valid");
        let RuntimeValue::Opaque(value) =
            execute_standard_terminal_table(&function, &revision, &rows, &active, &registry)
                .expect("the same artifact shape must execute identically")
        else {
            panic!("the terminal-table presenter must return one opaque value");
        };
        assert_eq!(
            value.opaque_type(),
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID
        );
        assert_eq!(
            value.canonical_payload(),
            frame_terminal_document("value\n-----\n3\n(1 row)\n")
        );
    }

    #[test]
    fn standard_csv_encode_dispatches_without_function_name_or_id_matching() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let other_function = FunctionId::from_bytes([0x42; 16]);
        let other_revision = FunctionRevisionId::from_bytes([0x44; 16]);
        let function = FunctionDefinition::new(
            other_function,
            name(&["other", "csv"]),
            FunctionDomain::Server,
            vec![csv_encode_parameter(STD_CSV_ENCODE_PARAMETER_ID)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            other_revision,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        let revision = csv_encode_revision(other_function, STD_CSV_ENCODE_PARAMETER_ID);
        let rows = ResultRows::new(
            [ResultColumn::new(
                "value",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .expect("the value column is valid")],
            [ResultRow::new([RuntimeValue::Integer(3)])],
        )
        .expect("the presenter rows are valid");
        let RuntimeValue::Opaque(value) =
            execute_standard_csv_encode(&function, &revision, &rows, &active, &registry)
                .expect("the same artifact shape must execute identically")
        else {
            panic!("the csv-encode presenter must return one opaque value");
        };
        assert_eq!(
            value.opaque_type(),
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
        );
        assert_eq!(
            value.canonical_payload(),
            frame_byte_stream(b"text/csv", b"value\n3\n")
        );
    }

    #[test]
    fn sealed_output_csv_requirement_emits_the_byte_stream_in_the_final_value_batch() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let requirement = InvocationOutputRequirement::new(
            Some(String::from("csv")),
            None,
            None,
            InvocationStreamingRequirement::Unspecified,
        )
        .expect("the csv output requirement is valid");
        let presented = present_sealed_standard_output(
            &requirement,
            RuntimeValue::Integer(42),
            &active,
            &registry,
        )
        .expect("the csv presenter must execute on the sealed canonical result");
        let RuntimeValue::Opaque(value) = &presented else {
            panic!("the csv presenter must return one opaque value");
        };
        assert_eq!(
            value.opaque_type(),
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
        );
        let mut expected = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        expected.extend_from_slice(&8_u32.to_be_bytes());
        expected.extend_from_slice(b"text/csv");
        expected.extend_from_slice(&10_u32.to_be_bytes());
        expected.extend_from_slice(b"result\n42\n");
        assert_eq!(value.canonical_payload(), expected);

        let principal = PrincipalId::from_bytes([0x65; 16]);
        let invocation = InvocationId::from_bytes([0x66; 16]);
        let events =
            crate::kernel::security::sealed_completed_events(principal, invocation, presented)
                .expect("the presented events are valid");
        let records = events.records();
        assert_eq!(records.len(), 3);
        match records[1].event().body() {
            InvocationEventBody::ValueBatch { values, .. } => {
                let [value] = values.as_slice() else {
                    panic!("the final ValueBatch must carry exactly one value");
                };
                let RuntimeValue::Opaque(opaque) = value.value() else {
                    panic!("the final ValueBatch must carry the presented opaque value");
                };
                assert_eq!(
                    opaque.opaque_type(),
                    orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
                );
                assert_eq!(opaque.canonical_payload(), expected);
            }
            other => panic!("expected a ValueBatch event, got {other:?}"),
        }
    }

    #[test]
    fn sealed_output_json_requirement_emits_the_byte_stream_in_the_final_value_batch() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let requirement = InvocationOutputRequirement::new(
            Some(String::from("json")),
            None,
            None,
            InvocationStreamingRequirement::Unspecified,
        )
        .expect("the json output requirement is valid");
        let presented = present_sealed_standard_output(
            &requirement,
            RuntimeValue::Integer(42),
            &active,
            &registry,
        )
        .expect("the json presenter must execute on the sealed canonical result");
        let RuntimeValue::Opaque(value) = &presented else {
            panic!("the json presenter must return one opaque value");
        };
        assert_eq!(
            value.opaque_type(),
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
        );
        let mut expected = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        expected.extend_from_slice(&16_u32.to_be_bytes());
        expected.extend_from_slice(b"application/json");
        expected.extend_from_slice(&2_u32.to_be_bytes());
        expected.extend_from_slice(b"42");
        assert_eq!(value.canonical_payload(), expected);

        let principal = PrincipalId::from_bytes([0x61; 16]);
        let invocation = InvocationId::from_bytes([0x62; 16]);
        let events =
            crate::kernel::security::sealed_completed_events(principal, invocation, presented)
                .expect("the presented events are valid");
        let records = events.records();
        assert_eq!(records.len(), 3);
        match records[1].event().body() {
            InvocationEventBody::ValueBatch { values, .. } => {
                let [value] = values.as_slice() else {
                    panic!("the final ValueBatch must carry exactly one value");
                };
                let RuntimeValue::Opaque(opaque) = value.value() else {
                    panic!("the final ValueBatch must carry the presented opaque value");
                };
                assert_eq!(
                    opaque.opaque_type(),
                    orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
                );
                assert_eq!(opaque.canonical_payload(), expected);
            }
            other => panic!("expected a ValueBatch event, got {other:?}"),
        }
    }

    #[test]
    fn sealed_output_table_requirement_emits_the_terminal_document_in_the_final_value_batch() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let requirement = InvocationOutputRequirement::new(
            Some(String::from("table")),
            None,
            None,
            InvocationStreamingRequirement::Unspecified,
        )
        .expect("the table output requirement is valid");
        let presented = present_sealed_standard_output(
            &requirement,
            RuntimeValue::Integer(42),
            &active,
            &registry,
        )
        .expect("the terminal-table presenter must execute on the sealed canonical result");
        let RuntimeValue::Opaque(value) = &presented else {
            panic!("the terminal-table presenter must return one opaque value");
        };
        assert_eq!(
            value.opaque_type(),
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID
        );
        assert_eq!(
            value.canonical_payload(),
            frame_terminal_document("result\n------\n42\n(1 row)\n")
        );

        let principal = PrincipalId::from_bytes([0x63; 16]);
        let invocation = InvocationId::from_bytes([0x64; 16]);
        let events =
            crate::kernel::security::sealed_completed_events(principal, invocation, presented)
                .expect("the presented events are valid");
        let records = events.records();
        assert_eq!(records.len(), 3);
        match records[1].event().body() {
            InvocationEventBody::ValueBatch { values, .. } => {
                let [value] = values.as_slice() else {
                    panic!("the final ValueBatch must carry exactly one value");
                };
                let RuntimeValue::Opaque(opaque) = value.value() else {
                    panic!("the final ValueBatch must carry the presented opaque value");
                };
                assert_eq!(
                    opaque.opaque_type(),
                    orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID
                );
                assert_eq!(
                    opaque.canonical_payload(),
                    frame_terminal_document("result\n------\n42\n(1 row)\n")
                );
            }
            other => panic!("expected a ValueBatch event, got {other:?}"),
        }
    }

    #[test]
    fn sealed_output_media_type_requirement_resolves_to_the_json_presenter() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let requirement = InvocationOutputRequirement::new(
            None,
            Some(String::from("application/json")),
            None,
            InvocationStreamingRequirement::Unspecified,
        )
        .expect("the media-type output requirement is valid");
        let presented = present_sealed_standard_output(
            &requirement,
            RuntimeValue::Text("hello".to_owned()),
            &active,
            &registry,
        )
        .expect("the media-type requirement must resolve to the json presenter");
        let RuntimeValue::Opaque(value) = &presented else {
            panic!("the json presenter must return one opaque value");
        };
        assert_eq!(
            value.opaque_type(),
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
        );
        let mut expected = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        expected.extend_from_slice(&16_u32.to_be_bytes());
        expected.extend_from_slice(b"application/json");
        expected.extend_from_slice(&7_u32.to_be_bytes());
        expected.extend_from_slice(b"\"hello\"");
        assert_eq!(value.canonical_payload(), expected);
    }

    #[test]
    fn sealed_output_unresolved_requirement_failures_are_closed() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);

        let alias = InvocationOutputRequirement::new(
            Some(String::from("xml")),
            None,
            None,
            InvocationStreamingRequirement::Unspecified,
        )
        .expect("the alias requirement is valid");
        assert!(matches!(
            present_sealed_standard_output(&alias, RuntimeValue::Integer(1), &active, &registry),
            Err(SealedPresentationError::OutputResolution(
                OutputResolutionError::UnresolvedAlias { alias }
            )) if alias == "xml"
        ));

        let media = InvocationOutputRequirement::new(
            None,
            Some(String::from("application/xml")),
            None,
            InvocationStreamingRequirement::Unspecified,
        )
        .expect("the media requirement is valid");
        assert!(matches!(
            present_sealed_standard_output(&media, RuntimeValue::Integer(1), &active, &registry),
            Err(SealedPresentationError::OutputResolution(
                OutputResolutionError::UnresolvedMediaType { media_type }
            )) if media_type == "application/xml"
        ));

        let type_name = InvocationOutputRequirement::new(
            None,
            None,
            Some(
                InvocationOutputTypeSelector::qualified_name(
                    QualifiedSemanticName::new(["std", "xml", "Value"]).expect("a qualified name"),
                )
                .expect("the type-name selector is valid"),
            ),
            InvocationStreamingRequirement::Unspecified,
        )
        .expect("the type-name requirement is valid");
        assert!(matches!(
            present_sealed_standard_output(
                &type_name,
                RuntimeValue::Integer(1),
                &active,
                &registry
            ),
            Err(SealedPresentationError::OutputResolution(
                OutputResolutionError::UnresolvedTypeName { .. }
            ))
        ));

        let error =
            present_sealed_standard_output(&alias, RuntimeValue::Integer(1), &active, &registry)
                .expect_err("an unresolved alias is a closed output-resolution failure");
        assert_eq!(error.spec_code(), "ORNA0702");
        assert_eq!(error.exit_code(), 5);
    }

    #[test]
    fn sealed_output_no_path_failures_are_closed() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);

        // An opaque canonical result has no path to the table sink: opaque
        // values cannot ride a ResultRows cell.
        let opaque = RuntimeValue::Opaque(
            OpaqueValue::new(
                &active,
                &registry,
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
                frame_terminal_document("x\n-\nx\n(1 row)\n"),
            )
            .expect("the opaque test value is valid"),
        );
        let table = InvocationOutputRequirement::new(
            Some(String::from("table")),
            None,
            None,
            InvocationStreamingRequirement::Unspecified,
        )
        .expect("the table requirement is valid");
        assert!(matches!(
            present_sealed_standard_output(&table, opaque, &active, &registry),
            Err(SealedPresentationError::NoPath)
        ));

        // A record canonical result has no path to the json sink: records are
        // rejected by both the argument channel and the json conversion.
        let record = RuntimeValue::Record(
            RecordValue::new(
                &active,
                PRESENTER_RECORD_TYPE,
                [
                    ("x".to_owned(), RuntimeValue::Integer(1)),
                    ("y".to_owned(), RuntimeValue::Text("a".to_owned())),
                ],
            )
            .expect("the record test value is valid"),
        );
        let json = InvocationOutputRequirement::new(
            Some(String::from("json")),
            None,
            None,
            InvocationStreamingRequirement::Unspecified,
        )
        .expect("the json requirement is valid");
        assert!(matches!(
            present_sealed_standard_output(&json, record, &active, &registry),
            Err(SealedPresentationError::NoPath)
        ));

        let error = present_sealed_standard_output(
            &table,
            RuntimeValue::Opaque(
                OpaqueValue::new(
                    &active,
                    &registry,
                    orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
                    frame_terminal_document("x\n-\nx\n(1 row)\n"),
                )
                .expect("the opaque test value is valid"),
            ),
            &active,
            &registry,
        )
        .expect_err("a result with no path to the offered sink is closed");
        assert_eq!(error.spec_code(), "ORNA0701");
        assert_eq!(error.exit_code(), 5);
    }

    #[test]
    fn terminal_table_renders_each_cell_form_and_the_fixed_layout() {
        let standard = presenter_standard();
        let active = presenter_active(&standard);

        let status = ResultRows::new(
            [
                ResultColumn::new("b", ResolvedType::scalar(StandardScalar::Boolean), false)
                    .expect("the boolean column is valid"),
                ResultColumn::new("n", ResolvedType::scalar(StandardScalar::BigInt), false)
                    .expect("the bigint column is valid"),
                ResultColumn::new("f", ResolvedType::scalar(StandardScalar::Float), false)
                    .expect("the float column is valid"),
                ResultColumn::new(
                    "t",
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                )
                .expect("the text column is valid"),
                ResultColumn::new(
                    "x",
                    ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                    false,
                )
                .expect("the bytes column is valid"),
                ResultColumn::new("r", ResolvedType::reference(PRESENTER_OBJECT_TYPE), false)
                    .expect("the reference column is valid"),
                ResultColumn::new("e", ResolvedType::named(PRESENTER_ENUM_TYPE), false)
                    .expect("the enum column is valid"),
                ResultColumn::new("c", ResolvedType::named(PRESENTER_RECORD_TYPE), false)
                    .expect("the record column is valid"),
            ],
            [ResultRow::new([
                RuntimeValue::Boolean(true),
                RuntimeValue::BigInt(-9_007_199_254_740_993),
                RuntimeValue::Float(RuntimeFloat::new(10.5).expect("10.5 is finite")),
                RuntimeValue::Text("héllo".to_owned()),
                RuntimeValue::Bytes(vec![0x00, 0xff]),
                RuntimeValue::Reference {
                    target: PRESENTER_OBJECT_TYPE,
                    object: ObjectId::from_bytes([0x55; 16]),
                },
                RuntimeValue::Enum(
                    EnumValue::new(active.catalogue(), PRESENTER_ENUM_TYPE, "qualified")
                        .expect("the enum label is declared"),
                ),
                RuntimeValue::Record(
                    RecordValue::new(
                        &active,
                        PRESENTER_RECORD_TYPE,
                        vec![
                            ("x".to_owned(), RuntimeValue::Integer(7)),
                            ("y".to_owned(), RuntimeValue::Text("z".to_owned())),
                        ],
                    )
                    .expect("the record value is valid"),
                ),
            ])],
        )
        .expect("the presenter rows are valid");
        let document = render_terminal_table(&active, &status).expect("the table renders");
        let object = ObjectId::from_bytes([0x55; 16]).canonical();
        let expected = format!(
            "b    n                 f    t     x    r                                 e         c\n\
             ---- ----------------- ---- ----- ---- --------------------------------- --------- --------------------\n\
             true -9007199254740993 10.5 héllo AP8= {object} qualified app.status{{x=7, y=z}}\n\
             (1 row)\n"
        );
        assert_eq!(document, expected);
    }

    #[test]
    fn terminal_table_rejects_control_characters_in_cells_and_headers() {
        let standard = presenter_standard();
        let active = presenter_active(&standard);

        let newline_text = ResultRows::new(
            [ResultColumn::new(
                "value",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                false,
            )
            .expect("the value column is valid")],
            [ResultRow::new([RuntimeValue::Text("a\nb".to_owned())])],
        )
        .expect("the presenter rows are valid");
        assert_presenter_rule(
            render_terminal_table(&active, &newline_text)
                .map(RuntimeValue::Text)
                .map_err(|rule| server_error(ServerSelectError::Presenter { rule })),
            "terminal table cells cannot contain control characters",
        );

        let tab_header = ResultRows::new(
            [ResultColumn::new(
                "val\tue",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .expect("the value column is valid")],
            [ResultRow::new([RuntimeValue::Integer(1)])],
        )
        .expect("the presenter rows are valid");
        assert_presenter_rule(
            render_terminal_table(&active, &tab_header)
                .map(RuntimeValue::Text)
                .map_err(|rule| server_error(ServerSelectError::Presenter { rule })),
            "terminal table column names cannot contain control characters",
        );
    }

    #[test]
    fn csv_renders_each_cell_form_and_quotes_embedded_delimiters() {
        let standard = presenter_standard();
        let active = presenter_active(&standard);

        let status = ResultRows::new(
            [
                ResultColumn::new("b", ResolvedType::scalar(StandardScalar::Boolean), false)
                    .expect("the boolean column is valid"),
                ResultColumn::new("n", ResolvedType::scalar(StandardScalar::BigInt), false)
                    .expect("the bigint column is valid"),
                ResultColumn::new("f", ResolvedType::scalar(StandardScalar::Float), false)
                    .expect("the float column is valid"),
                ResultColumn::new(
                    "t",
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                )
                .expect("the text column is valid"),
                ResultColumn::new(
                    "x",
                    ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                    false,
                )
                .expect("the bytes column is valid"),
                ResultColumn::new("r", ResolvedType::reference(PRESENTER_OBJECT_TYPE), false)
                    .expect("the reference column is valid"),
                ResultColumn::new("e", ResolvedType::named(PRESENTER_ENUM_TYPE), false)
                    .expect("the enum column is valid"),
                ResultColumn::new("c", ResolvedType::named(PRESENTER_RECORD_TYPE), false)
                    .expect("the record column is valid"),
            ],
            [ResultRow::new([
                RuntimeValue::Boolean(true),
                RuntimeValue::BigInt(-9_007_199_254_740_993),
                RuntimeValue::Float(RuntimeFloat::new(10.5).expect("10.5 is finite")),
                RuntimeValue::Text("a,b\"c".to_owned()),
                RuntimeValue::Bytes(vec![0x00, 0xff]),
                RuntimeValue::Reference {
                    target: PRESENTER_OBJECT_TYPE,
                    object: ObjectId::from_bytes([0x55; 16]),
                },
                RuntimeValue::Enum(
                    EnumValue::new(active.catalogue(), PRESENTER_ENUM_TYPE, "qualified")
                        .expect("the enum label is declared"),
                ),
                RuntimeValue::Record(
                    RecordValue::new(
                        &active,
                        PRESENTER_RECORD_TYPE,
                        vec![
                            ("x".to_owned(), RuntimeValue::Integer(7)),
                            ("y".to_owned(), RuntimeValue::Text("z".to_owned())),
                        ],
                    )
                    .expect("the record value is valid"),
                ),
            ])],
        )
        .expect("the presenter rows are valid");
        let document = render_csv_document(&active, &status).expect("the csv renders");
        let object = ObjectId::from_bytes([0x55; 16]).canonical();
        let expected = format!(
            "b,n,f,t,x,r,e,c\n\
             true,-9007199254740993,10.5,\"a,b\"\"c\",AP8=,{object},qualified,\"app.status{{x=7, y=z}}\"\n"
        );
        assert_eq!(document, expected);
    }

    #[test]
    fn csv_rejects_control_characters_in_cells_and_headers() {
        let standard = presenter_standard();
        let active = presenter_active(&standard);

        let newline_text = ResultRows::new(
            [ResultColumn::new(
                "value",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                false,
            )
            .expect("the value column is valid")],
            [ResultRow::new([RuntimeValue::Text("a\nb".to_owned())])],
        )
        .expect("the presenter rows are valid");
        assert_presenter_rule(
            render_csv_document(&active, &newline_text)
                .map(RuntimeValue::Text)
                .map_err(|rule| server_error(ServerSelectError::Presenter { rule })),
            "terminal table cells cannot contain control characters",
        );

        let comma_header = ResultRows::new(
            [
                ResultColumn::new("a,b", ResolvedType::scalar(StandardScalar::Integer), false)
                    .expect("the value column is valid"),
            ],
            [ResultRow::new([RuntimeValue::Integer(1)])],
        )
        .expect("the presenter rows are valid");
        let document = render_csv_document(&active, &comma_header).expect("the csv renders");
        assert_eq!(document, "\"a,b\"\n1\n");

        let tab_header = ResultRows::new(
            [ResultColumn::new(
                "val\tue",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .expect("the value column is valid")],
            [ResultRow::new([RuntimeValue::Integer(1)])],
        )
        .expect("the presenter rows are valid");
        assert_presenter_rule(
            render_csv_document(&active, &tab_header)
                .map(RuntimeValue::Text)
                .map_err(|rule| server_error(ServerSelectError::Presenter { rule })),
            "csv column names cannot contain control characters",
        );
    }

    #[test]
    fn standard_csv_encode_rejects_wrong_kind_format_and_version() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let function = csv_encode_function(
            STD_CSV_ENCODE_FUNCTION_ID,
            STD_CSV_ENCODE_PARAMETER_ID,
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        );
        let parameter = STD_CSV_ENCODE_PARAMETER_ID;
        let rows = ResultRows::new(
            [ResultColumn::new(
                "value",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .expect("the value column is valid")],
            [ResultRow::new([RuntimeValue::Integer(1)])],
        )
        .expect("the presenter rows are valid");

        let wrong_kind = presenter_revision(
            function.id(),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            server_csv_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Client,
                server_csv_encode::FORMAT_IDENTITY,
                server_csv_encode::FORMAT_VERSION,
                csv_encode_payload(parameter),
            ),
        );
        assert_presenter_artifact_rule(
            execute_standard_csv_encode(&function, &wrong_kind, &rows, &active, &registry),
            function.id(),
            "current revision must contain a SERVER artifact",
        );

        let wrong_format = presenter_revision(
            function.id(),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            server_csv_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_terminal_table::FORMAT_IDENTITY,
                server_csv_encode::FORMAT_VERSION,
                csv_encode_payload(parameter),
            ),
        );
        assert_presenter_artifact_rule(
            execute_standard_csv_encode(&function, &wrong_format, &rows, &active, &registry),
            function.id(),
            "current SERVER artifact must use orna.server-csv-encode",
        );

        let wrong_version = presenter_revision(
            function.id(),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            server_csv_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_csv_encode::FORMAT_IDENTITY,
                server_csv_encode::FORMAT_VERSION + 1,
                csv_encode_payload(parameter),
            ),
        );
        assert_presenter_artifact_rule(
            execute_standard_csv_encode(&function, &wrong_version, &rows, &active, &registry),
            function.id(),
            "current SERVER artifact must use orna.server-csv-encode version 1",
        );

        let wrong_language = presenter_revision(
            function.id(),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            "orna.language/9",
            csv_encode_artifact(parameter),
        );
        assert_presenter_artifact_rule(
            execute_standard_csv_encode(&function, &wrong_language, &rows, &active, &registry),
            function.id(),
            "current SERVER revision must use the csv-encode language version",
        );

        assert_eq!(
            execute_standard_csv_encode(
                &function,
                &csv_encode_revision(function.id(), parameter),
                &rows,
                &active,
                &registry,
            )
            .expect("the exact artifact must execute"),
            RuntimeValue::Opaque(
                OpaqueValue::new(
                    &active,
                    &registry,
                    orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
                    frame_byte_stream(b"text/csv", b"value\n1\n"),
                )
                .expect("the framed byte stream constructs"),
            )
        );
    }

    #[test]
    fn standard_csv_encode_artifacts_reject_each_decode_deviation() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let function = csv_encode_function(
            STD_CSV_ENCODE_FUNCTION_ID,
            STD_CSV_ENCODE_PARAMETER_ID,
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        );
        let parameter = STD_CSV_ENCODE_PARAMETER_ID;
        let rows = ResultRows::new(
            [ResultColumn::new(
                "value",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .expect("the value column is valid")],
            [ResultRow::new([RuntimeValue::Integer(1)])],
        )
        .expect("the presenter rows are valid");

        let mut invalid_magic = csv_encode_payload(parameter);
        invalid_magic[0] = b'X';
        let revision = presenter_revision(
            function.id(),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            server_csv_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_csv_encode::FORMAT_IDENTITY,
                server_csv_encode::FORMAT_VERSION,
                invalid_magic,
            ),
        );
        assert_csv_encode_decode_rule(
            execute_standard_csv_encode(&function, &revision, &rows, &active, &registry),
            CsvEncodePlanError::InvalidMagic,
        );

        let other_parameter = ParameterId::from_bytes([0x52; 16]);
        let revision = presenter_revision(
            function.id(),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            server_csv_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_csv_encode::FORMAT_IDENTITY,
                server_csv_encode::FORMAT_VERSION,
                csv_encode_payload(other_parameter),
            ),
        );
        assert_csv_encode_decode_rule(
            execute_standard_csv_encode(&function, &revision, &rows, &active, &registry),
            CsvEncodePlanError::UnexpectedParameter {
                actual: other_parameter,
                expected: parameter,
            },
        );

        let other_type = orna_standard::BIGINT_TYPE_ID;
        let revision = presenter_revision(
            function.id(),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            server_csv_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_csv_encode::FORMAT_IDENTITY,
                server_csv_encode::FORMAT_VERSION,
                CsvEncodePlan::new(parameter, other_type)
                    .expect("any identities form a valid csv-encode model")
                    .encode()
                    .expect("the canonical csv-encode model encodes"),
            ),
        );
        assert_csv_encode_decode_rule(
            execute_standard_csv_encode(&function, &revision, &rows, &active, &registry),
            CsvEncodePlanError::UnexpectedType {
                actual: other_type,
                expected: STD_DATA_ROWS_TYPE_ID,
            },
        );

        let truncated = csv_encode_payload(parameter)[..40].to_vec();
        let revision = presenter_revision(
            function.id(),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            server_csv_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_csv_encode::FORMAT_IDENTITY,
                server_csv_encode::FORMAT_VERSION,
                truncated,
            ),
        );
        assert_csv_encode_decode_rule(
            execute_standard_csv_encode(&function, &revision, &rows, &active, &registry),
            CsvEncodePlanError::Truncated,
        );

        let mut trailing = csv_encode_payload(parameter);
        trailing.push(0);
        let revision = presenter_revision(
            function.id(),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            server_csv_encode::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_csv_encode::FORMAT_IDENTITY,
                server_csv_encode::FORMAT_VERSION,
                trailing,
            ),
        );
        assert_csv_encode_decode_rule(
            execute_standard_csv_encode(&function, &revision, &rows, &active, &registry),
            CsvEncodePlanError::TrailingBytes,
        );
    }

    #[test]
    fn standard_csv_encode_signature_rejects_each_shape_deviation() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let parameter = STD_CSV_ENCODE_PARAMETER_ID;
        let revision = csv_encode_revision(STD_CSV_ENCODE_FUNCTION_ID, parameter);
        let rows = ResultRows::new(
            [ResultColumn::new(
                "value",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .expect("the value column is valid")],
            [ResultRow::new([RuntimeValue::Integer(1)])],
        )
        .expect("the presenter rows are valid");
        let run = |function: &FunctionDefinition| {
            execute_standard_csv_encode(function, &revision, &rows, &active, &registry)
        };

        let client = FunctionDefinition::new(
            STD_CSV_ENCODE_FUNCTION_ID,
            name(&["std", "csv", "encode"]),
            FunctionDomain::Client,
            vec![csv_encode_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_presenter_domain_rule(run(&client));

        let missing = FunctionDefinition::new(
            STD_CSV_ENCODE_FUNCTION_ID,
            name(&["std", "csv", "encode"]),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&missing),
            STD_CSV_ENCODE_FUNCTION_ID,
            "standard csv-encode presenters must declare exactly one required non-null std.data.Rows parameter",
        );

        let wrong_parameter_type = FunctionDefinition::new(
            STD_CSV_ENCODE_FUNCTION_ID,
            name(&["std", "csv", "encode"]),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter,
                "p_rows",
                0,
                ResolvedType::named(orna_standard::BIGINT_TYPE_ID),
                None,
            )],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&wrong_parameter_type),
            STD_CSV_ENCODE_FUNCTION_ID,
            "standard csv-encode presenters must declare one std.data.Rows parameter and one std.io.ByteStream result",
        );

        let wrong_result_type = FunctionDefinition::new(
            STD_CSV_ENCODE_FUNCTION_ID,
            name(&["std", "csv", "encode"]),
            FunctionDomain::Server,
            vec![csv_encode_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
            )),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&wrong_result_type),
            STD_CSV_ENCODE_FUNCTION_ID,
            "standard csv-encode presenters must declare one std.data.Rows parameter and one std.io.ByteStream result",
        );

        let definer = FunctionDefinition::new(
            STD_CSV_ENCODE_FUNCTION_ID,
            name(&["std", "csv", "encode"]),
            FunctionDomain::Server,
            vec![csv_encode_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Definer,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&definer),
            STD_CSV_ENCODE_FUNCTION_ID,
            "standard presenter functions must use INVOKER security",
        );

        let manual = FunctionDefinition::new(
            STD_CSV_ENCODE_FUNCTION_ID,
            name(&["std", "csv", "encode"]),
            FunctionDomain::Server,
            vec![csv_encode_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&manual),
            STD_CSV_ENCODE_FUNCTION_ID,
            "standard presenter functions must use READ ONLY transactions",
        );

        let volatile = FunctionDefinition::new(
            STD_CSV_ENCODE_FUNCTION_ID,
            name(&["std", "csv", "encode"]),
            FunctionDomain::Server,
            vec![csv_encode_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_CSV_ENCODE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Immutable,
        );
        assert_signature_rule(
            run(&volatile),
            STD_CSV_ENCODE_FUNCTION_ID,
            "standard presenter functions must use STABLE volatility",
        );

        // The exact pinned shape still executes after every rejection.
        assert_eq!(
            execute_standard_csv_encode(
                &csv_encode_function(
                    STD_CSV_ENCODE_FUNCTION_ID,
                    parameter,
                    STD_CSV_ENCODE_FUNCTION_REVISION_ID,
                ),
                &revision,
                &rows,
                &active,
                &registry,
            )
            .expect("the pinned shape must execute"),
            RuntimeValue::Opaque(
                OpaqueValue::new(
                    &active,
                    &registry,
                    orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
                    frame_byte_stream(b"text/csv", b"value\n1\n"),
                )
                .expect("the framed byte stream constructs"),
            )
        );
    }

    #[test]
    fn standard_terminal_table_rejects_wrong_kind_format_and_version() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let function = terminal_table_function(
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        );
        let parameter = STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID;
        let rows = ResultRows::new(
            [ResultColumn::new(
                "value",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .expect("the value column is valid")],
            [ResultRow::new([RuntimeValue::Integer(1)])],
        )
        .expect("the presenter rows are valid");

        let wrong_kind = presenter_revision(
            function.id(),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            server_terminal_table::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Client,
                server_terminal_table::FORMAT_IDENTITY,
                server_terminal_table::FORMAT_VERSION,
                terminal_table_payload(parameter),
            ),
        );
        assert_presenter_artifact_rule(
            execute_standard_terminal_table(&function, &wrong_kind, &rows, &active, &registry),
            function.id(),
            "current revision must contain a SERVER artifact",
        );

        let wrong_format = presenter_revision(
            function.id(),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            server_terminal_table::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_json_encode::FORMAT_IDENTITY,
                server_terminal_table::FORMAT_VERSION,
                terminal_table_payload(parameter),
            ),
        );
        assert_presenter_artifact_rule(
            execute_standard_terminal_table(&function, &wrong_format, &rows, &active, &registry),
            function.id(),
            "current SERVER artifact must use orna.server-terminal-table",
        );

        let wrong_version = presenter_revision(
            function.id(),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            server_terminal_table::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_terminal_table::FORMAT_IDENTITY,
                server_terminal_table::FORMAT_VERSION + 1,
                terminal_table_payload(parameter),
            ),
        );
        assert_presenter_artifact_rule(
            execute_standard_terminal_table(&function, &wrong_version, &rows, &active, &registry),
            function.id(),
            "current SERVER artifact must use orna.server-terminal-table version 1",
        );

        let wrong_language = presenter_revision(
            function.id(),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            "orna.language/9",
            terminal_table_artifact(parameter),
        );
        assert_presenter_artifact_rule(
            execute_standard_terminal_table(&function, &wrong_language, &rows, &active, &registry),
            function.id(),
            "current SERVER revision must use the terminal-table language version",
        );

        assert_eq!(
            execute_standard_terminal_table(
                &function,
                &terminal_table_revision(function.id(), parameter),
                &rows,
                &active,
                &registry,
            )
            .expect("the exact artifact must execute"),
            RuntimeValue::Opaque(
                OpaqueValue::new(
                    &active,
                    &registry,
                    orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
                    frame_terminal_document("value\n-----\n1\n(1 row)\n"),
                )
                .expect("the framed document constructs"),
            )
        );
    }

    #[test]
    fn standard_terminal_table_artifacts_reject_each_decode_deviation() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let function = terminal_table_function(
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        );
        let parameter = STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID;
        let rows = ResultRows::new(
            [ResultColumn::new(
                "value",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .expect("the value column is valid")],
            [ResultRow::new([RuntimeValue::Integer(1)])],
        )
        .expect("the presenter rows are valid");

        let mut invalid_magic = terminal_table_payload(parameter);
        invalid_magic[0] = b'X';
        let revision = presenter_revision(
            function.id(),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            server_terminal_table::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_terminal_table::FORMAT_IDENTITY,
                server_terminal_table::FORMAT_VERSION,
                invalid_magic,
            ),
        );
        assert_terminal_table_decode_rule(
            execute_standard_terminal_table(&function, &revision, &rows, &active, &registry),
            TerminalTablePlanError::InvalidMagic,
        );

        let other_parameter = ParameterId::from_bytes([0x51; 16]);
        let revision = presenter_revision(
            function.id(),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            server_terminal_table::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_terminal_table::FORMAT_IDENTITY,
                server_terminal_table::FORMAT_VERSION,
                terminal_table_payload(other_parameter),
            ),
        );
        assert_terminal_table_decode_rule(
            execute_standard_terminal_table(&function, &revision, &rows, &active, &registry),
            TerminalTablePlanError::UnexpectedParameter {
                actual: other_parameter,
                expected: parameter,
            },
        );

        let other_type = orna_standard::BIGINT_TYPE_ID;
        let revision = presenter_revision(
            function.id(),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            server_terminal_table::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_terminal_table::FORMAT_IDENTITY,
                server_terminal_table::FORMAT_VERSION,
                TerminalTablePlan::new(parameter, other_type)
                    .expect("any identities form a valid terminal-table model")
                    .encode()
                    .expect("the canonical terminal-table model encodes"),
            ),
        );
        assert_terminal_table_decode_rule(
            execute_standard_terminal_table(&function, &revision, &rows, &active, &registry),
            TerminalTablePlanError::UnexpectedType {
                actual: other_type,
                expected: STD_DATA_ROWS_TYPE_ID,
            },
        );

        let truncated = terminal_table_payload(parameter)[..40].to_vec();
        let revision = presenter_revision(
            function.id(),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            server_terminal_table::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_terminal_table::FORMAT_IDENTITY,
                server_terminal_table::FORMAT_VERSION,
                truncated,
            ),
        );
        assert_terminal_table_decode_rule(
            execute_standard_terminal_table(&function, &revision, &rows, &active, &registry),
            TerminalTablePlanError::Truncated,
        );

        let mut trailing = terminal_table_payload(parameter);
        trailing.push(0);
        let revision = presenter_revision(
            function.id(),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            server_terminal_table::LANGUAGE_VERSION_IDENTITY,
            artifact(
                ExecutableArtifactKind::Server,
                server_terminal_table::FORMAT_IDENTITY,
                server_terminal_table::FORMAT_VERSION,
                trailing,
            ),
        );
        assert_terminal_table_decode_rule(
            execute_standard_terminal_table(&function, &revision, &rows, &active, &registry),
            TerminalTablePlanError::TrailingBytes,
        );
    }

    #[test]
    fn standard_terminal_table_signature_rejects_each_shape_deviation() {
        let standard = presenter_standard();
        let registry = orna_standard::registered_opaque_codecs(&standard)
            .expect("the V3 opaque codecs register");
        let active = presenter_active(&standard);
        let parameter = STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID;
        let revision = terminal_table_revision(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID, parameter);
        let rows = ResultRows::new(
            [ResultColumn::new(
                "value",
                ResolvedType::scalar(StandardScalar::Integer),
                false,
            )
            .expect("the value column is valid")],
            [ResultRow::new([RuntimeValue::Integer(1)])],
        )
        .expect("the presenter rows are valid");
        let run = |function: &FunctionDefinition| {
            execute_standard_terminal_table(function, &revision, &rows, &active, &registry)
        };

        let client = FunctionDefinition::new(
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            name(&["std", "terminal", "present_table"]),
            FunctionDomain::Client,
            vec![terminal_table_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
            )),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_presenter_domain_rule(run(&client));

        let missing = FunctionDefinition::new(
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            name(&["std", "terminal", "present_table"]),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
            )),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&missing),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            "standard terminal-table presenters must declare exactly one required non-null std.data.Rows parameter",
        );

        let wrong_parameter_type = FunctionDefinition::new(
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            name(&["std", "terminal", "present_table"]),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter,
                "p_rows",
                0,
                ResolvedType::named(orna_standard::BIGINT_TYPE_ID),
                None,
            )],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
            )),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&wrong_parameter_type),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            "standard terminal-table presenters must declare one std.data.Rows parameter and one std.terminal.Document result",
        );

        let wrong_result_type = FunctionDefinition::new(
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            name(&["std", "terminal", "present_table"]),
            FunctionDomain::Server,
            vec![terminal_table_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
            )),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&wrong_result_type),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            "standard terminal-table presenters must declare one std.data.Rows parameter and one std.terminal.Document result",
        );

        let definer = FunctionDefinition::new(
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            name(&["std", "terminal", "present_table"]),
            FunctionDomain::Server,
            vec![terminal_table_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
            )),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            FunctionSecurity::Definer,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&definer),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            "standard presenter functions must use INVOKER security",
        );

        let manual = FunctionDefinition::new(
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            name(&["std", "terminal", "present_table"]),
            FunctionDomain::Server,
            vec![terminal_table_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
            )),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&manual),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            "standard presenter functions must use READ ONLY transactions",
        );

        let volatile = FunctionDefinition::new(
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            name(&["std", "terminal", "present_table"]),
            FunctionDomain::Server,
            vec![terminal_table_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::named(
                orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
            )),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Immutable,
        );
        assert_signature_rule(
            run(&volatile),
            STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            "standard presenter functions must use STABLE volatility",
        );

        // The exact pinned shape still executes after every rejection.
        assert_eq!(
            execute_standard_terminal_table(
                &terminal_table_function(
                    STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
                    parameter,
                    STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
                ),
                &revision,
                &rows,
                &active,
                &registry,
            )
            .expect("the pinned shape must execute"),
            RuntimeValue::Opaque(
                OpaqueValue::new(
                    &active,
                    &registry,
                    orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
                    frame_terminal_document("value\n-----\n1\n(1 row)\n"),
                )
                .expect("the framed document constructs"),
            )
        );
    }

    fn assert_presenter_conversion_rule(
        active: &ActiveDatabaseRevision,
        value: RuntimeValue,
        fragment: &str,
    ) {
        let error = encode_json_value(active, &value).expect_err("the value must be rejected");
        assert!(
            error.contains(fragment),
            "expected a rule mentioning {fragment:?}, got {error:?}"
        );
    }
}
