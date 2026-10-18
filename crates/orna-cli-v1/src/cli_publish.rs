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

use orna_runtime_v1::{PublicationTableIdentity, RuntimeState, TableMutation};
use orna_storage_v1::{CompactOvbProfile, LoosePath, RuntimePublicationCoordinator};

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

/// Admits a source-owned path already projected from the exact committed schema.
fn row_path(
    projection: &orna_project_v1::CommittedLooseRowProjection,
) -> Result<LoosePath, Diagnostic> {
    LoosePath::from_encoded_key(projection.table_root(), projection.key_path_components()).map_err(
        |error| {
            publish_error(
                "source row could not be admitted for publication",
                format!("{error:?}; the pending range was preserved"),
            )
        },
    )
}

/// Validates format-3 mutations against the pinned graph and committed source
/// binding, while resolving legacy row paths only for legacy repository formats.
/// Format-3 keys remain independent of host filename rules (ORNA-KEY-004).
fn validate_candidate_source_rows(
    repository: &orna_repository_v1::Repository,
    head: &orna_repository_v1::GitCommitRef,
    mutations: &[TableMutation],
) -> Result<BTreeMap<[u8; 16], LoosePath>, Diagnostic> {
    let loader = orna_project_v1::ProjectLoader::default();
    let format = repository
        .open_pinned_format_context(head.as_str())
        .map_err(|error| {
            publish_error(
                "repository format context could not be opened",
                format!("{error:?}; the pending range was preserved"),
            )
        })?;
    let mut row_paths = BTreeMap::new();
    let is_format3 = format.repository_format() == orna_repository_v1::RepositoryFormat::Format3;
    let mut format3_mutation_ids = is_format3.then(std::collections::BTreeSet::new);
    for mutation in mutations {
        if let Some(mutation_ids) = format3_mutation_ids.as_mut()
            && !mutation_ids.insert(mutation.id())
        {
            return Err(publish_error(
                "source rows have duplicate mutation identities",
                "the frozen range cannot be mapped unambiguously to committed source keys; the pending range was preserved",
            ));
        }
        let path = if !is_format3 {
            let projection = loader
                .resolve_committed_loose_row_projection(repository, head, mutation)
                .map_err(|error| {
                    publish_error(
                        "source row path could not be resolved",
                        format!("{error:?}; the pending range was preserved"),
                    )
                })?;
            row_path(&projection)?
        } else {
            let table_id = mutation.table_object_id().ok_or_else(|| {
                publish_error(
                    "source row could not be admitted for publication",
                    "the frozen mutation did not retain its admitted TableObjectId; the pending range was preserved",
                )
            })?;
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
            if schema.declaration_name() != mutation.table()
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
                publish_error(
                    title,
                    format!("{error:?}; the pending range was preserved"),
                )
            })?;
            let key_bytes = projected.key;
            orna_repository_v1::TypedKey::decode_canonical(&key_bytes)
                .map_err(|error| {
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

            continue;
        };
        if row_paths.insert(mutation.id(), path).is_some() {
            return Err(publish_error(
                "source rows have duplicate mutation identities",
                "the frozen range cannot be mapped unambiguously to committed source keys; the pending range was preserved",
            ));
        }
    }
    Ok(row_paths)
}


