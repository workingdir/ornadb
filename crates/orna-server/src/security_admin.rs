// Security administration returns the stable embedded-host error boundary.
#![allow(clippy::result_large_err)]
//! Local security administration for the Orna instance.
//!
//! This module runs the closed `orna security` command family (ADR 0065).
//! The `grant-execute` entry keeps the fixed catalogue-health grant path; the
//! administrative surface runs one closed operation against the local
//! instance with the same host inspection and kernel access as `orna inspect`
//! and `orna state`, and mirrors the `InstalledInspectError` failure and
//! render path.
//!
//! The host derives the session from the authenticated local peer
//! ([`PostgresKernel::authenticate_local_peer`]); the kernel is authoritative
//! for the `SecurityAdmin`-privilege enforcement gate, and the host checks the
//! local instance before dispatch.

use std::{fmt, io, io::Write};

use orna_core::security::AuthenticatedSession;
use orna_core::{
    FunctionId, PrincipalId,
    inspect::InspectPrivilege,
    security::{
        ExecuteDecision, ExecuteDenial, ExecuteGrant, PrincipalKind, PrivilegeClass,
        PrivilegeDecision, PrivilegeGrant, SecuritySnapshot,
    },
};
use orna_postgres::{PostgresKernel, PostgresKernelError};
use orna_runtime_v1::{
    CheckpointKey, RuntimeError, RuntimeState, StreamAdministrationOutcome, WriterLease,
};

use crate::{EmbeddedHostError, inspect_current_embedded_host};

/// A resolved stream-administration request from a trusted authenticated host.
///
/// The stream key is intentionally not decoded from an untrusted `sys.StreamRef`
/// here: the current server has no durable projection that resolves that public
/// reference. The caller must obtain this key from an authoritative runtime
/// observation first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthenticatedStreamAdminRequest {
    /// Stop new delivery admission and pause at the next transaction boundary.
    Pause {
        /// The authoritative stream identity selected by the host.
        stream: CheckpointKey,
        /// The requested public reason. The runtime cannot yet retain it.
        reason: Option<String>,
    },
    /// Resume an already paused stream.
    Resume {
        /// The authoritative stream identity selected by the host.
        stream: CheckpointKey,
    },
}

/// The truthful intermediate result of a durable authenticated stream action.
///
/// `PausePending` is not converted to `Bool`: the public pause contract needs
/// an invocation owner that waits for the active delivery boundary and records
/// its terminal result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticatedStreamAdminOutcome {
    Paused { changed: bool },
    PausePending { changed: bool },
    Running { changed: bool },
}

/// A closed failure before, during, or after a durable stream transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticatedStreamAdminError {
    /// The authenticated principal does not own the selected stream.
    OwnershipDenied,
    /// A lease or unresolved delivery state rejects the transition.
    Busy,
    /// A retained blocking failure prevents resume.
    BlockingFailure,
    /// The runtime store could not apply the writer-fenced transition.
    Runtime,
}

/// Applies one authenticated, ownership-checked stream transition through the
/// durable runtime backend.
///
/// This is a host adapter prerequisite, not the complete public `sys.admin`
/// entry point. A supplied pause reason is retained by the runtime in the
/// same writer-fenced transaction that admits the pause.
pub async fn run_authenticated_stream_admin(
    state: &RuntimeState,
    writer: WriterLease,
    session: &AuthenticatedSession,
    request: AuthenticatedStreamAdminRequest,
) -> Result<AuthenticatedStreamAdminOutcome, AuthenticatedStreamAdminError> {
    let stream = match &request {
        AuthenticatedStreamAdminRequest::Pause { stream, .. }
        | AuthenticatedStreamAdminRequest::Resume { stream } => stream,
    };
    if stream.consumer.principal.as_str() != session.principal().canonical() {
        return Err(AuthenticatedStreamAdminError::OwnershipDenied);
    }
    let outcome = match request {
        AuthenticatedStreamAdminRequest::Pause {
            stream,
            reason: Some(reason),
        } => state.pause_stream_with_reason(writer, stream, reason).await,
        AuthenticatedStreamAdminRequest::Pause {
            stream,
            reason: None,
        } => state.pause_stream(writer, stream).await,
        AuthenticatedStreamAdminRequest::Resume { stream } => {
            state.resume_stream(writer, stream).await
        }
    }
    .map_err(map_stream_runtime_error)?;
    match outcome {
        StreamAdministrationOutcome::Paused { changed } => {
            Ok(AuthenticatedStreamAdminOutcome::Paused { changed })
        }
        StreamAdministrationOutcome::PausePending { changed } => {
            Ok(AuthenticatedStreamAdminOutcome::PausePending { changed })
        }
        StreamAdministrationOutcome::Running { changed } => {
            Ok(AuthenticatedStreamAdminOutcome::Running { changed })
        }
        StreamAdministrationOutcome::Busy => Err(AuthenticatedStreamAdminError::Busy),
        StreamAdministrationOutcome::BlockingFailure => {
            Err(AuthenticatedStreamAdminError::BlockingFailure)
        }
    }
}

