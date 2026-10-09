//! `orna publish [--message MESSAGE]`: freezes the durable runtime pending
//! tail and projects it through the format-3 publication boundary as exactly
//! one Git commit.
//!
//! Source execution only stages durable runtime mutations; it never advances
//! the repository by itself (ORNA-PUB-019). This verb is the explicit
//! publication boundary: it selects one contiguous committed mutation range,
//! encodes the frozen batch as immutable native objects, advances the captured
//! `HEAD` with a compare-and-set, reconciles the ordinary index so the
//! published batch is not staged as a reversal (ORNA-PUB-010, ORNA-PUB-011),
//! and only then consumes that frozen range from the runtime tail
//! (ORNA-PUB-008). Nothing outside the frozen batch is committed.
//!
//! The publication commit is a real Git commit in the backing repository, so
//! `orna history`, `orna export` and `orna copy` all see it through the same
//! object store as any other commit.

use orna_runtime_v1::{RuntimeState, TableMutation};
use orna_storage_v1::{LoosePath, RuntimePublicationCoordinator};

use super::*;

const USAGE: &str = "usage: orna publish [--message MESSAGE]";
const DEFAULT_MESSAGE: &str = "orna: publish runtime data";

fn publish_error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2300",
        title,
        "run `orna publish` inside a format-3 repository with durable runtime changes",
        detail.into(),
    )
}

/// Parses the optional `--message` value. Every other argument is refused, so
/// a misspelled flag never publishes under an unintended message.
fn parse_options(arguments: &[String]) -> Result<String, Diagnostic> {
    let mut message = None;
    let mut words = arguments.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "--message" => {
                let value = words
                    .next()
                    .ok_or_else(|| publish_error("--message needs a value", USAGE))?;
                if value.is_empty() {
                    return Err(publish_error("--message needs a value", USAGE));
                }
                message = Some(value.to_owned());
            }
            flag if flag.starts_with("--") => {
                return Err(publish_error(
                    "Unknown publish flag",
                    format!("got {flag:?}; accepted: --message"),
                ));
            }
            extra => {
                return Err(publish_error(
                    "Publish takes no positional arguments",
                    format!("unexpected {extra:?}; {USAGE}"),
                ));
            }
        }
    }
    Ok(message.unwrap_or_else(|| DEFAULT_MESSAGE.to_owned()))
}

/// The one managed path the loose projection gives one frozen mutation.
///
/// The table name is the storage root and a canonical text key is the row
/// component, so the projected path is the same path a reader of the published
/// snapshot resolves. A non-text key has no representable loose path and is
/// refused rather than silently aliased.
fn row_path(mutation: &TableMutation) -> Result<LoosePath, orna_storage_v1::Error> {
    let key =
        std::str::from_utf8(mutation.key()).map_err(|_| orna_storage_v1::Error::InvalidKey)?;
    LoosePath::for_key(mutation.table(), &[key.to_owned()])
}

/// Names an already-committed snapshot as the publication result for a frozen
/// range with no visible change. The commit is the current `HEAD`, so the
/// completion receipt never claims an object the range did not produce.
fn publication_commit_id(
    head: &orna_repository_v1::GitCommitRef,
) -> Result<orna_runtime_v1::PublicationCommitId, Diagnostic> {
    orna_runtime_v1::PublicationCommitId::new(head.as_str().as_bytes().to_vec()).map_err(|error| {
        publish_error(
            "publication commit could not be named",
            format!("{error:?}"),
        )
    })
}

