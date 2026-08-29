use std::sync::{Arc, Mutex};

const RESOURCE_CANCELLATION_RUNNING: u8 = 0;
const RESOURCE_CANCELLATION_REQUESTED: u8 = 1;
const RESOURCE_CANCELLATION_COMMIT_STARTED: u8 = 2;
const RESOURCE_CANCELLATION_COMMITTED: u8 = 3;
const RESOURCE_CANCELLATION_ACCEPTANCE_COMMIT_STARTED: u8 = 4;
const RESOURCE_CANCELLATION_ACCEPTANCE_CANCEL_REQUESTED: u8 = 5;

/// Coordinates cancellation with resource acceptance and terminal commits.
///
/// Each commit-start transition is a cancellation linearisation point:
/// cancellation wins only while the request is still running. Once either
/// acceptance or terminal commit starts, that commit wins even if a later
/// cancellation arrives.
#[derive(Clone, Debug)]
pub struct ResourceCancellation {
    state: Arc<Mutex<u8>>,
    notify: Arc<tokio::sync::Notify>,
}

impl Default for ResourceCancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceCancellation {
    /// Creates a cancellation state for one resource request.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RESOURCE_CANCELLATION_RUNNING)),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Requests cancellation and returns whether this request won the race.
    pub fn request_cancel(&self) -> bool {
        let won = {
            let mut state = self
                .state
                .lock()
                .expect("resource cancellation state is not poisoned");
            match *state {
                RESOURCE_CANCELLATION_RUNNING => {
                    *state = RESOURCE_CANCELLATION_REQUESTED;
                    true
                }
                RESOURCE_CANCELLATION_ACCEPTANCE_COMMIT_STARTED => {
                    *state = RESOURCE_CANCELLATION_ACCEPTANCE_CANCEL_REQUESTED;
                    false
                }
                _ => false,
            }
        };
        if won {
            self.notify.notify_waiters();
        }
        won
    }

    /// Returns whether cancellation has won the terminal commit race.
    pub fn is_requested(&self) -> bool {
        matches!(
            *self
                .state
                .lock()
                .expect("resource cancellation state is not poisoned"),
            RESOURCE_CANCELLATION_REQUESTED | RESOURCE_CANCELLATION_ACCEPTANCE_CANCEL_REQUESTED
        )
    }

    /// Returns whether cancellation arrived during acceptance commit.
    #[doc(hidden)]
    pub fn is_acceptance_cancellation_requested(&self) -> bool {
        *self
            .state
            .lock()
            .expect("resource cancellation state is not poisoned")
            == RESOURCE_CANCELLATION_ACCEPTANCE_CANCEL_REQUESTED
    }

    /// Waits until cancellation wins the terminal commit race.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_requested() {
                return;
            }
            notified.await;
        }
    }

    /// Starts the durable acceptance commit if cancellation has not won.
    #[doc(hidden)]
    pub fn try_begin_acceptance_commit(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("resource cancellation state is not poisoned");
        if *state != RESOURCE_CANCELLATION_RUNNING {
            return false;
        }
        *state = RESOURCE_CANCELLATION_ACCEPTANCE_COMMIT_STARTED;
        true
    }

    /// Reopens terminal cancellation after durable acceptance has committed.
    #[doc(hidden)]
    pub fn acceptance_commit_finished(&self) {
        let mut state = self
            .state
            .lock()
            .expect("resource cancellation state is not poisoned");
        match *state {
            RESOURCE_CANCELLATION_ACCEPTANCE_COMMIT_STARTED => {
                *state = RESOURCE_CANCELLATION_RUNNING;
            }
            RESOURCE_CANCELLATION_ACCEPTANCE_CANCEL_REQUESTED => {
                *state = RESOURCE_CANCELLATION_REQUESTED;
            }
            _ => {}
        }
    }

    /// Starts the terminal commit if cancellation has not won.
    pub fn try_begin_commit(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("resource cancellation state is not poisoned");
        if *state != RESOURCE_CANCELLATION_RUNNING {
            return false;
        }
        *state = RESOURCE_CANCELLATION_COMMIT_STARTED;
        true
    }

    /// Records that the terminal transaction commit completed.
    pub fn commit_finished(&self) {
        let mut state = self
            .state
            .lock()
            .expect("resource cancellation state is not poisoned");
        if *state == RESOURCE_CANCELLATION_COMMIT_STARTED {
            *state = RESOURCE_CANCELLATION_COMMITTED;
        }
    }
}
