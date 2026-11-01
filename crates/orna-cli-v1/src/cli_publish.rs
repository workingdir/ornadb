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
//!
//! Only format 3 has a row writer. A format-1/2 `HEAD` stays readable but is
//! refused before recovery, runtime access or any freeze (ORNA-UPGRADE-003).

use orna_runtime_v1::{RuntimeState, TableMutation};
use orna_storage_v1::RuntimePublicationCoordinator;
use orna_repository_v1::{CandidateAssertionGate, publication_crash_barrier};
use super::assertion_gate::{AssertionGate, GatedCommand, refusal_diagnostic};

use super::*;

const USAGE_HELP: &str = "use `orna publish [--message MESSAGE]`";
const DEFAULT_MESSAGE: &str = "orna: publish runtime data";

fn publish_error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2300",
        title,
        "run `orna publish` inside a format-3 repository with durable runtime changes",
        detail.into(),
    )
}

/// Parses the optional `--message` value. Every other argument is a usage error
/// (E1002) that names the offending word, so a misspelled flag never publishes
/// under an unintended message.
fn parse_options(arguments: &[String]) -> Result<String, Diagnostic> {
    let mut message = None;
    let mut words = arguments.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "--message" => {
                let value = words
                    .next()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| usage_error("`--message` needs a value".to_owned()))?;
                message = Some(value.to_owned());
            }
            flag if flag.starts_with("--") => {
                return Err(usage_error(format!("unknown publish flag `{flag}`")));
            }
            extra => {
                return Err(usage_error(format!(
                    "publish takes no positional arguments; got `{extra}`"
                )));
            }
        }
    }
    Ok(message.unwrap_or_else(|| DEFAULT_MESSAGE.to_owned()))
}

fn usage_error(title: String) -> Diagnostic {
    Diagnostic {
        code: "E1002",
        title: std::borrow::Cow::Owned(title),
        help: USAGE_HELP,
        exit: Exit::Usage,
        detail: None,
        body: None,
    }
}

/// Refuses a format-1/2 `HEAD` before recovery, runtime access or any freeze,
/// so a refused publication writes nothing: legacy snapshots keep their
/// versioned readers but have no row writer (ORNA-UPGRADE-003). Unreadable
/// metadata is not decided here; the format-3 validation of the frozen range
/// reports it. When `orna migrate copy` already prepared this worktree, the
/// refusal names that candidate instead of the upgrade work item.
fn refuse_legacy_head(repository: &orna_repository_v1::Repository) -> Result<(), Diagnostic> {
    let head = repository.head().map_err(|error| {
        publish_error("committed snapshot could not be read", format!("{error:?}"))
    })?;
    let Some(head) = head else {
        return Ok(());
    };
    let Err(orna_storage_v1::Error::LegacyReadOnly { format }) =
        orna_storage_v1::writable_snapshot_context(repository, &head)
    else {
        return Ok(());
    };
    Err(legacy_read_only_refusal(
        "publication target is a read-only legacy snapshot",
        format,
        repository.holds_prepared_migration(),
    ))
}

