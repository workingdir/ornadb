//! Embedded PostgreSQL host and administrative interfaces for OrnaDB.

mod backend_shell;
mod embedded;
mod inspect;
mod invoke;
mod local_auth;
mod raw_call;
mod raw_client_dispatch;
mod raw_socket;
pub mod security_admin;
mod source_apply;
mod source_diagnostics;
mod source_diff;
mod user_state;

pub use backend_shell::{BackendShellError, run_backend_shell};
pub use embedded::{
    EmbeddedEngineIdentity, EmbeddedHostError, EmbeddedHostPaths, EmbeddedPostmaster,
    MaterialisedSupport, ReadyEmbeddedHost, initialise_embedded_cluster,
    inspect_current_embedded_host, materialise_support_data, private_database_config,
    run_embedded_server, start_embedded_postmaster,
};
pub use inspect::{
    InstalledInspectError, InstalledInspectErrorKind, InstalledInspectOutcome,
    InstalledInspectProjection, InstalledInspectRequest, run_inspect_with_kernel,
    run_installed_inspect,
};
#[cfg(feature = "test-hooks")]
pub use invoke::RawResourceRequestAuthorizer;
pub use invoke::{
    InstalledClientResourceExecutor, InstalledInvokeError, InstalledInvokeErrorKind,
    InstalledInvokeOutcome, InstalledInvokeRequest, RuntimeFamily, run_installed_invoke,
    run_invoke_with_kernel,
};
pub use local_auth::{LocalAuthenticationError, authenticate_local_stream};
pub use raw_call::{
    LocalRawCallError, LocalRawCallOutcome, run_local_raw_call, run_local_raw_call_with_argument,
    run_local_raw_call_with_argument_pair,
};
pub use raw_client_dispatch::{RawClientDispatch, RawClientDispatchResult};
#[cfg(feature = "test-hooks")]
pub use raw_socket::serve_local_raw_stream_with_resource_authorizer;
pub use raw_socket::{
    LocalRawSocketError, LocalRawSocketResources, LocalRawSocketServer, LocalRawSocketServerError,
    serve_local_raw_stream, start_local_raw_socket,
};
pub use security_admin::{
    InstalledSecurityAdminError, InstalledSecurityAdminErrorKind, InstalledSecurityAdminOperation,
    InstalledSecurityAdminOutcome, InstalledSecurityAdminRequest, parse_privilege_class,
    run_installed_security_admin, run_security_admin_with_kernel,
};
pub use source_apply::{
    InstalledSourceApplyDiagnostics, InstalledSourceApplyError, InstalledSourceApplyHostFailure,
    InstalledSourceApplyOutcome, InstalledSourceApplySuccess, run_installed_source_apply,
};
pub use source_diff::{
    InstalledSourceDiffDiagnostics, InstalledSourceDiffError, InstalledSourceDiffHostFailure,
    InstalledSourceDiffOutcome, InstalledSourceDiffReport, run_installed_source_diff,
    run_source_diff_with_kernel,
};
pub use user_state::{
    AuthenticatedClientStateAdapter, AuthenticatedClientStateError, InstalledUserStateChange,
    InstalledUserStateError, InstalledUserStateErrorKind, InstalledUserStateExpectedType,
    InstalledUserStateInstance, InstalledUserStateOperation, InstalledUserStateOutcome,
    InstalledUserStateRequest, run_installed_user_state, run_user_state_with_kernel,
};

use std::fmt;

use orna_postgres::{PostgresKernel, PostgresKernelError};
use orna_standard::{StandardLibraryError, StandardUpgradeError};

