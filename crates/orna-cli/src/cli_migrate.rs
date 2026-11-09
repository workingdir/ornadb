//! `orna migrate` and `orna upgrade`: prepare a format-3 copy of a legacy
//! worktree and activate it in one confirmed step.

use super::*;

/// Names why the migration source failed format admission. The title carries
/// the `FormatContextError` message, so the cause is user-visible; the stable
/// code is kept in the detail.
#[allow(clippy::needless_pass_by_value)]
fn migration_source_admission_diagnostic(error: orna_repository::FormatContextError) -> Diagnostic {
    let title = format!("migration source could not be admitted: {error}");
    Diagnostic {
        code: "E2100",
        title: std::borrow::Cow::Owned(title),
        help: "verify the source worktree and retry with the same migration coordinates",
        exit: Exit::Target,
        detail: Some(format!("{} ({error:?})", error.code())),
        body: None,
    }
}

#[allow(clippy::too_many_lines)]
pub(super) fn run_migrate(endpoint: &Endpoint, arguments: &[String]) -> Result<(), Diagnostic> {
    let usage = || {
        Diagnostic::usage(
            "E1002",
            "unsupported `migrate` arguments",
            "use `migrate copy DEST [--annotation-defaults N] --no-persistent-consumers` or `migrate abort DEST`",
        )
    };
    let mut arguments = arguments.iter().map(String::as_str);
    let subcommand = arguments.next();
    if subcommand == Some("abort") {
        return run_migrate_abort(endpoint, arguments);
    }
    if subcommand != Some("copy") {
        return Err(usage());
    }
    let destination = arguments.next().ok_or_else(|| {
        Diagnostic::usage(
            "E1001",
            "`migrate copy` needs a destination",
            "supply DEST after `migrate copy`",
        )
    })?;
    let mut annotation_defaults = None;
    let mut no_persistent_consumers = false;
    while let Some(option) = arguments.next() {
        match option {
            "--annotation-defaults" if annotation_defaults.is_none() => {
                let value = arguments.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`--annotation-defaults` needs a count",
                        "supply the legacy metadata entry count after `--annotation-defaults`",
                    )
                })?;
                annotation_defaults = Some(value.parse::<u32>().map_err(|_| {
                    Diagnostic::usage(
                        "E1002",
                        "invalid `--annotation-defaults` count",
                        "supply a nonnegative integer",
                    )
                })?);
            }
            "--no-persistent-consumers" if !no_persistent_consumers => {
                no_persistent_consumers = true;
            }
            _ => return Err(usage()),
        }
    }
    if !no_persistent_consumers {
        return Err(Diagnostic::usage(
            "E1001",
            "`migrate copy` needs an explicit consumer-continuity choice",
            "supply `--no-persistent-consumers` to acknowledge that persistent consumer checkpoints are carried under recorded identity correspondence",
        ));
    }
    let source = std::path::Path::new(local_project_path(endpoint)?);
    let repository = orna_repository::Repository::discover(source).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "legacy migration source could not be opened",
            "run the command from a local format-1 or format-2 worktree",
            format!("{error:?}"),
        )
    })?;
    let prepared = prepare_migration_copy(
        &repository,
        std::path::Path::new(destination),
        annotation_defaults,
    )?;
    print_prepared_copy(&prepared);
    Ok(())
}

fn print_prepared_copy(prepared: &PreparedMigrationCopy) {
    let copy = &prepared.copy;
    println!(
        "prepared or recovered format-3 migration copy: source_format={} source_commit={} candidate_commit={} destination={} carried_checkpoints={} carried_history={}",
        copy.source_format_number(),
        copy.source_commit().as_str(),
        copy.candidate_journal().new_head().as_str(),
        copy.destination().display(),
        prepared.checkpoints,
        prepared.history,
    );
}

/// A prepared or recovered format-3 migration copy and the stream state carried
/// into it. The one-step upgrade publishes `copy` once its candidate is confirmed.
struct PreparedMigrationCopy {
    copy: orna_repository::migration_copy::MigratedCopy,
    checkpoints: usize,
    history: usize,
}

