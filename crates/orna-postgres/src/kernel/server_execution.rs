//! Execution of the initial immutable SERVER `SELECT` subset.
//!
//! This module accepts only a recovered active revision and a canonical server
//! plan. It never derives SQL from semantic names or accepts caller SQL.

// Server execution preserves the accepted error and presentation layouts.
#![allow(clippy::result_large_err)]
#![allow(clippy::large_enum_variant)]
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
    sync::OnceLock,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures_util::{
    TryStreamExt,
    future::{Either, select},
};
use orna_artifact::server_csv_encode::{self, CsvEncodePlan, CsvEncodePlanError};
use orna_artifact::server_json_encode::{self, JsonEncodePlan, JsonEncodePlanError};
use orna_artifact::server_parameter_echo::{self, ServerParameterEcho, ServerParameterEchoError};
use orna_artifact::server_plan::{
    self, DistinctServerPlan, Expression, ExpressionKind, FieldStep, IdentitySelectedServerPlan,
    Ordering, SelectBindValue as UniqueTextSelectBindValue, ServerPlan, SortDirection,
    UniqueTextSelectedServerPlan,
};
use orna_artifact::server_terminal_table::{self, TerminalTablePlan, TerminalTablePlanError};
use orna_client::{ClientReferenceLoader, ClientReferenceObject};
use orna_core::{
    FieldId, FunctionId, FunctionRevisionId, ObjectId, ParameterId, PrincipalId, SourceUnitId,
    TypeId,
    canonical_hash::artifact_payload_digest,
    catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn,
        FunctionReturnColumnDefinition, FunctionSecurity, FunctionTransaction, FunctionVolatility,
        ParameterDefinition, QualifiedSemanticName, TypeLookupName, ValueTypeKind,
        ValueTypeMutability, ValueTypePersistence,
    },
    invocation::{
        InvocationClientOffer, InvocationOutputRequirement, InvocationOutputTypeSelector,
        InvocationStreamingRequirement,
    },
    presenter::{
        AmbiguousOutputSelector, OutputResolutionError, PresenterEntry, PresenterRegistry,
    },
    revision::{
        ActiveDatabaseRevision, CatalogueHashContext, DefinitionReferenceKind,
        DefinitionReferenceTarget, ExecutableArtifact, ExecutableArtifactKind,
        FunctionRevisionRecord, RevisionPair, Sha256Digest, SourceOrigin, StandardExecutable,
    },
    security::{AuthorisedInvocation, InvocationTarget},
    types::{ResolvedType, StandardScalar, TypeDescriptor},
    value::{
        ConstructedValueKind, EnumValue, FunctionArgument, OpaqueCodecRegistry, OpaqueValue,
        OpaqueValueError, RecordValue, ResultColumn, ResultRow, ResultRows, ResultRowsError,
        RuntimeFloat, RuntimeType, RuntimeValue,
    },
};
use orna_protocol::{ValueCodecError, decode_active_value, decode_rows, encode_active_value};
use orna_standard::{
    BYTE_STREAM_MAGIC, INTEGER_TYPE_ID, JSON_MAGIC, STANDARD_LIBRARY_V8_REVISION_ID,
    STANDARD_LIBRARY_V9_REVISION_ID, STD_IO_BYTE_STREAM_TYPE_ID, STD_TERMINAL_DOCUMENT_TYPE_ID,
    TERMINAL_DOCUMENT_MAGIC,
};
use tokio::sync::mpsc;
use tokio_postgres::{
    Client, IsolationLevel, Row, Statement, Transaction,
    types::{ToSql, Type},
};

