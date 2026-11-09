//! The storage error taxonomy and the repository-error mappings that keep the
//! CLI-visible codes stable.

use std::{fmt, sync::Arc};

use orna_repository::{FormatContextError, GraphError, RepositoryError};

/// The underlying failure behind [`Error::RepositoryUnavailable`].
///
/// The error keeps rendering the stable "repository projection is unavailable"
/// text, so an existing caller's rendered code and exit status do not change.
/// The cause is reachable through [`std::error::Error::source`] instead. The
/// payload is erased behind `Arc`, so cloning shares the original cause chain
/// and equality compares the rendered causes.
#[derive(Clone, Debug)]
pub struct RepositoryUnavailableCause(Arc<dyn std::error::Error + Send + Sync>);

impl RepositoryUnavailableCause {
    fn new(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self(Arc::new(error))
    }

    fn as_error(&self) -> &(dyn std::error::Error + 'static) {
        &*self.0
    }
}

impl fmt::Display for RepositoryUnavailableCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&*self.0, f)
    }
}

impl PartialEq for RepositoryUnavailableCause {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_string() == other.0.to_string()
    }
}

impl Eq for RepositoryUnavailableCause {}

/// A refused projection shape that no lower layer reports as a failure.
#[derive(Clone, Debug)]
struct RepositoryUnavailableReason(String);

impl RepositoryUnavailableReason {
    fn new(reason: impl fmt::Display) -> Self {
        Self(reason.to_string())
    }
}

impl fmt::Display for RepositoryUnavailableReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RepositoryUnavailableReason {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidTableRoot,
    InvalidKey,
    UnsafePath,
    IncompleteStaging,
    InvalidObjectId,
    /// The publication target is a format-1/2 compatibility snapshot. It
    /// remains readable, but it has no row writer until it is migrated to
    /// format 3.
    LegacyReadOnly {
        format: u8,
    },
    RecoveryIndexConflict,
    RefConflict,
    ManagedWorktreeConflict,
    /// A legacy loose row file the migration retires was edited after capture.
    /// The file is kept on disk and named so the edit is not lost.
    LegacyPathChanged {
        path: String,
    },
    PublicationPending,
    InvalidPublicationJournal,
    InvalidTransition,
    RuntimeUnavailable,
    /// The repository projection could not be produced or read. The cause is
    /// kept on the source chain while the rendered message stays stable, so
    /// every distinct failure is diagnosable without changing an error code.
    RepositoryUnavailable(RepositoryUnavailableCause),
    /// Another owner holds the declared maintenance boundary (ORNA-CAP-006).
    RepositoryBusy,
    /// A publish step names a candidate or base object that Git no longer
    /// holds. The publish is refused before any ref or journal state changes
    /// (ORNA-CAP-006, ORNA-GIT-010).
    PublicationObjectUnavailable,
    RepositoryMalformed(orna_repository::FormatContextError),
}

impl Error {
    /// Wraps a lower-layer failure without changing the rendered message.
    pub(crate) fn repository_unavailable(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::RepositoryUnavailable(RepositoryUnavailableCause::new(error))
    }

    /// Names a projection shape refused at this boundary that has no lower
    /// error value to carry.
    pub(crate) fn repository_unavailable_reason(reason: impl fmt::Display) -> Self {
        Self::RepositoryUnavailable(RepositoryUnavailableCause::new(
            RepositoryUnavailableReason::new(reason),
        ))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidTableRoot => "invalid table root",
            Self::InvalidKey => "invalid loose row key",
            Self::UnsafePath => "unsafe loose row path",
            Self::IncompleteStaging => "incomplete publication staging",
            Self::InvalidObjectId => "invalid object id",
            Self::LegacyReadOnly { format } => {
                return write!(
                    f,
                    "repository format {format} is a read-only legacy snapshot; migrate it to format 3 before publishing"
                );
            }
            Self::RecoveryIndexConflict => "recovery index conflict",
            Self::RefConflict => "publication ref conflict",
            Self::ManagedWorktreeConflict => "managed worktree content conflict",
            Self::LegacyPathChanged { path } => {
                return write!(
                    f,
                    "legacy file {path} was edited after capture; the migration retires it and kept the edit on disk"
                );
            }
            Self::PublicationPending => "publication remains pending before ref advance",
            Self::InvalidPublicationJournal => "invalid publication journal",
            Self::InvalidTransition => "invalid publication transition",
            Self::RuntimeUnavailable => "runtime contract unavailable",
            Self::RepositoryUnavailable(_) => "repository projection is unavailable",
            Self::RepositoryBusy => "repository maintenance boundary is held by another owner",
            Self::PublicationObjectUnavailable => {
                "a publication object is no longer held by the repository"
            }
            Self::RepositoryMalformed(error) => {
                return write!(f, "repository projection is malformed: {error}");
            }
        })
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RepositoryUnavailable(cause) => Some(cause.as_error()),
            _ => None,
        }
    }
}

pub(crate) fn map_publication_repository_error(error: RepositoryError) -> Error {
    match error {
        RepositoryError::InvalidPublicationJournal | RepositoryError::InvalidCompactManifest => {
            Error::InvalidPublicationJournal
        }
        RepositoryError::StaleIndex { .. } => Error::RecoveryIndexConflict,
        RepositoryError::StaleHead => Error::RefConflict,
        RepositoryError::ManagedContentConflict => Error::ManagedWorktreeConflict,
        RepositoryError::LegacyPathChanged(path) => Error::LegacyPathChanged { path },
        RepositoryError::PublicationObjectUnavailable => Error::PublicationObjectUnavailable,
        RepositoryError::PublicationPending => Error::PublicationPending,
        RepositoryError::RuntimeCompletionRequired => Error::InvalidTransition,
        other => Error::repository_unavailable(other),
    }
}

/// Keeps a malformed native graph distinct from an unavailable one at the
/// storage boundary; every other format fault keeps its cause under
/// `RepositoryUnavailable`.
pub(crate) fn repository_format_error(error: FormatContextError) -> Error {
    match error {
        malformed @ FormatContextError::GraphMalformed(_) => Error::RepositoryMalformed(malformed),
        other => Error::repository_unavailable(other),
    }
}

/// Keeps a malformed native graph distinct from an unavailable one for the
/// graph operations a publication runs; every other graph fault, including
/// quota, cancellation and missing objects, keeps its cause under
/// `RepositoryUnavailable`.
pub(crate) fn graph_operation_error(error: GraphError) -> Error {
    if error.is_malformed() {
        Error::RepositoryMalformed(FormatContextError::GraphMalformed(error))
    } else {
        Error::repository_unavailable(error)
    }
}
