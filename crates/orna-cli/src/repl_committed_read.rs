//! Committed-state REPL reads for table scans.
//!
//! A scan evaluates over the CWD snapshot the session is bound to: the
//! committed rows at the bound HEAD, overlaid by unpublished local rows. It
//! takes no writer lease and admits only table reads, so it publishes nothing.
//! Opening the runtime applies the same schema migrations and recovery checks
//! as every other REPL verb.

use orna_project_repl::{AdmittedReplSession, ReplOutcome};
use orna_repository::Repository;
use orna_runtime::{RuntimePublicationMetadataRows, RuntimeState};

use super::{
    PinnedCommittedRows, ReplTransactionScope, ReplTransactions, RequestIdentity,
    STORE_COMMIT_REMEDY, committed_rows_at, repl, repl_effect_failure, request_nonce,
    transactions_unavailable,
};

/// Refuses the input when HEAD is no longer the commit this session bound,
/// the same guard a write takes.
pub(super) fn bound_head_unchanged(
    repository: &Repository,
    transactions: &ReplTransactions,
) -> Result<(), repl::ReplFailure> {
    let (current_commit, _) = repository
        .open_pinned_format_context_with_commit("HEAD")
        .map_err(|_| {
            transactions_unavailable(
                "the repository has no committed HEAD with a readable store",
                STORE_COMMIT_REMEDY,
            )
        })?;
    if current_commit != transactions.commit {
        return Err(repl_effect_failure(
            "the project changed since this REPL session started",
            "restart the REPL to bind to the current HEAD",
        ));
    }
    Ok(())
}

/// Reads the durable `sys.Storage` and `sys.MaintenanceJob` rows when `source`
/// names either relation; `None` otherwise, so other inputs pay no read.
pub(super) fn publication_rows_for_source(
    runtime: &tokio::runtime::Runtime,
    state: &RuntimeState,
    source: &str,
) -> Result<Option<RuntimePublicationMetadataRows>, repl::ReplFailure> {
    if !orna_application::source_requires_publication_metadata(source) {
        return Ok(None);
    }
    runtime
        .block_on(state.publication_metadata_rows())
        .map(Some)
        .map_err(|_| repl::ReplFailure::from("ORNA-REPL-EFFECT".to_owned()))
}

/// Evaluates a table-read-only input over the CWD snapshot: the committed
/// rows at the bound HEAD, overlaid by unpublished local rows.
pub(super) fn submit_committed_read(
    repository: &Repository,
    transactions: &ReplTransactions,
    session: &mut AdmittedReplSession,
    source: &str,
) -> Result<ReplOutcome, repl::ReplFailure> {
    bound_head_unchanged(repository, transactions)?;
    let state = transactions
        .runtime
        .block_on(RuntimeState::open(
            repository,
            transactions.identity,
            transactions.initial_digest,
        ))
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    // The runtime doc for `begin_admitted_table_activation_over_committed`
    // requires the pin to be read before the rows it binds.
    let pin = transactions
        .runtime
        .block_on(state.published_pin())
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let (committed_rows, storage_orders) = committed_rows_at(
        repository,
        &transactions.commit,
        &transactions.table_schemas,
        &transactions.tables,
    )
    .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let committed =
        PinnedCommittedRows::new(pin, committed_rows).map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let snapshot = transactions
        .runtime
        .block_on(
            state.begin_admitted_table_activation_over_committed(&transactions.tables, committed),
        )
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let publication_rows = publication_rows_for_source(&transactions.runtime, &state, source)?;
    let request = RequestIdentity {
        session_id: transactions.session_id,
        request_id: request_nonce().to_be_bytes(),
    };
    let scope = ReplTransactionScope::new(
        &transactions.project,
        repository,
        &transactions.format_context,
        &snapshot,
        &storage_orders,
        request,
        publication_rows,
    )
    .with_catalogue_identities(&transactions.catalogue_identities);
    transactions
        .authority
        .evaluate_read_repl_input(session, source, &scope)
        .map_err(repl::ReplFailure::from)
}

/// Submits an input that stages no effect: the effect-free path evaluates it
/// and publishes its outcome or diagnostic.
pub(super) fn submit_effect_free(
    session: &mut AdmittedReplSession,
    source: &str,
) -> Result<ReplOutcome, repl::ReplFailure> {
    session
        .submit_outcome(source, orna_evaluator::RelationDisplay::Window)
        .map_err(repl::ReplFailure::from)
}
