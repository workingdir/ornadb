//! Offline embedded PostgreSQL upgrade command.

use std::fmt;

/// A failure while validating or running the default instance upgrade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EmbeddedUpgradeError {
    /// The caller does not have the exact Orna service identity.
    ServiceIdentity,
    /// Package maintenance has not committed the ready state.
    PackageIncomplete,
    /// The default managed instance is absent.
    InstanceNotInstalled,
    /// The installed instance or durable transition state is invalid.
    InvalidInstance,
    /// The default instance is running or retains its exclusive lock.
    InstanceRunning,
    /// This executable has no accepted forward edge from the installed engine.
    UnsupportedEngine,
    /// A linked role or durable transition failed after upgrade work began.
    UpgradeIncomplete,
}

impl fmt::Display for EmbeddedUpgradeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ServiceIdentity => "orna: server upgrade must run as the orna service account",
            Self::PackageIncomplete => "orna: package maintenance is incomplete",
            Self::InstanceNotInstalled => "orna: the default Orna instance is not installed",
            Self::InvalidInstance => "orna: the default Orna instance is invalid",
            Self::InstanceRunning => "orna: the default Orna instance is running",
            Self::UnsupportedEngine => {
                "orna: this Orna executable cannot upgrade the installed PostgreSQL engine"
            }
            Self::UpgradeIncomplete => "orna: PostgreSQL upgrade did not complete",
        })
    }
}

impl std::error::Error for EmbeddedUpgradeError {}

/// Validates and, when authorised, upgrades the stopped default instance.
pub fn run_embedded_upgrade() -> Result<(), EmbeddedUpgradeError> {
    crate::embedded::upgrade_default_instance()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_the_exact_upgrade_diagnostics() {
        let diagnostics = [
            (
                EmbeddedUpgradeError::ServiceIdentity,
                "orna: server upgrade must run as the orna service account",
            ),
            (
                EmbeddedUpgradeError::PackageIncomplete,
                "orna: package maintenance is incomplete",
            ),
            (
                EmbeddedUpgradeError::InstanceNotInstalled,
                "orna: the default Orna instance is not installed",
            ),
            (
                EmbeddedUpgradeError::InvalidInstance,
                "orna: the default Orna instance is invalid",
            ),
            (
                EmbeddedUpgradeError::InstanceRunning,
                "orna: the default Orna instance is running",
            ),
            (
                EmbeddedUpgradeError::UnsupportedEngine,
                "orna: this Orna executable cannot upgrade the installed PostgreSQL engine",
            ),
            (
                EmbeddedUpgradeError::UpgradeIncomplete,
                "orna: PostgreSQL upgrade did not complete",
            ),
        ];
        for (error, expected) in diagnostics {
            assert_eq!(error.to_string(), expected);
        }
    }
}