fn map_stream_runtime_error(_: RuntimeError) -> AuthenticatedStreamAdminError {
    AuthenticatedStreamAdminError::Runtime
}

/// A closed failure from the fixed catalogue-health execution-grant command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityGrantError {
    /// The default local instance is absent.
    InstanceNotInstalled,
    /// The local instance or its readiness evidence is invalid.
    InstanceInvalid,
    /// The running executable cannot verify the local embedded engine.
    EngineInvalid,
    /// The active revision could not be recovered.
    RecoveryFailed,
    /// The fixed-service grant could not be committed and verified.
    GrantFailed,
    /// The private asynchronous runtime could not be created.
    RuntimeFailed,
}

impl fmt::Display for SecurityGrantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InstanceNotInstalled => "orna: the default Orna instance is not installed",
            Self::InstanceInvalid => "orna: the default Orna instance is invalid",
            Self::EngineInvalid => "orna: the embedded PostgreSQL engine is not valid",
            Self::RecoveryFailed => {
                "orna: security grant-execute could not recover the active revision"
            }
            Self::GrantFailed => "orna: security grant-execute did not commit",
            Self::RuntimeFailed => "orna: security grant-execute runtime could not start",
        })
    }
}

impl std::error::Error for SecurityGrantError {}

/// Grants the fixed catalogue-health service permission for one active function.
///
/// The host inspection retains the instance guards for the complete recovery
/// and grant operation. The function identity has already been parsed from
/// the exact canonical command argument by the command parser.
pub fn run_installed_security_grant(function: FunctionId) -> Result<(), SecurityGrantError> {
    let host = inspect_current_embedded_host().map_err(map_host_error)?;
    let kernel = PostgresKernel::new(host.config().clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| SecurityGrantError::RuntimeFailed)?;

    runtime.block_on(async {
        let active = kernel
            .recover()
            .await
            .map_err(|_| SecurityGrantError::RecoveryFailed)?;
        kernel
            .grant_catalogue_health_service_execute(active.pair(), function)
            .await
            .map_err(|_| SecurityGrantError::GrantFailed)
            .map(|_| ())
    })
}

fn map_host_error(error: EmbeddedHostError) -> SecurityGrantError {
    match error {
        EmbeddedHostError::Io(source) if source.kind() == io::ErrorKind::NotFound => {
            SecurityGrantError::InstanceNotInstalled
        }
        _ => SecurityGrantError::InstanceInvalid,
    }
}

