use super::*;

pub(crate) fn repository_init_diagnostic(
    error: &orna_repository::RepositoryInitError,
) -> Diagnostic {
    let code = error.code();
    let (title, help) = match error {
        orna_repository::RepositoryInitError::GitUnavailable => (
            "Git is not available",
            "install or enable Git, then retry `orna init`",
        ),
        orna_repository::RepositoryInitError::GitOperationFailed => (
            "Git could not initialize the repository",
            "check that Git can initialize the target directory, then retry `orna init`",
        ),
        orna_repository::RepositoryInitError::LocalStateUnavailable => (
            "local repository state is unavailable",
            "check local filesystem access for the target directory, then retry `orna init`",
        ),
        orna_repository::RepositoryInitError::UnsafeMetadataPath => (
            "repository metadata has an unsafe path",
            "replace unsafe metadata links or non-regular entries before retrying `orna init`",
        ),
        orna_repository::RepositoryInitError::MetadataIncomplete => (
            "repository metadata is incomplete",
            "restore complete metadata from a valid snapshot or initialize a new repository",
        ),
        orna_repository::RepositoryInitError::MetadataMalformed => (
            "repository metadata is invalid",
            "restore valid metadata from a compatible repository snapshot before retrying",
        ),
        orna_repository::RepositoryInitError::MetadataUnsupported => (
            "repository metadata format is unsupported",
            "use an Orna version that supports this repository format",
        ),
        orna_repository::RepositoryInitError::MetadataChanged => (
            "repository metadata changed during initialization",
            "ensure no other process changes repository metadata, then retry `orna init`",
        ),
        orna_repository::RepositoryInitError::RepositoryBusy => (
            "repository initialization is already in progress",
            "wait for the other initialization to finish, then retry `orna init`",
        ),
        orna_repository::RepositoryInitError::PlatformUnsupported => (
            "repository initialization is unsupported on this platform",
            "use a supported platform to initialize this repository",
        ),
    };
    Diagnostic::target(code, title, help)
}

pub(crate) fn initialize_repository(
    target: Option<&std::path::Path>,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let target = target.unwrap_or_else(|| std::path::Path::new("."));
    orna_repository::initialize_repository(target)
        .map_err(|error| repository_init_diagnostic(&error))?;
    write_success_line("initialized Orna repository", color_enabled)
}
/// Maps a fetch failure to its documented exit status: an unavailable remote is a
/// connection failure (3) and a malformed advertisement is a protocol failure (7);
/// every other fetch refusal keeps the target status.
pub(crate) fn fetch_failure(error: &orna_repository::FetchError) -> Diagnostic {
    let exit = match error {
        orna_repository::FetchError::RemoteUnavailable => Exit::Connection,
        orna_repository::FetchError::MalformedAdvertisement => Exit::Protocol,
        _ => Exit::Target,
    };
    Diagnostic {
        code: "E2100",
        title: Cow::Borrowed("Git fetch failed"),
        help: "check the configured remote and retry `fetch`",
        exit,
        detail: Some(error.to_string()),
        body: None,
    }
}

pub(crate) fn run_fetch(
    endpoint: &Endpoint,
    remote: &str,
    branch: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let repository = discover_worktree(
        endpoint,
        "run `fetch` inside a Git worktree or provide a local project path",
    )?;
    let requested = orna_repository::RequestedRef::branch(branch.to_owned()).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "fetch branch is invalid",
            "supply a valid Git branch name",
            error.to_string(),
        )
    })?;
    let report = repository
        .fetch_with_local_continuity(remote.to_owned(), [requested])
        .map_err(|error| fetch_failure(&error))?;
    if let Some(continuity) = report.continuity()
        && let Some(diagnostic) = fetch_continuity_diagnostic(continuity)
    {
        return Err(diagnostic);
    }
    let fetched = report.ordinary().first().ok_or_else(|| {
        Diagnostic::target(
            "E2100",
            "Git fetch returned no requested ref",
            "check the configured remote and retry `fetch`",
        )
    })?;
    let (state, state_color) = if fetched.updated() {
        ("updated", AnsiColor::Yellow)
    } else {
        ("unchanged", AnsiColor::Dim)
    };
    let mut stdout = io::stdout().lock();
    write_styled(&mut stdout, AnsiColor::Green, b"fetched ", color_enabled)
        .and_then(|()| {
            write_styled_display(
                &mut stdout,
                AnsiColor::Cyan,
                fetched.source(),
                color_enabled,
            )
        })
        .and_then(|()| stdout.write_all(b" -> "))
        .and_then(|()| {
            write_styled_display(
                &mut stdout,
                AnsiColor::Cyan,
                fetched.destination(),
                color_enabled,
            )
        })
        .and_then(|()| stdout.write_all(b" ("))
        .and_then(|()| write_styled_display(&mut stdout, state_color, state, color_enabled))
        .and_then(|()| stdout.write_all(b")\n"))
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "could not write command output",
                "check that stdout is available, then retry the command",
            )
        })?;
    Ok(())
}

