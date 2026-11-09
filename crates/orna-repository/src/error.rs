//! Extracted from `lib.rs` along its seams; contents are moved verbatim.

use super::*;

/// Safe public repository-boundary failures. Internal Git command lines,
/// worktree paths, and Git stderr are intentionally not exposed here.
#[derive(Debug)]
pub enum RepositoryError {
    GitUnavailable,
    GitOperationFailed,
    CandidateAssertionRefused(CandidateAssertionRefusal),
    LocalStateUnavailable,
    NotAWorktree,
    InvalidObjectId,
    InvalidInternalRef,
    UnsupportedObjectFormat,
    SnapshotNotFound,
    SnapshotNotReachable,
    InvalidSelector,
    InvalidBranchName,
    /// A branch of this name already exists; branch creation never moves one.
    BranchExists,
    InvalidCommitMessage,
    /// No author or committer identity is configured for a reviewed commit,
    /// and Git is not allowed to guess one.
    MissingGitIdentity,
    DetachedHead,
    InvalidPublicationJournal,
    /// A publish step names a candidate or base object that Git no longer
    /// holds. The publish is refused before any ref, index, or journal state
    /// changes, so the prepared candidate is not silently left unpinned
    /// (ORNA-CAP-006, ORNA-GIT-010).
    PublicationObjectUnavailable,
    InvalidFormatMigration,
    /// A prepared migration copy journals `.orna/migration.json`, the record
    /// copies prepared before ORNA-META-001 carried. It cannot be prepared,
    /// recovered, or published; discarding it is the only remedy.
    LegacyMigrationCopy,
    /// The prepared migration copy's publication journal has moved past the
    /// prepared stage. Activation began, so the copy is rolled forward by
    /// activation rather than discarded or aborted.
    MigrationCopyAdvanced,
    /// The declared annotation-default count is not the count the decoded
    /// legacy rows carry (ORNA-UPGRADE-004). The migration refuses before any
    /// copy is installed.
    MigrationAnnotationDefaultsMismatch {
        declared: u32,
        decoded: u32,
    },
    InvalidCompactManifest,
    UnsafeManagedPath,
    NoManagedPaths,
    SysGitNothingToCommit,
    UnbornHead,
    StaleIndex {
        expected: IndexGeneration,
        actual: IndexGeneration,
    },
    StaleCwd,
    StaleReviewedCandidate,
    /// The staged candidate violates the format-3 snapshot invariants a
    /// reviewed commit validates before creating its commit
    /// (`sys.admin.assertion_failed`).
    InvalidCommitCandidate,
    ManagedContentConflict,
    /// A legacy file the migration retires changed after capture. The file
    /// holds local edits that the migration would otherwise delete.
    LegacyPathChanged(String),
    /// Recovery found a pathname or private quarantine state that changed
    /// while it was being reconciled. The unexpected bytes remain on disk.
    PublicationRecoveryConflict,
    RepositoryBusy,
    GitIndexLockPresent,
    StaleHead,
    PublicationPending,
    CheckoutPlanStale,
    CheckoutDiscardSetMismatch,
    CheckoutExecutionUnsafe,
    CheckoutTargetInvalid,
    InvalidCheckoutJournal,
    CheckoutRecoveryRequired,
    RuntimeCompletionRequired,
    /// Native graph work failed while building or pinning a publication
    /// candidate. The graph context's own error identity is preserved.
    Graph(GraphError),
    /// Source format admission failed. The format context's own error
    /// identity is preserved so callers can report the exact cause.
    FormatContext(FormatContextError),
    /// This profile implements atomic index replacement only on POSIX
    /// filesystems. Windows requires a separately validated replacement path.
    PlatformUnsupported,
    /// A committed source tree exceeds the walk-size or depth limit before
    /// its entries are expanded.
    SourceTreeLimit,
    /// The migration store could not admit a legacy relation; the message
    /// names the refused row so callers can report it without a raw dump.
    MigrationStoreRefused(String),
    /// The format-2 source holds compact-storage-v2 tables. No format-2 compact
    /// decoder exists yet, so the migration refuses before any destination is
    /// created; see MIGRATION.md and bead ornadb-5il1.486.
    Format2CompactUnimplemented,
}