/// Prepares (or recovers) the format-3 migration copy of `repository` at
/// `destination`, carrying its persistent stream checkpoints. Nothing is
/// published here; activation is a separate step.
// A declared count is an assertion against the count the decoded legacy rows
// carry; an omitted one is the decoded count itself (ORNA-UPGRADE-004).
#[allow(clippy::too_many_lines)]
fn prepare_migration_copy(
    repository: &orna_repository::Repository,
    destination: &std::path::Path,
    annotation_defaults: Option<u32>,
) -> Result<PreparedMigrationCopy, Diagnostic> {
    let declared_annotation_defaults = annotation_defaults
        .map(orna_repository::MigrationAnnotationDefaults::new)
        .transpose()
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "migration annotation metadata was invalid",
                "supply the count derived from decoded legacy metadata, or omit the flag",
                format!("{error:?}"),
            )
        })?;
    // A source whose runtime holds unpublished mutations has rows the copy would
    // not receive, while its stream checkpoints may already cover them. Refuse
    // before any destination is created so the source keeps its tail (ORNA-UPGRADE-006).
    refuse_unpublished_tail(repository)?;
    // Persistent stream checkpoints are carried into the prepared destination
    // under the identity correspondence recorded there before any row is
    // imported. Live per-attempt stream state is not carried: a source holding
    // it refuses before a copy is built and its state is left unchanged.
    let source_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "migration source runtime could not be inspected",
                "retry `migrate copy` once the local runtime can start",
            )
        })?;
    let carry = source_runtime
        .block_on(orna_runtime::read_stream_carry(repository))
        .map_err(|error| match error {
            orna_runtime::StreamCarryError::NotCarriable(tables) => {
                Diagnostic::target_with_detail(
                    "E2100",
                    "migration source holds live per-attempt stream state",
                    "let the live stream attempts finish or be cancelled, then retry; no copy was prepared and the source is unchanged",
                    format!("tables={}", tables.join(", ")),
                )
            }
            orna_runtime::StreamCarryError::Runtime(error) => Diagnostic::target_with_detail(
                "E2100",
                "migration source stream state could not be read",
                "retry `migrate copy` once the source runtime can be read; no copy was prepared",
                format!("{error:?}"),
            ),
        })?;
    let continuity = carry.continuity_record().map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "migration consumer continuity could not be represented",
            "retry with the migration continuity inventory available",
            format!("{error:?}"),
        )
    })?;
    let copy = orna_repository::migration_copy::prepare_format3_migration(
        repository,
        destination,
        continuity,
        declared_annotation_defaults,
        &|staged: &orna_repository::Repository,
          database: orna_repository::DatabaseId|
         -> Result<
            orna_repository::migration_copy::MigrationStore,
            orna_repository::RepositoryError,
        > {
            let store = orna_application::build_migration_store(staged, database)?;
            // The declared count is an assertion: it must equal the Blob values the
            // decoded rows carry. A mismatch refuses before any candidate is built.
            if let Some(declared) = declared_annotation_defaults {
                let decoded = store.annotation_default_entries;
                if declared.entry_count() != decoded {
                    return Err(
                        orna_repository::RepositoryError::MigrationAnnotationDefaultsMismatch {
                            declared: declared.entry_count(),
                            decoded,
                        },
                    );
                }
            }
            // The store was built from a source that could still move. A source
            // that gained unpublished runtime mutations in that window keeps its
            // tail: refuse before the candidate is built or pinned.
            let unpublished = crate::source_unpublished_mutation_count(repository)
                .map_err(|_| orna_repository::RepositoryError::PublicationRecoveryConflict)?;
            if unpublished != 0 {
                return Err(orna_repository::RepositoryError::PublicationRecoveryConflict);
            }
            Ok(store)
        },
    )
    .map_err(|error| match error {
        orna_repository::RepositoryError::FormatContext(context) => {
            migration_source_admission_diagnostic(context)
        }
        orna_repository::RepositoryError::LegacyMigrationCopy => Diagnostic::target(
            "E2100",
            "prepared migration copy predates the migration-record removal",
            "discard it with `orna migrate abort DEST`, then run `orna migrate copy` again",
        ),
        orna_repository::RepositoryError::Format2CompactUnimplemented => Diagnostic::target(
            "E2100",
            "format-2 source holds compact-storage-v2 tables, which have no decoder yet",
            "see MIGRATION.md and bead ornadb-5il1.486; no copy was prepared and the source is unchanged",
        ),
        orna_repository::RepositoryError::MigrationAnnotationDefaultsMismatch { declared, decoded } => {
            Diagnostic::target_with_detail(
                "E2100",
                "declared annotation defaults do not match the decoded legacy rows",
                "remove `--annotation-defaults` to use the decoded count, or pass the decoded Blob count; no copy was prepared and the source is unchanged",
                format!("declared={declared} decoded={decoded}"),
            )
        }
        other => Diagnostic::target_with_detail(
            "E2100",
            "migration copy could not be prepared or recovered",
            "verify the source worktree and retry with the same migration coordinates",
            format!("{other:?}"),
        ),
    })?;
    let destination_repository = orna_repository::Repository::discover(copy.destination())
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "migration destination could not be opened",
                "verify the prepared destination and retry with the same migration coordinates",
                format!("{error:?}"),
            )
        })?;
    let recorded_journal = destination_repository
        .read_publication_journal()
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "migration destination journal could not be read",
                "verify the prepared destination and retry with the same migration coordinates",
                format!("{error:?}"),
            )
        })?;
    let recorded_continuity = recorded_journal
        .as_ref()
        .and_then(|journal| journal.migration_continuity());
    source_runtime
        .block_on(orna_runtime::import_stream_carry(
            &destination_repository,
            &carry,
            recorded_continuity,
        ))
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "migration stream checkpoints could not be carried",
                "verify the destination continuity record; a destination already holding different carried checkpoints is refused and no checkpoint was imported",
                format!("{error:?}"),
            )
        })?;
    // Boundary: the carry is committed in the destination runtime and the
    // command has not yet reported it. A restart re-imports the same carry,
    // which the import recognises as already present.
    orna_repository::publication_crash_barrier("migration-carried");
    Ok(PreparedMigrationCopy {
        copy,
        checkpoints: carry.checkpoint_count(),
        history: carry.history_count(),
    })
}

