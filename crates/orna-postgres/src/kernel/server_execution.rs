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

#[path = "server_execution/contract.rs"]
mod contract;
pub use contract::{ServerSelectContext, ServerSelectError, ServerSelectResult};

#[path = "server_execution/presentation.rs"]
mod presentation;
pub(crate) use presentation::{
    SealedPresentationError, execute_standard_json_encode, present_sealed_standard_output,
};
#[cfg(test)]
use presentation::{
    encode_json_value, execute_standard_csv_encode, execute_standard_terminal_table,
    frame_byte_stream, frame_terminal_document, render_csv_document, render_terminal_table,
    resolve_sealed_presenter_type_name, retained_terminal_table_target, sealed_result_rows,
};

#[path = "server_execution/raw.rs"]
mod raw;
pub(crate) use raw::{
    execute_authorised_raw_server_select, raw_identity_selected_server_select_target_is_selected,
    raw_server_target_is_unavailable, raw_unique_text_selected_server_select_target_is_selected,
};
#[cfg(test)]
use raw::{into_raw_server_values_for_context, raw_result_type_is_supported};

#[path = "server_execution/client_references.rs"]
mod client_references;
pub(crate) use client_references::load_client_reference_loader;

#[path = "server_execution/validation.rs"]
mod validation;
#[cfg(test)]
use validation::{
    add_expression_references, distinct_reference_error,
    expected_identity_selected_body_references, expected_unordered_body_references,
    field_path_type, supports_distinct_projection_type, supports_equality_type,
    supports_ordering_type, supports_result_type, validate_target_entry_count,
};
use validation::{
    argument_error, artifact_error, distinct_error, function_signature_error,
    result_columns_for_projections, validate_distinct_function_signature, validate_distinct_plan,
    validate_distinct_reference_evidence, validate_function_signature,
    validate_identity_selected_arguments, validate_identity_selected_function_signature,
    validate_identity_selected_plan, validate_identity_selected_reference_evidence,
    validate_no_arguments, validate_plan, validate_reference_evidence, validate_target_entries,
    validate_unique_text_selected_arguments, validate_unique_text_selected_function_signature,
    validate_unique_text_selected_plan, validate_unique_text_selected_reference_evidence,
};

#[path = "server_execution/lowering.rs"]
mod lowering;
#[cfg(test)]
use lowering::{
    RuntimeResultColumns, effective_query_limit, expected_postgres_type, lower_select_projections,
    ordering_sql, variable_payload_limit,
};
use lowering::{
    SelectBindValue, VariableGuard, is_variable_type, lower_distinct_plan,
    lower_identity_selected_plan, lower_plan, lower_unique_text_selected_plan,
    validate_prepared_columns,
};

#[path = "server_execution/rows.rs"]
mod rows;
#[cfg(test)]
use rows::validate_identity_selected_cardinality;
use rows::{
    ResultCardinality, ResultReadShape, add_payload, canonical_record_payload_len, decode_value,
    initial_payload_len, logical_payload_len, stream_rows,
};

#[path = "server_execution/resources.rs"]
mod resources;
pub(crate) use resources::{
    run_authenticated_server_resource_stream, run_authenticated_standard_resource_stream,
};

#[path = "server_execution/standard_echo.rs"]
mod standard_echo;
pub(crate) use standard_echo::execute_standard_parameter_echo;

#[path = "server_execution/operations.rs"]
mod operations;
pub(crate) use operations::execute_authorised_server_select;
use operations::prepare_active_transaction;
#[cfg(test)]
use operations::{DecodedServerPlan, decode_plan, row_execution_function};
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