use super::security::{
    AuthenticatedServerResourceEvent, ResourceCancellation, ResourceProducerCancelled,
    ResourceProducerCommand, ResourceProducerCompleted, ResourceProducerExit,
    ResourceProducerFailed, ResourceProducerPull,
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
/// The reserved Work ADR 0087 `std.data.Rows` value-type identity (`...12`).
///
/// Keep this local until the V8 standard snapshot exports the same identity;
/// the sealed presenter registry below is the migration seam.
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
    pub(crate) fn new(
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
/// Preloads the object graph reachable from authenticated CLIENT arguments.
///
/// Every generated relation and field identifier comes from the active
/// catalogue. The supplied transaction remains the sole storage session, so
/// all roots and recursively followed references observe one snapshot.
pub(crate) async fn load_client_reference_loader(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    principal: PrincipalId,
    security_context_digest: Sha256Digest,
    roots: &[FunctionArgument],
) -> Result<ClientReferenceLoader, PostgresKernelError> {
    let mut pending = VecDeque::new();
    let mut visited = BTreeSet::new();
    for argument in roots {
        if let RuntimeValue::Reference { target, object } = argument.value() {
            enqueue_client_reference_object(active, &mut visited, &mut pending, *target, *object)?;
        }
    }

    let mut objects = Vec::with_capacity(visited.len());
    while let Some((target, object)) = pending.pop_front() {
        let object_definition = active
            .catalogue()
            .object_type_by_id(target)
            .ok_or_else(|| {
                server_error(ServerSelectError::PlanInvariant {
                    rule: "client reference loader target must be an active object type",
                })
            })?;
        let fields = object_definition.fields();
        let supported_fields = fields
            .iter()
            .filter(|field| client_reference_loader_field_supported(active, field.resolved_type()))
            .collect::<Vec<_>>();
        let columns = supported_fields
            .iter()
            .map(|field| {
                ResultColumn::new(
                    field_name(field.id()),
                    field.resolved_type(),
                    field.nullable(),
                )
                .map_err(ServerSelectError::ResultRows)
                .map_err(server_error)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut projections = String::from(OBJECT_ID_COLUMN);
        for field in &supported_fields {
            projections.push_str(", ");
            projections.push_str(&field_name(field.id()));
        }
        let sql = format!(
            "SELECT {projections} FROM {DATA_SCHEMA}.{} \
             WHERE {OBJECT_ID_COLUMN} = $1 LIMIT 2",
            relation_name(target),
        );
        if sql.len() > SQL_LIMIT {
            return Err(server_error(ServerSelectError::ComplexityLimit {
                category: "generated client reference loader SQL bytes",
                maximum: SQL_LIMIT,
            }));
        }
        let object_bytes = object.to_bytes().to_vec();
        let rows = transaction
            .query(&sql, &[&object_bytes])
            .await
            .map_err(PostgresKernelError::Database)?;
        if rows.len() > 1 {
            return Err(server_error(ServerSelectError::Cardinality {
                rule: "more than one row was returned for the requested client reference object",
            }));
        }
        let Some(row) = rows.into_iter().next() else {
            // Missing objects remain absent. The evaluator maps that absence
            // to its redacted FieldPath error.
            continue;
        };

        let returned_bytes = row.try_get::<usize, Vec<u8>>(0).map_err(|source| {
            server_error(ServerSelectError::RowDecode {
                row: 0,
                column: 0,
                source,
            })
        })?;
        let returned_bytes: [u8; 16] = returned_bytes.try_into().map_err(|_| {
            server_error(ServerSelectError::ValueInvariant {
                row: 0,
                column: 0,
                rule: "client reference object rows must contain exactly 16 object-id bytes",
            })
        })?;
        if ObjectId::from_bytes(returned_bytes) != object {
            return Err(server_error(ServerSelectError::ValueInvariant {
                row: 0,
                column: 0,
                rule: "client reference object row identity must equal the requested object",
            }));
        }

        let mut values = Vec::with_capacity(columns.len());
        for (field_index, column) in columns.iter().enumerate() {
            values.push(decode_value(
                active,
                &row,
                0,
                field_index.saturating_add(1),
                column,
            )?);
        }

        let mut field_values = Vec::with_capacity(supported_fields.len());
        for (field, value) in supported_fields.iter().zip(values) {
            if let RuntimeValue::Reference {
                target: child_target,
                object: child_object,
            } = &value
            {
                enqueue_client_reference_object(
                    active,
                    &mut visited,
                    &mut pending,
                    *child_target,
                    *child_object,
                )?;
            }
            field_values.push((field.id(), value));
        }
        objects.push(ClientReferenceObject::new(target, object, field_values));
    }

    ClientReferenceLoader::new(active.pair(), principal, security_context_digest, objects).map_err(
        |_| {
            PostgresKernelError::CatalogueInvariant(
                "client reference loader contains duplicate object identities",
            )
        },
    )
}

fn client_reference_loader_field_supported(
    active: &ActiveDatabaseRevision,
    resolved_type: ResolvedType,
) -> bool {
    let runtime = resolve_catalogue_runtime_type(
        active.catalogue(),
        active.catalogue_hash_context(),
        resolved_type,
    );
    matches!(
        runtime,
        ResolvedRuntimeType::CatalogueEnum(_)
            | ResolvedRuntimeType::Record(_)
            | ResolvedRuntimeType::Reference(_)
    ) || matches!(
        runtime.compatibility_scalar(),
        Some(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        )
    )
}

fn enqueue_client_reference_object(
    active: &ActiveDatabaseRevision,
    visited: &mut BTreeSet<(TypeId, ObjectId)>,
    pending: &mut VecDeque<(TypeId, ObjectId)>,
    target: TypeId,
    object: ObjectId,
) -> Result<(), PostgresKernelError> {
    if active.catalogue().object_type_by_id(target).is_none() {
        return Ok(());
    }
    if !visited.insert((target, object)) {
        return Ok(());
    }
    if visited.len() > TARGET_ENTRY_LIMIT {
        return Err(server_error(ServerSelectError::ComplexityLimit {
            category: "client reference loader objects",
            maximum: TARGET_ENTRY_LIMIT,
        }));
    }
    pending.push_back((target, object));
    Ok(())
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

struct PreparedServerExecution {
    revision: FunctionRevisionId,
    statement: Statement,
    binds: Vec<SelectBindValue>,
    columns: Vec<ResultColumn>,
    guards: Vec<VariableGuard>,
    variable_payload_limit: usize,
    cardinality: ResultCardinality,
}

async fn prepare_active_transaction(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    context: ServerSelectContext,
    arguments: &[FunctionArgument],
) -> Result<PreparedServerExecution, PostgresKernelError> {
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
    // Resource dispatch admits scalar and stream functions, while the
    // canonical SELECT planner/result model is row-shaped. Adapt both to one
    // internal value column so all existing validation, lowering, and
    // canonical RuntimeValue decoding stay on the same path as ROWS
    // execution. Only an original SINGLE retains exact-one cardinality.
    let row_execution_function = row_execution_function(function);
    let execution_function = row_execution_function.as_ref().unwrap_or(function);
    let scalar_result = matches!(function.return_type(), FunctionReturn::Single(_));
    let stream_result = matches!(function.return_type(), FunctionReturn::Stream(_));
    let (columns, lowered, cardinality) = match &decoded {
        DecodedServerPlan::V1(plan) => {
            validate_function_signature(execution_function)?;
            validate_no_arguments(arguments)?;
            validate_plan(active, execution_function, plan)?;
            validate_reference_evidence(active, execution_function, plan)?;
            let columns = result_columns_for_projections(execution_function, &plan.projections)?;
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
            (
                columns,
                lowered,
                if scalar_result {
                    ResultCardinality::ExactlyOne
                } else {
                    ResultCardinality::BoundedMany
                },
            )
        }
        DecodedServerPlan::V2(plan) => {
            validate_identity_selected_function_signature(active.catalogue(), execution_function)?;
            validate_identity_selected_plan(
                active.catalogue(),
                active.catalogue_hash_context(),
                execution_function,
                plan,
            )?;
            validate_identity_selected_reference_evidence(active, execution_function, plan)?;
            let object = validate_identity_selected_arguments(
                active.catalogue(),
                active.catalogue_hash_context(),
                execution_function,
                plan,
                arguments,
            )?;
            let columns = result_columns_for_projections(execution_function, plan.projections())?;
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
            (
                columns,
                lowered,
                if scalar_result {
                    ResultCardinality::ExactlyOne
                } else if stream_result {
                    ResultCardinality::BoundedMany
                } else {
                    ResultCardinality::AtMostOne
                },
            )
        }
        DecodedServerPlan::V3(plan) => {
            validate_distinct_function_signature(execution_function)?;
            validate_no_arguments(arguments)?;
            validate_distinct_plan(
                active.catalogue(),
                active.catalogue_hash_context(),
                execution_function,
                plan,
            )?;
            validate_distinct_reference_evidence(active, execution_function, plan)?;
            let columns = result_columns_for_projections(execution_function, plan.projections())?;
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
            (
                columns,
                lowered,
                if scalar_result {
                    ResultCardinality::ExactlyOne
                } else {
                    ResultCardinality::BoundedMany
                },
            )
        }
        DecodedServerPlan::V4(plan) => {
            validate_unique_text_selected_function_signature(execution_function)?;
            validate_unique_text_selected_plan(
                active.catalogue(),
                active.catalogue_hash_context(),
                execution_function,
                plan,
            )?;
            validate_unique_text_selected_reference_evidence(active, execution_function, plan)?;
            let selector = validate_unique_text_selected_arguments(
                active.catalogue(),
                active.catalogue_hash_context(),
                execution_function,
                plan,
                arguments,
            )?;
            let columns = result_columns_for_projections(execution_function, plan.projections())?;
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
            (
                columns,
                lowered,
                if scalar_result {
                    ResultCardinality::ExactlyOne
                } else if stream_result {
                    ResultCardinality::BoundedMany
                } else {
                    ResultCardinality::AtMostOne
                },
            )
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
    Ok(PreparedServerExecution {
        revision: revision.id(),
        statement,
        binds: lowered.binds,
        columns,
        guards: lowered.guards,
        variable_payload_limit: lowered.variable_payload_limit,
        cardinality,
    })
}

async fn execute_active_transaction(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    context: ServerSelectContext,
    arguments: &[FunctionArgument],
) -> Result<ServerSelectResult, PostgresKernelError> {
    let prepared =
        prepare_active_transaction(transaction, active, function, context, arguments).await?;
    let rows = stream_rows(
        transaction,
        &prepared.statement,
        &prepared.binds,
        ResultReadShape {
            active,
            columns: &prepared.columns,
            guards: &prepared.guards,
            variable_payload_limit: prepared.variable_payload_limit,
            cardinality: prepared.cardinality,
        },
    )
    .await?;
    Ok(ServerSelectResult::new(
        context.pair(),
        context.function(),
        prepared.revision,
        rows,
    ))
}

/// Runs one authenticated SERVER resource query against one owned transaction.
///
/// Planning, statement preparation, the query stream, and every decoded row stay
/// in this task. The command receiver is deliberately pull-driven: at most one
/// row is decoded for one command, and a row that exceeds byte credit is retained
/// as one bounded pending value rather than materialising the result set.
pub(crate) async fn run_authenticated_server_resource_stream(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
    commands: &mut mpsc::Receiver<ResourceProducerCommand>,
    cancellation: &ResourceCancellation,
) -> Result<ResourceProducerExit, PostgresKernelError> {
    let target = authorisation.target();
    if target.revision() != active.pair() {
        return Err(server_error(ServerSelectError::AuthorisationMismatch {
            authorised: Box::new(target),
            active: active.pair(),
        }));
    }
    let function = active
        .catalogue()
        .function_by_id(target.function())
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
    let prepared =
        prepare_active_transaction(transaction, active, function, context, arguments).await?;
    let parameters = prepared
        .binds
        .iter()
        .map(SelectBindValue::as_to_sql)
        .collect::<Vec<_>>();
    let stream = transaction
        .query_raw(&prepared.statement, parameters)
        .await
        .map_err(PostgresKernelError::Database)?;
    futures_util::pin_mut!(stream);

    let mut rows_seen = 0usize;
    let mut cells = 0usize;
    let mut payload = initial_payload_len(&prepared.columns)?;
    let mut pending: Option<(RuntimeValue, u64)> = None;
    let mut batch_sequence = 0u64;
    let mut final_batch_sequence = 0u64;
    let mut total_items = 0u64;
    let mut total_bytes = 0u64;

    loop {
        let cancelled = cancellation.cancelled();
        let received = commands.recv();
        futures_util::pin_mut!(cancelled, received);
        let command = match select(cancelled, received).await {
            Either::Left(((), _received)) => {
                return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: None,
                }));
            }
            Either::Right((command, _cancelled)) => command,
        };
        let Some(ResourceProducerCommand::Pull(ResourceProducerPull { credit, response })) =
            command
        else {
            return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: None,
            }));
        };
        let scalar = matches!(
            prepared.cardinality,
            ResultCardinality::ExactlyOne | ResultCardinality::AtMostOne
        );
        if (credit.item_count == 0 && rows_seen == 0)
            || (credit.byte_count == 0 && !scalar && rows_seen == 0)
        {
            return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                response: Some(response),
                error: server_error(ServerSelectError::Argument {
                    parameter: None,
                    rule: "resource pull credit must be non-zero",
                }),
            }));
        }
        if cancellation.is_requested() {
            return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: Some(response),
            }));
        }

        let (value, byte_count) = if let Some(value) = pending.take() {
            value
        } else {
            let cancelled = cancellation.cancelled();
            let next_row = stream.try_next();
            futures_util::pin_mut!(cancelled, next_row);
            let row = match select(cancelled, next_row).await {
                Either::Left(((), _next_row)) => {
                    return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                        response: Some(response),
                    }));
                }
                Either::Right((row, _cancelled)) => match row {
                    Ok(row) => row,
                    Err(error) => {
                        return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                            response: Some(response),
                            error: PostgresKernelError::Database(error),
                        }));
                    }
                },
            };
            let Some(row) = row else {
                if let Err(error) = prepared.cardinality.finish(rows_seen) {
                    return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                        response: Some(response),
                        error,
                    }));
                }
                return Ok(ResourceProducerExit::Completed(ResourceProducerCompleted {
                    response,
                    final_batch_sequence,
                    total_items,
                    total_bytes,
                }));
            };
            if let Err(error) = prepared.cardinality.validate(rows_seen.saturating_add(1)) {
                return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                    response: Some(response),
                    error,
                }));
            }
            if rows_seen == ROW_LIMIT {
                return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                    response: Some(response),
                    error: server_error(ServerSelectError::RowLimit { maximum: ROW_LIMIT }),
                }));
            }
            cells = match cells.checked_add(prepared.columns.len()) {
                Some(cells) => cells,
                None => {
                    return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                        response: Some(response),
                        error: server_error(ServerSelectError::CellLimit {
                            maximum: CELL_LIMIT,
                        }),
                    }));
                }
            };
            if cells > CELL_LIMIT {
                return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                    response: Some(response),
                    error: server_error(ServerSelectError::CellLimit {
                        maximum: CELL_LIMIT,
                    }),
                }));
            }
            let decoded = (|| -> Result<(RuntimeValue, u64), PostgresKernelError> {
                let row_index = rows_seen;
                for (guard_index, guard) in prepared.guards.iter().enumerate() {
                    let accepted = row
                        .try_get::<usize, bool>(prepared.columns.len() + guard_index)
                        .map_err(|source| {
                            server_error(ServerSelectError::RowDecode {
                                row: row_index,
                                column: prepared.columns.len() + guard_index,
                                source,
                            })
                        })?;
                    if !accepted {
                        return Err(server_error(ServerSelectError::VariablePayload {
                            row: row_index,
                            column: guard.column,
                            maximum: prepared.variable_payload_limit,
                        }));
                    }
                }
                let mut values = Vec::with_capacity(prepared.columns.len());
                for (column_index, column) in prepared.columns.iter().enumerate() {
                    let value = decode_value(active, &row, row_index, column_index, column)?;
                    let value_payload = match &value {
                        RuntimeValue::Record(_) => {
                            canonical_record_payload_len(active, &value, row_index, column_index)?
                        }
                        _ => logical_payload_len(&value)?,
                    };
                    payload = add_payload(payload, value_payload)?;
                    values.push(value);
                }
                rows_seen = rows_seen.saturating_add(1);
                let [value] = values.try_into().map_err(|_| {
                    server_error(ServerSelectError::PreparedResult {
                        rule: "resource SERVER execution must produce exactly one value per row",
                    })
                })?;
                let encoded = encode_active_value(active, &value).map_err(|source| {
                    server_error(ServerSelectError::ValueCodec {
                        row: row_index,
                        column: 0,
                        source,
                    })
                })?;
                let byte_count = u64::try_from(encoded.len()).map_err(|_| {
                    server_error(ServerSelectError::PayloadLimit {
                        maximum: PAYLOAD_LIMIT,
                    })
                })?;
                Ok((value, byte_count))
            })();
            let (value, byte_count) = match decoded {
                Ok(value) => value,
                Err(error) => {
                    return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                        response: Some(response),
                        error,
                    }));
                }
            };
            if !matches!(prepared.cardinality, ResultCardinality::BoundedMany) {
                let cancelled = cancellation.cancelled();
                let next_row = stream.try_next();
                futures_util::pin_mut!(cancelled, next_row);
                let lookahead = match select(cancelled, next_row).await {
                    Either::Left(((), _next_row)) => {
                        return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                            response: Some(response),
                        }));
                    }
                    Either::Right((row, _cancelled)) => row,
                };
                match lookahead {
                    Err(error) => {
                        return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                            response: Some(response),
                            error: PostgresKernelError::Database(error),
                        }));
                    }
                    Ok(Some(_)) => {
                        if let Err(error) =
                            prepared.cardinality.validate(rows_seen.saturating_add(1))
                        {
                            return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                                response: Some(response),
                                error,
                            }));
                        }
                    }
                    Ok(None) => {}
                }
            }
            (value, byte_count)
        };
        if credit.item_count == 0 {
            pending = Some((value, byte_count));
            if response
                .send(Ok(AuthenticatedServerResourceEvent::Waiting {
                    required_bytes: byte_count,
                }))
                .is_err()
            {
                return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: None,
                }));
            }
            continue;
        }
        if byte_count > credit.byte_count {
            pending = Some((value, byte_count));
            if response
                .send(Ok(AuthenticatedServerResourceEvent::Waiting {
                    required_bytes: byte_count,
                }))
                .is_err()
            {
                return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: None,
                }));
            }
            continue;
        }
        if cancellation.is_requested() {
            return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: Some(response),
            }));
        }
        total_items = match total_items.checked_add(1) {
            Some(total_items) => total_items,
            None => {
                return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                    response: Some(response),
                    error: server_error(ServerSelectError::RowLimit { maximum: ROW_LIMIT }),
                }));
            }
        };
        total_bytes = match total_bytes.checked_add(byte_count) {
            Some(total_bytes) => total_bytes,
            None => {
                return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                    response: Some(response),
                    error: server_error(ServerSelectError::PayloadLimit {
                        maximum: PAYLOAD_LIMIT,
                    }),
                }));
            }
        };
        let event = AuthenticatedServerResourceEvent::Values {
            batch_sequence,
            item_count: 1,
            byte_count,
            values: vec![value],
        };
        final_batch_sequence = batch_sequence;
        batch_sequence = match batch_sequence.checked_add(1) {
            Some(batch_sequence) => batch_sequence,
            None => {
                return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                    response: Some(response),
                    error: server_error(ServerSelectError::RowLimit { maximum: ROW_LIMIT }),
                }));
            }
        };
        if response.send(Ok(event)).is_err() {
            return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: None,
            }));
        }
    }
}