fn run_migrate_abort<'a>(
    endpoint: &Endpoint,
    mut arguments: impl Iterator<Item = &'a str>,
) -> Result<(), Diagnostic> {
    let destination = arguments.next().ok_or_else(|| {
        Diagnostic::usage(
            "E1001",
            "`migrate abort` needs a destination",
            "supply DEST after `migrate abort`",
        )
    })?;
    if arguments.next().is_some() {
        return Err(Diagnostic::usage(
            "E1002",
            "unsupported `migrate abort` arguments",
            "use `migrate abort DEST`",
        ));
    }
    let source = std::path::Path::new(local_project_path(endpoint)?);
    let repository = orna_repository::Repository::discover(source).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "legacy migration source could not be opened",
            "run the command from a local format-1 or format-2 worktree",
            format!("{error:?}"),
        )
    })?;
    let destination = std::path::PathBuf::from(destination);
    let outcome = orna_repository::migration_copy::discard_prepared_format3_migration(
        &repository,
        &destination,
    )
    .map_err(|error| match error {
        orna_repository::RepositoryError::MigrationCopyAdvanced => Diagnostic::target(
            "E2100",
            "migration copy has begun activation and cannot be discarded",
            "finish activation with `orna upgrade --to-format 3 DEST --yes-activate`; nothing was removed",
        ),
        other => Diagnostic::target_with_detail(
            "E2100",
            "migration copy could not be discarded",
            "verify that DEST is the prepared copy of this source; the source worktree is unchanged",
            format!("{other:?}"),
        ),
    })?;
    match outcome {
        orna_repository::migration_copy::DiscardOutcome::Discarded => println!(
            "discarded prepared format-3 migration copy: destination={}",
            destination.display()
        ),
        orna_repository::migration_copy::DiscardOutcome::NothingToDiscard => println!(
            "nothing to discard: no prepared format-3 migration copy at destination={}",
            destination.display()
        ),
        orna_repository::migration_copy::DiscardOutcome::AlreadyActivated => println!(
            "nothing discarded: destination is an activated format-3 repository, destination={}",
            destination.display()
        ),
    }
    Ok(())
}