/// Test-only subprocess barrier. When unset, publication has no extra work.
/// Integration tests set an exact boundary and marker path; the parent then
/// SIGKILLs this process after the marker is durable. Kept in the CLI rather
/// than a library so normal library callers never inherit ambient test state.
fn publication_crash_barrier(boundary: &str) {
    let Ok(selected) = std::env::var("ORNA_TEST_PUBLICATION_CRASH_AT") else { return; };
    if selected != boundary { return; }
    let Ok(marker) = std::env::var("ORNA_TEST_PUBLICATION_CRASH_MARKER") else { return; };
    std::fs::write(marker, boundary).expect("write publication crash marker");
    loop { std::thread::park(); }
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

async fn recover_pending_publication(
    repository: &orna_repository_v1::Repository,
    state: &RuntimeState,
    journal: Option<&orna_repository_v1::PublicationJournal>,
) -> Result<(), Diagnostic> {
    if !journal.is_some_and(|journal| journal.compact_manifest().is_some()) {
        RuntimePublicationCoordinator::recover(repository, state)
            .await
            .map_err(|error| {
                describe_storage_failure("pending publication could not be recovered", &error)
            })?;
        return Ok(());
    }

    let pending = repository
        .recover_compact_repository_boundary()
        .map_err(|error| {
            publish_error(
                "compact publication recovery boundary could not be established",
                format!("{error:?}; journal and pending tail were preserved"),
            )
        })?;
    let Some(pending) = pending else {
        return Ok(());
    };

    if let Some(receipt) = state
        .persisted_compact_publication_receipt(&pending)
        .await
        .map_err(|error| {
            publish_error(
                "persisted compact publication receipt could not be verified",
                format!("{error:?}; journal and pending tail were preserved"),
            )
        })?
    {
        repository
            .finish_compact_with_receipt(&receipt)
            .map_err(|error| {
                publish_error(
                    "compact publication could not be finalized",
                    format!("{error:?}; the validated receipt and journal were retained"),
                )
            })?;
        return Ok(());
    }

    let journal = repository.read_publication_journal().map_err(|error| {
        publish_error(
            "compact publication journal could not be refreshed",
            format!("{error:?}; journal and pending tail were preserved"),
        )
    })?;
    let Some(journal) = journal else {
        return Err(publish_error(
            "compact publication journal disappeared during recovery",
            "no new publication range was selected",
        ));
    };
    let witness = journal.compact_manifest().ok_or_else(|| {
        publish_error(
            "compact publication source could not be verified",
            "the compact journal witness is missing; the journal and pending tail were preserved",
        )
    })?;
    let freeze = state
        .publication_freeze(pending.runtime_intent_id())
        .await
        .map_err(|error| {
            publish_error(
                "compact publication freeze could not be recovered",
                format!("{error:?}; journal and pending tail were preserved"),
            )
        })?;
    if freeze.checkpoint.digest != pending.cleanup_watermark() {
        return Err(publish_error(
            "compact publication freeze does not match its journal",
            "the journal and pending tail were preserved",
        ));
    }
    let mutations = state
        .pending_table_mutations_through(&freeze)
        .await
        .map_err(|error| {
            publish_error(
                "compact publication table identity could not be recovered",
                format!("{error:?}; journal and pending tail were preserved"),
            )
        })?;
    let table_id = *witness.table().as_bytes();
    let mut table_name: Option<String> = None;
    for mutation in &mutations {
        if !mutation
            .table_object_id()
            .is_some_and(|object_id| object_id.bytes() == table_id)
        {
            return Err(publish_error(
                "compact publication table identity does not match its journal",
                "persisted typed mutation identity is missing or conflicts with the source-bound manifest; the journal and pending tail were preserved",
            ));
        }
        match table_name.as_deref() {
            Some(existing) if existing != mutation.table() => {
                return Err(publish_error(
                    "compact publication spans multiple tables",
                    "the source-bound identity is ambiguous; the journal and pending tail were preserved",
                ));
            }
            Some(_) => {}
            None => table_name = Some(mutation.table().to_owned()),
        }
    }
    let Some(table_name) = table_name else {
        return Err(publish_error(
            "compact publication has no frozen table mutations",
            "no source-bound table identity can be established; the journal and pending tail were preserved",
        ));
    };
    let manifest = repository
        .read_compact_manifest(pending.commit(), witness.table())
        .map_err(|error| {
            publish_error(
                "compact publication manifest could not be verified",
                format!("{error:?}; journal and pending tail were preserved"),
            )
        })?
        .ok_or_else(|| {
            publish_error(
                "compact publication manifest is missing",
                "the journal and pending tail were preserved",
            )
        })?;
    let schema_fingerprint = manifest.schema();
    let descriptor = orna_storage_v1::trusted_compact_schema_descriptor(
        repository,
        journal.old_head(),
        witness.table(),
        schema_fingerprint,
        &journal,
        pending.commit(),
    )
    .map_err(|error| {
        publish_error(
            "compact publication source schema is not trusted",
            format!("{error:?}; journal and pending tail were preserved"),
        )
    })?;
    let profile = CompactOvbProfile::new(descriptor.clone()).map_err(|error| {
        publish_error(
            "compact publication source schema is invalid",
            format!("{error:?}; journal and pending tail were preserved"),
        )
    })?;
    if profile.table_id() != table_id || profile.schema_fingerprint() != schema_fingerprint {
        return Err(publish_error(
            "compact publication source schema does not match its journal",
            "the journal and pending tail were preserved",
        ));
    }
    for mutation in &mutations {
        if let Some(row) = mutation.value() {
            let rekey_to = mutation.rekey_to();
            if rekey_to.is_some() {
                descriptor
                    .validate_stored_key(table_id, mutation.key())
                    .map_err(|error| {
                        publish_error(
                            "compact publication rekey is invalid for its committed schema",
                            format!("{error:?}; journal and pending tail were preserved"),
                        )
                    })?;
            }
            profile
                .validate_complete_row(
                    &descriptor,
                    rekey_to.unwrap_or_else(|| mutation.key()),
                    row,
                )
                .map_err(|error| {
                    let message = if rekey_to.is_some() {
                        "compact publication rekey is invalid for its committed schema"
                    } else {
                        "compact publication row is invalid for its committed schema"
                    };
                    publish_error(
                        message,
                        format!("{error:?}; journal and pending tail were preserved"),
                    )
                })?;
        }
    }
    let table_identity = PublicationTableIdentity {
        table: table_name,
        table_id,
        schema_fingerprint,
        candidate_generation: witness.generation(),
    };
    RuntimePublicationCoordinator::recover_compact_validated(
        repository,
        state,
        &profile,
        &table_identity,
    )
    .await
    .map_err(|error| {
        publish_error(
            "source-bound compact publication recovery failed",
            format!("{error:?}; retry `orna publish` before selecting another range"),
        )
    })?;
    Ok(())
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
    runtime.block_on(recover_pending_publication(
        &repository,
        &state,
        journal.as_ref(),
    ))?;
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
        .map_err(|_| publish_error("publication intent could not be created", USAGE))?;

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
        let row_paths = validate_candidate_source_rows(&repository, &head, &frozen)?;
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
            |mutation| {
                row_paths
                    .get(&mutation.id())
                    .cloned()
                    .ok_or(orna_storage_v1::Error::InvalidMutationId)
            },
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
            return Ok::<_, Diagnostic>(None);
        };
        coordinator
            .publish_with_stage_hook(&repository, |stage| match stage {
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
}
