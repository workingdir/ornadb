//! Ownership of application work admitted by a live session.
//!
//! A supervisor admits leases per session, cancels them on deletion, and joins
//! every root and descendant lease before a session is retired.

use futures::channel::oneshot;
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    task::{Poll, Waker},
};

/// Failures reported by the application work supervisor and its leases.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseError {
    Limit,
    Closed,
    DeletionFailed,
    ApplicationDrainRequired,
}

pub type Result<T> = std::result::Result<T, LeaseError>;

/// Shared ownership state for application work admitted by a live session.
///
/// A synchronous callback holds a lease until it returns. An asynchronous
/// adapter may retain the lease, poll its cancellation receiver, and register
/// descendants with [`LiveApplicationWorkLease::spawn_child`]. Session
/// deletion changes admission before requesting cancellation, then waits until
/// every root and descendant lease has been dropped.
#[derive(Clone)]
pub struct LiveApplicationWorkSupervisor {
    state: Arc<Mutex<ApplicationWorkState>>,
}

struct ApplicationWorkState {
    next_id: u64,
    sessions: BTreeMap<[u8; 16], ApplicationSessionWork>,
}

impl ApplicationWorkState {
    /// The work record of `session`, created on first use. A new session is
    /// refused once `MAX_APPLICATION_SESSIONS` are tracked.
    fn session_entry(&mut self, session: [u8; 16]) -> Result<&mut ApplicationSessionWork> {
        if !self.sessions.contains_key(&session) && self.sessions.len() >= MAX_APPLICATION_SESSIONS
        {
            return Err(LeaseError::Limit);
        }
        Ok(self
            .sessions
            .entry(session)
            .or_insert_with(ApplicationSessionWork::new))
    }
}

struct ApplicationSessionWork {
    phase: ApplicationWorkPhase,
    work: BTreeMap<u64, ApplicationWorkEntry>,
    /// Requests whose lease was dropped without acknowledging its terminal
    /// boundary. A session with any such request cannot be joined or retired
    /// as successfully drained.
    failed_requests: BTreeSet<[u8; 16]>,
    waiters: Vec<Waker>,
}

impl ApplicationSessionWork {
    fn new() -> Self {
        Self {
            phase: ApplicationWorkPhase::Open,
            work: BTreeMap::new(),
            failed_requests: BTreeSet::new(),
            waiters: Vec::new(),
        }
    }

    fn has_failed(&self) -> bool {
        !self.failed_requests.is_empty()
    }

    /// Parks `waker` until the next release, unless it is already parked.
    /// Fails when `MAX_APPLICATION_JOIN_WAITERS` distinct tasks already wait.
    fn park(&mut self, waker: &Waker) -> Result<()> {
        if !self.waiters.iter().any(|waiter| waiter.will_wake(waker)) {
            if self.waiters.len() >= MAX_APPLICATION_JOIN_WAITERS {
                return Err(LeaseError::Limit);
            }
            self.waiters.push(waker.clone());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplicationWorkPhase {
    Open,
    Draining,
    Deleted,
}

struct ApplicationWorkEntry {
    request: [u8; 16],
    parent: Option<u64>,
    cancellation: Option<oneshot::Sender<()>>,
    cancelled: Arc<AtomicBool>,
}

impl ApplicationWorkEntry {
    /// Marks the work cancelled and wakes its cancellation receiver once.
    fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(cancellation) = self.cancellation.take() {
            // A send fails only when the receiver is gone, which means the
            // work has nothing left to cancel.
            let _ = cancellation.send(());
        }
    }
}

fn request_work_ids(session_work: &ApplicationSessionWork, request: [u8; 16]) -> BTreeSet<u64> {
    let mut pending = session_work
        .work
        .iter()
        .filter_map(|(id, entry)| (entry.request == request).then_some(*id))
        .collect::<Vec<_>>();
    let mut owned = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !owned.insert(id) {
            continue;
        }
        pending.extend(
            session_work
                .work
                .iter()
                .filter_map(|(child, entry)| (entry.parent == Some(id)).then_some(*child)),
        );
    }
    owned
}

const MAX_APPLICATION_SESSIONS: usize = 4096;
const MAX_APPLICATION_WORK_PER_SESSION: usize = 4096;
const MAX_APPLICATION_JOIN_WAITERS: usize = 8;

fn lock_work_state(state: &Mutex<ApplicationWorkState>) -> MutexGuard<'_, ApplicationWorkState> {
    state.lock().expect("application work state poisoned")
}

