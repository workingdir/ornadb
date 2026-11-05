//! Explicit native clock operations admitted through the generated host registry.

use std::time::Duration;

use orna_sys_macros::sys_host_operation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockProviderError {
    Denied,
    Cancelled,
}

impl ClockProviderError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Denied => "sys.host.clock.denied",
            Self::Cancelled => "sys.host.clock.cancelled",
        }
    }
}

/// An optional bounded wall-clock waiting capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockProvider {
    maximum_sleep: Duration,
}

impl ClockProvider {
    /// Authorizes sleeps no longer than `maximum_sleep`.
    #[must_use]
    pub const fn new(maximum_sleep: Duration) -> Self {
        Self { maximum_sleep }
    }

    #[sys_host_operation(
        r###"{"name":"std.concurrent.sleep","version":{"major":1,"minor":0},"signature":"fn std.concurrent.sleep(duration: Duration): Null","effects":["invoke"],"preconditions":["the host explicitly installed a clock provider","duration is nonnegative and no greater than the configured maximum"],"failures":["sys.host.clock.denied","sys.host.clock.cancelled"],"role":"host.std.concurrent.clock@1.0","provider":"orna.sys.host.clock.v1","implementation":"sleep"}"###
    )]
    pub fn sleep(&self, duration: Duration) -> Result<(), ClockProviderError> {
        self.sleep_with_cancellation(duration, || false)
    }

    /// Waits in short intervals so an evaluator cancellation request can stop
    /// the host wait promptly without detaching work.
    pub fn sleep_with_cancellation(
        &self,
        duration: Duration,
        mut cancellation_requested: impl FnMut() -> bool,
    ) -> Result<(), ClockProviderError> {
        if duration > self.maximum_sleep {
            return Err(ClockProviderError::Denied);
        }
        let deadline = std::time::Instant::now()
            .checked_add(duration)
            .ok_or(ClockProviderError::Denied)?;
        loop {
            if cancellation_requested() {
                return Err(ClockProviderError::Cancelled);
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_wait_observes_cancellation_before_blocking() {
        let provider = ClockProvider::new(Duration::from_secs(5));
        assert_eq!(
            provider.sleep_with_cancellation(Duration::from_secs(5), || true),
            Err(ClockProviderError::Cancelled)
        );
    }
}
