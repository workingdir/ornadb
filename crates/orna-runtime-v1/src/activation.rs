use std::cell::RefCell;
use std::future::Future;

use super::{
    CatalogueAdmission, FaultInjector, RuntimeError, RuntimeState, RuntimeTableActivationSnapshot,
    TableMutation, WriterLease,
};

tokio::task_local! {
    /// Runtime administration transitions and nested activations on the same
    /// state are rejected while its activation callback is running. The
    /// task-local scope follows async calls without serializing independent
    /// activations on other tasks.
    static ACTIVE_ACTIVATION_STATES: RefCell<Vec<usize>>;
}

pub(crate) fn runtime_activation_active(state: &RuntimeState) -> bool {
    let state_id = std::ptr::from_ref(state) as usize;
    ACTIVE_ACTIVATION_STATES
        .try_with(|states| states.borrow().contains(&state_id))
        .unwrap_or(false)
}

/// Runs one async application callback inside the runtime's reentrancy fence.
/// Reentrant administration on this state is rejected before opening its
/// transaction, while the caller still owns the writer lease.
pub async fn with_activation_scope<T, E, F, Fut>(
    state: &RuntimeState,
    lease: WriterLease,
    callback: F,
) -> Result<Result<T, E>, RuntimeError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let state_id = std::ptr::from_ref(state) as usize;
    let mut active = ACTIVE_ACTIVATION_STATES
        .try_with(|states| states.borrow().clone())
        .unwrap_or_default();
    if active.contains(&state_id) {
        return Err(RuntimeError::AdminBusy);
    }
    active.push(state_id);

    ACTIVE_ACTIVATION_STATES
        .scope(RefCell::new(active), async {
            if state.current_lease().await? != Some(lease) {
                return Err(RuntimeError::OwnerLost);
            }
            Ok(callback().await)
        })
        .await
}

/// Staged table changes and the typed value produced by one activation.
///
/// The evaluator can use the staged result for read-your-writes semantics while
/// the runtime retains ownership of the mutations until the atomic commit.
#[derive(Debug)]
pub struct ActivationWork<T> {
    mutations: Vec<TableMutation>,
    next_digest: [u8; 32],
    result: T,
    catalogue_admission: Option<CatalogueAdmission>,
}

impl<T> ActivationWork<T> {
    /// Creates work for the runtime activation boundary.
    pub fn new(mutations: Vec<TableMutation>, next_digest: [u8; 32], result: T) -> Self {
        Self {
            mutations,
            next_digest,
            result,
            catalogue_admission: None,
        }
    }

    /// Includes a complete resolved source catalogue in this activation's
    /// atomic runtime commit. Its predecessor must be the activation's pinned
    /// starting capture; catalogue identities are committed with the table
    /// writes and next capture, or none of them become visible.
    pub fn with_catalogue_admission(mut self, admission: CatalogueAdmission) -> Self {
        self.catalogue_admission = Some(admission);
        self
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
    let state_id = std::ptr::from_ref(state) as usize;
    let mut active = ACTIVE_ACTIVATION_STATES
        .try_with(|states| states.borrow().clone())
        .unwrap_or_default();
    if active.contains(&state_id) {
        return Err(ActivationError::Runtime(RuntimeError::AdminBusy));
    }
    active.push(state_id);

    ACTIVE_ACTIVATION_STATES
        .scope(RefCell::new(active), async {
            run_table_activation_inner(state, lease, tables, faults, evaluator).await
        })
        .await
}

async fn run_table_activation_inner<T, E, F, Fut>(
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
    // Reject an owner superseded before evaluation starts. The commit repeats
    // this check transactionally to catch a takeover racing this precheck.
    if state
        .current_lease()
        .await
        .map_err(ActivationError::Runtime)?
        != Some(lease)
    {
        return Err(ActivationError::Runtime(RuntimeError::OwnerLost));
    }

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
        catalogue_admission,
    } = work;
    match catalogue_admission {
        Some(admission) => {
            state
                .commit_catalogue_table_activation(
                    lease,
                    snapshot.context(),
                    &admission,
                    &mutations,
                    next_digest,
                    faults,
                )
                .await
        }
        None => {
            state
                .commit_table_activation(lease, snapshot.context(), &mutations, next_digest, faults)
                .await
        }
    }
    .map_err(ActivationError::Runtime)?;
    Ok(result)
}
