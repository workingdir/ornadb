#![recursion_limit = "512"]

//! Final Orna 1.1.0 portable sys catalogue and bounded, pre-effect admission.
//!
//! Resolution and durable transaction ownership stay with the evaluator and
//! runtime that own those concerns. The portable `sys` declaration schema is
//! collected from annotated descriptor methods in `system_api`. The local
//! supervisor below only provides a bounded execution and await seam for
//! admitted work.

pub use orna_sys_model::abi_version::AbiVersion;
pub use orna_sys_model::host_registry_model::{
    HostBindingDeclaration, HostOperationDescriptor, HostProviderRole, SystemHostOperationRegistry,
};

mod introspection;
pub use introspection::{
    DefinitionRef, Dependency, DependencyConfidence, DependencyGraph, DependencyGraphError,
    DependencyInput, DependencyKind, DisjunctStormBranchDescription,
    DisjunctStormCascadeDescription, DisjunctStormDescription, DisjunctStormLimitRebindDescription,
    ExplainError, ExplainedPlan, ExpressionRef, FileRef, FunctionPlanDescription, FunctionRef,
    MAX_DEPENDENCY_EDGES, MAX_DEPENDENCY_OBJECTS, MAX_PLAN_EXPRESSIONS, MAX_PLAN_NODES,
    MAX_REFERENCE_BYTES, MutableBranchSnapshot, Plan, PlanByteCapHandoffRoute,
    PlanByteCapScopeSegment, PlanDetail, PlanNode, PlanNodeKind, PlanNodeRef, PlanNullOrder,
    PlanOrdering, PlanSortDirection, PlanWindowFrameBound, QueryCheckpointGenerationDescription,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryLimitPushdownDescription, QueryMutationDescription, QueryMutationKind,
    QueryPairedCheckpointSegmentCompactionChainDescription,
    QueryPairedCheckpointSegmentCompactionStepDescription,
    QueryPairedCheckpointSpillRestoreChainDescription,
    QueryPairedCheckpointSpillRestoreOccurrenceDescription,
    QueryPairedSegmentRotationChainDescription, QueryPairedSegmentRotationDescription,
    QueryPairedStreamCompactionChainDescription, QueryPairedStreamCompactionOccurrenceDescription,
    QueryPairedStreamRotationChainDescription, QueryPairedStreamRotationOccurrenceDescription,
    QueryPairedWalRotationChainDescription, QueryPairedWalRotationOccurrenceDescription,
    QueryPartialIndexDescription, QueryPlanDescription, QuerySourceStatistics,
    QueryWindowAggregatePushdownDescription, QueryWindowSpillDescription, SnapshotRef, SourceSpan,
    explain_function, explain_query, explain_query_with_conjunct_disjunct_limit_chain,
    explain_query_with_decorrelated_subqueries,
    explain_query_with_disjunct_branch_limit_conjunct_cascade,
    explain_query_with_disjunct_conjunct_limit_chain, explain_query_with_disjunct_limit_chain,
    explain_query_with_disjunct_limit_conjunct_chain,
    explain_query_with_disjunct_storm_branch_limit_chains, explain_query_with_disjunct_storm_chain,
    explain_query_with_input_disjunct_limit_conjunct_chain,
    explain_query_with_input_limit_conjunct_disjunct_chain,
    explain_query_with_input_limit_conjunct_disjunct_limit_conjunct_chain,
    explain_query_with_input_limit_disjunct_chain,
    explain_query_with_input_limit_disjunct_conjunct_chain,
    explain_query_with_join_pair_identities,
    explain_query_with_join_pair_identities_and_limit_pushdowns,
    explain_query_with_join_pair_identities_and_limit_window_aggregate_pushdowns,
    explain_query_with_join_pair_identities_and_window_aggregate_pushdowns,
    explain_query_with_join_pair_identities_limit_window_aggregate_and_spill_pushdowns,
    explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns,
    explain_query_with_limit_chain,
    explain_query_with_paired_cost_restoration_and_window_pushdowns,
    explain_query_with_paired_cost_restoration_and_window_spill_pushdowns,
    explain_query_with_partial_indexes,
    explain_query_with_partial_indexes_and_decorrelated_subqueries,
    explain_query_with_partial_indexes_and_decorrelated_subqueries_and_join_pair_identities,
    explain_query_with_partial_indexes_and_join_pair_identities,
    explain_query_with_partial_indexes_and_paired_checkpoint_compaction_and_segment_rotation_chains,
    explain_query_with_partial_indexes_and_paired_checkpoint_rotation_and_stream_compaction_chains,
    explain_query_with_partial_indexes_and_paired_checkpoint_rotation_stream_and_spill_restore_chains,
    explain_query_with_partial_indexes_and_paired_checkpoint_segment_compaction_chains,
    explain_query_with_partial_indexes_and_paired_checkpoint_stream_rotation_chains,
    explain_query_with_partial_indexes_and_paired_checkpoint_wal_rotation_chains,
    explain_query_with_window_aggregate_pushdowns,
};