/// Resolves once `finished` yields a result for the session's work record; the
/// polling task is parked until then. Fails with [`LeaseError::Closed`] when the
/// session record is gone.
fn join_session<T: 'static>(
    state: Arc<Mutex<ApplicationWorkState>>,
    session: [u8; 16],
    finished: impl Fn(&ApplicationSessionWork) -> Option<Result<T>> + 'static,
) -> Pin<Box<dyn Future<Output = Result<T>>>> {
    Box::pin(futures::future::poll_fn(move |context| {
        let mut state = lock_work_state(&state);
        let Some(session_work) = state.sessions.get_mut(&session) else {
            return Poll::Ready(Err(LeaseError::Closed));
        };
        if let Some(result) = finished(session_work) {
            return Poll::Ready(result);
        }
        match session_work.park(context.waker()) {
            Ok(()) => Poll::Pending,
            Err(error) => Poll::Ready(Err(error)),
        }
    }))
}

/// A lease held by one admitted application callback or descendant.
pub struct LiveApplicationWorkLease {
    supervisor: LiveApplicationWorkSupervisor,
    session: [u8; 16],
    request: [u8; 16],
    id: u64,
    cancellation: oneshot::Receiver<()>,
    cancelled: Arc<AtomicBool>,
    completed: bool,
}

impl LiveApplicationWorkSupervisor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ApplicationWorkState {
                next_id: 0,
                sessions: BTreeMap::new(),
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ApplicationWorkState> {
        lock_work_state(&self.state)
    }

    /// Admits a root application callback for a session.
    ///
    /// # Errors
    ///
    /// Returns [`LeaseError::Closed`] after deletion admission has stopped new
    /// application work.
    pub fn admit(&self, session: [u8; 16], request: [u8; 16]) -> Result<LiveApplicationWorkLease> {
        self.admit_with_parent(session, request, None)
    }

    fn admit_with_parent(
        &self,
        session: [u8; 16],
        request: [u8; 16],
        parent: Option<u64>,
    ) -> Result<LiveApplicationWorkLease> {
        let mut state = self.lock();
        let id = state.next_id;
        state.next_id = id.checked_add(1).ok_or(LeaseError::Limit)?;
        let session_work = state.session_entry(session)?;
        if session_work.phase != ApplicationWorkPhase::Open
            || parent.is_some_and(|parent| {
                session_work
                    .work
                    .get(&parent)
                    .is_none_or(|entry| entry.cancelled.load(Ordering::Acquire))
            })
        {
            return Err(LeaseError::Closed);
        }
        if session_work.work.len() >= MAX_APPLICATION_WORK_PER_SESSION {
            return Err(LeaseError::Limit);
        }
        let (sender, cancellation) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        session_work.work.insert(
            id,
            ApplicationWorkEntry {
                request,
                parent,
                cancellation: Some(sender),
                cancelled: Arc::clone(&cancelled),
            },
        );
        Ok(LiveApplicationWorkLease {
            supervisor: self.clone(),
            session,
            request,
            id,
            cancellation,
            cancelled,
            completed: false,
        })
    }

    /// Requests cancellation for one in-band request and its recursively
    /// owned application children without draining the rest of the session.
    /// The request identity is retained on every lease so a protocol cancel
    /// can signal the exact running or queued target.
    #[must_use]
    pub fn cancel_request(&self, session: [u8; 16], request: [u8; 16]) -> bool {
        self.begin_request_cancellation(session, request)
    }

