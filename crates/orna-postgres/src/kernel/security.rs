// These internal execution seams preserve the accepted error and state layouts.
#![allow(clippy::large_enum_variant)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
// Result APIs intentionally preserve the accepted public `PostgresKernelError` layout.
#![allow(clippy::result_large_err)]
#![allow(clippy::let_and_return)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::question_mark)]
#![allow(clippy::nonminimal_bool)]
#![allow(clippy::match_like_matches_macro)]
#[path = "security/audit.rs"]
mod audit;
#[path = "security/audit_codec.rs"]
mod audit_codec;
#[path = "security/audit_recovery.rs"]
mod audit_recovery;
#[path = "security/audit_writer.rs"]
mod audit_writer;
#[path = "security/authenticated_select.rs"]
mod authenticated_select;
#[path = "security/authentication.rs"]
mod authentication;
#[path = "security/client_evaluation.rs"]
mod client_evaluation;
#[path = "security/inspect_capture.rs"]
mod inspect_capture;
#[path = "security/local_identity.rs"]
mod local_identity;
#[path = "security/persistence.rs"]
mod persistence;
#[path = "security/raw_call.rs"]
mod raw_call;
#[path = "security/recovery.rs"]
mod recovery;
#[path = "security/resource.rs"]
mod resource;
#[path = "security/resource_cancellation.rs"]
mod resource_cancellation;
#[path = "security/resource_dispatch.rs"]
mod resource_dispatch;
#[path = "security/resource_finalization.rs"]
mod resource_finalization;
#[path = "security/resource_producer.rs"]
mod resource_producer;
#[path = "security/resource_producer_start.rs"]
mod resource_producer_start;
#[path = "security/resource_stream.rs"]
mod resource_stream;
#[path = "security/revision_guard.rs"]
mod revision_guard;
#[path = "security/sealed_audit.rs"]
mod sealed_audit;
#[path = "security/sealed_dispatch.rs"]
mod sealed_dispatch;
#[path = "security/sealed_events.rs"]
mod sealed_events;
#[path = "security/sealed_invocation.rs"]
mod sealed_invocation;
#[path = "security/sealed_server_contract.rs"]
mod sealed_server_contract;
#[path = "security/sealed_server_execution.rs"]
mod sealed_server_execution;
#[path = "security/sealed_server_stream.rs"]
mod sealed_server_stream;
#[path = "security/sealed_server_target.rs"]
mod sealed_server_target;
#[path = "security/target_resolution.rs"]
mod target_resolution;

use audit::*;
pub(crate) use audit_codec::encode_principal_kind;
use audit_codec::*;
pub(crate) use audit_recovery::recover_invocation_audit_events;
use audit_recovery::*;
pub use audit_writer::ResourceAuditTerminalOutcome;
#[cfg(test)]
use audit_writer::validate_resource_audit_nested_invocation;
pub(crate) use audit_writer::{
    append_invocation_audit_event, append_resource_audit_event, append_security_audit_event,
};
use audit_writer::{
    resource_audit_invariant, resource_parent_invocation_is_owned_in_transaction,
    validate_resource_audit_lineage, validate_resource_lineage, validate_resource_state_context,
};
use inspect_capture::{
    capture_completed_resource_inspect_snapshot, capture_sealed_invocation_snapshot,
};
use local_identity::append_client_capability_audit;
pub(crate) use local_identity::security_snapshots_match;
use persistence::*;
pub(crate) use persistence::{encode_privilege_class, recover_security_snapshot_for_active};
pub use raw_call::{AuthenticatedRawCallResult, RecordArgumentPreflight};
pub use resource::{
    AuthenticatedServerResourceAccepted, AuthenticatedServerResourceEvent,
    AuthenticatedServerResourceKind, AuthenticatedServerResourceProducer,
    AuthenticatedServerResourceResult, AuthenticatedServerResourceStart, ResourceCredit,
};