mod diagnostic;
pub use diagnostic::{AdmissionError, ArgumentTypeDetail, AwaitError, Diagnostic, DiagnosticField};

mod ids;
pub use ids::{
    FunctionId, FunctionIdentity, InvocationId, InvocationRef, ObjectRef, PlanRef, RevisionId,
    RuntimeId, SnapshotId, TypeId, TypeRef,
};

mod invocation;
pub use invocation::{
    Admission, AdmissionRequest, CancellationToken, ExecutionBoundary, FunctionDescriptor,
    InvocationCompletion, InvocationContext, InvocationExecutor, InvocationHandle,
    InvocationIdentity, InvocationMode, InvocationResult, InvocationState, InvocationStatus,
    Parameter, RetainedInvocationResult, SystemEffect, SystemFunctionDescriptor, TerminalClass,
    TransactionMode, TypeWitness,
};

mod runtime;
pub use runtime::Runtime;
pub(crate) use runtime::hex;

mod supervisor;
pub use supervisor::RuntimeSupervisor;

mod value;
pub use value::{
    Argument, ArgumentMap, CANONICAL_VALUE_CODEC_V1, InvocationArgumentMetadata,
    InvocationMetadata, RetainedValue, SecretProjection, TypedValue, ValueMetadata,
    ValueMetadataError, ValueMetadataFacts, ValueMetadataResolver, system_secret_projection,
    system_value_metadata,
};

mod provider;
pub use provider::{
    AbiType, EffectSet, FailureCode, FunctionSignature, OperationContract, OperationId,
    OperationParameter, Precondition, ProviderAbiError, ProviderDiagnostic, ProviderFailure,
    ProviderId, ProviderOffer, ProviderRoleRegistry, SemanticRoleContract, SemanticRoleId,
    SystemDispatchResult, SystemDispatchTable, SystemOperationProvider, SystemProviderAbi,
    system_dispatch_table, system_provider_abi, system_provider_abi_json,
    system_provider_abi_schema_json, validate_provider_offer,
};

mod provider_linkage;
pub use provider_linkage::{
    SystemProviderHost, SystemProviderLink, SystemProviderLinkage, SystemSurfaceMember,
    system_provider_linkage,
};

mod host_context;
pub use host_context::{
    BUILD_ID, IMPLEMENTATION_NAME, IMPLEMENTATION_VERSION, LANGUAGE_VERSION,
    PRESENTATION_PROTOCOL_VERSION, RuntimeMode, SUPPORTED_PROFILES, SYS_VERSION, StdSnapshotPin,
    SysHostContext, new_runtime_id,
};

mod host_registry;
pub use host_registry::{
    system_host_binding_modules_json, system_host_binding_stubs, system_host_operation_registry,
    system_host_operation_registry_json, system_host_operation_registry_schema_json,
};

mod host_environment;
pub use host_environment::{
    EnvironmentDispatchValue, EnvironmentProvider, EnvironmentProviderError,
};

mod host_process;
pub use host_process::{ProcessProvider, ProcessProviderError, ProcessRequest, ProcessRunOutput};

mod host_clock;
pub use host_clock::{ClockProvider, ClockProviderError};

mod host_filesystem;
pub use host_filesystem::{
    CaptureFileError, CaptureFileStream, FilesystemProvider, FilesystemProviderError,
    HostFilesystemMetadata,
};

mod host_network;
pub use host_network::{HostHttpHandle, HostHttpResponse, HttpProvider, HttpProviderError};

mod system_api;
pub use system_api::*;

pub mod format_api_selection;

/// Returns final-format declaration metadata for a portable system function.
///
/// This context-free compatibility lookup assumes format 3. Callers handling
/// historical repositories must use [`format_api_selection`] with the
/// repository's verified recorded format/profile. The descriptor is static
/// metadata and does not grant invocation or administrative authority.
pub fn system_function_descriptor(name: &str) -> Option<&'static SystemFunctionDescriptor> {
    let context = format_api_selection::SystemFormatContext::from_recorded_pair(
        format_api_selection::RepositoryFormat::Final3,
        format_api_selection::ReaderContext::Final20261005,
    )?;
    format_api_selection::system_function_descriptor_for(context, name)
}

#[cfg(test)]
mod tests;