/// `orna upgrade --to-format 3`, in two forms.
///
/// Without DEST, the one-step form prepares the candidate in a disposable copy
/// beside the worktree, prints its review, and publishes it only after explicit
/// confirmation of that candidate. Publication swaps the activated copy in for
/// the worktree path and leaves the old snapshot at the copy's path.
///
/// With DEST and `--yes-activate`, the two-step form publishes a copy that
/// `orna migrate copy DEST` already prepared. The source is never changed.
pub(super) fn run_upgrade(endpoint: &Endpoint, arguments: &[String]) -> Result<(), Diagnostic> {
    let arguments = parse_upgrade_arguments(arguments)?;
    match arguments.destination {
        Some(destination) => run_upgrade_prepared(endpoint, destination, arguments.confirm),
        None => run_upgrade_in_place(endpoint, &arguments),
    }
}

/// `orna upgrade --to-format 3 DEST --yes-activate [--confirm ID]`: activates the
/// prepared format-3 copy at DEST by publishing its candidate. Without a matching
/// `--confirm` the candidate id is still printed and a warning is written.
fn run_upgrade_prepared(
    endpoint: &Endpoint,
    destination: &str,
    confirm: Option<&str>,
) -> Result<(), Diagnostic> {
    let source = std::path::Path::new(local_project_path(endpoint)?);
    let repository = orna_repository::Repository::discover(source).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "upgrade source could not be opened",
            "run the command from the local format-1 or format-2 worktree that the copy was prepared from",
            format!("{error:?}"),
        )
    })?;
    let destination = std::path::PathBuf::from(destination);
    let candidate =
        orna_repository::migration_copy::prepared_candidate(&destination).map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "prepared migration copy could not be read for activation",
                "verify that DEST is the prepared copy of this source; nothing was published",
                format!("{error:?}"),
            )
        })?;
    match confirm {
        Some(id) if id != candidate.as_str() => {
            return Err(Diagnostic::target_with_detail(
                "E2100",
                "confirmation names a different candidate than the prepared copy",
                "re-run with the candidate_commit printed by the review; nothing was published",
                format!("confirmed={id} candidate={}", candidate.as_str()),
            ));
        }
        Some(_) => {}
        None => eprintln!(
            "warning: publishing candidate_commit={} without `--confirm {}`; a confirmed run binds activation to the reviewed candidate",
            candidate.as_str(),
            candidate.as_str(),
        ),
    }
    eprintln!(
        "reviewed prepared candidate: candidate_commit={} destination={}",
        candidate.as_str(),
        destination.display()
    );
    match activate_prepared(&repository, &destination, &candidate)? {
        orna_repository::migration_copy::ActivationOutcome::Activated => println!(
            "activated format-3 migration copy: destination={}",
            destination.display()
        ),
        orna_repository::migration_copy::ActivationOutcome::AlreadyActivated => println!(
            "already activated format-3 migration copy: destination={}",
            destination.display()
        ),
    }
    Ok(())
}

