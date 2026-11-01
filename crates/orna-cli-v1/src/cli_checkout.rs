//! Branch creation and checkout verbs (ORNA-BRANCH-001 to ORNA-BRANCH-003,
//! ORNA-CHECKOUT-001).

use std::borrow::Cow;

use super::assertion_gate::{GatedCommand, refusal_diagnostic};
use super::checkout_assertion_gate::CheckoutGate;
use super::cli_stage::repository;
use super::{Diagnostic, Endpoint, Exit};
use orna_repository_v1::{Repository, RepositoryError, RuntimeGeneration, RuntimeOwnerLock};
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};

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

/// `orna checkout NAME`. A target whose commit is HEAD's commit only changes the
/// attachment: Git keeps the index, worktree and CWD, so pending and staged
/// source state is preserved (ORNA-BRANCH-002). Any other commit runs through
/// [`checkout_different_commit`] (ORNA-BRANCH-003).
pub(super) fn run_checkout(endpoint: &Endpoint, name: &str) -> Result<(), Diagnostic> {
    let repo = repository(endpoint)?;
    let reading = read_checkout_runtime(&repo)?;
    let plan = repo
        .plan_checkout(name, reading.generation)
        .map_err(|error| checkout_diagnostic(&error))?;
    if plan.expected_head() == Some(plan.target().commit()) {
        return repo
            .execute_same_commit_checkout(&plan)
            .map_err(|error| checkout_diagnostic(&error));
    }
    checkout_different_commit(&repo, name)
}

/// Moves the checkout to a different commit under the CHECKOUT-3 fence. The
/// target is planned again while the fence is held, so the generation the plan
/// names is the one the install verifies. A pending draft, or an uncommitted
/// change on a path the target changes, is refused before any Git state moves:
/// carrying those forward is not built, and a refusal keeps the drafts and the
/// worktree (ORNA-CHECKOUT-001).
fn checkout_different_commit(repo: &Repository, name: &str) -> Result<(), Diagnostic> {
    let fence = CheckoutFence::acquire(repo)?;
    let plan = repo
        .plan_checkout(name, fence.reading.generation)
        .map_err(|error| checkout_diagnostic(&error))?;
    if plan.expected_head() == Some(plan.target().commit()) {
        return repo
            .execute_same_commit_checkout(&plan)
            .map_err(|error| checkout_diagnostic(&error));
    }
    if fence.reading.pending > 0 {
        return Err(pending_refusal(fence.reading.pending));
    }
    let conflicts: Vec<String> = plan
        .git()
        .conflicting_paths()
        .iter()
        .map(|path| path.as_path().display().to_string())
        .collect();
    if !conflicts.is_empty() {
        return Err(conflict_refusal(&conflicts.join(", ")));
    }
    repo.execute_nonconflicting_git_checkout(&plan, &CheckoutGate)
        .map_err(|error| checkout_diagnostic(&error))
}

/// The runtime owner lock held from the generation read until the checkout is
/// installed or refused. While it is held no other Orna process can admit a
/// write, so the plan is made against the generation that stays current
/// (CHECKOUT-3 step 2). The lock is released when the fence is dropped.
struct CheckoutFence {
    _owner: Option<RuntimeOwnerLock>,
    reading: RuntimeReading,
}

impl CheckoutFence {
    fn acquire(repo: &Repository) -> Result<Self, Diagnostic> {
        let Some((identity, digest)) = checkout_runtime_identity(repo)? else {
            return Ok(Self {
                _owner: None,
                reading: RuntimeReading::plain_git(),
            });
        };
        let owner = repo
            .acquire_runtime_owner_lock(super::invocation_owner_id())
            .map_err(|error| match error {
                RepositoryError::RepositoryBusy => runtime_in_use(),
                _ => checkout_runtime_unavailable(),
            })?;
        let reading = read_runtime(repo, identity, digest)?;
        Ok(Self {
            _owner: Some(owner),
            reading,
        })
    }
}

/// The CWD generation and the pending draft count that a checkout plan is made
/// against.
struct RuntimeReading {
    generation: RuntimeGeneration,
    pending: usize,
}

impl RuntimeReading {
    /// A plain Git repository has no runtime state: generation zero and no
    /// drafts. Its checkout is planned from Git state alone (ORNA-CLI-001).
    fn plain_git() -> Self {
        Self {
            generation: RuntimeGeneration::new(0),
            pending: 0,
        }
    }
}