/// Runs one verified-standard SERVER resource target through the same bounded
/// pull protocol as an application target.
///
/// The standard executable is already pinned by the protected resource
/// decision. Standard resource targets currently use the closed parameter-echo
/// engine; no SQL preparation or PostgreSQL row stream is involved.
pub(crate) async fn run_authenticated_standard_resource_stream(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    executable: &StandardExecutable,
    arguments: &[FunctionArgument],
    commands: &mut mpsc::Receiver<ResourceProducerCommand>,
    cancellation: &ResourceCancellation,
) -> Result<ResourceProducerExit, PostgresKernelError> {
    let target = authorisation.target();
    if target.revision() != active.pair() {
        return Err(server_error(ServerSelectError::AuthorisationMismatch {
            authorised: Box::new(target),
            active: active.pair(),
        }));
    }
    let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
        server_error(ServerSelectError::FunctionNotActive {
            pair: active.pair(),
            function: target.function(),
        })
    })?;
    let function = standard
        .catalogue()
        .function_by_id(target.function())
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: target.function(),
            })
        })?;
    if executable.function() != target.function()
        || executable.revision().function() != target.function()
        || executable.revision().id() != function.current_revision()
    {
        return Err(server_error(ServerSelectError::FunctionNotActive {
            pair: active.pair(),
            function: target.function(),
        }));
    }
    let value = execute_standard_parameter_echo(function, executable.revision(), arguments)?;
    let encoded = encode_active_value(active, &value).map_err(|source| {
        server_error(ServerSelectError::ValueCodec {
            row: 0,
            column: 0,
            source,
        })
    })?;
    let byte_count = u64::try_from(encoded.len()).map_err(|_| {
        server_error(ServerSelectError::PayloadLimit {
            maximum: PAYLOAD_LIMIT,
        })
    })?;
    let mut emitted = false;

    loop {
        let cancelled = cancellation.cancelled();
        let received = commands.recv();
        futures_util::pin_mut!(cancelled, received);
        let command = match select(cancelled, received).await {
            Either::Left(((), _received)) => {
                return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: None,
                }));
            }
            Either::Right((command, _cancelled)) => command,
        };
        let Some(ResourceProducerCommand::Pull(ResourceProducerPull { credit, response })) =
            command
        else {
            return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: None,
            }));
        };
        if credit.item_count == 0 && !emitted {
            return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                response: Some(response),
                error: server_error(ServerSelectError::Argument {
                    parameter: None,
                    rule: "resource pull credit must be non-zero",
                }),
            }));
        }
        if cancellation.is_requested() {
            return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: Some(response),
            }));
        }
        if !emitted {
            if byte_count > credit.byte_count {
                if response
                    .send(Ok(AuthenticatedServerResourceEvent::Waiting {
                        required_bytes: byte_count,
                    }))
                    .is_err()
                {
                    return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                        response: None,
                    }));
                }
                continue;
            }
            emitted = true;
            if response
                .send(Ok(AuthenticatedServerResourceEvent::Values {
                    batch_sequence: 0,
                    item_count: 1,
                    byte_count,
                    values: vec![value.clone()],
                }))
                .is_err()
            {
                return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: None,
                }));
            }
            continue;
        }
        return Ok(ResourceProducerExit::Completed(ResourceProducerCompleted {
            response,
            final_batch_sequence: 0,
            total_items: 1,
            total_bytes: byte_count,
        }));
    }
}

