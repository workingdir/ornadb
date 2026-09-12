//! Explicit, per-evaluation cancellation for evaluator-owned work.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::{EvaluationError, error};

/// A cancellation request shared by one evaluator operation and its owner.
///
/// This is deliberately not part of Limits: limits are immutable evaluator
/// configuration, while this flag is live operation state.
#[derive(Clone, Debug)]
pub struct CancellationToken {
    requested: Arc<AtomicBool>,
    /// Once this token has observed a request, the observation cannot be
    /// undone by an owner resetting the source flag.
    observed: Arc<AtomicBool>,
    #[cfg(test)]
    checks: Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    request_after: Arc<std::sync::atomic::AtomicU64>,
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::from_shared(Arc::new(AtomicBool::new(false)))
    }

    /// Creates a token backed by an externally-owned cancellation flag.
    ///
    /// The external flag is only a request source. Once this token observes
    /// it set, the observation is latched for the lifetime of all clones.
    #[must_use]
    pub fn from_shared(requested: Arc<AtomicBool>) -> Self {
        Self {
            requested,
            observed: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            checks: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            #[cfg(test)]
            request_after: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Requests cancellation of the one evaluation using this token.
    pub fn request(&self) {
        self.observed.store(true, Ordering::Release);
        self.requested.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_requested(&self) -> bool {
        if self.observed.load(Ordering::Acquire) {
            return true;
        }
        if self.requested.load(Ordering::Acquire) {
            self.observed.store(true, Ordering::Release);
            return true;
        }
        false
    }

    /// Fails with the non-recoverable evaluator cancellation diagnostic.
    pub(crate) fn check(&self) -> Result<(), EvaluationError> {
        #[cfg(test)]
        {
            let checks = self.checks.fetch_add(1, Ordering::Relaxed) + 1;
            let request_after = self.request_after.load(Ordering::Relaxed);
            if request_after != 0 && checks >= request_after {
                self.request();
            }
        }
        (!self.is_requested())
            .then_some(())
            .ok_or_else(|| error("ORNA-EVAL-CANCELLED"))
    }

    #[cfg(test)]
    pub(crate) fn request_after_checks(&self, checks: u64) {
        self.request_after.store(checks, Ordering::Relaxed);
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::CancellationToken;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::thread;

    #[test]
    fn shared_flag_true_is_latched_after_external_reset() {
        let shared = Arc::new(AtomicBool::new(true));
        let token = CancellationToken::from_shared(Arc::clone(&shared));

        assert!(token.is_requested());
        shared.store(false, Ordering::Release);
        assert!(token.is_requested());
    }

    #[test]
    fn owner_thread_source_request_cannot_be_uncancelled_by_source_reset() {
        let shared = Arc::new(AtomicBool::new(false));
        let token = CancellationToken::from_shared(Arc::clone(&shared));
        let owner = thread::spawn({
            let shared = Arc::clone(&shared);
            move || shared.store(true, Ordering::Release)
        });
        owner.join().expect("owner request thread panicked");

        assert!(
            token.is_requested(),
            "evaluator must observe the owner flag"
        );
        shared.store(false, Ordering::Release);
        assert!(token.clone().is_requested(), "clones must share the latch");
    }
}