/// A failure while opening a standard-backed application database.
#[derive(Debug)]
#[non_exhaustive]
pub enum OpenStandardDatabaseError {
    /// The private PostgreSQL kernel could not complete its operation.
    Kernel {
        /// The kernel failure.
        source: PostgresKernelError,
    },
    /// Standard-upgrade preparation could not produce an atomic upgrade.
    StandardUpgrade {
        /// The standard-upgrade preparation failure.
        source: StandardUpgradeError,
    },
    /// The required accepted standard library could not be verified as active.
    StandardLibrary {
        /// The standard-library failure.
        source: StandardLibraryError,
    },
}

impl fmt::Display for OpenStandardDatabaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel { source } => source.fmt(formatter),
            Self::StandardUpgrade { source } => source.fmt(formatter),
            Self::StandardLibrary { source } => source.fmt(formatter),
        }
    }
}

impl std::error::Error for OpenStandardDatabaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Kernel { source } => Some(source),
            Self::StandardUpgrade { source } => Some(source),
            Self::StandardLibrary { source } => Some(source),
        }
    }
}

fn retained_verified_standard_snapshot(
    revision: orna_core::StandardLibraryRevisionId,
) -> Result<orna_core::revision::VerifiedStandardLibrarySnapshot, StandardLibraryError> {
    match revision {
        revision if revision == orna_standard::STANDARD_LIBRARY_REVISION_ID => {
            orna_standard::retained_standard_library_snapshot()
                .and_then(orna_standard::verify_standard_library_snapshot)
        }
        revision if revision == orna_standard::STANDARD_LIBRARY_V2_REVISION_ID => {
            orna_standard::retained_standard_library_v2_snapshot()
                .and_then(orna_standard::verify_standard_library_v2_snapshot)
        }
        revision if revision == orna_standard::STANDARD_LIBRARY_V3_REVISION_ID => {
            orna_standard::retained_standard_library_v3_snapshot()
                .and_then(orna_standard::verify_standard_library_v3_snapshot)
        }
        revision if revision == orna_standard::STANDARD_LIBRARY_V4_REVISION_ID => {
            orna_standard::retained_standard_library_v4_snapshot()
                .and_then(orna_standard::verify_standard_library_v4_snapshot)
        }
        revision if revision == orna_standard::STANDARD_LIBRARY_V5_REVISION_ID => {
            orna_standard::retained_standard_library_v5_snapshot()
                .and_then(orna_standard::verify_standard_library_v5_snapshot)
        }
        revision if revision == orna_standard::STANDARD_LIBRARY_V6_REVISION_ID => {
            orna_standard::retained_standard_library_v6_snapshot()
                .and_then(orna_standard::verify_standard_library_v6_snapshot)
        }
        revision if revision == orna_standard::STANDARD_LIBRARY_V7_REVISION_ID => {
            orna_standard::retained_standard_library_v7_snapshot()
                .and_then(orna_standard::verify_standard_library_v7_snapshot)
        }
        revision if revision == orna_standard::STANDARD_LIBRARY_V8_REVISION_ID => {
            orna_standard::retained_standard_library_v8_snapshot()
                .and_then(orna_standard::verify_standard_library_v8_snapshot)
        }
        revision if revision == orna_standard::STANDARD_LIBRARY_V9_REVISION_ID => {
            orna_standard::retained_standard_library_v9_snapshot()
                .and_then(orna_standard::verify_standard_library_v9_snapshot)
        }
        _ => Err(StandardLibraryError::Unavailable),
    }
}