/// Adapts one scalar or stream SERVER signature to the internal one-column
/// result model.
///
/// The SQL planner and decoder intentionally operate on ResultRows. A scalar
/// or stream resource therefore uses the same validated plan with a synthetic
/// value column, while retaining the exact resolved item type. This is an
/// execution-only view; the recovered catalogue definition is never replaced
/// or persisted.
fn row_execution_function(function: &FunctionDefinition) -> Option<FunctionDefinition> {
    let result_type = match function.return_type() {
        FunctionReturn::Single(result_type) | FunctionReturn::Stream(result_type) => result_type,
        FunctionReturn::Rows(_) => return None,
    };
    Some(FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        function.domain(),
        function.parameters().to_vec(),
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "value",
            0,
            *result_type,
        )]),
        function.current_revision(),
        function.security(),
        function.transaction(),
        function.volatility(),
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
    execute_standard_json_encode_bound(
        function,
        revision,
        StandardJsonEncodeBinding::Arguments(arguments),
        active,
        registry,
    )
}

/// One validated binding form for the closed JSON presenter.
///
/// Ordinary execution receives a [`FunctionArgument`], whose general-purpose
/// constructor intentionally rejects typed nulls. The sealed presenter route
/// has already obtained the canonical result and therefore binds that result
/// directly, after this engine checks the same pinned parameter identity.
enum StandardJsonEncodeBinding<'a> {
    Arguments(&'a [FunctionArgument]),
    Value {
        parameter: ParameterId,
        value: &'a RuntimeValue,
    },
}