    /// Requests cancellation for one request tree, then waits until every
    /// matching lease has released its activation-owned resources. This is a
    /// targeted join: unrelated application work in the same session remains
    /// admitted and running.
    ///
    /// # Panics
    ///
    /// Panics if the supervisor state mutex is poisoned.
    #[must_use]
    pub fn cancel_and_join_request(
        &self,
        session: [u8; 16],
        request: [u8; 16],
    ) -> Pin<Box<dyn Future<Output = Result<bool>>>> {
        if !self.begin_request_cancellation(session, request) {
            return Box::pin(std::future::ready(Ok(false)));
        }
        join_session(Arc::clone(&self.state), session, move |session_work| {
            request_work_ids(session_work, request).is_empty().then(|| {
                (!session_work.failed_requests.contains(&request))
                    .then_some(true)
                    .ok_or(LeaseError::DeletionFailed)
            })
        })
    }

    fn begin_request_cancellation(&self, session: [u8; 16], request: [u8; 16]) -> bool {
        let mut state = self.lock();
        let Some(session_work) = state.sessions.get_mut(&session) else {
            return false;
        };
        let pending = request_work_ids(session_work, request);
        if pending.is_empty() {
            return false;
        }
        for id in pending {
            if let Some(entry) = session_work.work.get_mut(&id) {
                entry.cancel();
            }
        }
        true
    }

    /// Prevents new work, recursively requests cancellation, and joins every
    /// root and descendant currently owned by the session.
    ///
    /// # Panics
    ///
    /// Panics if the supervisor state mutex is poisoned while beginning the
    /// drain or while polling the join.
    #[must_use]
    pub fn cancel_and_join(&self, session: [u8; 16]) -> Pin<Box<dyn Future<Output = Result<()>>>> {
        if let Err(error) = self.begin_draining(session) {
            return Box::pin(std::future::ready(Err(error)));
        }
        join_session(Arc::clone(&self.state), session, |session_work| {
            session_work.work.is_empty().then(|| {
                (!session_work.has_failed())
                    .then_some(())
                    .ok_or(LeaseError::DeletionFailed)
            })
        })
    }

    fn begin_draining(&self, session: [u8; 16]) -> Result<()> {
        let mut state = self.lock();
        let session_work = state.session_entry(session)?;
        if session_work.phase == ApplicationWorkPhase::Deleted {
            return Ok(());
        }
        session_work.phase = ApplicationWorkPhase::Draining;
        let roots = session_work
            .work
            .iter()
            .filter_map(|(id, entry)| entry.parent.is_none().then_some(*id))
            .collect::<Vec<_>>();
        let mut visited = BTreeSet::new();
        let mut pending = roots;
        // A malformed external adapter cannot strand a retained descendant.
        pending.extend(session_work.work.keys().copied());
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            if let Some(entry) = session_work.work.get_mut(&id) {
                entry.cancel();
            }
            pending.extend(
                session_work
                    .work
                    .iter()
                    .filter_map(|(child, entry)| (entry.parent == Some(id)).then_some(*child)),
            );
        }
        Ok(())
    }

    /// Marks a successfully joined session permanently closed to new work.
    ///
    /// # Errors
    ///
    /// Returns [`LeaseError::ApplicationDrainRequired`] if a lease remains.
    ///
    /// # Panics
    ///
    /// Panics if the supervisor state mutex is poisoned.
    pub fn retire(&self, session: [u8; 16]) -> Result<()> {
        let mut state = self.lock();
        let session_work = state.session_entry(session)?;
        if session_work.work.is_empty() && !session_work.has_failed() {
            session_work.phase = ApplicationWorkPhase::Deleted;
            Ok(())
        } else {
            Err(LeaseError::ApplicationDrainRequired)
        }
    }

    /// Releases the deletion tombstone after the transport has durably
    /// completed session deletion. A failed deletion retains the tombstone
    /// so a retry cannot reopen application admission.
    ///
    /// # Panics
    ///
    /// Panics if the supervisor state mutex is poisoned.
    pub fn forget(&self, session: [u8; 16]) {
        let mut state = self.lock();
        if state.sessions.get(&session).is_some_and(|session_work| {
            session_work.phase == ApplicationWorkPhase::Deleted && session_work.work.is_empty()
        }) {
            state.sessions.remove(&session);
        }
    }

    /// Cancels and joins every session still owned by an executable actor.
    /// The actor uses this only after it stops accepting commands, leaving
    /// its task join set as the final owner of application futures.
    ///
    /// # Panics
    ///
    /// Panics if any supervisor state mutex is poisoned while beginning the
    /// drain or joining sessions.
    #[must_use]
    pub fn cancel_and_join_all(&self) -> Pin<Box<dyn Future<Output = Result<()>>>> {
        let sessions = self.lock().sessions.keys().copied().collect::<Vec<_>>();
        let mut admission_error = None;
        for session in &sessions {
            if let Err(error) = self.begin_draining(*session)
                && admission_error.is_none()
            {
                admission_error = Some(error);
            }
        }
        let supervisor = self.clone();
        Box::pin(async move {
            let mut first_error = admission_error;
            for session in sessions {
                if let Err(error) = supervisor.cancel_and_join(session).await
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
            }
            first_error.map_or(Ok(()), Err)
        })
    }

    /// Whether the session no longer admits new work or still has admitted
    /// work in flight.
    ///
    /// # Panics
    ///
    /// Panics if the supervisor state mutex is poisoned.
    #[must_use]
    pub fn is_draining_or_active(&self, session: [u8; 16]) -> bool {
        self.lock()
            .sessions
            .get(&session)
            .is_some_and(|session_work| {
                session_work.phase != ApplicationWorkPhase::Open || !session_work.work.is_empty()
            })
    }

    fn release(&self, session: [u8; 16], id: u64) {
        let mut state = self.lock();
        if let Some(session_work) = state.sessions.get_mut(&session) {
            session_work.work.remove(&id);
            let waiters = std::mem::take(&mut session_work.waiters);
            drop(state);
            for waiter in waiters {
                waiter.wake();
            }
        }
    }
}