/// One closed `orna security` administrative operation (ADR 0065).
///
/// The operations match the sealed `sys.security.*` identities registered in
/// the core: the three session-identity reads, the grant listing, the
/// protected SERVER admin mutations, and the two checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledSecurityAdminOperation {
    /// `sys.security.session_principal()`.
    SessionPrincipal,
    /// `sys.security.effective_principal()`.
    EffectivePrincipal,
    /// `sys.security.active_roles()`.
    ActiveRoles,
    /// Lists direct and privilege-class grants for one principal.
    ListGrants {
        /// The principal whose grants are returned.
        grantee: PrincipalId,
    },
    /// `sys.security.create_principal(id, kind)`.
    CreatePrincipal {
        /// The new principal identity.
        principal: PrincipalId,
        /// The closed principal kind.
        kind: PrincipalKind,
    },
    /// `sys.security.disable_principal(id)`.
    DisablePrincipal {
        /// The principal to disable.
        principal: PrincipalId,
    },
    /// `sys.security.create_role(id)`.
    CreateRole {
        /// The new role identity.
        role: PrincipalId,
    },
    /// `sys.security.grant_role(role, member)`.
    GrantRole {
        /// The role identity.
        role: PrincipalId,
        /// The member principal.
        member: PrincipalId,
    },
    /// `sys.security.revoke_role(role, member)`.
    RevokeRole {
        /// The role identity.
        role: PrincipalId,
        /// The member principal.
        member: PrincipalId,
    },
    /// `sys.security.grant_privilege(grantee, class, object)`.
    GrantPrivilege {
        /// The grantee principal.
        grantee: PrincipalId,
        /// The closed privilege class.
        class: PrivilegeClass,
        /// The optional function object; `None` is a class-wide grant.
        object: Option<FunctionId>,
    },
    /// `sys.security.revoke_privilege(grantee, class, object)`.
    RevokePrivilege {
        /// The grantee principal.
        grantee: PrincipalId,
        /// The closed privilege class.
        class: PrivilegeClass,
        /// The optional function object; `None` is a class-wide grant.
        object: Option<FunctionId>,
    },
    /// `sys.security.can_execute(principal, function)`.
    CanExecute {
        /// The principal to decide.
        principal: PrincipalId,
        /// The function to test.
        function: FunctionId,
    },
    /// `sys.security.has_privilege(principal, class, object)`.
    HasPrivilege {
        /// The principal to decide.
        principal: PrincipalId,
        /// The requested closed privilege class.
        class: PrivilegeClass,
        /// The optional object; `None` is a class-wide request.
        object: Option<FunctionId>,
    },
}

/// One complete installed `orna security` command request (ADR 0065).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct InstalledSecurityAdminRequest {
    /// The closed administrative operation to run.
    pub operation: InstalledSecurityAdminOperation,
}

impl InstalledSecurityAdminRequest {
    /// Creates one complete installed security-admin command request.
    pub const fn new(operation: InstalledSecurityAdminOperation) -> Self {
        Self { operation }
    }

    /// Returns the closed operation to run.
    pub const fn operation(&self) -> InstalledSecurityAdminOperation {
        self.operation
    }
}

/// The terminal public result of one installed security-admin command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledSecurityAdminOutcome {
    /// The command completed and its record was rendered.
    Completed,
}

/// The closed failure class of one installed security-admin command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledSecurityAdminErrorKind {
    /// The command failed closed as a usage error.
    Usage,
    /// The protected operation failed closed with a security error: an
    /// admin-privilege denial, an unknown principal, or an invariant.
    Kernel,
    /// A rendered record could not reach standard output.
    Rendering,
    /// Host inspection, recovery, authentication, or another failure.
    Internal,
}

/// A failure that prevents or ends one installed security-admin command.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct InstalledSecurityAdminError {
    kind: InstalledSecurityAdminErrorKind,
    message: String,
    code: Option<&'static str>,
}

impl InstalledSecurityAdminError {
    /// Creates one closed security-admin failure with its message.
    pub fn new(kind: InstalledSecurityAdminErrorKind, message: String) -> Self {
        Self {
            kind,
            message,
            code: None,
        }
    }

    /// Creates one closed failure carrying a stable audit reason.
    pub fn with_code(
        kind: InstalledSecurityAdminErrorKind,
        message: String,
        code: &'static str,
    ) -> Self {
        Self {
            kind,
            message,
            code: Some(code),
        }
    }

    /// Returns the closed failure class.
    pub const fn kind(&self) -> InstalledSecurityAdminErrorKind {
        self.kind
    }

    /// Returns the stable closed audit reason.
    pub const fn code(&self) -> Option<&'static str> {
        self.code
    }

    /// Returns the closed failure message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for InstalledSecurityAdminError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "orna security: {}", self.message)
    }
}

impl std::error::Error for InstalledSecurityAdminError {}

/// Runs one local `orna security` command in-process.
///
/// The host inspection retains the local instance guards for the complete
/// authentication, operation, and rendering path. The kernel additionally
/// gates every mutation on the `SecurityAdmin` privilege. The result record is
/// written to `stdout` as one JSON line; failures are returned to the CLI.
///
/// # Errors
///
/// Returns [`InstalledSecurityAdminError`] for host inspection, recovery,
/// authentication, kernel, privilege, or rendering failures.
pub fn run_installed_security_admin(
    request: InstalledSecurityAdminRequest,
    stdout: &mut impl Write,
) -> Result<InstalledSecurityAdminOutcome, InstalledSecurityAdminError> {
    let host = inspect_current_embedded_host().map_err(map_admin_host_error)?;
    let kernel = PostgresKernel::new(host.config().clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            InstalledSecurityAdminError::new(
                InstalledSecurityAdminErrorKind::Internal,
                "the private runtime could not start".to_owned(),
            )
        })?;

    runtime.block_on(run_security_admin_with_kernel(kernel, request, stdout))
}

