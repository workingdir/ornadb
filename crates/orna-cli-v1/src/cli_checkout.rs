//! Branch creation and same-commit checkout verbs (ORNA-BRANCH-001, ORNA-BRANCH-002).

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

/// `orna checkout NAME`: selects a branch or commit whose snapshot equals HEAD
/// (ORNA-BRANCH-002). Git keeps the index, worktree and CWD across an attachment
/// change between attachments of one commit, so pending and staged source state
/// is preserved. A different-commit target needs the CHECKOUT-3 plan and is
/// refused here before anything changes.
pub(super) fn run_checkout(endpoint: &Endpoint, name: &str) -> Result<(), Diagnostic> {
    let repo = repository(endpoint)?;
    let generation = runtime_generation(&repo)?;
    let plan = repo
        .plan_checkout(name, generation)
        .map_err(|error| checkout_diagnostic(&error))?;
    if plan.expected_head() != Some(plan.target().commit()) {
        return Err(Diagnostic::target(
            "E2100",
            "checkout to a different commit is not available yet",
            "the target commit differs from HEAD, so nothing changed; `orna checkout NAME` selects a branch or commit at the current HEAD",
        ));
    }
    repo.execute_same_commit_checkout(&plan)
        .map_err(|error| checkout_diagnostic(&error))
}

/// Captures the CWD generation that the checkout plan must match. The runtime
/// state is read, not changed; a plan captured under another generation is
/// refused as stale. A plain Git repository has no runtime identity, so it has
/// no runtime state to protect: its generation is zero and the checkout is
/// planned from Git state alone (ORNA-CLI-001, ORNA-BRANCH-002).
fn runtime_generation(
    repo: &orna_repository_v1::Repository,
) -> Result<orna_repository_v1::RuntimeGeneration, Diagnostic> {
    let (identity, initial_digest) = match super::runtime_identity(repo) {
        Ok(identity) => identity,
        Err(_) if is_plain_git_repository(repo) => {
            return Ok(orna_repository_v1::RuntimeGeneration::new(0));
        }
        Err(error) => return Err(error),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| checkout_runtime_unavailable())?;
    let state = runtime
        .block_on(orna_runtime_v1::RuntimeState::open(
            repo,
            identity,
            initial_digest,
        ))
        .map_err(|_| checkout_runtime_unavailable())?;
    let capture = runtime
        .block_on(state.capture())
        .map_err(|_| checkout_runtime_unavailable())?;
    let generation =
        u64::try_from(capture.generation()).map_err(|_| checkout_runtime_unavailable())?;
    Ok(orna_repository_v1::RuntimeGeneration::new(generation))
}

/// True for a repository with a known, writable HEAD format and no database
/// metadata. Any other failure to derive the runtime identity keeps E2200.
fn is_plain_git_repository(repo: &orna_repository_v1::Repository) -> bool {
    repo.require_known_head_format().is_ok()
        && repo.require_writable_head_format().is_ok()
        && matches!(
            orna_repository_v1::inspect_metadata_for_runtime(repo),
            Ok(None)
        )
}

fn checkout_runtime_unavailable() -> Diagnostic {
    Diagnostic::target(
        "E2200",
        "local runtime state needed by checkout could not be read",
        "retry after checking local runtime state; no change was made",
    )
}

fn checkout_diagnostic(error: &RepositoryError) -> Diagnostic {
    match error {
        RepositoryError::InvalidSelector => Diagnostic::usage(
            "E1002",
            "checkout target is not a branch or commit",
            "name an existing branch or commit, such as `orna checkout main`; no change was made",
        ),
        RepositoryError::RepositoryBusy => Diagnostic::target(
            "E2100",
            "repository coordination lock is busy",
            "wait for the active repository operation to finish, then retry; no change was made",
        ),
        RepositoryError::CheckoutPlanStale | RepositoryError::StaleCwd => Diagnostic::target(
            "E2100",
            "the checkout plan went stale before it ran",
            "retry `orna checkout NAME`; no change was made",
        ),
        RepositoryError::CheckoutExecutionUnsafe => Diagnostic::target(
            "E2100",
            "checkout refused: the target is not the current commit",
            "no change was made; checkout to a different commit is not available yet",
        ),
        other => Diagnostic::target_with_detail(
            "E2100",
            "checkout could not be completed",
            "check the repository state, then retry; no change was made",
            format!("{other:?}"),
        ),
    }
}