/// Validates format-3 mutations against the pinned graph and committed source
/// binding. Format-3 keys remain independent of host filename rules
/// (ORNA-KEY-004).
fn validate_candidate_source_rows(
    repository: &orna_repository_v1::Repository,
    head: &orna_repository_v1::GitCommitRef,
    mutations: &[TableMutation],
) -> Result<(), Diagnostic> {
    let loader = orna_project_v1::ProjectLoader::default();
    let format = repository
        .open_pinned_format_context(head.as_str())
        .map_err(|error| {
            publish_error(
                "repository format context could not be opened",
                format!("{error:?}; the pending range was preserved"),
            )
        })?;
    let repository_format = format.repository_format();
    if repository_format != orna_repository_v1::RepositoryFormat::Format3 {
        return Err(describe_storage_failure(
            "publication target is a read-only legacy snapshot",
            &orna_storage_v1::Error::LegacyReadOnly {
                format: repository_format.number(),
            },
        ));
    }
    // "A table publication generation is bound to its captured root and
    // schema" (PUB-3 step 4): `orna commit` admits a table's relation, and
    // publication never creates one. A row whose table has no relation in the
    // captured store stops here with that remedy.
    let committed_relations = repository
        .committed_store_relations(head)
        .map_err(|error| {
            publish_error(
                "format-3 committed store could not be read",
                format!("{error}; the pending range was preserved"),
            )
        })?;
    let mut mutation_ids = std::collections::BTreeSet::new();
    for mutation in mutations {
        if !mutation_ids.insert(mutation.id()) {
            return Err(publish_error(
                "source rows have duplicate mutation identities",
                "the frozen range cannot be mapped unambiguously to committed source keys; the pending range was preserved",
            ));
        }
        let table_id = mutation.table_object_id().ok_or_else(|| {
            publish_error(
                "source row could not be admitted for publication",
                "the frozen mutation did not retain its admitted TableObjectId; the pending range was preserved",
            )
        })?;
        if !committed_relations.contains(&table_id.bytes()) {
            return Err(Diagnostic::target_with_detail(
                "E2300",
                "source row table has no relation in the committed store",
                "stage the table's source and run `orna commit -m MESSAGE` to admit it, then retry `orna publish`",
                format!(
                    "table {:?} has object id {} but the committed .orna/store has no relation for it; the pending range was preserved",
                    mutation.table(),
                    orna_repository_v1::Uuid::from_bytes(table_id.bytes()).simple(),
                ),
            ));
        }
        let schema = loader
            .resolve_committed_source_table_schema(repository, head, table_id)
            .map_err(|error| {
                publish_error(
                    "source row schema could not be resolved",
                    format!("{error:?}; the pending range was preserved"),
                )
            })?;
        let binding = schema.binding();
        let snapshot_id = format.snapshot_pin().snapshot_id();
        let database_id = format.require_database_id().map_err(|error| {
            publish_error(
                "format-3 database identity could not be verified",
                format!("{error:?}; the pending range was preserved"),
            )
        })?;
        if schema.qualified_name() != mutation.table()
            || schema.table_object_id() != table_id
            || schema.source_commit() != head
            || schema.snapshot_id() != &snapshot_id
            || schema.database_id() != database_id.as_bytes()
            || binding.database_id() != database_id
            || binding.snapshot_id() != &snapshot_id
            || *binding.table_object_id() != table_id.bytes()
            || binding.declaration_name() != Some(schema.declaration_name())
            || binding.descriptor() != schema.descriptor()
        {
            return Err(publish_error(
                "source row table binding does not match its committed format-3 context",
                "table identity, descriptor, database or snapshot conflicts with the selected snapshot; the pending range was preserved",
            ));
        }
        let value_context = format.open_value_context(table_id.bytes()).map_err(|error| {
            publish_error(
                "format-3 row value context could not be opened",
                format!("{error:?}; the pending range was preserved"),
            )
        })?;
        let projected = orna_storage_v1::project_runtime_mutation_for_descriptor(
            binding.descriptor(),
            table_id.bytes(),
            mutation,
            &value_context,
        )
        .map_err(|error| {
            let title = if orna_storage_v1::canonicalize_runtime_key(
                binding.descriptor(),
                table_id.bytes(),
                mutation.key(),
            )
            .is_err()
            {
                "source row key is not canonical"
            } else {
                "source row does not match its committed schema"
            };
            publish_error(title, format!("{error:?}; the pending range was preserved"))
        })?;
        let key_bytes = projected.key;
        orna_repository_v1::TypedKey::decode_canonical(&key_bytes).map_err(|error| {
            publish_error(
                "source row key is not canonical",
                format!("{error:?}; the pending range was preserved"),
            )
        })?;
        match projected.value.as_deref() {
            Some(fields) => {
                binding
                    .descriptor()
                    .validate_stored_row(table_id.bytes(), &key_bytes, fields)
                    .map_err(|error| {
                        publish_error(
                            "source row does not match its committed schema",
                            format!("{error:?}; the pending range was preserved"),
                        )
                    })?;
                if let Some(new_key_bytes) = projected.rekey_to.as_deref() {
                    binding
                        .descriptor()
                        .validate_stored_row(table_id.bytes(), new_key_bytes, fields)
                        .map_err(|error| {
                            publish_error(
                                "source row rekey does not match its committed schema",
                                format!("{error:?}; the pending range was preserved"),
                            )
                        })?;
                }
            }
            None => {
                binding
                    .descriptor()
                    .validate_stored_key(table_id.bytes(), &key_bytes)
                    .map_err(|error| {
                        publish_error(
                            "source row key does not match its committed schema",
                            format!("{error:?}; the pending range was preserved"),
                        )
                    })?;
            }
        }
        let row_map = format.load_row_map(table_id.bytes()).map_err(|error| {
            publish_error(
                "format-3 row map could not be loaded",
                format!("{error:?}; the pending range was preserved"),
            )
        })?;
        let graph = format.open_native_graph(&row_map).map_err(|error| {
            publish_error(
                "format-3 native graph could not be opened",
                format!("{error:?}; the pending range was preserved"),
            )
        })?;
        let scope = graph.open_read_scope().map_err(|error| {
            publish_error(
                "format-3 native graph read scope could not be opened",
                format!("{error:?}; the pending range was preserved"),
            )
        })?;
        let committed_descriptor = graph.read_schema_descriptor(&scope).map_err(|error| {
            publish_error(
                "format-3 committed table binding could not be read",
                format!("{error:?}; the pending range was preserved"),
            )
        })?;
        if &committed_descriptor != binding.descriptor() {
            return Err(publish_error(
                "source row table binding does not match its committed format-3 context",
                "the native graph descriptor conflicts with the source binding; the pending range was preserved",
            ));
        }
    }
    Ok(())
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
    refuse_legacy_head(&repository)?;
    let journal = repository.read_publication_journal().map_err(|error| {
        publish_error(
            "publication recovery journal could not be read",
            format!("{error:?}; runtime state was not modified"),
        )
    })?;
    if !repository.runtime_paths().state_db().exists() {
        if journal.is_some() {
            return Err(publish_error(
                "publication recovery requires local runtime state",
                "a publication journal is present but its runtime state database is missing; restore the database before retrying. The journal was preserved",
            ));
        }
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
            let detail = if journal.is_some() {
                format!("{error:?}; the unresolved publication journal was not changed and no new range was selected. Repair the runtime state, then retry `orna publish`")
            } else {
                format!("{error:?}; retry `orna publish`")
            };
            publish_error("local runtime could not be opened", detail)
        })?;
    // Reconcile an interrupted publication before reading or freezing the tail.
    runtime
        .block_on(RuntimePublicationCoordinator::recover(&repository, &state))
        .map_err(|error| {
            describe_storage_failure("pending publication could not be recovered", &error)
        })?;
    // Never select another range if recovery returned with a journal still
    // present, including when a new publication raced with recovery.
    if repository
        .read_publication_journal()
        .map_err(|error| {
            publish_error(
                "publication recovery journal could not be read",
                format!("{error:?}; no new publication range was selected"),
            )
        })?
        .is_some()
    {
        return Err(publish_error(
            "publication recovery remains pending",
            "the repository journal is still present; the journal and pending tail were preserved; no new range was selected",
        ));
    }
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
        .map_err(|_| publish_error("publication intent could not be created", USAGE_HELP))?;

    let commit = runtime.block_on(async {
        let freeze = state
            .freeze(intent_id, &checkpoint)
            .await
            .map_err(|error| {
                publish_error("runtime tail could not be frozen", format!("{error:?}"))
            })?;
        publication_crash_barrier("capture");
        let frozen = state
            .pending_table_mutations_through(&freeze)
            .await
            .map_err(|error| {
                publish_error(
                    "frozen runtime range could not be read",
                    format!("{error:?}"),
                )
            })?;
        publication_crash_barrier("row");
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
        validate_candidate_source_rows(&repository, &head, &frozen)?;
        // The frozen range is the only content this call may commit: the
        // candidate is built from the captured base plus exactly these rows.
        let counts = table_row_counts(&frozen);
        let index = repository.index_generation().map_err(|error| {
            publish_error("ordinary index could not be read", format!("{error:?}"))
        })?;
        let prepared = RuntimePublicationCoordinator::prepare_allow_noop(
            &repository,
            &state,
            &head,
            index,
            &freeze,
            // Format-3 rows have no loose path, so the coordinator never asks.
            |_| Err(orna_storage_v1::Error::InvalidKey),
            &message,
        )
        .await
        .map_err(|error| {
            describe_storage_failure("publication candidate could not be prepared", &error)
        })?;
        publication_crash_barrier("candidate");
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
            return Ok::<_, Diagnostic>(Publication::NoVisibleChange(counts));
        };
        // ORNA-ASSERT-045: the candidate's assertions run before any ref, pin or
        // journal names it, so a refused publication leaves no ref, pin or
        // journal behind and the frozen range stays pending.
        let gate = AssertionGate {
            command: GatedCommand::Publish,
        };
        if let Err(refusal) = gate.validate(&repository, coordinator.candidate()) {
            let error =
                orna_repository_v1::RepositoryError::CandidateAssertionRefused(refusal.clone());
            return Err(refusal_diagnostic(&refusal, gate.command, &error));
        }
        coordinator
            .publish_with_stage_hook(&repository, |stage| match stage {
                orna_repository_v1::PublicationJournalStage::Prepared => {
                    publication_crash_barrier("prepared")
                }
                orna_repository_v1::PublicationJournalStage::RefAdvanced => {
                    publication_crash_barrier("ref-cas")
                }
                orna_repository_v1::PublicationJournalStage::IndexReconciled => {
                    publication_crash_barrier("index")
                }
                _ => {}
            })
            .map_err(|error| {
                describe_storage_failure("publication commit could not be advanced", &error)
            })?;
        publication_crash_barrier("ref-index");
        coordinator
            .complete(&repository, &state, &freeze)
            .await
            .map_err(|error| {
                describe_storage_failure("published runtime range could not be consumed", &error)
            })?;
        publication_crash_barrier("tail");
        let commit = repository
            .head()
            .map_err(|error| {
                publish_error("published snapshot could not be read", format!("{error:?}"))
            })?
            .ok_or_else(|| publish_error("publication left no committed snapshot", USAGE_HELP))?;
        Ok(Publication::Published(commit.as_str().to_owned(), counts))
    })?;

    match commit {
        Publication::Published(commit, counts) => {
            println!("published {commit}");
            for (table, rows) in counts {
                println!("  {table}  {rows} rows");
            }
        }
        // ORNA-UX-004: lead with what happened to the user's changes. The
        // range is consumed without a commit, so say that its mutations
        // matched the committed rows and were cleared, and where they were.
        Publication::NoVisibleChange(counts) => {
            let mutations = counts.values().sum::<usize>();
            let (noun, verb) = if mutations == 1 {
                ("mutation", "was")
            } else {
                ("mutations", "were")
            };
            println!(
                "nothing to publish: {mutations} runtime {noun} made no visible change to the committed snapshot and {verb} cleared"
            );
            for (table, rows) in counts {
                println!("  {table}  {rows} unchanged rows");
            }
        }
    }
    Ok(())
}