/// Runs one local `orna security` command against a caller-supplied
/// kernel (ADR 0065 live-proof seam).
///
/// The public entry [`run_installed_security_admin`] inspects the local
/// instance and delegates here; the live proof drives the exact
/// authenticate-operate-render path against the Compose PostgreSQL test
/// kernel with the invoking process's local peer credentials.
#[doc(hidden)]
pub async fn run_security_admin_with_kernel(
    kernel: PostgresKernel,
    request: InstalledSecurityAdminRequest,
    stdout: &mut impl Write,
) -> Result<InstalledSecurityAdminOutcome, InstalledSecurityAdminError> {
    let uid = nix::unistd::geteuid().as_raw();
    let session = kernel
        .authenticate_local_peer(uid)
        .await
        .map_err(map_kernel_admin_error)?;

    match request.operation() {
        InstalledSecurityAdminOperation::SessionPrincipal => {
            let principal = kernel.session_principal(&session);
            write_identity_line(stdout, "session_principal", principal)?;
        }
        InstalledSecurityAdminOperation::EffectivePrincipal => {
            let principal = kernel.effective_principal(&session);
            write_identity_line(stdout, "effective_principal", principal)?;
        }
        InstalledSecurityAdminOperation::ActiveRoles => {
            let roles = kernel.active_roles(&session);
            write_roles_line(stdout, &roles)?;
        }
        InstalledSecurityAdminOperation::ListGrants { grantee } => {
            let (execute_grants, privilege_grants) = kernel
                .list_grants(&session, grantee)
                .await
                .map_err(map_kernel_admin_error)?;
            write_grants_line(stdout, grantee, &execute_grants, &privilege_grants)?;
        }
        InstalledSecurityAdminOperation::CreatePrincipal { principal, kind } => {
            let snapshot = kernel
                .create_principal(&session, principal, kind)
                .await
                .map_err(map_kernel_admin_error)?;
            write_snapshot_line(stdout, "create_principal", &snapshot)?;
        }
        InstalledSecurityAdminOperation::DisablePrincipal { principal } => {
            let snapshot = kernel
                .disable_principal(&session, principal)
                .await
                .map_err(map_kernel_admin_error)?;
            write_snapshot_line(stdout, "disable_principal", &snapshot)?;
        }
        InstalledSecurityAdminOperation::CreateRole { role } => {
            let snapshot = kernel
                .create_role(&session, role)
                .await
                .map_err(map_kernel_admin_error)?;
            write_snapshot_line(stdout, "create_role", &snapshot)?;
        }
        InstalledSecurityAdminOperation::GrantRole { role, member } => {
            let snapshot = kernel
                .grant_role(&session, role, member)
                .await
                .map_err(map_kernel_admin_error)?;
            write_snapshot_line(stdout, "grant_role", &snapshot)?;
        }
        InstalledSecurityAdminOperation::RevokeRole { role, member } => {
            let snapshot = kernel
                .revoke_role(&session, role, member)
                .await
                .map_err(map_kernel_admin_error)?;
            write_snapshot_line(stdout, "revoke_role", &snapshot)?;
        }
        InstalledSecurityAdminOperation::GrantPrivilege {
            grantee,
            class,
            object,
        } => {
            let snapshot = kernel
                .grant_privilege(&session, grantee, class, object)
                .await
                .map_err(map_kernel_admin_error)?;
            write_snapshot_line(stdout, "grant_privilege", &snapshot)?;
        }
        InstalledSecurityAdminOperation::RevokePrivilege {
            grantee,
            class,
            object,
        } => {
            let snapshot = kernel
                .revoke_privilege(&session, grantee, class, object)
                .await
                .map_err(map_kernel_admin_error)?;
            write_snapshot_line(stdout, "revoke_privilege", &snapshot)?;
        }
        InstalledSecurityAdminOperation::CanExecute {
            principal,
            function,
        } => {
            let decision = kernel
                .can_execute(principal, function)
                .await
                .map_err(map_kernel_admin_error)?;
            write_execute_line(stdout, principal, function, decision)?;
        }
        InstalledSecurityAdminOperation::HasPrivilege {
            principal,
            class,
            object,
        } => {
            let decision = kernel
                .has_privilege(principal, class, object)
                .await
                .map_err(map_kernel_admin_error)?;
            write_has_privilege_line(stdout, principal, class, object, decision)?;
        }
    }
    Ok(InstalledSecurityAdminOutcome::Completed)
}