pub use resource_cancellation::ResourceCancellation;
pub(crate) use resource_producer::{
    ResourceProducerCancelled, ResourceProducerCommand, ResourceProducerCompleted,
    ResourceProducerExit, ResourceProducerFailed, ResourceProducerPull,
};
use revision_guard::lock_active_revision_for_resource;
pub(crate) use revision_guard::{
    lock_active_revision, require_complete_function_set, require_complete_function_targets,
};
use sealed_audit::{
    append_allowed_invocation_audit, append_allowed_invocation_audit_evidence,
    append_linked_invocation_audit, append_sealed_denied_audit, append_unresolved_invocation_audit,
};
pub(crate) use sealed_events::sealed_completed_events;
use sealed_events::{
    finish_sealed_failure, sealed_completed_events_from_values, sealed_failure_events,
    sealed_failure_result,
};
pub(crate) use sealed_invocation::InvocationAuditDecision;
use sealed_invocation::{
    PreparedSealedTarget, SealedInvocationFailureClass, SealedInvocationPreparedOutcome,
};
pub use sealed_invocation::{
    SealedInvocationContinuation, SealedInvocationExecution, SealedInvocationOperation,
    SealedInvocationPreflight, SealedInvocationResult,
};
#[cfg(test)]
use sealed_server_contract::{
    bind_authenticated_resource_arguments, classify_sealed_server_error,
    resource_target_shape_is_supported, resource_values_from_server_result,
    sealed_server_result_kind,
};
use sealed_server_contract::{
    resource_target_security_is_supported, sealed_server_target_is_mutation,
};
use sealed_server_execution::execute_sealed_server_after_audit;
#[cfg(test)]
use sealed_server_stream::sealed_server_stream_completed_event;
use sealed_server_stream::start_sealed_server_stream_producer;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::SystemTime,
};
pub(crate) use target_resolution::is_admitted_security_identity;
#[cfg(test)]
use target_resolution::resolve_resource_target_in_catalogues;
use target_resolution::{
    SealedResolvedTarget, authorise_sealed_target, bind_sealed_invoke_arguments,
    resolve_resource_target, resolve_sealed_target, sealed_security_target,
    sealed_target_invariant, sealed_target_security_is_supported,
};

