//! Fixed-service security administration for the installed Orna instance.

use std::{fmt, io};

use orna_core::FunctionId;
use orna_postgres::PostgresKernel;

use orna_server::{EmbeddedHostError, inspect_ready_embedded_host};

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
