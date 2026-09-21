use std::future::Future;

use super::{FaultInjector, RuntimeError, RuntimeState, RuntimeTableActivationSnapshot, TableMutation,
    WriterLease};

/// Staged table changes and the typed value produced by one activation.
///
/// The evaluator can use the staged result for read-your-writes semantics while
/// the runtime retains ownership of the mutations until the atomic commit.
#[derive(Debug)]
pub struct ActivationWork<T> {
    mutations: Vec<TableMutation>,
    next_digest: [u8; 32],
    result: T,
}

impl<T> ActivationWork<T> {
    /// Creates work for the runtime activation boundary.
    pub fn new(mutations: Vec<TableMutation>, next_digest: [u8; 32], result: T) -> Self {
        Self {
            mutations,
            next_digest,
            result,
        }
    }

    /// Returns the typed table mutations staged for commit.
    pub fn mutations(&self) -> &[TableMutation] {
        &self.mutations
    }

    /// Returns the caller-validated digest to publish with the activation.
    pub fn next_digest(&self) -> [u8; 32] {
        self.next_digest
    }

    /// Consumes the work and returns the evaluator's typed result.
    pub fn into_result(self) -> T {
        self.result
    }
}

/// Failure at either the runtime admission/commit boundary or in evaluation.
#[derive(Debug)]
pub enum ActivationError<E> {
    Runtime(RuntimeError),
    Evaluator(E),
}

impl<E> From<RuntimeError> for ActivationError<E> {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(error)
    }
}

/// Runs one table activation against a single immutable admission snapshot.
///
/// Source execution supplies the evaluator and stages typed writes in
/// [`ActivationWork`]. The evaluator may calculate its returned value from
/// staged writes (read-your-writes); only a successful runtime commit makes
/// those writes visible to later activations.
pub async fn run_table_activation<T, E, F, Fut>(
    state: &RuntimeState,
    lease: WriterLease,
    tables: &[&str],
    faults: &dyn FaultInjector,
    evaluator: F,
) -> Result<T, ActivationError<E>>
where
    F: FnOnce(&RuntimeTableActivationSnapshot) -> Fut,
    Fut: Future<Output = Result<ActivationWork<T>, E>>,
{
    let snapshot = state
        .begin_table_activation(tables)
        .await
        .map_err(ActivationError::Runtime)?;
    let work = evaluator(&snapshot)
        .await
        .map_err(ActivationError::Evaluator)?;
    let ActivationWork {
        mutations,
        next_digest,
        result,
    } = work;
    state
        .commit_table_activation(
            lease,
            snapshot.context(),
            &mutations,
            next_digest,
            faults,
        )
        .await
        .map_err(ActivationError::Runtime)?;
    Ok(result)
}