/// Publishes `candidate` at `destination`. Both forms of `orna upgrade` share it.
fn activate_prepared(
    repository: &orna_repository::Repository,
    destination: &std::path::Path,
    candidate: &orna_repository::GitCommitRef,
) -> Result<orna_repository::migration_copy::ActivationOutcome, Diagnostic> {
    let store_for = |staged: &orna_repository::Repository,
                     database: orna_repository::DatabaseId| {
        activation_store(repository, staged, database)
    };
    orna_repository::migration_copy::activate_prepared_format3_migration(
        repository,
        destination,
        candidate,
        crate::invocation_owner_id(),
        &store_for,
    )
    .map_err(|error| {
        if matches!(error, orna_repository::RepositoryError::LegacyPathChanged(_)) {
            // The path is the title so it shows without `--debug`: the edited
            // legacy file is the one thing the user must restore.
            let mut diagnostic = Diagnostic::target_with_detail(
                "E2300",
                "legacy file was edited after capture",
                "restore the legacy file named in the title to its committed bytes or move the edit aside, then re-run `orna upgrade --to-format 3 DEST --yes-activate`; the source worktree is unchanged",
                format!("{error:?}"),
            );
            diagnostic.title = std::borrow::Cow::Owned(error.to_string());
            return diagnostic;
        }
        Diagnostic::target_with_detail(
            "E2100",
            "prepared migration copy could not be activated",
            "verify that DEST is the prepared copy of this source and re-run the same upgrade to resume; the source worktree is unchanged",
            format!("{error:?}"),
        )
    })
}

/// `orna upgrade --to-format 3 [--annotation-defaults N] --no-persistent-consumers
/// [--confirm ID]`: the one-step form. It prepares the candidate in a sibling
/// `<worktree>-format3-candidate` copy, reviews it, and publishes it in place only
/// after the candidate is confirmed. A refused confirmation keeps the prepared copy
/// (abort it with `orna migrate abort`) and leaves the worktree untouched.
fn run_upgrade_in_place(
    endpoint: &Endpoint,
    arguments: &UpgradeArguments<'_>,
) -> Result<(), Diagnostic> {
    let source = std::path::Path::new(local_project_path(endpoint)?);
    let repository = orna_repository::Repository::discover(source).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "upgrade source could not be opened",
            "run the command from the local format-1 or format-2 worktree to upgrade",
            format!("{error:?}"),
        )
    })?;
    let root = repository.worktree().canonicalize().map_err(|_| {
        Diagnostic::target(
            "E2100",
            "upgrade source worktree could not be resolved",
            "run the command from the local format-1 or format-2 worktree to upgrade",
        )
    })?;
    let git_metadata = std::fs::symlink_metadata(root.join(".git")).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "upgrade source has no Git directory",
            "run the command from a standalone worktree whose `.git` is a directory; nothing was changed",
        )
    })?;
    if !git_metadata.is_dir() {
        return Err(Diagnostic::target(
            "E2100",
            "in-place upgrade needs a standalone worktree",
            "run it from the worktree whose `.git` is a directory; a linked worktree is refused and nothing was changed",
        ));
    }
    let (Some(parent), Some(name)) = (root.parent(), root.file_name()) else {
        return Err(Diagnostic::target(
            "E2100",
            "upgrade source worktree has no parent directory",
            "move the worktree under a directory the candidate copy can be created in; nothing was changed",
        ));
    };
    let destination = parent.join(format!("{}-format3-candidate", name.to_string_lossy()));
    if repository.open_format_context().is_ok_and(|context| {
        context.repository_format() == orna_repository::RepositoryFormat::Format3
    }) {
        return Err(Diagnostic::target(
            "E2100",
            "in-place upgrade already completed; nothing changed",
            "the worktree is already format-3; the sibling copy holds the previous snapshot; nothing was changed",
        ));
    }
    let activated = orna_repository::migration_copy::finished_activation_head(&destination)
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "prepared migration copy could not be inspected",
                "remove the sibling copy with `orna migrate abort` and re-run; the worktree is unchanged",
                format!("{error:?}"),
            )
        })?;
    // A finished activation whose exchange did not complete is resumed. Its
    // candidate is re-derived from the source in a verification copy, and only
    // a source that reproduces the activated candidate may publish it.
    let review_destination = if activated.is_some() {
        parent.join(format!("{}-format3-verify", name.to_string_lossy()))
    } else {
        destination.clone()
    };
    let prepared = prepare_migration_copy(
        &repository,
        &review_destination,
        arguments.annotation_defaults,
    )?;
    print_prepared_copy(&prepared);
    let candidate = prepared.copy.candidate_journal().new_head().clone();
    print_in_place_review(&root, &destination, activated.is_some());
    confirm_and_activate(
        &repository,
        &candidate,
        activated.as_ref(),
        &destination,
        &review_destination,
        arguments.confirm,
    )?;
    // The exchange is the last point the source can still move: refuse a tail
    // that appeared since the copy was prepared, leaving the worktree untouched.
    refuse_unpublished_tail(&repository)?;
    orna_repository::migration_copy::exchange_activated_copy(
        &repository,
        &prepared.copy,
        &destination,
    )
    .map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "activated candidate could not be published in place",
            "the worktree is unchanged because the exchange is atomic; the activated copy is kept at destination; re-run the same command to resume the exchange",
            format!("{error:?}"),
        )
    })?;
    println!(
        "published format-3 repository in place: worktree={} candidate_commit={} rollback_snapshot={}",
        root.display(),
        candidate.as_str(),
        destination.display()
    );
    Ok(())
}