/// Writes one JSON line for an identity-returning operation.
fn write_identity_line(
    stdout: &mut impl Write,
    operation: &str,
    principal: PrincipalId,
) -> Result<(), InstalledSecurityAdminError> {
    let line = format!(
        "{{\"operation\":\"{operation}\",\"principal\":\"{}\"}}\n",
        principal.canonical()
    );
    write_admin_line(stdout, &line)
}

/// Writes one JSON line for a principal's direct and privilege-class grants.
fn write_grants_line(
    stdout: &mut impl Write,
    grantee: PrincipalId,
    execute_grants: &[ExecuteGrant],
    privilege_grants: &[PrivilegeGrant],
) -> Result<(), InstalledSecurityAdminError> {
    let grants = execute_grants
        .iter()
        .map(|grant| format!("{{\"function\":\"{}\"}}", grant.function().canonical()))
        .collect::<Vec<_>>()
        .join(",");
    let privileges = privilege_grants
        .iter()
        .map(|grant| {
            let object = grant
                .object()
                .map(|function| format!("\"{}\"", function.canonical()))
                .unwrap_or_else(|| "null".to_owned());
            format!("{{\"class\":\"{}\",\"object\":{object}}}", grant.class())
        })
        .collect::<Vec<_>>()
        .join(",");
    let line = format!(
        "{{\"operation\":\"list_grants\",\"principal\":\"{}\",\"grants\":[{grants}],\"privileges\":[{privileges}]}}\n",
        grantee.canonical(),
    );
    write_admin_line(stdout, &line)
}

/// Writes one JSON line for the active-roles operation.
fn write_roles_line(
    stdout: &mut impl Write,
    roles: &[PrincipalId],
) -> Result<(), InstalledSecurityAdminError> {
    let roles_list = roles
        .iter()
        .map(|principal| format!("\"{}\"", principal.canonical()))
        .collect::<Vec<_>>()
        .join(",");
    let line = format!("{{\"operation\":\"active_roles\",\"roles\":[{roles_list}]}}\n");
    write_admin_line(stdout, &line)
}

/// Writes one JSON line for an execute decision.
fn write_execute_line(
    stdout: &mut impl Write,
    principal: PrincipalId,
    function: FunctionId,
    decision: ExecuteDecision,
) -> Result<(), InstalledSecurityAdminError> {
    let (result, reason) = match decision {
        ExecuteDecision::Allowed(_) => (true, None),
        ExecuteDecision::Denied(denial) => {
            let reason = match denial {
                ExecuteDenial::InvalidSession => "execute:invalid-session",
                ExecuteDenial::UnknownFunction => "execute:unknown-function",
                ExecuteDenial::RevisionMismatch => "execute:revision-mismatch",
                ExecuteDenial::MissingExecuteGrant => "execute:missing-grant",
                ExecuteDenial::UnsupportedSecurityDefiner => "execute:unsupported-security-definer",
            };
            (false, Some(reason))
        }
    };
    let reason = reason
        .map(|value| format!("\"{value}\""))
        .unwrap_or_else(|| "null".to_owned());
    let line = format!(
        "{{\"operation\":\"can_execute\",\"principal\":\"{}\",\"function\":\"{}\",\"result\":{result},\"reason\":{reason}}}\n",
        principal.canonical(),
        function.canonical(),
    );
    write_admin_line(stdout, &line)
}

/// Writes one JSON line for a privilege decision.
fn write_has_privilege_line(
    stdout: &mut impl Write,
    principal: PrincipalId,
    class: PrivilegeClass,
    object: Option<FunctionId>,
    decision: PrivilegeDecision,
) -> Result<(), InstalledSecurityAdminError> {
    let (result, reason) = match decision {
        PrivilegeDecision::Allowed { .. } => (true, None),
        PrivilegeDecision::Denied(denial) => (false, Some(denial.audit_reason())),
    };
    let reason = reason
        .map(|value| format!("\"{value}\""))
        .unwrap_or_else(|| "null".to_owned());
    let object = object
        .map(|function| format!("\"{}\"", function.canonical()))
        .unwrap_or_else(|| "null".to_owned());
    let line = format!(
        "{{\"operation\":\"has_privilege\",\"principal\":\"{}\",\"class\":\"{class}\",\"object\":{object},\"result\":{result},\"reason\":{reason}}}\n",
        principal.canonical(),
    );
    write_admin_line(stdout, &line)
}