/// What one publication did with its frozen runtime range.
enum Publication {
    /// A commit holds the range; rows per table.
    Published(String, BTreeMap<String, usize>),
    /// Every row already matched the committed snapshot, so the range was
    /// consumed without a commit; mutations per table.
    NoVisibleChange(BTreeMap<String, usize>),
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

/// The remedy for a format-1/2 snapshot. This build has no row writer and no
/// in-place upgrade for it, so the help names the work item that delivers the
/// upgrade instead of a command that cannot finish the job. It is the same
/// remedy that every refused runtime write reports.
const LEGACY_READ_ONLY_HELP: &str = orna_repository_v1::LegacyWriteRefused::REMEDY;

/// The remedy inside a prepared migration copy: its format-3 candidate is
/// journalled, but nothing in this build activates it.
const MIGRATION_PREPARED_HELP: &str = "this build has no writer for format-1/2 snapshots; a format-3 migration candidate is prepared here but not activated; activation is tracked as ornadb-5il1.192";

/// Remedy for a managed-path edit that recovery refused (ORNA-PUB-010). The edit
/// stays on disk; the user either admits it or discards it, then retries.
const MANAGED_EDIT_HELP: &str = "the edited bytes are retained on disk and were not overwritten (copy them elsewhere if you need them); keeping edits to orna-managed store paths is not supported; to discard the edit, run `git show HEAD:<path> > <path>` (`git checkout -- <path>` does not restore a skip-worktree store path), then retry `orna publish`";

/// ORNA-PUB-012: names the read-only legacy format, states that nothing was
/// written, and gives the remedy that fits the worktree's migration state.
fn legacy_read_only_refusal(
    title: &'static str,
    format: u8,
    migration_prepared: bool,
) -> Diagnostic {
    let (help, kept) = if migration_prepared {
        (
            MIGRATION_PREPARED_HELP,
            "the migration journal and its candidate were left as they were",
        )
    } else {
        (LEGACY_READ_ONLY_HELP, "the pending range was preserved")
    };
    Diagnostic::target_with_detail(
        "E2300",
        title,
        help,
        format!(
            "repository format {format} is a read-only legacy snapshot: its loose and compact row writers are retired; no Git object, ref, index or worktree change was made and {kept}"
        ),
    )
}

/// ORNA-PUB-012: name the condition that paused publication and keep the
/// remedy actionable, without implying the pending tail was discarded.
pub(super) fn describe_storage_failure(title: &'static str, error: &orna_storage_v1::Error) -> Diagnostic {
    use orna_storage_v1::Error as E;
    let detail = match error {
        E::LegacyReadOnly { format } => return legacy_read_only_refusal(title, *format, false),
        E::ManagedWorktreeConflict => {
            return Diagnostic::target_with_detail(
                "E2300",
                title,
                MANAGED_EDIT_HELP,
                "a managed path was edited after capture; the edit is retained on disk and HEAD was not changed"
                    .to_owned(),
            );
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
    fn canonical_string_mutation_key_matches_native_typed_key() {
        let encoded_key = orna_foundation_v1::CanonicalValue::new(
            orna_foundation_v1::OvbRaw::Text("london".to_owned()),
        )
        .unwrap()
        .encode()
        .expect("canonical source key encoding");
        let mutation = TableMutation::new([1; 16], "Music", encoded_key, Some(vec![1]))
            .expect("bounded mutation key");
        let row_key = orna_repository_v1::TypedKey::Text("london".to_owned());
        assert_eq!(
            mutation.key(),
            row_key.canonical_bytes().expect("canonical row-map key")
        );
        assert_eq!(
            orna_repository_v1::TypedKey::decode_canonical(mutation.key())
                .expect("canonical typed key"),
            row_key
        );
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

    #[test]
    fn managed_edit_refusal_names_the_discard_remedy_only() {
        let diagnostic = describe_storage_failure(
            "pending publication could not be recovered",
            &orna_storage_v1::Error::ManagedWorktreeConflict,
        );
        assert_eq!(diagnostic.code, "E2300");
        assert_eq!(diagnostic.help, MANAGED_EDIT_HELP);
        assert!(diagnostic.help.contains("not supported"));
        assert!(diagnostic.help.contains("`git show HEAD:<path> > <path>`"));
        assert!(!diagnostic.help.contains("git add"));
        assert!(!diagnostic.help.contains("orna commit"));
        assert!(diagnostic.detail.as_deref().is_some_and(|detail| {
            detail.contains("the edit is retained on disk and HEAD was not changed")
        }));
    }

    /// Runs the MANAGED_EDIT_HELP discard sequence literally against a skip-worktree
    /// store payload in a real Git repository: discard must restore HEAD bytes.
    #[test]
    fn managed_edit_help_discard_sequence_restores_head_on_skip_worktree_store_path() {
        use std::process::Command;
        let directory = tempfile::TempDir::new().unwrap();
        let root = directory.path();
        let run = |script: &str| {
            let status = Command::new("sh")
                .arg("-c")
                .arg(script)
                .current_dir(root)
                .output()
                .unwrap();
            status.status.success()
        };
        let path = ".orna/store/data";
        assert!(run("git init -q . && git config user.email t@t && git config user.name t"));
        std::fs::create_dir_all(root.join(".orna/store")).unwrap();
        std::fs::write(root.join(path), b"committed\n").unwrap();
        assert!(run("git add -A && git commit -q -m base"));
        assert!(run(&format!("git update-index --skip-worktree {path}")));

        std::fs::write(root.join(path), b"discard me\n").unwrap();
        assert!(run(&format!("git show HEAD:{path} > {path}")));
        assert_eq!(std::fs::read(root.join(path)).unwrap(), b"committed\n");
    }

    #[test]
    fn legacy_snapshot_refusal_names_the_format_and_the_upgrade_work_item() {
        let diagnostic = describe_storage_failure(
            "publication target is a read-only legacy snapshot",
            &orna_storage_v1::Error::LegacyReadOnly { format: 2 },
        );
        assert_eq!(diagnostic.code, "E2300");
        assert_eq!(
            diagnostic.title,
            "publication target is a read-only legacy snapshot"
        );
        assert_eq!(diagnostic.help, LEGACY_READ_ONLY_HELP);
        assert_eq!(
            diagnostic.help,
            "this build has no writer or in-place upgrade for format-1/2 snapshots; the format-3 upgrade is tracked as ornadb-5il1.184"
        );
        assert!(!diagnostic.help.contains("migrate copy"));
        assert!(
            diagnostic
                .detail
                .as_deref()
                .is_some_and(|detail| detail.starts_with("repository format 2 is a read-only"))
        );
    }

    #[test]
    fn prepared_migration_refusal_names_the_unactivated_candidate() {
        let diagnostic =
            legacy_read_only_refusal("publication target is a read-only legacy snapshot", 1, true);
        assert_eq!(diagnostic.code, "E2300");
        assert_eq!(diagnostic.help, MIGRATION_PREPARED_HELP);
        assert!(
            diagnostic
                .help
                .ends_with("a format-3 migration candidate is prepared here but not activated; activation is tracked as ornadb-5il1.192")
        );
        assert!(!diagnostic.help.contains("migrate copy"));
        assert!(diagnostic.detail.as_deref().is_some_and(|detail| {
            detail.starts_with("repository format 1 is a read-only")
                && detail
                    .ends_with("the migration journal and its candidate were left as they were")
        }));
    }
}
