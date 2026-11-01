//! Branch creation and checkout verbs (ORNA-BRANCH-001 to ORNA-BRANCH-003).

use super::cli_stage::repository;
use super::{Diagnostic, Endpoint};
use orna_repository_v1::RepositoryError;

/// `orna branch NAME`: creates a branch at HEAD. The ref is written with Git's
/// nonexistence CAS; the index, worktree and CWD are unchanged and no commit is
/// made (ORNA-BRANCH-001).
pub(super) fn run_branch_create(endpoint: &Endpoint, name: &str) -> Result<(), Diagnostic> {
    let repo = repository(endpoint)?;
    repo.create_branch_at_head(name)
        .map(|_| ())
        .map_err(|error| branch_create_diagnostic(name, &error))
}

fn branch_create_diagnostic(name: &str, error: &RepositoryError) -> Diagnostic {
    match error {
        RepositoryError::InvalidBranchName => Diagnostic::usage(
            "E1002",
            "invalid branch name",
            "use a name Git accepts, such as `orna branch feature-x`; HEAD and @ are reserved",
        ),
        RepositoryError::BranchExists => Diagnostic::target_with_detail(
            "E2100",
            "branch already exists",
            "choose another name, or switch to the existing branch with `orna checkout NAME`; no branch was created",
            format!("refs/heads/{name}"),
        ),
        RepositoryError::UnbornHead => Diagnostic::target(
            "E2100",
            "branch needs a commit to start from",
            "make a first commit on the current branch, then retry; no branch was created",
        ),
        RepositoryError::StaleHead => Diagnostic::target(
            "E2100",
            "HEAD moved while the branch was being created",
            "retry `orna branch NAME`; no branch was created",
        ),
        RepositoryError::RepositoryBusy => Diagnostic::target(
            "E2100",
            "repository coordination lock is busy",
            "wait for the active repository operation to finish, then retry; no branch was created",
        ),
        other => Diagnostic::target_with_detail(
            "E2100",
            "branch could not be created",
            "check the repository state, then retry; no branch was created",
            format!("{other:?}"),
        ),
    }
}
