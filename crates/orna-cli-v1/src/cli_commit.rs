//! Publishes the exact candidate admitted by an explicit review.

use super::*;

fn usage() -> Diagnostic {
    Diagnostic::usage(
        "E1002",
        "commit requires a message",
        "use `orna commit -m MESSAGE` to publish the reviewed index candidate",
    )
}

fn message(arguments: &[String]) -> Result<&str, Diagnostic> {
    if arguments.len() != 2 || !matches!(arguments[0].as_str(), "-m" | "--message") || arguments[1].is_empty() {
        return Err(usage());
    }
    Ok(&arguments[1])
}

/// Reports one review or commit failure with its own code and remedy
/// (16-administration failure classes, ORNA-UX-004). Failures without a
/// dedicated class keep the command's generic code with `generic_title`.
fn failure(error: &orna_repository_v1::RepositoryError, generic_title: &'static str) -> Diagnostic {
    use orna_repository_v1::RepositoryError;
    let (code, title, help) = match error {
        RepositoryError::InvalidCommitMessage => {
            return Diagnostic::usage(
                "E1002",
                "commit message is invalid",
                "use `orna commit -m MESSAGE` with a non-empty message",
            );
        }
        RepositoryError::MissingGitIdentity => (
            "E2301",
            "Git author identity is not configured",
            "set `user.name` and `user.email` with `git config`, then retry; no commit was created",
        ),
        // sys.admin.assertion_failed
        RepositoryError::InvalidCommitCandidate => (
            "E2302",
            "staged candidate is not a valid format-3 snapshot",
            "restore the staged `.orna` metadata, `.orna/store` and `main.orna`, then retry; no commit was created",
        ),
        // sys.git.stale_review
        RepositoryError::StaleReviewedCandidate | RepositoryError::StaleCwd => (
            "E2303",
            "staged candidate changed while it was being committed",
            "review the staged changes again, then retry `orna commit -m MESSAGE`",
        ),
        // sys.git.nothing_to_commit
        RepositoryError::SysGitNothingToCommit => (
            "E2304",
            "nothing is staged to commit",
            "stage changes with `orna add` or `git add`, then retry `orna commit -m MESSAGE`",
        ),
        _ => (
            "E2300",
            generic_title,
            "check the repository state, then retry `orna commit -m MESSAGE`",
        ),
    };
    Diagnostic::target_with_detail(code, title, help, format!("{error:?}"))
}

pub(super) fn run(endpoint: &Endpoint, arguments: &[String]) -> Result<(), Diagnostic> {
    let message = message(arguments)?;
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300",
            "Git worktree could not be discovered",
            "run `orna commit -m MESSAGE` inside a local Orna Git worktree",
            format!("{error:?}"),
        )
    })?;
    let (identity, initial_digest) = runtime_identity(&repository)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Diagnostic::target_with_detail(
            "E2300", "local runtime could not be started", "retry `orna commit -m MESSAGE`", format!("{error:?}"),
        ))?;
    let state = runtime.block_on(RuntimeState::open(&repository, identity, initial_digest)).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300", "local runtime could not be opened", "repair the local Orna runtime state before committing", format!("{error:?}"),
        )
    })?;
    // An interrupted publication is recovered before the index is reviewed:
    // recovery restores the exact state the review's intent is derived from,
    // and a stranded Orna index lock would otherwise block this commit.
    let pending = repository.read_publication_journal().map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300", "publication journal could not be read", "repair the local Orna publication state before committing", format!("{error:?}"),
        )
    })?;
    let reviewed = match pending.as_ref() {
        Some(journal) => repository.publication_journal_is_reviewed(journal).map_err(|error| {
            Diagnostic::target_with_detail(
                "E2300", "publication journal could not be classified", "repair the local Orna publication state before committing", format!("{error:?}"),
            )
        })?,
        None => false,
    };
    if reviewed {
        match repository.recover_publication() {
            Ok(_) | Err(orna_repository_v1::RepositoryError::RuntimeCompletionRequired) => {}
            Err(error) => {
                return Err(Diagnostic::target_with_detail(
                    "E2300", "pending reviewed commit could not be recovered", "repair the local Orna publication state and retry", format!("{error:?}"),
                ));
            }
        }
    } else {
        runtime
            .block_on(orna_storage_v1::RuntimePublicationCoordinator::recover(&repository, &state))
            .map_err(|error| {
                super::cli_publish::describe_storage_failure("pending publication could not be recovered", &error)
            })?;
    }
    let capture = runtime.block_on(state.capture()).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300", "runtime candidate could not be captured", "retry after the local runtime state is available", format!("{error:?}"),
        )
    })?;
    let review = repository
        .review_current_candidate(&capture)
        .map_err(|error| failure(&error, "staged candidate could not be reviewed"))?;
    let commit = repository
        .commit_reviewed_candidate(&review, message)
        .map_err(|error| failure(&error, "reviewed candidate could not be committed"))?;
    println!("[{}] {message}", commit.as_str());
    Ok(())
}
