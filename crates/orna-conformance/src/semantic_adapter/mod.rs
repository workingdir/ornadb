//! Semantic and runtime conformance seams for the frozen v1 contracts.
//!
//! `orna-check` is a real, deterministic analyzer, but intentionally
//! exposes one combined analysis result rather than separate compiler phases.
//! This adapter executes it and only reports a phase result where that result
//! is meaningful. `RuntimeEvaluator` remains an explicit seam: the bounded
//! evaluator below handles pure expression units, while the durable runtime
//! still owns module execution, effects, and scenario lifecycles.

// Every stage function returns `StageOutcome<Diagnostic>` with the full
// diagnostic the adapter reports: code, message, notes and source span. Boxing
// it would push a Box onto every caller to save an allocation on an error path.
#![allow(clippy::result_large_err)]

use crate::{
    ConformanceAdapter, ProjectEnvironment, ProjectExpectations, ProjectUnit, Scenario, SourceUnit,
    StageOutcome, SyntaxAdapter,
    row_admission::{admit_project_rows, preflight_project_rows},
};
use num_bigint::BigInt;
use orna_check::{
    AssertionOwner, AssertionPlan, Catalogue, DIAG_AMBIGUOUS, DIAG_IMPORT, EffectSummary,
    ModuleInput, Namespace, StandardDependencyProfile, SystemProviders,
    UNRESOLVED_TABLE_DEPENDENCY, analyze_with_providers, assertion_dependencies_apply,
};
use orna_cursor::{
    CursorError, TransactionKey, TransactionKeyType, TransactionTableKey, decimal_key_cmp,
    encoded_key, encoded_table_key, record_field, transaction_continuation_cursor,
    transaction_cursor_key,
};
use orna_evaluator::{
    ActivationEffectReport, ActivationExplainCatalogue, EffectGate, EffectHandler, Environment,
    EvaluationError, ExplainCatalogue, Functions, Limits as EvaluatorLimits, NominalDefinition,
    NominalDefinitions, NominalField, PureFunction as RetainedFunction, RelationPage, StepBudget,
    SysHostAttachment, SysHostContext, evaluate_expression, evaluate_expression_with_functions,
    evaluate_with_functions_and_budget, evaluate_with_functions_and_nominals,
    invoke_named_with_effects_and_budget, invoke_named_with_effects_nominals_and_budget,
    invoke_named_with_nominals, source_enum_definitions,
};
use orna_project::{LoadedProject, ProjectLoader, ResolvedTableSchema, admit_format3_row};
use orna_repository::Repository;
use orna_runtime::{
    CheckpointResetRequest, FaultInjector, FaultPoint, ListStreamSource, NoFault, RequestIdentity,
    RequestStatus, RunObservationRegistration, RunningTableRequestContinuation, RuntimeError,
    RuntimeIdentity, RuntimePublicationMetadataRows, RuntimeState, StreamFailurePayload,
    StreamHandler, StreamHandlerResult, StreamItem, StreamObservationRegistration,
    StreamRunControl, StreamRunOutcome, StreamSource, StreamSourcePoll, StreamStep,
    StreamTableCandidateValidator, StreamTableDeliveryError, StreamTableMutationBatch,
    StreamValidatedTableDeliveryCommit, StreamValidatedTableMutationBatch,
    TableActivationCandidateValidator, TableActivationError, TableMutation, TableObjectId,
    TerminalOutcome, ValidatedTableActivationCommit, ValidatedTableRequestActivationCommit,
    WriterLease,
};
use orna_storage::RuntimePublicationCoordinator;
use orna_stream::{
    AssertionOwnerKind, AsyncCheckpointBackend, CheckpointPrecondition, CommitIntent, CommitResult,
    DiagnosticClass, DiagnosticCode, FailureIdentity, FailureRecord, FailureStatus, LeasePurpose,
    SafeDiagnostic,
};
use orna_stream_identity::{
    CheckpointKey, Component, ConsumerIdentity, DeliveryIdentity, Position,
};
use orna_syntax::{parse_expression, parse_module};
use orna_syntax_ast::{
    Declaration, Expr, Pattern, SourceSpan, Statement, TypeMember, TypeRepresentation,
};
use orna_sys::{
    AdmissionError, AdmissionRequest, ExecutionBoundary, FunctionDescriptor, InvocationCompletion,
    InvocationExecutor, InvocationHandle, InvocationState, RuntimeSupervisor, TypedValue,
};
use orna_sys_id::SysRelation;
use orna_table::{ActivationError, DatabaseActivation, DatabaseRuntime, TableError};
use orna_value::{Diagnostic, DiagnosticSeverity, OvbRaw, SafeText, Value};
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    ops::Bound,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
mod adapter;
mod admission;
mod assertions;
mod bounded;
mod candidates;
mod durable;
mod durable_fixtures;
mod durable_project;
mod durable_request;
mod durable_stream;
mod effects;
mod project_graph;
mod relation;
mod runtime_adapter;
mod stream_admission;
mod support;
mod table_effects;
mod transactional;

pub use adapter::*;
use admission::*;
use assertions::*;
pub use bounded::*;
pub use candidates::*;
pub use durable::*;
use durable_fixtures::*;
use durable_request::*;
use effects::*;
use project_graph::*;
use relation::*;
pub use runtime_adapter::*;
pub use stream_admission::*;
use support::*;
use table_effects::*;
pub use transactional::*;

#[cfg(test)]
#[path = "tests/transaction_admission.rs"]
mod transaction_admission_tests;

#[cfg(test)]
#[path = "tests/bounded.rs"]
mod bounded_tests;

#[cfg(test)]
#[path = "tests/durable/mod.rs"]
mod durable_tests;

#[cfg(test)]
#[path = "tests/list_stream.rs"]
mod list_stream_tests;

#[cfg(test)]
#[path = "tests/effect_report.rs"]
mod effect_report_tests;
