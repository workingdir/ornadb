use super::*;
/// Acquires the exclusive active-revision lock for transaction-bound execution.
///
/// Active-pointer writers use the same lock before replacement, so the caller
/// keeps its revision and security snapshot stable while the transaction is
/// open.
pub(crate) async fn lock_active_revision(
    transaction: &Transaction<'_>,
    expected: RevisionPair,
) -> Result<(), PostgresKernelError> {
    let row = transaction
        .query_one(
            "SELECT source_revision_id, catalogue_revision_id
             FROM _orna_kernel.active_revision
             WHERE singleton = true
             FOR UPDATE",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    validate_locked_active_revision(&row, expected)
}

/// Acquires a shared active-revision lock for one accepted resource producer.
///
/// Multiple resource producers can validate the same pinned revision together.
/// Active-pointer writers acquire `FOR UPDATE`, which conflicts with this lock
/// and waits until every producer releases its terminal transaction.
pub(super) async fn lock_active_revision_for_resource(
    transaction: &Transaction<'_>,
    expected: RevisionPair,
) -> Result<(), PostgresKernelError> {
    let row = transaction
        .query_one(
            "SELECT source_revision_id, catalogue_revision_id
             FROM _orna_kernel.active_revision
             WHERE singleton = true
             FOR KEY SHARE",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    validate_locked_active_revision(&row, expected)
}

fn validate_locked_active_revision(
    row: &Row,
    expected: RevisionPair,
) -> Result<(), PostgresKernelError> {
    let active = RevisionPair::new(
        orna_core::SourceRevisionId::from_bytes(exact_id(
            row,
            "source_revision_id",
            "active source revision is not exactly 16 bytes",
        )?),
        orna_core::CatalogueRevisionId::from_bytes(exact_id(
            row,
            "catalogue_revision_id",
            "active catalogue revision is not exactly 16 bytes",
        )?),
    );
    if expected != active {
        return Err(PostgresKernelError::SecurityRevisionMismatch { expected, active });
    }
    Ok(())
}

/// Requires a recovered target list to cover the canonical non-system
/// function set before any other security rows are materialized.
///
/// This is the pre-construction counterpart to [`require_complete_function_set`].
/// Keeping the comparison at the target-authority boundary means a deleted
/// application target cannot be hidden by a later dangling grant error.
pub(crate) fn require_complete_function_targets(
    active: &ActiveDatabaseRevision,
    targets: &[SecurityFunctionTarget],
) -> Result<(), PostgresKernelError> {
    let mut functions = targets
        .iter()
        .map(|target| target.function())
        .collect::<Vec<_>>();
    functions.sort_unstable();
    if security_function_targets(active) != functions {
        return Err(PostgresKernelError::SecurityFunctionSetMismatch);
    }
    Ok(())
}

pub(crate) fn require_complete_function_set(
    active: &ActiveDatabaseRevision,
    snapshot: &SecuritySnapshot,
) -> Result<(), PostgresKernelError> {
    if security_function_targets(active) != snapshot.functions().collect::<Vec<_>>() {
        return Err(PostgresKernelError::SecurityFunctionSetMismatch);
    }
    Ok(())
}

/// Returns the exact non-system `EXECUTE` target universe for one active revision.
///
/// The application catalogue and its pinned verified standard snapshot are both
/// identity authorities. The standard side is empty until standard functions
/// are admitted, but it remains part of this one ordered target set.
fn security_function_targets(active: &ActiveDatabaseRevision) -> Vec<FunctionId> {
    let mut functions = active
        .catalogue()
        .functions()
        .iter()
        .map(|function| function.id())
        .collect::<Vec<_>>();
    if let Some(standard) = active.catalogue_hash_context().standard() {
        functions.extend(
            standard
                .catalogue()
                .functions()
                .iter()
                .map(|function| function.id()),
        );
    }
    functions.retain(|function| system_function_by_id(*function).is_none());
    functions.sort_unstable();
    functions
}