/// Prints the resume note and the publication review: the candidate replaces
/// the worktree, and the old snapshot stays at the destination as the rollback
/// copy until it is removed.
fn print_in_place_review(root: &std::path::Path, destination: &std::path::Path, resumed: bool) {
    if resumed {
        println!(
            "resume: destination={} is already activated; the candidate re-derived from the source is verified against it before the exchange",
            destination.display()
        );
    }
    println!(
        "review: publication replaces worktree={} with the activated candidate; the old snapshot stays at destination={} as the rollback copy",
        root.display(),
        destination.display()
    );
    println!(
        "review: abort before publication with `orna migrate abort {}`; the worktree is unchanged until then",
        destination.display()
    );
}

/// Confirms the reviewed candidate and activates it in the sibling copy. A
/// resumed run already has an activated sibling, so after the confirmation it
/// only discards its verification copy.
fn confirm_and_activate(
    repository: &orna_repository::Repository,
    candidate: &orna_repository::GitCommitRef,
    activated: Option<&orna_repository::GitCommitRef>,
    destination: &std::path::Path,
    review_destination: &std::path::Path,
    confirm: Option<&str>,
) -> Result<(), Diagnostic> {
    let confirmed = match activated {
        Some(head) if head != candidate => Err(Diagnostic::target_with_detail(
            "E2100",
            "activated sibling was built from a different candidate",
            "the source changed since the sibling was activated; inspect and remove the sibling copy by hand, then re-run; the worktree is unchanged",
            format!(
                "activated={} candidate={}",
                head.as_str(),
                candidate.as_str()
            ),
        )),
        _ => confirm_candidate(confirm, candidate),
    };
    if let Err(error) = confirmed {
        if activated.is_some() {
            // The verification copy is not the activated sibling; removing it
            // leaves the sibling in place for the next run.
            let _ = discard_verification_copy(repository, review_destination);
        }
        return Err(error);
    }
    if activated.is_none() {
        refuse_unpublished_tail(repository)?;
        activate_prepared(repository, destination, candidate)?;
    } else {
        discard_verification_copy(repository, review_destination)?;
    }
    Ok(())
}

/// Confirms the reviewed candidate: `--confirm` must name it, or an interactive
/// terminal must type it back.
fn confirm_candidate(
    confirm: Option<&str>,
    candidate: &orna_repository::GitCommitRef,
) -> Result<(), Diagnostic> {
    match confirm {
        Some(id) if id != candidate.as_str() => Err(Diagnostic::target_with_detail(
            "E2100",
            "confirmation names a different candidate than the reviewed one",
            "re-run with the candidate_commit printed above; the worktree is unchanged and the sibling copy is kept",
            format!("confirmed={id} candidate={}", candidate.as_str()),
        )),
        Some(_) => Ok(()),
        None if confirm_interactively(candidate.as_str())? => Ok(()),
        None => Err(Diagnostic::target_with_detail(
            "E2100",
            "publication was not confirmed",
            "re-run with `--confirm <candidate_commit>` shown above, or confirm at an interactive terminal; the worktree is unchanged and the sibling copy is kept",
            format!("candidate={}", candidate.as_str()),
        )),
    }
}

