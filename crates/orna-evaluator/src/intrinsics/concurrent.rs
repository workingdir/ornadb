//! Standard-library dispatch for the concurrent family.
//!
//! Pure motion from `lib.rs` (stage S6): the dispatch functions and their
//! helpers keep their bodies; only the module boundary changed.

use crate::*;
use crate::{Context, Value, error};

impl Context<'_, '_> {
    pub(crate) fn concurrent(
        &mut self,
        name: &str,
        values: Vec<Value>,
    ) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("parallel", [Value::List(callbacks)]) => self.concurrent_parallel(callbacks),
            ("race", [Value::List(callbacks)]) => self.concurrent_race(callbacks),
            (
                "timeout",
                [
                    callback,
                    Value::Duration {
                        seconds,
                        nanosecond,
                    },
                ],
            ) => self.concurrent_timeout(callback, seconds, *nanosecond),
            ("parallel" | "race", [_]) | ("timeout", [_, _]) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    /// Runs every callback as a parallel task; a non-cancellation failure
    /// aggregates into one error before any value is returned.
    fn concurrent_parallel(&mut self, callbacks: &List) -> Result<Value, EvaluationError> {
        self.items(callbacks.len())?;
        if callbacks.is_empty() {
            return Ok(Value::list(Vec::new()));
        }
        let (outcomes, _) = self.fork_task_callbacks(callbacks, TaskPolicy::Parallel)?;
        let failures = outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref()?.as_ref().err())
            .filter(|failure| failure.code() != "ORNA-EVAL-CANCELLED")
            .cloned()
            .collect::<Vec<_>>();
        if !failures.is_empty() {
            return Err(aggregate_task_failures(&failures));
        }
        let mut output = Vec::with_capacity(outcomes.len());
        for outcome in outcomes {
            match outcome {
                Some(Ok(value)) => output.push(value),
                Some(Err(failure)) => return Err(failure),
                None => return Err(error("ORNA-EVAL-CANCELLED")),
            }
        }
        Ok(Value::list(output))
    }
    /// Races the callbacks and returns the winner, or the first non-cancelled
    /// failure when no task produced a value.
    fn concurrent_race(&mut self, callbacks: &List) -> Result<Value, EvaluationError> {
        self.items(callbacks.len())?;
        if callbacks.is_empty() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let (outcomes, winner) = self.fork_task_callbacks(callbacks, TaskPolicy::Race)?;
        if let Some(winner) = winner {
            return Ok(winner);
        }
        outcomes
            .into_iter()
            .find_map(|outcome| {
                outcome.and_then(|result| match result {
                    Err(failure) if failure.code() != "ORNA-EVAL-CANCELLED" => Some(Err(failure)),
                    _ => None,
                })
            })
            .unwrap_or_else(|| Err(error("ORNA-EVAL-ERROR")))
    }
    /// Runs one callback under a wall-clock timeout on a scoped thread. The
    /// parent's cancellation request wins over the timeout, and the child's
    /// materializations are absorbed after the thread joins.
    fn concurrent_timeout(
        &mut self,
        callback: &Value,
        seconds: &BigInt,
        nanosecond: u32,
    ) -> Result<Value, EvaluationError> {
        self.check_call_room()?;
        if seconds.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let seconds = seconds.to_u64().ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        let timeout = std::time::Duration::new(seconds, nanosecond);
        let shared_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let parent = self.cancellation.cloned();
        let environment = self.task_environment();
        self.disable_call_reuse_for_fork();
        let child_effects = self
            .effects
            .as_deref_mut()
            .and_then(|effects| effects.fork_task_child(0));
        let materializations: MaterializationSink = Arc::new(std::sync::Mutex::new(Vec::new()));
        let result = std::thread::scope(|scope| {
            let (sender, receiver) = std::sync::mpsc::channel();
            let shared_for_task = Arc::clone(&shared_cancel);
            let callback = callback.clone();
            let task_parent = parent.clone();
            let sink_for_task = Arc::clone(&materializations);
            scope.spawn(move || {
                let cancellation =
                    CancellationToken::child(Arc::clone(&shared_for_task), task_parent);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    invoke_task_callback(
                        callback,
                        environment,
                        cancellation,
                        child_effects,
                        &sink_for_task,
                    )
                }))
                .unwrap_or_else(|_| Err(error("ORNA-EVAL-ERROR")));
                let _ = sender.send(result);
            });
            let started = std::time::Instant::now();
            loop {
                if parent.as_ref().is_some_and(CancellationToken::is_requested) {
                    shared_cancel.store(true, std::sync::atomic::Ordering::Release);
                    while receiver.recv().is_ok() {}
                    return Err(error("ORNA-EVAL-CANCELLED"));
                }
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    match receiver.try_recv() {
                        Ok(result) => return result,
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            shared_cancel.store(true, std::sync::atomic::Ordering::Release);
                            let _ = receiver.recv();
                            return Err(error("ORNA-EVAL-TIMEOUT"));
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            return Err(error("ORNA-EVAL-ERROR"));
                        }
                    }
                }
                match receiver.recv_timeout(remaining.min(std::time::Duration::from_millis(10))) {
                    Ok(result) => return result,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(error("ORNA-EVAL-ERROR"));
                    }
                }
            }
        });
        self.absorb_materializations(vec![take_materializations(&materializations)])?;
        result
    }
}
