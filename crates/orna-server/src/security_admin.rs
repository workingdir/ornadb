//! Fixed-service security administration for the installed Orna instance.
//!
//! This module runs the closed `orna security` command family (ADR 0065).
//! The `grant-execute` entry keeps the fixed-service
//! [`run_installed_security_grant`] path; the administrative surface
//! ([`run_installed_security_admin`]) runs one closed operation against the
//! fixed private instance with the same host inspection and kernel access as
//! `orna inspect` and `orna state`, and mirrors the `InstalledInspectError`
//! failure and render path.
//!
//! The host derives the session from the authenticated local peer
//! ([`PostgresKernel::authenticate_local_peer`]); the kernel is authoritative
//! for the `SecurityAdmin`-privilege enforcement gate, and the host
//! additionally checks the exact installed service identity through
//! [`inspect_ready_embedded_host`] before dispatch, exactly like the
//! fixed-service grant path.

use std::{fmt, io, io::Write};

use orna_core::{
    FunctionId, PrincipalId,
    inspect::InspectPrivilege,
    security::{
        ExecuteDecision, ExecuteDenial, ExecuteGrant, PrincipalKind, PrivilegeClass,
        PrivilegeDecision, PrivilegeGrant, SecuritySnapshot,
    },
};
use orna_postgres::{PostgresKernel, PostgresKernelError};

use crate::{EmbeddedHostError, inspect_ready_embedded_host};

/// A closed failure from the fixed-service execution-grant command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityGrantError {
    /// The caller does not have the exact installed Orna service identity.
    ServiceAccountRequired,
    /// Package maintenance has not committed the ready state.
    PackageIncomplete,
    /// The default managed instance is absent.
    InstanceNotInstalled,
    /// The installed instance or its readiness evidence is invalid.
    InstanceInvalid,
    /// The running executable cannot verify the installed embedded engine.
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
            Self::ServiceAccountRequired => {
                "orna: security grant-execute must run as the orna service account"
            }
            Self::PackageIncomplete => "orna: package maintenance is incomplete",
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
/// The host inspection retains the package and instance guards for the complete
/// recovery and grant operation. The function identity has already been parsed
/// from the exact canonical command argument by the command parser.
pub fn run_installed_security_grant(function: FunctionId) -> Result<(), SecurityGrantError> {
    let host = inspect_ready_embedded_host().map_err(map_host_error)?;
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
        EmbeddedHostError::InvalidServiceIdentity => SecurityGrantError::ServiceAccountRequired,
        EmbeddedHostError::InvalidPackageState => SecurityGrantError::PackageIncomplete,
        EmbeddedHostError::Engine(_)
        | EmbeddedHostError::InvalidEngineManifest
        | EmbeddedHostError::InvalidDistributionManifest => SecurityGrantError::EngineInvalid,
        EmbeddedHostError::Io(ref source) if source.kind() == io::ErrorKind::NotFound => {
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

/// Runs one installed `orna security` command in-process.
///
/// The host inspection retains the package and instance guards for the
/// complete authentication, operation, and rendering path; the calling
/// process must hold the exact installed service identity, exactly like
/// the fixed-service grant path. The kernel additionally gates every
/// mutation on the `SecurityAdmin` privilege. The result record is
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
    let host = inspect_ready_embedded_host().map_err(map_admin_host_error)?;
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

/// Runs one installed `orna security` command against a caller-supplied
/// kernel (ADR 0065 live-proof seam).
///
/// The public entry [`run_installed_security_admin`] inspects the fixed
/// private instance and delegates here; the live proof drives the exact
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
    match error {
        EmbeddedHostError::InvalidServiceIdentity => InstalledSecurityAdminError::with_code(
            InstalledSecurityAdminErrorKind::Internal,
            "orna security must run as the exact installed Orna service account".to_owned(),
            "security_admin:missing-service-identity",
        ),
        EmbeddedHostError::InvalidPackageState => InstalledSecurityAdminError::new(
            InstalledSecurityAdminErrorKind::Internal,
            "package maintenance is incomplete".to_owned(),
        ),
        _ => InstalledSecurityAdminError::new(
            InstalledSecurityAdminErrorKind::Internal,
            format!("the installed Orna instance could not be inspected: {error}"),
        ),
    }
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