/// Refuses while the source holds unpublished runtime mutations: its rows would
/// miss them while its stream checkpoints may already cover them (ORNA-UPGRADE-006).
fn refuse_unpublished_tail(repository: &orna_repository::Repository) -> Result<(), Diagnostic> {
    let unpublished_count = crate::source_unpublished_mutation_count(repository)?;
    if unpublished_count != 0 {
        return Err(Diagnostic::target_with_detail(
            "E2100",
            "migration source holds unpublished runtime mutations",
            "make the source's unpublished runtime mutations durable where its HEAD allows a publish, or discard them, then retry; the source is unchanged",
            format!("unpublished_mutations={unpublished_count}"),
        ));
    }
    Ok(())
}

/// Removes the verification copy a resumed run prepared; the activated sibling
/// and the worktree are not touched.
fn discard_verification_copy(
    repository: &orna_repository::Repository,
    verification: &std::path::Path,
) -> Result<(), Diagnostic> {
    let detail = match orna_repository::migration_copy::discard_prepared_format3_migration(
        repository,
        verification,
    ) {
        Ok(orna_repository::migration_copy::DiscardOutcome::Discarded) => return Ok(()),
        Ok(outcome) => format!("outcome={outcome:?}"),
        Err(error) => format!("{error:?}"),
    };
    Err(Diagnostic::target_with_detail(
        "E2100",
        "verification copy could not be removed",
        "remove the verification copy with `orna migrate abort` and re-run; the worktree and the activated sibling are unchanged",
        detail,
    ))
}

/// Asks for the candidate commit id on a terminal and reports whether it was
/// typed exactly. Without a terminal on both stdin and stderr nothing is asked.
fn confirm_interactively(candidate: &str) -> Result<bool, Diagnostic> {
    use std::io::{BufRead, IsTerminal, Write};
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return Ok(false);
    }
    let prompt_failed = || {
        Diagnostic::target(
            "E2100",
            "confirmation prompt could not be read",
            "re-run with `--confirm <candidate_commit>`; nothing was changed",
        )
    };
    eprint!("Type the candidate_commit above to publish in place, or press Enter to abort: ");
    std::io::stderr().flush().map_err(|_| prompt_failed())?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|_| prompt_failed())?;
    Ok(line.trim() == candidate)
}

/// The parsed `orna upgrade --to-format 3` arguments.
struct UpgradeArguments<'a> {
    destination: Option<&'a str>,
    confirm: Option<&'a str>,
    annotation_defaults: Option<u32>,
}