/// Applies the complete accepted standard chain to the latest retained
/// standard. The V1-to-V2 operation intentionally starts from the bare active
/// revision and atomically retains V1 with V2; each later child is prepared
/// from a recovered active parent.
async fn bootstrap_latest_standard(
    kernel: &PostgresKernel,
    active: orna_core::revision::ActiveDatabaseRevision,
) -> Result<orna_core::revision::VerifiedStandardLibrarySnapshot, OpenStandardDatabaseError> {
    let (active, _) = apply_standard_upgrade_step(
        kernel,
        &active,
        orna_standard::prepare_standard_upgrade_v1_to_v2,
    )
    .await?;
    let (active, _) = apply_standard_upgrade_step(
        kernel,
        &active,
        orna_standard::prepare_standard_upgrade_v2_to_v3,
    )
    .await?;
    let (active, _) = apply_standard_upgrade_step(
        kernel,
        &active,
        orna_standard::prepare_standard_upgrade_v3_to_v4,
    )
    .await?;
    let (active, _) = apply_standard_upgrade_step(
        kernel,
        &active,
        orna_standard::prepare_standard_upgrade_v4_to_v5,
    )
    .await?;
    let (active, _) = apply_standard_upgrade_step(
        kernel,
        &active,
        orna_standard::prepare_standard_upgrade_v5_to_v6,
    )
    .await?;
    let (active, _) = apply_standard_upgrade_step(
        kernel,
        &active,
        orna_standard::prepare_standard_upgrade_v6_to_v7,
    )
    .await?;
    let (active, _) = apply_standard_upgrade_step(
        kernel,
        &active,
        orna_standard::prepare_standard_upgrade_v7_to_v8,
    )
    .await?;
    let (_, expected) = apply_standard_upgrade_step(
        kernel,
        &active,
        orna_standard::prepare_standard_upgrade_v8_to_v9,
    )
    .await?;
    Ok(expected)
}

async fn continue_standard_to_v9(
    kernel: &PostgresKernel,
    mut active: orna_core::revision::ActiveDatabaseRevision,
    prepares: &[fn(
        &orna_core::revision::ActiveDatabaseRevision,
    ) -> Result<orna_standard::StandardUpgrade, StandardUpgradeError>],
) -> Result<orna_core::revision::VerifiedStandardLibrarySnapshot, OpenStandardDatabaseError> {
    let mut expected = None;
    for prepare in prepares {
        let (next_active, next_expected) =
            apply_standard_upgrade_step(kernel, &active, *prepare).await?;
        active = next_active;
        expected = Some(next_expected);
    }
    expected.ok_or_else(|| OpenStandardDatabaseError::StandardLibrary {
        source: StandardLibraryError::Unavailable,
    })
}

async fn apply_standard_upgrade_step(
    kernel: &PostgresKernel,
    active: &orna_core::revision::ActiveDatabaseRevision,
    prepare: fn(
        &orna_core::revision::ActiveDatabaseRevision,
    ) -> Result<orna_standard::StandardUpgrade, StandardUpgradeError>,
) -> Result<
    (
        orna_core::revision::ActiveDatabaseRevision,
        orna_core::revision::VerifiedStandardLibrarySnapshot,
    ),
    OpenStandardDatabaseError,
> {
    let upgrade =
        prepare(active).map_err(|source| OpenStandardDatabaseError::StandardUpgrade { source })?;
    let expected = upgrade.verified_standard_snapshot().clone();
    kernel
        .apply_standard_upgrade(&upgrade)
        .await
        .map_err(|source| OpenStandardDatabaseError::Kernel { source })?;
    let active = kernel
        .recover()
        .await
        .map_err(|source| OpenStandardDatabaseError::Kernel { source })?;
    Ok((active, expected))
}