impl Drop for LiveApplicationWorkLease {
    fn drop(&mut self) {
        if !self.completed
            && let Some(session_work) = self.supervisor.lock().sessions.get_mut(&self.session)
        {
            session_work.failed_requests.insert(self.request);
        }
        self.supervisor.release(self.session, self.id);
    }
}

impl LiveApplicationWorkLease {
    /// Returns whether cancellation has been requested for this lease or its
    /// session has entered deletion.
    ///
    /// # Panics
    ///
    /// Panics if the supervisor state mutex is poisoned.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
            || self
                .supervisor
                .lock()
                .sessions
                .get(&self.session)
                .is_none_or(|session_work| session_work.phase != ApplicationWorkPhase::Open)
    }

    /// Fences a callback's next state-changing boundary.
    ///
    /// # Errors
    ///
    /// Returns [`LeaseError::Closed`] after cancellation or deletion admission.
    pub fn check_active(&self) -> Result<()> {
        (!self.is_cancelled())
            .then_some(())
            .ok_or(LeaseError::Closed)
    }

    /// Returns the lock-free cancellation flag for a worker thread that owns
    /// this lease. The flag is set before the compatibility future is woken.
    #[must_use]
    pub fn cancellation_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    /// Poll this receiver in an async application adapter to observe the
    /// supervisor's cancellation request.
    pub fn cancellation(&mut self) -> &mut oneshot::Receiver<()> {
        &mut self.cancellation
    }

    /// Acknowledges that the application operation has reached its terminal
    /// boundary. A dropped lease without this acknowledgement fails a
    /// deletion join closed rather than being mistaken for successful work.
    pub fn complete(&mut self) {
        if !self.completed {
            self.completed = true;
            self.supervisor.release(self.session, self.id);
        }
    }

    /// Registers a recursively owned child under this lease.
    ///
    /// # Errors
    ///
    /// Returns [`LeaseError::Closed`] when the parent or session is no longer
    /// admitted.
    pub fn spawn_child(&self) -> Result<LiveApplicationWorkLease> {
        self.supervisor
            .admit_with_parent(self.session, self.request, Some(self.id))
    }
}

impl Default for LiveApplicationWorkSupervisor {
    fn default() -> Self {
        Self::new()
    }
}