impl fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitUnavailable => f.write_str("Git is unavailable"),
            Self::CandidateAssertionRefused(_) => {
                f.write_str("candidate table assertions refused the commit")
            }
            Self::GitOperationFailed => f.write_str("Git repository operation failed"),
            Self::LocalStateUnavailable => f.write_str("local Orna state is unavailable"),
            Self::NotAWorktree => f.write_str("not inside a Git worktree"),
            Self::InvalidObjectId => f.write_str("invalid native Git object ID"),
            Self::InvalidInternalRef => f.write_str("invalid Orna internal Git ref"),
            Self::UnsupportedObjectFormat => f.write_str("unsupported Git object format"),
            Self::SnapshotNotFound => f.write_str("committed snapshot was not found"),
            Self::SnapshotNotReachable => {
                f.write_str("committed snapshot is not reachable from this repository")
            }
            Self::InvalidSelector => f.write_str("invalid Git snapshot selector"),
            Self::InvalidBranchName => f.write_str("invalid Git branch name"),
            Self::BranchExists => f.write_str("a Git branch with this name already exists"),
            Self::InvalidCommitMessage => f.write_str("invalid Git commit message"),
            Self::MissingGitIdentity => {
                f.write_str("no Git author or committer identity is configured")
            }
            Self::DetachedHead => f.write_str("publication requires a symbolic Git HEAD"),
            Self::InvalidPublicationJournal => f.write_str("invalid publication journal"),
            Self::PublicationObjectUnavailable => {
                f.write_str("a publication object is no longer held by the repository")
            }
            Self::InvalidFormatMigration => {
                f.write_str("invalid or unsupported repository format migration")
            }
            Self::MigrationAnnotationDefaultsMismatch { declared, decoded } => write!(
                f,
                "declared annotation defaults {declared} do not match the {decoded} Blob values the legacy rows carry"
            ),
            Self::LegacyMigrationCopy => f.write_str(
                "prepared migration copy carries the retired migration record; discard it and prepare again",
            ),
            Self::MigrationCopyAdvanced => f.write_str(
                "prepared migration copy has begun activation; activate it instead of discarding it",
            ),
            Self::InvalidCompactManifest => f.write_str("invalid compact storage manifest"),
            Self::UnsafeManagedPath => f.write_str("unsafe managed path"),
            Self::NoManagedPaths => f.write_str("at least one managed path is required"),
            Self::SysGitNothingToCommit => f.write_str("sys.git.nothing_to_commit"),
            Self::UnbornHead => f.write_str("cannot create a branch from an unborn HEAD"),
            Self::StaleIndex { .. } => {
                f.write_str("Git index changed since the expected generation")
            }
            Self::StaleCwd => f.write_str("Git CWD changed during observation"),
            Self::StaleReviewedCandidate => {
                f.write_str("reviewed commit candidate is stale or forged")
            }
            Self::InvalidCommitCandidate => {
                f.write_str("staged commit candidate is not a valid format-3 snapshot")
            }
            Self::ManagedContentConflict => {
                f.write_str("managed worktree content changed since capture")
            }
            Self::LegacyPathChanged(path) => {
                write!(f, "legacy file {path} changed since capture; it is not retired")
            }
            Self::PublicationRecoveryConflict => {
                f.write_str("publication recovery found unexpected local state")
            }
            Self::RepositoryBusy => {
                f.write_str("repository is busy with an active Git or Orna operation")
            }
            Self::GitIndexLockPresent => {
                f.write_str("Git index lock is present; resolve it with Git before retrying")
            }
            Self::StaleHead => f.write_str("Git HEAD changed during repository operation"),
            Self::PublicationPending => {
                f.write_str("publication remains pending before Git ref advancement")
            }
            Self::CheckoutPlanStale => f.write_str("checkout preflight is stale"),
            Self::CheckoutDiscardSetMismatch => {
                f.write_str("checkout discard set does not match the preflight")
            }
            Self::CheckoutExecutionUnsafe => {
                f.write_str("checkout target does not match the current commit")
            }
            Self::CheckoutTargetInvalid => {
                f.write_str("checkout target does not admit as a repository snapshot")
            }
            Self::InvalidCheckoutJournal => f.write_str("invalid checkout recovery journal"),
            Self::CheckoutRecoveryRequired => {
                f.write_str("checkout recovery requires explicit operator resolution")
            }
            Self::RuntimeCompletionRequired => {
                f.write_str("runtime publication completion is required")
            }
            Self::Graph(error) => write!(f, "native graph operation failed: {error}"),
            Self::FormatContext(error) => write!(f, "repository format context failed: {error}"),
            Self::PlatformUnsupported => {
                f.write_str("atomic Git index replacement is unsupported on this platform")
            }
            Self::SourceTreeLimit => f.write_str("source tree exceeds the walk limit"),
            Self::MigrationStoreRefused(detail) => {
                write!(f, "migration store refused a legacy row: {detail}")
            }
            Self::Format2CompactUnimplemented => f.write_str(
                "format-2 compact-storage-v2 tables have no decoder yet; see MIGRATION.md and bead ornadb-5il1.486",
            ),
        }
    }
}
impl std::error::Error for RepositoryError {}