/// Writes one JSON line summarising a completed mutation snapshot.
fn write_snapshot_line(
    stdout: &mut impl Write,
    operation: &str,
    snapshot: &SecuritySnapshot,
) -> Result<(), InstalledSecurityAdminError> {
    let line = format!(
        "{{\"operation\":\"{operation}\",\"principals\":{},\"roles\":{},\"grants\":{},\"privileges\":{}}}\n",
        snapshot.principals().count(),
        snapshot.memberships().count(),
        snapshot.execute_grants().count(),
        snapshot.privilege_grants().count(),
    );
    write_admin_line(stdout, &line)
}

/// Writes one rendered line to the supplied writer.
fn write_admin_line(
    stdout: &mut impl Write,
    line: &str,
) -> Result<(), InstalledSecurityAdminError> {
    stdout.write_all(line.as_bytes()).map_err(|_| {
        InstalledSecurityAdminError::new(
            InstalledSecurityAdminErrorKind::Rendering,
            format!("the security-admin record could not be written: {line:?}"),
        )
    })
}

/// Maps one host inspection failure to the closed admin failure class.
fn map_admin_host_error(error: EmbeddedHostError) -> InstalledSecurityAdminError {
    InstalledSecurityAdminError::new(
        InstalledSecurityAdminErrorKind::Internal,
        format!("the local Orna instance could not be inspected: {error}"),
    )
}

/// Maps one kernel failure to the closed admin failure class.
fn map_kernel_admin_error(error: PostgresKernelError) -> InstalledSecurityAdminError {
    match error {
        PostgresKernelError::SecurityAdminDenied { reason } => {
            InstalledSecurityAdminError::with_code(
                InstalledSecurityAdminErrorKind::Kernel,
                format!(
                    "security administration was denied: {}",
                    reason.audit_reason()
                ),
                reason.audit_reason(),
            )
        }
        PostgresKernelError::LocalPeerAuthentication(error) => InstalledSecurityAdminError::new(
            InstalledSecurityAdminErrorKind::Internal,
            format!("the local peer could not authenticate: {error}"),
        ),
        other => InstalledSecurityAdminError::new(
            InstalledSecurityAdminErrorKind::Kernel,
            format!("the security operation failed: {other}"),
        ),
    }
}

/// Parses one closed privilege class from its canonical display string.
pub fn parse_privilege_class(value: &str) -> Option<PrivilegeClass> {
    match value {
        "execute" => Some(PrivilegeClass::Execute),
        "security_admin" => Some(PrivilegeClass::SecurityAdmin),
        "inspect:own-invocation" => Some(PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation)),
        "inspect:session-invocations" => Some(PrivilegeClass::Inspect(
            InspectPrivilege::SessionInvocations,
        )),
        "inspect:any-invocation" => Some(PrivilegeClass::Inspect(InspectPrivilege::AnyInvocation)),
        "inspect:values" => Some(PrivilegeClass::Inspect(InspectPrivilege::Values)),
        "inspect:source" => Some(PrivilegeClass::Inspect(InspectPrivilege::Source)),
        "inspect:security-details" => {
            Some(PrivilegeClass::Inspect(InspectPrivilege::SecurityDetails))
        }
        "inspect:runtime-internals" => {
            Some(PrivilegeClass::Inspect(InspectPrivilege::RuntimeInternals))
        }
        _ => None,
    }
}