use orna_artifact::client_plan::{CAPABILITY_FORMAT_VERSION, CapabilityClientPlan, ResourceKind};
use orna_client::{
    ClientExecutionError, ClientExecutionResult, ClientResourceCompletion,
    ClientResourceExecutionError, ClientResourceExecutor, ClientStateContext, ClientStateStore,
    client_function_arguments_match, client_security_context_digest,
    evaluate_client_function_in_state_context_with_grants_and_arguments as evaluate_authorised_client_function_with_state_context_and_arguments,
    evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation as evaluate_authorised_client_function_with_state_context_and_arguments_and_executor,
    evaluate_client_function_with_grants as evaluate_authorised_client_function,
};
use orna_core::{
    CatalogueRevisionId, FunctionId, FunctionRevisionId, InspectEpochId, InvocationAuditEventId,
    InvocationId, ObjectId, PrincipalId, SecurityAuditEventId, SourceRevisionId,
    StandardLibraryRevisionId,
    catalogue::{FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity},
    inspect::{InspectOutcomeKind, InspectPrivilege, InspectSnapshotOptions},
    invocation::{
        InvocationArgument, InvocationClientOffer, InvocationEventBody, InvocationFailure,
        InvocationFailurePhase, InvocationOutputRequirement, InvocationParameterSelector,
        InvocationRetryability, InvocationTarget as InvocationRequestTarget, InvokeEvent,
        InvokeValue, ProtectedInvocationDecision, decide_protected_invocation,
    },
    revision::{ActiveDatabaseRevision, CatalogueHashContext, RevisionPair, StandardExecutable},
    security::{
        AuthenticatedSession, AuthorisedInvocation, CATALOGUE_HEALTH_FUNCTION_ID,
        CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, ExecuteDecision, ExecuteDenial, ExecuteGrant,
        InspectDenial, InspectEpochScope, InvocationTarget, LocalPeerAuthenticationError,
        LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus, PrivilegeClass,
        PrivilegeDenial, PrivilegeGrant, RoleMembership, SecurityAdminAuditOperation,
        SecurityAuditDecision, SecurityAuditDenial, SecurityAuditEvent, SecurityAuditKind,
        SecurityAuditOutcome, SecurityFunctionTarget, SecuritySnapshot, SessionBindingError,
        TargetClass, UserStateAuditOperation,
    },
    state::UserStateCell,
    system::{
        SYS_INVOKE_FUNCTION_ID, SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID,
        SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
        SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID, SYS_SECURITY_GRANT_ROLE_FUNCTION_ID,
        SYS_SECURITY_PRINCIPAL_TYPE_ID, SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_ID,
        SYS_SECURITY_REVOKE_ROLE_FUNCTION_ID, SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
        SystemFunctionDefinition, SystemFunctionKind, system_function_by_id,
        system_function_by_name,
    },
    types::TypeDescriptor,
    value::{
        FunctionArgument, OpaqueCodecRegistry, RecordValue, ResultRows, RuntimeType, RuntimeValue,
    },
};
use orna_protocol::{
    CallFailure, InvocationEventBatch, InvocationEventRecord, ResourceArgument,
    ResourceKind as ProtocolResourceKind, ResourceRequest, RetainedInvokeRequest,
    decode_retained_invoke_request, encode_active_value, encode_rows_value,
};
use orna_standard::{
    STANDARD_LIBRARY_V8_REVISION_ID, STANDARD_LIBRARY_V9_REVISION_ID, registered_opaque_codecs,
};
use sha2::{Digest, Sha256};
use tokio_postgres::{IsolationLevel, Row, Transaction, types::FromSqlOwned};

use super::PostgresSession;

use crate::{
    PostgresKernel, PostgresKernelError, RawServerTargetError,
    bootstrap::require_current_migrations,
    physical::establish_trusted_search_path,
    recovery::{load_verified_standard_library, recover_active_revision},
    server_execution::{
        SealedPresentationError, ServerSelectError, ServerSelectResult,
        execute_authorised_raw_server_select, execute_authorised_server_select,
        execute_standard_json_encode, execute_standard_parameter_echo,
        load_client_reference_loader, present_sealed_standard_output,
        raw_identity_selected_server_select_target_is_selected, raw_server_target_is_unavailable,
        raw_unique_text_selected_server_select_target_is_selected,
        run_authenticated_server_resource_stream, run_authenticated_standard_resource_stream,
    },
    server_mutation_execution::{
        RawServerReferenceMutation, ServerInsertError, execute_authorised_raw_server_insert,
        execute_authorised_raw_server_insert_with_arguments,
        execute_authorised_raw_server_reference_mutation, raw_server_delete_target_is_unavailable,
        raw_server_insert_target_is_selected, raw_server_insert_target_is_unavailable,
        raw_server_reference_mutation_target, raw_server_reference_value_update_target_is_selected,
        raw_server_update_target_is_unavailable,
    },
    server_runtime::{configure_and_recover, runtime_types_match},
    state::load_user_state_in_transaction,
};

pub(crate) fn finish_security_session<T>(
    operation: Result<T, PostgresKernelError>,
    shutdown: Result<(), PostgresKernelError>,
) -> Result<T, PostgresKernelError> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn finish_authenticated_dispatch_session<T>(
    operation: Result<T, PostgresKernelError>,
    shutdown: Result<(), PostgresKernelError>,
) -> Result<T, PostgresKernelError> {
    shutdown?;
    operation
}

#[cfg(test)]
#[path = "security/tests.rs"]
mod tests;
