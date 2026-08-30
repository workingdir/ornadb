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