#[cfg(test)]
mod stream_admin_tests {
    use super::*;
    use std::{
        path::{Path, PathBuf},
        process::Command,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use orna_core::{
        CatalogueRevisionId, PrincipalId, SourceRevisionId,
        revision::RevisionPair,
        security::{Principal, PrincipalKind, PrincipalStatus, SecuritySnapshot},
    };
    use orna_repository_v1::Repository;
    use orna_runtime_v1::{Component, ConsumerIdentity, RuntimeIdentity};

    const OWNER: PrincipalId = PrincipalId::from_bytes([0x31; 16]);
    const OTHER: PrincipalId = PrincipalId::from_bytes([0x32; 16]);
    const REVISION: RevisionPair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x33; 16]),
        CatalogueRevisionId::from_bytes([0x34; 16]),
    );

    fn run_git(path: &Path, arguments: &[&str]) {
        assert!(
            Command::new("git")
                .args(arguments)
                .current_dir(path)
                .status()
                .expect("git must run")
                .success()
        );
    }

    fn repository() -> (PathBuf, Repository) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "orna-stream-admin-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&path).expect("temporary repository directory");
        run_git(&path, &["init", "--quiet"]);
        run_git(&path, &["config", "user.email", "test@example.invalid"]);
        run_git(&path, &["config", "user.name", "test"]);
        run_git(&path, &["config", "commit.gpgsign", "false"]);
        let repository = Repository::discover(&path).expect("repository discovery");
        (path, repository)
    }

    fn session(principal: PrincipalId) -> AuthenticatedSession {
        SecuritySnapshot::new(
            REVISION,
            vec![],
            vec![Principal::new(
                principal,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )
        .expect("security snapshot")
        .bind_authenticated_session(principal, vec![])
        .expect("authenticated session")
    }

    fn stream(principal: PrincipalId) -> CheckpointKey {
        CheckpointKey {
            consumer: ConsumerIdentity {
                principal: Component::new(principal.canonical()).expect("principal component"),
                root: Component::new("root").expect("root component"),
                function: Component::new("callback").expect("function component"),
                binding: Component::new("binding").expect("binding component"),
            },
            source_format: Component::new("test").expect("source format"),
            source: Component::new("source").expect("source"),
            partition_format: Component::new("test").expect("partition format"),
            partition: Component::new("partition").expect("partition"),
            position_format: Component::new("test").expect("position format"),
        }
    }

    #[tokio::test]
    async fn authenticated_owner_reaches_durable_pause_and_resume() {
        let (path, repository) = repository();
        let state = RuntimeState::open(
            &repository,
            RuntimeIdentity {
                database_id: [0x41; 16],
                repository_id: [0x42; 16],
            },
            [0x43; 32],
        )
        .await
        .expect("runtime state");
        let writer = state.acquire_lease([0x44; 16]).await.expect("writer lease");
        let request = AuthenticatedStreamAdminRequest::Pause {
            stream: stream(OWNER),
            reason: None,
        };
        assert_eq!(
            run_authenticated_stream_admin(&state, writer, &session(OWNER), request).await,
            Ok(AuthenticatedStreamAdminOutcome::Paused { changed: true }),
        );
        assert_eq!(
            run_authenticated_stream_admin(
                &state,
                writer,
                &session(OWNER),
                AuthenticatedStreamAdminRequest::Resume {
                    stream: stream(OWNER),
                },
            )
            .await,
            Ok(AuthenticatedStreamAdminOutcome::Running { changed: true }),
        );
        drop(state);
        std::fs::remove_dir_all(path).expect("temporary repository cleanup");
    }

    #[tokio::test]
    async fn foreign_stream_is_rejected_and_pause_reason_is_retained() {
        let (path, repository) = repository();
        let state = RuntimeState::open(
            &repository,
            RuntimeIdentity {
                database_id: [0x51; 16],
                repository_id: [0x52; 16],
            },
            [0x53; 32],
        )
        .await
        .expect("runtime state");
        let writer = state.acquire_lease([0x54; 16]).await.expect("writer lease");
        assert_eq!(
            run_authenticated_stream_admin(
                &state,
                writer,
                &session(OWNER),
                AuthenticatedStreamAdminRequest::Pause {
                    stream: stream(OTHER),
                    reason: None,
                },
            )
            .await,
            Err(AuthenticatedStreamAdminError::OwnershipDenied),
        );
        let owned = stream(OWNER);
        assert_eq!(
            run_authenticated_stream_admin(
                &state,
                writer,
                &session(OWNER),
                AuthenticatedStreamAdminRequest::Pause {
                    stream: owned.clone(),
                    reason: Some("operator request".into()),
                },
            )
            .await,
            Ok(AuthenticatedStreamAdminOutcome::Paused { changed: true }),
        );
        assert_eq!(
            state.stream_pause_reason(&owned).await,
            Ok(Some("operator request".into())),
        );
        drop(state);
        std::fs::remove_dir_all(path).expect("temporary repository cleanup");
    }
}