/// Bootstraps and opens one database with the accepted standard library active.
///
/// The returned kernel has completed bare bootstrap and verified recovery. A
/// bare database, or an intermediate V2-V8 commit from an interrupted fresh
/// chain, is advanced through the complete accepted upgrade chain to V9 before
/// it returns; intentionally installed V1 and V9 snapshots are verified in
/// place.
pub async fn open_standard_database(
    kernel: PostgresKernel,
) -> Result<PostgresKernel, OpenStandardDatabaseError> {
    kernel
        .bootstrap()
        .await
        .map_err(|source| OpenStandardDatabaseError::Kernel { source })?;
    let active = kernel
        .recover()
        .await
        .map_err(|source| OpenStandardDatabaseError::Kernel { source })?;
    let installed_revision = active
        .catalogue_hash_context()
        .standard()
        .map(|standard| standard.revision());
    let expected = match installed_revision {
        None => bootstrap_latest_standard(&kernel, active).await?,
        Some(
            revision @ (orna_standard::STANDARD_LIBRARY_REVISION_ID
            | orna_standard::STANDARD_LIBRARY_V9_REVISION_ID),
        ) => retained_verified_standard_snapshot(revision)
            .map_err(|source| OpenStandardDatabaseError::StandardLibrary { source })?,
        Some(orna_standard::STANDARD_LIBRARY_V8_REVISION_ID) => {
            continue_standard_to_v9(
                &kernel,
                active,
                &[orna_standard::prepare_standard_upgrade_v8_to_v9],
            )
            .await?
        }
        Some(orna_standard::STANDARD_LIBRARY_V7_REVISION_ID) => {
            continue_standard_to_v9(
                &kernel,
                active,
                &[
                    orna_standard::prepare_standard_upgrade_v7_to_v8,
                    orna_standard::prepare_standard_upgrade_v8_to_v9,
                ],
            )
            .await?
        }
        Some(orna_standard::STANDARD_LIBRARY_V2_REVISION_ID) => {
            continue_standard_to_v9(
                &kernel,
                active,
                &[
                    orna_standard::prepare_standard_upgrade_v2_to_v3,
                    orna_standard::prepare_standard_upgrade_v3_to_v4,
                    orna_standard::prepare_standard_upgrade_v4_to_v5,
                    orna_standard::prepare_standard_upgrade_v5_to_v6,
                    orna_standard::prepare_standard_upgrade_v6_to_v7,
                    orna_standard::prepare_standard_upgrade_v7_to_v8,
                    orna_standard::prepare_standard_upgrade_v8_to_v9,
                ],
            )
            .await?
        }
        Some(orna_standard::STANDARD_LIBRARY_V3_REVISION_ID) => {
            continue_standard_to_v9(
                &kernel,
                active,
                &[
                    orna_standard::prepare_standard_upgrade_v3_to_v4,
                    orna_standard::prepare_standard_upgrade_v4_to_v5,
                    orna_standard::prepare_standard_upgrade_v5_to_v6,
                    orna_standard::prepare_standard_upgrade_v6_to_v7,
                    orna_standard::prepare_standard_upgrade_v7_to_v8,
                    orna_standard::prepare_standard_upgrade_v8_to_v9,
                ],
            )
            .await?
        }
        Some(orna_standard::STANDARD_LIBRARY_V4_REVISION_ID) => {
            continue_standard_to_v9(
                &kernel,
                active,
                &[
                    orna_standard::prepare_standard_upgrade_v4_to_v5,
                    orna_standard::prepare_standard_upgrade_v5_to_v6,
                    orna_standard::prepare_standard_upgrade_v6_to_v7,
                    orna_standard::prepare_standard_upgrade_v7_to_v8,
                    orna_standard::prepare_standard_upgrade_v8_to_v9,
                ],
            )
            .await?
        }
        Some(orna_standard::STANDARD_LIBRARY_V5_REVISION_ID) => {
            continue_standard_to_v9(
                &kernel,
                active,
                &[
                    orna_standard::prepare_standard_upgrade_v5_to_v6,
                    orna_standard::prepare_standard_upgrade_v6_to_v7,
                    orna_standard::prepare_standard_upgrade_v7_to_v8,
                    orna_standard::prepare_standard_upgrade_v8_to_v9,
                ],
            )
            .await?
        }
        Some(orna_standard::STANDARD_LIBRARY_V6_REVISION_ID) => {
            continue_standard_to_v9(
                &kernel,
                active,
                &[
                    orna_standard::prepare_standard_upgrade_v6_to_v7,
                    orna_standard::prepare_standard_upgrade_v7_to_v8,
                    orna_standard::prepare_standard_upgrade_v8_to_v9,
                ],
            )
            .await?
        }
        Some(revision) => retained_verified_standard_snapshot(revision)
            .map_err(|source| OpenStandardDatabaseError::StandardLibrary { source })?,
    };
    let active = kernel
        .recover()
        .await
        .map_err(|source| OpenStandardDatabaseError::Kernel { source })?;
    let Some(selected) = active.catalogue_hash_context().standard() else {
        return Err(OpenStandardDatabaseError::StandardLibrary {
            source: StandardLibraryError::Unavailable,
        });
    };
    if selected.revision() != expected.revision() {
        return Err(OpenStandardDatabaseError::StandardLibrary {
            source: StandardLibraryError::Unavailable,
        });
    }
    if selected.catalogue().revision() != expected.catalogue().revision() {
        return Err(OpenStandardDatabaseError::StandardLibrary {
            source: StandardLibraryError::CatalogueIdentityMismatch {
                expected: expected.catalogue().revision(),
                actual: selected.catalogue().revision(),
            },
        });
    }
    if selected.digest() != expected.digest() {
        return Err(OpenStandardDatabaseError::StandardLibrary {
            source: StandardLibraryError::AcceptedDigestMismatch {
                expected: expected.digest(),
                actual: selected.digest(),
            },
        });
    }
    Ok(kernel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_postgres::PostgresKernel;
    use std::future::Future;
    use tokio_postgres::Config;

    #[test]
    fn exposes_the_owned_standard_database_opening_interface() {
        let kernel = PostgresKernel::new(Config::new());
        let future = open_standard_database(kernel);
        let _: &dyn Future<Output = Result<PostgresKernel, OpenStandardDatabaseError>> = &future;
    }

    #[test]
    fn standard_database_open_errors_are_transparent() {
        let errors = [
            (
                OpenStandardDatabaseError::Kernel {
                    source: PostgresKernelError::CatalogueInvariant("host opener test"),
                },
                "private PostgreSQL catalogue invariant failed: host opener test",
            ),
            (
                OpenStandardDatabaseError::StandardUpgrade {
                    source: StandardUpgradeError::StandardLibrary {
                        source: StandardLibraryError::Unavailable,
                    },
                },
                "the standard library is not installed",
            ),
            (
                OpenStandardDatabaseError::StandardLibrary {
                    source: StandardLibraryError::Unavailable,
                },
                "the standard library is not installed",
            ),
        ];

        for (error, expected) in errors {
            assert_eq!(error.to_string(), expected);
            assert_eq!(
                std::error::Error::source(&error).map(ToString::to_string),
                Some(expected.to_owned())
            );
        }
    }
    #[test]
    fn retained_standard_dispatch_covers_each_accepted_revision() {
        let revisions = [
            orna_standard::STANDARD_LIBRARY_REVISION_ID,
            orna_standard::STANDARD_LIBRARY_V2_REVISION_ID,
            orna_standard::STANDARD_LIBRARY_V3_REVISION_ID,
            orna_standard::STANDARD_LIBRARY_V4_REVISION_ID,
            orna_standard::STANDARD_LIBRARY_V5_REVISION_ID,
            orna_standard::STANDARD_LIBRARY_V6_REVISION_ID,
            orna_standard::STANDARD_LIBRARY_V7_REVISION_ID,
            orna_standard::STANDARD_LIBRARY_V8_REVISION_ID,
            orna_standard::STANDARD_LIBRARY_V9_REVISION_ID,
        ];

        for revision in revisions {
            let verified = retained_verified_standard_snapshot(revision)
                .expect("every accepted standard revision has a retained verifier");
            assert_eq!(verified.revision(), revision);
        }

        let unknown = orna_core::StandardLibraryRevisionId::from_bytes([0xff; 16]);
        assert!(matches!(
            retained_verified_standard_snapshot(unknown),
            Err(StandardLibraryError::Unavailable)
        ));
    }
}
