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
    let capture = runtime.block_on(state.capture()).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300", "runtime candidate could not be captured", "retry after the local runtime state is available", format!("{error:?}"),
        )
    })?;
    let review = repository.review_current_candidate(&capture).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300", "staged candidate could not be reviewed", "resolve candidate errors and retry; no commit was created", format!("{error:?}"),
        )
    })?;
    let mut intent = [0_u8; 16];
    getrandom::fill(&mut intent).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300", "commit intent could not be created", "retry the reviewed commit", format!("{error:?}"),
        )
    })?;
    let commit = repository.commit_reviewed_candidate(
        &review,
        review.index_tree(),
        review.pending_generation(),
        intent,
        message,
    ).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300", "reviewed candidate could not be committed", "candidate state may have changed; review it again before retrying", format!("{error:?}"),
        )
    })?;
    println!("[{}] {message}", commit.as_str());
    Ok(())
}