/// Parses both forms of `orna upgrade --to-format 3`. Every refusal happens
/// before the repository is opened, so nothing is changed.
fn parse_upgrade_arguments(arguments: &[String]) -> Result<UpgradeArguments<'_>, Diagnostic> {
    let mut words = arguments.iter().map(String::as_str);
    let mut format_three = false;
    let mut yes_activate = false;
    let mut no_persistent_consumers = false;
    let mut confirm = None;
    let mut annotation_defaults = None;
    let mut destination = None;
    while let Some(word) = words.next() {
        match word {
            "--to-format" if !format_three => {
                let format = words.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`--to-format` needs a format",
                        "supply `--to-format 3` after `upgrade`",
                    )
                })?;
                if format != "3" {
                    return Err(Diagnostic::usage(
                        "E1002",
                        "unsupported `--to-format` target",
                        "only `--to-format 3` is supported; nothing was changed",
                    ));
                }
                format_three = true;
            }
            "--yes-activate" if !yes_activate => yes_activate = true,
            "--no-persistent-consumers" if !no_persistent_consumers => {
                no_persistent_consumers = true;
            }
            "--confirm" if confirm.is_none() => {
                confirm = Some(words.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`--confirm` needs a candidate commit id",
                        "supply the candidate_commit printed by the review after `--confirm`",
                    )
                })?);
            }
            "--annotation-defaults" if annotation_defaults.is_none() => {
                let value = words.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`--annotation-defaults` needs a count",
                        "supply the legacy metadata entry count after `--annotation-defaults`",
                    )
                })?;
                annotation_defaults = Some(value.parse::<u32>().map_err(|_| {
                    Diagnostic::usage(
                        "E1002",
                        "invalid `--annotation-defaults` count",
                        "supply a nonnegative integer",
                    )
                })?);
            }
            _ if destination.is_none() && !word.starts_with("--") => destination = Some(word),
            _ => {
                return Err(Diagnostic::usage(
                    "E1002",
                    "unsupported `upgrade` arguments",
                    "use `upgrade --to-format 3 [--annotation-defaults N] --no-persistent-consumers [--confirm ID]`, or `upgrade --to-format 3 DEST --yes-activate [--confirm ID]`; nothing was changed",
                ));
            }
        }
    }
    if !format_three {
        return Err(Diagnostic::usage(
            "E1001",
            "`upgrade` needs `--to-format 3`",
            "supply `--to-format 3` after `upgrade`; nothing was changed",
        ));
    }
    check_upgrade_form(
        destination,
        yes_activate,
        annotation_defaults,
        no_persistent_consumers,
    )?;
    Ok(UpgradeArguments {
        destination,
        confirm,
        annotation_defaults,
    })
}

/// Refuses an `upgrade --to-format 3` argument shape that belongs to neither
/// form. Runs after the arguments are parsed and before the repository is
/// opened, so a refusal changes nothing.
fn check_upgrade_form(
    destination: Option<&str>,
    yes_activate: bool,
    annotation_defaults: Option<u32>,
    no_persistent_consumers: bool,
) -> Result<(), Diagnostic> {
    if destination.is_some() {
        if !yes_activate {
            return Err(Diagnostic::usage(
                "E1002",
                "`upgrade` activates the prepared copy only with `--yes-activate`",
                "re-run with `--yes-activate` to publish the prepared candidate into DEST; nothing was changed",
            ));
        }
        if annotation_defaults.is_some() || no_persistent_consumers {
            return Err(Diagnostic::usage(
                "E1002",
                "`--annotation-defaults` and `--no-persistent-consumers` belong to the preparation",
                "the prepared copy already records them; pass them to `orna migrate copy DEST` or to the one-step `orna upgrade --to-format 3`; nothing was changed",
            ));
        }
    } else {
        if yes_activate {
            return Err(Diagnostic::usage(
                "E1002",
                "`--yes-activate` needs a DEST",
                "the one-step form publishes after `--confirm` or an interactive confirmation; nothing was changed",
            ));
        }
        if !no_persistent_consumers {
            return Err(Diagnostic::usage(
                "E1001",
                "`upgrade --to-format 3` needs an explicit consumer-continuity choice",
                "supply `--no-persistent-consumers` to acknowledge that persistent consumer checkpoints are carried under recorded identity correspondence",
            ));
        }
    }
    Ok(())
}

/// The store a prepared copy must match: exactly what a fresh preparation of
/// `source` builds. The source may move after preparation, so this runs on
/// every re-derivation and refuses, before the copy's ref moves, while the
/// source holds unpublished runtime mutations.
fn activation_store(
    source: &orna_repository::Repository,
    staged: &orna_repository::Repository,
    database: orna_repository::DatabaseId,
) -> Result<orna_repository::migration_copy::MigrationStore, orna_repository::RepositoryError> {
    let store = orna_application::build_migration_store(staged, database)?;
    let unpublished = crate::source_unpublished_mutation_count(source)
        .map_err(|_| orna_repository::RepositoryError::PublicationRecoveryConflict)?;
    if unpublished != 0 {
        return Err(orna_repository::RepositoryError::PublicationRecoveryConflict);
    }
    Ok(store)
}