/// Publishes the checked-out branch together with the continuity refs an
/// ordinary push synchronizes (`ORNA-GIT-008`).
///
/// The branch defaults to the branch `HEAD` is attached to, so pushing from a
/// detached `HEAD` is refused rather than guessing a name. A remote that is
/// missing a selected continuity ref, or names it with a different object ID,
/// is reported as a conflict before anything is transferred.
/// A pending publication is recovered first; an unresolved journal blocks push.
pub(crate) fn run_push(
    endpoint: &Endpoint,
    remote: &str,
    branch: Option<&str>,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let repository = discover_worktree(
        endpoint,
        "run `push` inside a Git worktree or provide a local project path",
    )?;
    let branch = match branch {
        Some(branch) => branch.to_owned(),
        None => repository
            .checked_out_branch()
            .map_err(|_| {
                Diagnostic::target(
                    "E2100",
                    "the checked-out branch could not be read",
                    "run `push REMOTE BRANCH` or check out a branch first",
                )
            })?
            .ok_or_else(|| {
                Diagnostic::target(
                    "E2100",
                    "`push` needs a branch when HEAD is detached",
                    "run `push REMOTE BRANCH` or check out a branch first",
                )
            })?,
    };
    if repository
        .read_publication_journal()
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2200",
                "local publication state could not be read",
                "recover or repair the pending local publication before retrying `push`",
                error.to_string(),
            )
        })?
        .is_some()
    {
        if !repository.runtime_paths().state_db().exists() {
            return Err(Diagnostic::target(
                "E2200",
                "pending local publication has no runtime state",
                "restore the repository's runtime state and recover the publication before `push`",
            ));
        }
        let (identity, initial_digest) = runtime_identity(&repository)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| {
                Diagnostic::target(
                    "E2200",
                    "local runtime publication recovery could not start",
                    "retry `push` after checking the local runtime state",
                )
            })?;
        let state = runtime
            .block_on(RuntimeState::open(&repository, identity, initial_digest))
            .map_err(|error| {
                Diagnostic::target_with_detail(
                    "E2200",
                    "local runtime publication state could not be opened",
                    "recover or repair the pending local publication before retrying `push`",
                    error.to_string(),
                )
            })?;
        cli_publish::recover_pending_publication(&runtime, &repository, &state)
            .map_err(push_publication_recovery_diagnostic)?;
    }
    repository
        .push_ordinary_with_local_continuity(remote, branch.clone())
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "Git push failed",
                "check the configured remote, its continuity refs, and retry `push`",
                error.to_string(),
            )
        })?;
    let mut stdout = io::stdout().lock();
    write_styled(&mut stdout, AnsiColor::Green, b"pushed ", color_enabled)
        .and_then(|()| write_styled_display(&mut stdout, AnsiColor::Cyan, remote, color_enabled))
        .and_then(|()| stdout.write_all(b" -> "))
        .and_then(|()| write_styled_display(&mut stdout, AnsiColor::Cyan, &branch, color_enabled))
        .and_then(|()| stdout.write_all(b"\n"))
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "could not write command output",
                "check that stdout is available, then retry the command",
            )
        })?;
    Ok(())
}

/// Renders a recovered-publication failure for `orna push`. The recovery
/// sequence is shared with `orna publish`; only the remedy text differs.
pub(crate) fn push_publication_recovery_diagnostic(
    failure: cli_publish::PublicationRecoveryFailure,
) -> Diagnostic {
    use cli_publish::PublicationRecoveryFailure;
    match failure {
        PublicationRecoveryFailure::Recover(error) => Diagnostic::target_with_detail(
            "E2200",
            "pending local publication could not be recovered",
            "resolve the publication recovery error before retrying `push`",
            error.to_string(),
        ),
        PublicationRecoveryFailure::Unreadable(error) => Diagnostic::target_with_detail(
            "E2200",
            "local publication state could not be verified",
            "verify the pending publication before retrying `push`",
            error.to_string(),
        ),
        PublicationRecoveryFailure::StillPending => Diagnostic::target(
            "E2200",
            "local publication recovery remains pending",
            "resolve the publication recovery before retrying `push`",
        ),
    }
}

pub(crate) fn fetch_continuity_diagnostic(
    continuity: orna_repository::RemoteContinuity,
) -> Option<Diagnostic> {
    if continuity.permits_continuity_claim() {
        return None;
    }
    Some(Diagnostic::target_with_detail(
        "E2100",
        "Orna continuity witness was rejected",
        "do not claim allocator or checkpoint continuity until required refs/orna witnesses match",
        format!("remote continuity state: {continuity:?}"),
    ))
}
