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

#[path = "server_mutation_execution/database.rs"]
mod database;
#[cfg(test)]
use database::unique_constraint;
use database::{
    UniqueConstraint, UniqueConstraints, execute_delete, execute_insert, execute_update,
};

#[path = "server_mutation_execution/operations.rs"]
mod operations;
use operations::*;

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

#[cfg(test)]
#[path = "server_mutation_execution/tests.rs"]
mod tests;