pub(super) fn run(endpoint: &Endpoint, arguments: &[String]) -> Result<(), Diagnostic> {
    let message = parse_options(arguments)?;
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    // A repository that never opened a runtime has no durable tail at all.
    if !repository.runtime_paths().state_db().exists() {
        println!("nothing to publish: no durable runtime changes");
        return Ok(());
    }
    let (identity, initial_digest) = runtime_identity(&repository)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| publish_error("local runtime could not be started", "retry `orna publish`"))?;
    let state = runtime
        .block_on(RuntimeState::open(&repository, identity, initial_digest))
        .map_err(|error| {
            publish_error(
                "local runtime could not be opened",
                format!("{error:?}; retry `orna publish`"),
            )
        })?;
    let pending = runtime.block_on(state.pending_count()).map_err(|error| {
        publish_error(
            "pending runtime changes could not be read",
            format!("{error:?}"),
        )
    })?;
    if pending == 0 {
        println!("nothing to publish: no durable runtime changes");
        return Ok(());
    }
    let checkpoint = runtime
        .block_on(state.latest_checkpoint())
        .map_err(|error| {
            publish_error("runtime checkpoint could not be read", format!("{error:?}"))
        })?
        .ok_or_else(|| {
            publish_error(
                "pending runtime changes have no committed checkpoint",
                "commit the durable changes before publishing them",
            )
        })?;
    // Each publication names a durable intent. A fresh identity per run keeps
    // two invocations from sharing one frozen range (ORNA-PUB-006).
    let mut intent_id = [0_u8; 16];
    getrandom::fill(&mut intent_id)
        .map_err(|_| publish_error("publication intent could not be created", USAGE))?;

    let (commit, counts) = runtime.block_on(async {
        let freeze = state
            .freeze(intent_id, &checkpoint)
            .await
            .map_err(|error| {
                publish_error("runtime tail could not be frozen", format!("{error:?}"))
            })?;
        let frozen = state
            .pending_table_mutations_through(&freeze)
            .await
            .map_err(|error| {
                publish_error(
                    "frozen runtime range could not be read",
                    format!("{error:?}"),
                )
            })?;
        // The frozen range is the only content this call may commit: the
        // candidate is built from the captured base plus exactly these rows.
        let counts = table_row_counts(&frozen);
        let head = repository
            .head()
            .map_err(|error| {
                publish_error("committed snapshot could not be read", format!("{error:?}"))
            })?
            .ok_or_else(|| {
                publish_error(
                    "repository has no committed snapshot to publish onto",
                    "make an initial commit before publishing runtime data",
                )
            })?;
        let index = repository.index_generation().map_err(|error| {
            publish_error("ordinary index could not be read", format!("{error:?}"))
        })?;
        let prepared = RuntimePublicationCoordinator::prepare_from_runtime_allow_noop(
            &repository,
            &state,
            &head,
            index,
            &freeze,
            row_path,
            &message,
        )
        .await
        .map_err(|error| {
            describe_storage_failure("publication candidate could not be prepared", &error)
        })?;
        let Some(mut coordinator) = prepared else {
            // Every row in the frozen range already matches the committed
            // snapshot, so there is no visible change to publish. Consume the
            // frozen intent instead of failing on an empty commit: the save
            // that produced the range stays successful and the pending tail
            // does not grow without bound (ORNA-PUB-006).
            state
                .complete_publication(&freeze, &publication_commit_id(&head)?)
                .await
                .map_err(|error| {
                    publish_error(
                        "unpublishable runtime range could not be consumed",
                        format!("{error:?}"),
                    )
                })?;
            return Ok::<_, Diagnostic>(None);
        };
        coordinator.publish(&repository).map_err(|error| {
            describe_storage_failure("publication commit could not be advanced", &error)
        })?;
        coordinator
            .complete(&repository, &state, &freeze)
            .await
            .map_err(|error| {
                describe_storage_failure("published runtime range could not be consumed", &error)
            })?;
        let commit = repository
            .head()
            .map_err(|error| {
                publish_error("published snapshot could not be read", format!("{error:?}"))
            })?
            .ok_or_else(|| publish_error("publication left no committed snapshot", USAGE))?;
        Ok(Some((commit.as_str().to_owned(), counts)))
    })?;

    match commit {
        Some((commit, counts)) => {
            println!("published {commit}");
            for (table, rows) in counts {
                println!("  {table}  {rows} rows");
            }
        }
        /* A no-op range consumes the frozen range without creating a commit. */
        None => println!("nothing to publish: the runtime changes are already committed"),
    }
    Ok(())
}

/// Rows per table in the frozen batch, in table order. A mutation that was
/// folded away by an earlier mutation of the same key is already collapsed by
/// the lowering step, so this counts the frozen range's final mutations.
fn table_row_counts(mutations: &[TableMutation]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for mutation in mutations {
        *counts.entry(mutation.table().to_owned()).or_insert(0) += 1;
    }
    counts
}

/// ORNA-PUB-012: name the condition that paused publication and keep the
/// remedy actionable, without implying the pending tail was discarded.
fn describe_storage_failure(title: &'static str, error: &orna_storage_v1::Error) -> Diagnostic {
    use orna_storage_v1::Error as E;
    let detail = match error {
        E::ExternalConflict { path, .. } => format!(
            "managed path {} changed outside the publication lock; resolve it, then retry",
            path.as_managed_path().as_path().display()
        ),
        E::IndexConflict { path } => format!(
            "ordinary index disagrees with the captured base at {}; unstage that path, then retry",
            path.as_managed_path().as_path().display()
        ),
        E::ManagedWorktreeConflict => {
            "a managed path was edited after capture; keep or discard that edit, then retry"
                .to_owned()
        }
        E::RecoveryIndexConflict => {
            "index and worktree match neither recorded state; resolve the conflict before retrying"
                .to_owned()
        }
        E::PublicationPending => {
            "an earlier publication is still pending; run recovery before publishing again"
                .to_owned()
        }
        E::RefConflict => {
            "the selected snapshot moved during publication; retry on the new snapshot".to_owned()
        }
        E::RuntimeUnavailable => "local runtime state is unavailable".to_owned(),
        E::RepositoryUnavailable => "the backing repository is unavailable".to_owned(),
        other => format!("{other}"),
    };
    publish_error(title, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn message_defaults_and_is_refused_without_a_value() {
        assert_eq!(parse_options(&args(&[])).unwrap(), DEFAULT_MESSAGE);
        assert_eq!(
            parse_options(&args(&["--message", "Retitle the recording"])).unwrap(),
            "Retitle the recording"
        );
        assert!(parse_options(&args(&["--message"])).is_err());
        assert!(parse_options(&args(&["--message", ""])).is_err());
    }

    #[test]
    fn unknown_flags_and_positional_arguments_are_refused() {
        assert!(parse_options(&args(&["--force"])).is_err());
        assert!(parse_options(&args(&["extra"])).is_err());
        assert!(parse_options(&args(&["--message", "x", "extra"])).is_err());
    }

    #[test]
    fn frozen_batch_counts_rows_per_table_in_table_order() {
        let mutation = |id: u8, table: &str, key: &str| {
            TableMutation::new([id; 16], table, key.as_bytes().to_vec(), Some(vec![id]))
                .expect("a bounded table mutation")
        };
        let frozen = [
            mutation(1, "sensors.Reading", "a"),
            mutation(2, "warehouse.Event", "b"),
            mutation(3, "sensors.Reading", "c"),
        ];
        let counts = table_row_counts(&frozen);
        assert_eq!(
            counts.into_iter().collect::<Vec<_>>(),
            vec![
                ("sensors.Reading".to_owned(), 2),
                ("warehouse.Event".to_owned(), 1)
            ]
        );
    }
}