/// Executes the closed JSON presenter after selecting one of its validated
/// binding seams. All artifact, revision, function-signature, and parameter
/// checks stay in this common path for both ordinary and sealed execution.
fn execute_standard_json_encode_bound(
    function: &FunctionDefinition,
    revision: &FunctionRevisionRecord,
    binding: StandardJsonEncodeBinding<'_>,
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
    let value = match binding {
        StandardJsonEncodeBinding::Arguments(arguments) => {
            validate_standard_json_encode_argument(parameter, arguments)?
        }
        StandardJsonEncodeBinding::Value {
            parameter: bound_parameter,
            value,
        } => {
            if bound_parameter != parameter {
                return Err(argument_error(
                    Some(bound_parameter),
                    "standard json-encode arguments must bind the pinned parameter identity",
                ));
            }
            value
        }
    };
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
/// The V1-V7 compatibility snapshots do not provide retained table-presenter
/// records, so the route constructs the compatibility selector records here: alias
/// `json` -> `std.json.encode` (input `std.json.Value`, output
/// `std.io.ByteStream` with media type `application/json`), alias `table` ->
/// `std.terminal.present_table` (input `std.data.Rows`, output
/// `std.terminal.Document`, no media type), and alias `csv` ->
/// `std.csv.encode` (input `std.data.Rows`, output `std.io.ByteStream` with
/// media type `text/csv`). The table selector is only compatibility metadata:
/// V8/V9 execution resolves the function and retained executable from the
/// active verified standard in `retained_terminal_table_target`. All entries
/// stream nothing and carry the default priority.
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

/// Resolves a qualified presenter input type against the active application
/// catalogue first, then the exact verified standard snapshot pinned by the
/// active catalogue-hash context.
///
/// A name present in both catalogues is deliberately not silently selected.
/// Only multiple aliases tied at the highest presenter priority are reported
/// as `Ambiguous`; a collision with zero or one matching presenter aliases is
/// retained as the closed unresolved type-name error instead of fabricating a
/// presenter tie.
fn resolve_sealed_presenter_type_name(
    name: &QualifiedSemanticName,
    active: &ActiveDatabaseRevision,
) -> Result<TypeId, OutputResolutionError> {
    let lookup = TypeLookupName::qualified(name.clone());
    let application = active.catalogue().type_id_by_name(&lookup);
    let standard = active
        .catalogue_hash_context()
        .standard()
        .and_then(|snapshot| snapshot.catalogue().type_id_by_name(&lookup));

    match (application, standard) {
        (Some(application), Some(standard)) => {
            let mut matching = sealed_presenter_registry()
                .entries()
                .iter()
                .filter(|entry| entry.input_type() == application || entry.input_type() == standard)
                .peekable();
            let Some(first) = matching.next() else {
                return Err(OutputResolutionError::UnresolvedTypeName {
                    name: name.to_string(),
                });
            };
            let best_priority = first.priority();
            let mut aliases = vec![first.alias().to_owned()];
            for entry in matching {
                if entry.priority() < best_priority {
                    break;
                }
                aliases.push(entry.alias().to_owned());
            }
            if aliases.len() > 1 {
                Err(OutputResolutionError::Ambiguous {
                    selector: AmbiguousOutputSelector::TypeName(name.to_string()),
                    aliases,
                })
            } else {
                Err(OutputResolutionError::UnresolvedTypeName {
                    name: name.to_string(),
                })
            }
        }
        (Some(type_id), None) | (None, Some(type_id)) => Ok(type_id),
        (None, None) => Err(OutputResolutionError::UnresolvedTypeName {
            name: name.to_string(),
        }),
    }
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
/// bounded `ResultRows`; an opaque value with the exact `std.data.Rows` type
/// is decoded back to its complete shape, while other values use the legacy
/// one-column, one-row `result` wrapper. An unresolved alias,
/// media type, or type name is [`SealedPresentationError::OutputResolution`]
/// (`ORNA0702`); a result the matched presenter cannot accept is
/// [`SealedPresentationError::NoPath`] (`ORNA0701`). The presented opaque
/// value replaces the canonical value in the final `ValueBatch`.
pub(crate) fn present_sealed_standard_output(
    requirement: &InvocationOutputRequirement,
    value: RuntimeValue,
    client_offer: &InvocationClientOffer,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<RuntimeValue, SealedPresentationError> {
    let presenter_registry = sealed_presenter_registry();
    let entry = if requirement.alias().is_none() && requirement.media_type().is_none() {
        match requirement.type_selector() {
            Some(InvocationOutputTypeSelector::QualifiedName(name)) => {
                let type_id = resolve_sealed_presenter_type_name(name, active)
                    .map_err(SealedPresentationError::OutputResolution)?;
                presenter_registry
                    .resolve_input_type(type_id)
                    .map_err(|error| match error {
                        OutputResolutionError::UnresolvedTypeName { .. } => {
                            OutputResolutionError::UnresolvedTypeName {
                                name: name.to_string(),
                            }
                        }
                        other => other,
                    })
                    .map_err(SealedPresentationError::OutputResolution)?
            }
            _ => presenter_registry
                .resolve_requirement(requirement, |_| None)
                .map_err(SealedPresentationError::OutputResolution)?,
        }
    } else {
        presenter_registry
            .resolve_requirement(requirement, |name| {
                resolve_sealed_presenter_type_name(name, active).ok()
            })
            .map_err(SealedPresentationError::OutputResolution)?
    };
    let requirement_matches = match requirement.streaming() {
        InvocationStreamingRequirement::Unspecified | InvocationStreamingRequirement::Preferred => {
            true
        }
        InvocationStreamingRequirement::Required => entry.streaming(),
        InvocationStreamingRequirement::Forbidden => !entry.streaming(),
        _ => false,
    };
    if !requirement_matches {
        return Err(SealedPresentationError::NoPath);
    }
    let output_descriptor = TypeDescriptor::named(entry.output_type());
    let sink_matches = client_offer.sink_offers().iter().any(|offer| {
        if offer.descriptor() != &output_descriptor {
            return false;
        }
        if entry.streaming() && !offer.streaming() {
            return false;
        }
        match entry.media_type() {
            Some(media_type) => offer.media_types().iter().any(|offered| {
                offered == media_type
                    || (entry.output_type() == orna_standard::STD_IO_BYTE_STREAM_TYPE_ID
                        && offered == "application/octet-stream")
            }),
            None => true,
        }
    });
    if !sink_matches {
        return Err(SealedPresentationError::NoPath);
    }
    match entry.function() {
        STD_JSON_ENCODE_FUNCTION_ID => execute_standard_json_encode_bound(
            &sealed_json_encode_definition(),
            &sealed_json_encode_revision(),
            StandardJsonEncodeBinding::Value {
                parameter: STD_JSON_ENCODE_PARAMETER_ID,
                value: &value,
            },
            active,
            registry,
        )
        .map_err(sealed_presenter_engine_error),
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID => {
            let rows = sealed_result_rows(value, active, registry)?;
            if let Some((function, revision)) =
                retained_terminal_table_target(active).map_err(sealed_presenter_engine_error)?
            {
                execute_standard_terminal_table(function, revision, &rows, active, registry)
                    .map_err(sealed_presenter_engine_error)
            } else {
                execute_standard_terminal_table(
                    &sealed_terminal_table_definition(),
                    &sealed_terminal_table_revision(),
                    &rows,
                    active,
                    registry,
                )
                .map_err(sealed_presenter_engine_error)
            }
        }
        STD_CSV_ENCODE_FUNCTION_ID => {
            let rows = sealed_result_rows(value, active, registry)?;
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

/// Resolves the retained V8/V9 table presenter from the active verified standard.
///
/// V1-V7 intentionally return `None` so their historical synthetic presenter
/// remains explicit. Once Rows is part of the standard snapshot, a missing or
/// crossed executable is a closed target failure: the compatibility presenter
/// must not make the table sink available.
fn retained_terminal_table_target(
    active: &ActiveDatabaseRevision,
) -> Result<Option<(&FunctionDefinition, &FunctionRevisionRecord)>, PostgresKernelError> {
    let Some(standard) = active.catalogue_hash_context().standard() else {
        return Ok(None);
    };
    if !matches!(
        standard.revision(),
        STANDARD_LIBRARY_V8_REVISION_ID | STANDARD_LIBRARY_V9_REVISION_ID
    ) {
        return Ok(None);
    }

    let function = standard
        .catalogue()
        .function_by_id(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            })
        })?;
    let executable = standard
        .executables()
        .iter()
        .find(|executable| executable.function() == STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            })
        })?;
    if executable.revision().function() != function.id()
        || executable.revision().id() != function.current_revision()
    {
        return Err(server_error(ServerSelectError::FunctionNotActive {
            pair: active.pair(),
            function: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        }));
    }
    Ok(Some((function, executable.revision())))
}