/// Reads the checkout's runtime reading. A plain Git repository has no runtime
/// state, so it reads as the plain Git reading.
fn read_checkout_runtime(repo: &Repository) -> Result<RuntimeReading, Diagnostic> {
    match checkout_runtime_identity(repo)? {
        Some((identity, digest)) => read_runtime(repo, identity, digest),
        None => Ok(RuntimeReading::plain_git()),
    }
}

/// The runtime identity of a repository with Orna metadata. A plain Git
/// repository has none and maps to `None` (ORNA-CLI-001). Any other failure to
/// derive the identity keeps E2200.
fn checkout_runtime_identity(
    repo: &Repository,
) -> Result<Option<(RuntimeIdentity, [u8; 32])>, Diagnostic> {
    match super::runtime_identity(repo) {
        Ok(identity) => Ok(Some(identity)),
        Err(_) if is_plain_git_repository(repo) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Reads the CWD generation and the pending draft count from the local runtime.
fn read_runtime(
    repo: &Repository,
    identity: RuntimeIdentity,
    digest: [u8; 32],
) -> Result<RuntimeReading, Diagnostic> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| checkout_runtime_unavailable())?;
    let state = runtime
        .block_on(RuntimeState::open(repo, identity, digest))
        .map_err(|_| checkout_runtime_unavailable())?;
    let capture = runtime
        .block_on(state.capture())
        .map_err(|_| checkout_runtime_unavailable())?;
    let generation =
        u64::try_from(capture.generation()).map_err(|_| checkout_runtime_unavailable())?;
    let pending = runtime
        .block_on(state.pending_count())
        .map_err(|_| checkout_runtime_unavailable())?;
    Ok(RuntimeReading {
        generation: RuntimeGeneration::new(generation),
        pending,
    })
}

/// True for a repository with a known, writable HEAD format and no database
/// metadata. Any other failure to derive the runtime identity keeps E2200.
fn is_plain_git_repository(repo: &Repository) -> bool {
    repo.require_known_head_format().is_ok()
        && repo.require_writable_head_format().is_ok()
        && matches!(
            orna_repository_v1::inspect_metadata_for_runtime(repo),
            Ok(None)
        )
}

/// A different-commit checkout is refused while drafts are pending: carrying
/// them forward (CHECKOUT-3 step 4) is not built, so they would be left behind.
fn pending_refusal(pending: usize) -> Diagnostic {
    Diagnostic {
        code: "E2100",
        title: Cow::Owned(format!(
            "checkout to a different commit is refused while {pending} unpublished change(s) are pending"
        )),
        help: "publish the pending change(s) with `orna publish`, then retry `orna checkout NAME`; no change was made",
        exit: Exit::Target,
        detail: None,
        body: None,
    }
}

/// A different-commit checkout is refused when the target changes a path that
/// has uncommitted Git changes: the checkout would overwrite them.
fn conflict_refusal(paths: &str) -> Diagnostic {
    Diagnostic {
        code: "E2100",
        title: Cow::Owned(format!(
            "checkout would overwrite uncommitted changes to {paths}"
        )),
        help: "commit or revert those changes, then retry `orna checkout NAME`; no change was made",
        exit: Exit::Target,
        detail: None,
        body: None,
    }
}

fn runtime_in_use() -> Diagnostic {
    Diagnostic::target(
        "E2100",
        "local runtime is in use by another Orna process",
        "wait for the other Orna process to finish, then retry `orna checkout NAME`; no change was made",
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
        RepositoryError::CandidateAssertionRefused(refusal) => {
            refusal_diagnostic(refusal, GatedCommand::Checkout, error)
        }
        RepositoryError::CheckoutTargetInvalid => Diagnostic::target(
            "E2100",
            "checkout target could not be admitted",
            "the target commit's Orna format, schema or store is not admitted by this build; no change was made",
        ),
        RepositoryError::CheckoutExecutionUnsafe => Diagnostic::target(
            "E2100",
            "checkout refused: the working tree cannot be moved to the target safely",
            "no change was made; retry `orna checkout NAME`",
        ),
        other => Diagnostic::target_with_detail(
            "E2100",
            "checkout could not be completed",
            "check the repository state, then retry; no change was made",
            format!("{other:?}"),
        ),
    }
}