/// Converts the canonical sealed result to the bounded `ResultRows` model the
/// terminal-table and CSV engines accept.
///
/// A registered `std.data.Rows` opaque value carries the complete result shape
/// and is decoded against the already pinned active revision and registry.
/// Other values retain the legacy one-column, one-row `result` wrapper. Opaque,
/// constructed, and invocation-carrier values have no scalar path to the
/// presentation sinks (`ORNA0701`).
fn sealed_result_rows(
    value: RuntimeValue,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<ResultRows, SealedPresentationError> {
    if let RuntimeValue::Opaque(opaque) = &value
        && opaque.opaque_type() == STD_DATA_ROWS_TYPE_ID
    {
        return decode_rows(active, registry, opaque.canonical_payload())
            .map_err(|_| SealedPresentationError::NoPath);
    }
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
/// The ordinary engine accepts exactly one argument bound to the pinned
/// parameter. Its [`FunctionArgument`] boundary rejects typed nulls; the
/// sealed route uses [`StandardJsonEncodeBinding::Value`] instead, after the
/// common artifact, signature, and parameter checks. The returned value is the
/// already bound typed value, whose conversion to JSON is the presenter's
/// closed lossless rule.
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
/// object), lists (arrays), maps (objects), and canonical `std.json.Value`
/// payloads. Every other runtime form (enums, records, other opaque values,
/// options, and invocation carriers) cannot be represented without loss and
/// is rejected.
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
        RuntimeValue::Opaque(value) if value.opaque_type() == STD_JSON_VALUE_TYPE_ID => {
            decode_std_json_value(value)
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

/// Decodes the already-validated canonical payload of one `std.json.Value`.
fn decode_std_json_value(value: &OpaqueValue) -> Result<serde_json::Value, &'static str> {
    let payload = value.canonical_payload();
    let magic = JSON_MAGIC.as_bytes();
    if !payload.starts_with(magic) {
        return Err("std.json.encode cannot decode a canonical std.json.Value payload");
    }
    let length_start = magic.len();
    let body_start = length_start
        .checked_add(4)
        .ok_or("std.json.encode cannot decode a canonical std.json.Value payload")?;
    let length_bytes = payload
        .get(length_start..body_start)
        .ok_or("std.json.encode cannot decode a canonical std.json.Value payload")?;
    let body_length = u32::from_be_bytes(
        length_bytes
            .try_into()
            .map_err(|_| "std.json.encode cannot decode a canonical std.json.Value payload")?,
    ) as usize;
    let body = payload
        .get(body_start..)
        .filter(|body| body.len() == body_length)
        .ok_or("std.json.encode cannot decode a canonical std.json.Value payload")?;
    serde_json::from_slice(body)
        .map_err(|_| "std.json.encode cannot decode a canonical std.json.Value payload")
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
/// trailing newline. Cells render with the same closed value rules as the
/// terminal table, then receive RFC-4180-style quoting: a cell containing a
/// comma, double quote, CR, or LF is quoted and embedded quotes are doubled.
/// NUL and other forbidden control characters remain rejected, while CR and LF
/// are valid CSV data.
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
        reject_csv_control_characters(
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
            let cell = render_csv_cell(active, value)?;
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
    render_cell(active, value, reject_control_characters)
}

/// Renders one CSV cell, allowing RFC-4180 CR/LF data while rejecting other
/// forbidden control characters.
fn render_csv_cell(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
) -> Result<String, &'static str> {
    render_cell(active, value, reject_csv_control_characters)
}

fn render_cell(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    validate: fn(&str, &'static str) -> Result<(), &'static str>,
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
        RuntimeValue::Record(value) => render_record_cell_with(active, value, validate)?,
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
    validate(
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
fn render_record_cell_with(
    active: &ActiveDatabaseRevision,
    record: &RecordValue,
    validate: fn(&str, &'static str) -> Result<(), &'static str>,
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
        cell.push_str(&render_cell(active, value, validate)?);
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

/// Rejects controls that cannot be represented as CSV data. CR and LF are
/// explicitly allowed because RFC-4180 quoting preserves them inside fields.
fn reject_csv_control_characters(text: &str, rule: &'static str) -> Result<(), &'static str> {
    if text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\r' | '\n'))
    {
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
    ExactlyOne,
}

impl ResultCardinality {
    fn validate(self, row_count: usize) -> Result<(), PostgresKernelError> {
        match self {
            Self::BoundedMany => Ok(()),
            Self::AtMostOne => validate_identity_selected_cardinality(row_count),
            Self::ExactlyOne if row_count > 1 => {
                Err(server_error(ServerSelectError::Cardinality {
                    rule: "a scalar SERVER SELECT returned more than one row",
                }))
            }
            Self::ExactlyOne => Ok(()),
        }
    }

    fn finish(self, row_count: usize) -> Result<(), PostgresKernelError> {
        if matches!(self, Self::ExactlyOne) && row_count == 0 {
            return Err(server_error(ServerSelectError::Cardinality {
                rule: "a scalar SERVER SELECT returned zero rows",
            }));
        }
        Ok(())
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
    shape.cardinality.finish(rows.len())?;
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
#[path = "server_execution/tests.rs"]
mod tests;
