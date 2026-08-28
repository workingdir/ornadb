use std::{
    error::Error,
    fmt,
    num::NonZeroU64,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Live policy and cancellation fences shared by leases issued by one host.
///
/// The values are control-plane state only. They do not persist a lease or
/// perform an operation.
#[derive(Debug)]
pub(crate) struct LeaseFences {
    policy: AtomicU64,
    cancellation: AtomicU64,
}

impl LeaseFences {
    pub(crate) fn new(policy: u64, cancellation: u64) -> Arc<Self> {
        Arc::new(Self {
            policy: AtomicU64::new(policy),
            cancellation: AtomicU64::new(cancellation),
        })
    }

    pub(crate) fn current(&self) -> (u64, u64) {
        (
            self.policy.load(Ordering::SeqCst),
            self.cancellation.load(Ordering::SeqCst),
        )
    }

    pub(crate) fn set_policy(&self, value: u64) {
        self.policy.store(value, Ordering::SeqCst);
    }

    pub(crate) fn set_cancellation(&self, value: u64) {
        self.cancellation.store(value, Ordering::SeqCst);
    }
}

/// The lifecycle state of an ephemeral capability lease.
///
/// The states deliberately model the effect boundary rather than a retry
/// policy. In particular, [`Self::EffectUnknown`] is terminal: callers must
/// resolve an uncertain outcome outside this state machine and must not retry
/// through the lease.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LeaseState {
    /// The lease was admitted but has not been used.
    Active,
    /// The lease has been acquired for one operation.
    InUse,
    /// The effect intent has been recorded by the caller's control plane.
    EffectInFlight,
    /// The effect-start fence has been crossed.
    EffectStarted,
    /// The effect completed successfully.
    Committed,
    /// Revocation has started but has not reached its terminal state.
    Revoking,
    /// The lease was revoked without a committed effect.
    Revoked,
    /// The effect outcome cannot be classified as committed or no-effect.
    EffectUnknown,
    /// The lease has been released and cannot be used again.
    Released,
}

/// An immutable view of an ephemeral capability lease's identity and fences.
///
/// A snapshot never grants authority and contains no host handle. Its state is
/// the state at the instant the snapshot was taken; subsequent lease
/// transitions do not mutate an existing snapshot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LeaseSnapshot {
    invocation_id: NonZeroU64,
    state: LeaseState,
    policy_fence: u64,
    cancellation_fence: u64,
}

impl LeaseSnapshot {
    /// Returns the non-zero invocation identity as its numeric value.
    pub const fn invocation_id(self) -> u64 {
        self.invocation_id.get()
    }

    /// Returns the non-zero invocation identity without erasing its invariant.
    pub const fn nonzero_invocation_id(self) -> NonZeroU64 {
        self.invocation_id
    }

    /// Returns the lease state captured by this snapshot.
    pub const fn state(self) -> LeaseState {
        self.state
    }

    /// Returns the policy fence captured when the lease was issued.
    pub const fn policy_fence(self) -> u64 {
        self.policy_fence
    }

    /// Returns the cancellation fence captured when the lease was issued.
    pub const fn cancellation_fence(self) -> u64 {
        self.cancellation_fence
    }
}

/// Errors raised when a lease transition cannot be applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeaseError {
    /// The lease constructor was given the reserved zero invocation identity.
    ZeroInvocationId,
    /// The requested transition is not an edge from the current state.
    InvalidTransition {
        /// The state in which the operation was attempted.
        state: LeaseState,
        /// The transition operation that was requested.
        operation: &'static str,
    },
    /// A start or commit was presented with a fence different from the lease
    /// admission snapshot.
    FenceMismatch {
        /// The policy fence captured by the lease.
        expected_policy_fence: u64,
        /// The policy fence supplied by the caller.
        actual_policy_fence: u64,
        /// The cancellation fence captured by the lease.
        expected_cancellation_fence: u64,
        /// The cancellation fence supplied by the caller.
        actual_cancellation_fence: u64,
    },
}

impl fmt::Display for LeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroInvocationId => {
                formatter.write_str("CLIENT VM lease invocation identity must be non-zero")
            }
            Self::InvalidTransition { state, operation } => write!(
                formatter,
                "CLIENT VM lease operation {operation} is invalid in {state:?} state"
            ),
            Self::FenceMismatch {
                expected_policy_fence,
                actual_policy_fence,
                expected_cancellation_fence,
                actual_cancellation_fence,
            } => write!(
                formatter,
                "CLIENT VM lease fence mismatch: policy {actual_policy_fence} (expected {expected_policy_fence}), cancellation {actual_cancellation_fence} (expected {expected_cancellation_fence})"
            ),
        }
    }
}

impl Error for LeaseError {}

/// An opaque, invocation-scoped capability lease for the Stage 1 VM control
/// plane.
///
/// The lease is intentionally move-only and has no serialization, host
/// operation, clock, persistence, or retry behavior. The policy and
/// cancellation fences are immutable for the lease lifetime. Callers must
/// present both values again at the effect-start and commit fences.
pub struct EphemeralCapabilityLease {
    snapshot: LeaseSnapshot,
    fences: Arc<LeaseFences>,
}

impl EphemeralCapabilityLease {
    /// Creates an active lease for a non-zero invocation identity.
    ///
    /// `policy_fence` and `cancellation_fence` are caller-owned control-plane
    /// values captured in the lease snapshot. They may be zero when zero is
    /// the valid initial epoch for the owning controller; only the invocation
    /// identity has a non-zero construction requirement.
    #[cfg(test)]
    pub(crate) fn new(
        invocation_id: u64,
        policy_fence: u64,
        cancellation_fence: u64,
    ) -> Result<Self, LeaseError> {
        Self::new_with_fences(
            invocation_id,
            policy_fence,
            cancellation_fence,
            LeaseFences::new(policy_fence, cancellation_fence),
        )
    }

    pub(crate) fn new_with_fences(
        invocation_id: u64,
        policy_fence: u64,
        cancellation_fence: u64,
        fences: Arc<LeaseFences>,
    ) -> Result<Self, LeaseError> {
        let invocation_id = NonZeroU64::new(invocation_id).ok_or(LeaseError::ZeroInvocationId)?;
        Ok(Self {
            snapshot: LeaseSnapshot {
                invocation_id,
                state: LeaseState::Active,
                policy_fence,
                cancellation_fence,
            },
            fences,
        })
    }

    /// Returns the immutable lease snapshot at the time of this call.
    pub const fn snapshot(&self) -> LeaseSnapshot {
        self.snapshot
    }

    /// Returns the invocation identity bound to this lease.
    pub const fn invocation_id(&self) -> u64 {
        self.snapshot.invocation_id()
    }

    /// Returns the non-zero invocation identity without erasing its invariant.
    pub const fn nonzero_invocation_id(&self) -> NonZeroU64 {
        self.snapshot.nonzero_invocation_id()
    }

    /// Returns the current lifecycle state.
    pub const fn state(&self) -> LeaseState {
        self.snapshot.state
    }

    /// Returns the policy fence captured at lease creation.
    pub const fn policy_fence(&self) -> u64 {
        self.snapshot.policy_fence
    }

    /// Returns the cancellation fence captured at lease creation.
    pub const fn cancellation_fence(&self) -> u64 {
        self.snapshot.cancellation_fence
    }

    /// Acquires the active lease for one operation (`Active` -> `InUse`).
    pub fn acquire(&mut self) -> Result<(), LeaseError> {
        self.require_state(LeaseState::Active, "acquire")?;
        self.require_fences(self.snapshot.policy_fence, self.snapshot.cancellation_fence)?;
        self.snapshot.state = LeaseState::InUse;
        Ok(())
    }

    /// Alias for [`Self::acquire`] using the state-machine terminology.
    pub fn begin_use(&mut self) -> Result<(), LeaseError> {
        self.acquire()
    }

    /// Records the operation's effect intent (`InUse` -> `EffectInFlight`).
    ///
    /// This method only changes in-memory control-plane state. It does not
    /// append an audit record or perform a host effect.
    pub fn effect_intent(&mut self) -> Result<(), LeaseError> {
        self.require_state(LeaseState::InUse, "effect_intent")?;
        self.require_fences(self.snapshot.policy_fence, self.snapshot.cancellation_fence)?;
        self.snapshot.state = LeaseState::EffectInFlight;
        Ok(())
    }

    /// Crosses the effect-start fence (`EffectInFlight` -> `EffectStarted`).
    ///
    /// Both caller-supplied fences must match the immutable lease snapshot.
    /// A mismatch leaves the lease unchanged.
    pub fn effect_started(
        &mut self,
        policy_fence: u64,
        cancellation_fence: u64,
    ) -> Result<(), LeaseError> {
        self.require_state(LeaseState::EffectInFlight, "effect_started")?;
        self.require_fences(policy_fence, cancellation_fence)?;
        self.snapshot.state = LeaseState::EffectStarted;
        Ok(())
    }

    /// Commits the effect (`EffectStarted` -> `Committed`).
    ///
    /// Both caller-supplied fences must match the immutable lease snapshot.
    /// A mismatch leaves the lease unchanged.
    pub fn commit(&mut self, policy_fence: u64, cancellation_fence: u64) -> Result<(), LeaseError> {
        self.require_state(LeaseState::EffectStarted, "commit")?;
        self.require_fences(policy_fence, cancellation_fence)?;
        self.snapshot.state = LeaseState::Committed;
        Ok(())
    }

    /// Classifies the effect outcome as uncertain (`EffectStarted` ->
    /// `EffectUnknown`).
    ///
    /// This is terminal and never enables an automatic retry.
    pub fn mark_unknown(&mut self) -> Result<(), LeaseError> {
        self.require_state(LeaseState::EffectStarted, "mark_unknown")?;
        self.snapshot.state = LeaseState::EffectUnknown;
        Ok(())
    }

    /// Alias for [`Self::mark_unknown`].
    pub fn unknown(&mut self) -> Result<(), LeaseError> {
        self.mark_unknown()
    }

    /// Begins the explicit pre-effect revocation transition.
    ///
    /// `Active`, `InUse`, and `EffectInFlight` move to `Revoking`. An effect
    /// that has already crossed the start fence follows the ADR's direct
    /// `EffectStarted` -> `Revoked` edge and therefore does not expose an
    /// intermediate revocation state.
    pub fn begin_revoke(&mut self) -> Result<(), LeaseError> {
        match self.snapshot.state {
            LeaseState::Active | LeaseState::InUse | LeaseState::EffectInFlight => {
                self.snapshot.state = LeaseState::Revoking;
                Ok(())
            }
            LeaseState::EffectStarted => {
                self.snapshot.state = LeaseState::Revoked;
                Ok(())
            }
            state => Err(LeaseError::InvalidTransition {
                state,
                operation: "begin_revoke",
            }),
        }
    }

    /// Completes the explicit `Revoking` -> `Revoked` transition.
    pub fn finish_revoke(&mut self) -> Result<(), LeaseError> {
        self.require_state(LeaseState::Revoking, "finish_revoke")?;
        self.snapshot.state = LeaseState::Revoked;
        Ok(())
    }

    /// Revokes a lease through its allowed graph path in one convenience call.
    ///
    /// Call [`Self::begin_revoke`] and [`Self::finish_revoke`] separately when
    /// the intermediate `Revoking` state must be observed by a controller.
    pub fn revoke(&mut self) -> Result<(), LeaseError> {
        self.begin_revoke()?;
        if self.snapshot.state == LeaseState::Revoking {
            self.finish_revoke()?;
        }
        Ok(())
    }

    /// Releases a terminal lease (`Committed`, `Revoked`, or `EffectUnknown`).
    ///
    /// Non-terminal leases, including `Active` and `Revoking`, cannot be
    /// released directly. A released lease has no outgoing transitions.
    pub fn release(&mut self) -> Result<(), LeaseError> {
        match self.snapshot.state {
            LeaseState::Committed | LeaseState::Revoked | LeaseState::EffectUnknown => {
                self.snapshot.state = LeaseState::Released;
                Ok(())
            }
            state => Err(LeaseError::InvalidTransition {
                state,
                operation: "release",
            }),
        }
    }

    fn require_state(
        &self,
        expected: LeaseState,
        operation: &'static str,
    ) -> Result<(), LeaseError> {
        if self.snapshot.state == expected {
            Ok(())
        } else {
            Err(LeaseError::InvalidTransition {
                state: self.snapshot.state,
                operation,
            })
        }
    }

    fn require_fences(&self, policy_fence: u64, cancellation_fence: u64) -> Result<(), LeaseError> {
        let expected = (self.snapshot.policy_fence, self.snapshot.cancellation_fence);
        let supplied = (policy_fence, cancellation_fence);
        let current = self.fences.current();
        if supplied == expected && current == expected {
            return Ok(());
        }
        let actual = if supplied == expected {
            current
        } else {
            supplied
        };
        Err(LeaseError::FenceMismatch {
            expected_policy_fence: expected.0,
            actual_policy_fence: actual.0,
            expected_cancellation_fence: expected.1,
            actual_cancellation_fence: actual.1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY_FENCE: u64 = 11;
    const CANCELLATION_FENCE: u64 = 13;

    fn lease() -> EphemeralCapabilityLease {
        EphemeralCapabilityLease::new(7, POLICY_FENCE, CANCELLATION_FENCE).unwrap()
    }

    fn release_after(lease: &mut EphemeralCapabilityLease, state: LeaseState) {
        assert_eq!(lease.state(), state);
        lease.release().unwrap();
        assert_eq!(lease.state(), LeaseState::Released);
    }

    #[test]
    fn full_effect_path_reaches_committed_and_released() {
        let mut lease = lease();
        assert_eq!(lease.state(), LeaseState::Active);
        lease.acquire().unwrap();
        assert_eq!(lease.state(), LeaseState::InUse);
        lease.effect_intent().unwrap();
        assert_eq!(lease.state(), LeaseState::EffectInFlight);
        lease
            .effect_started(POLICY_FENCE, CANCELLATION_FENCE)
            .unwrap();
        assert_eq!(lease.state(), LeaseState::EffectStarted);
        lease.commit(POLICY_FENCE, CANCELLATION_FENCE).unwrap();
        release_after(&mut lease, LeaseState::Committed);
    }

    #[test]
    fn pre_effect_paths_revoke_through_revoking() {
        let mut active = lease();
        active.begin_revoke().unwrap();
        assert_eq!(active.state(), LeaseState::Revoking);
        assert!(active.release().is_err());
        active.finish_revoke().unwrap();
        release_after(&mut active, LeaseState::Revoked);

        let mut in_use = lease();
        in_use.acquire().unwrap();
        in_use.begin_revoke().unwrap();
        assert_eq!(in_use.state(), LeaseState::Revoking);
        in_use.finish_revoke().unwrap();
        release_after(&mut in_use, LeaseState::Revoked);

        let mut in_flight = lease();
        in_flight.acquire().unwrap();
        in_flight.effect_intent().unwrap();
        in_flight.begin_revoke().unwrap();
        assert_eq!(in_flight.state(), LeaseState::Revoking);
        in_flight.finish_revoke().unwrap();
        release_after(&mut in_flight, LeaseState::Revoked);
    }

    #[test]
    fn started_effect_can_be_revoked_or_marked_unknown() {
        let mut revoked = lease();
        revoked.acquire().unwrap();
        revoked.effect_intent().unwrap();
        revoked
            .effect_started(POLICY_FENCE, CANCELLATION_FENCE)
            .unwrap();
        revoked.begin_revoke().unwrap();
        release_after(&mut revoked, LeaseState::Revoked);

        let mut unknown = lease();
        unknown.acquire().unwrap();
        unknown.effect_intent().unwrap();
        unknown
            .effect_started(POLICY_FENCE, CANCELLATION_FENCE)
            .unwrap();
        unknown.mark_unknown().unwrap();
        release_after(&mut unknown, LeaseState::EffectUnknown);
    }

    #[test]
    fn convenience_revoke_completes_without_hiding_explicit_states() {
        let mut lease = lease();
        lease.revoke().unwrap();
        assert_eq!(lease.state(), LeaseState::Revoked);
        lease.release().unwrap();
    }

    #[test]
    fn illegal_transitions_leave_state_unchanged() {
        let mut lease = lease();
        assert!(matches!(
            lease.effect_intent(),
            Err(LeaseError::InvalidTransition {
                state: LeaseState::Active,
                operation: "effect_intent",
            })
        ));
        assert_eq!(lease.state(), LeaseState::Active);

        lease.acquire().unwrap();
        assert!(matches!(
            lease.commit(POLICY_FENCE, CANCELLATION_FENCE),
            Err(LeaseError::InvalidTransition {
                state: LeaseState::InUse,
                operation: "commit",
            })
        ));
        assert_eq!(lease.state(), LeaseState::InUse);

        lease.effect_intent().unwrap();
        lease
            .effect_started(POLICY_FENCE, CANCELLATION_FENCE)
            .unwrap();
        lease.commit(POLICY_FENCE, CANCELLATION_FENCE).unwrap();
        assert!(matches!(
            lease.mark_unknown(),
            Err(LeaseError::InvalidTransition {
                state: LeaseState::Committed,
                operation: "mark_unknown",
            })
        ));
        assert_eq!(lease.state(), LeaseState::Committed);
        lease.release().unwrap();
        assert!(matches!(
            lease.acquire(),
            Err(LeaseError::InvalidTransition {
                state: LeaseState::Released,
                operation: "acquire",
            })
        ));
    }

    #[test]
    fn effect_fences_must_match_for_start_and_commit() {
        let mut lease = lease();
        lease.acquire().unwrap();
        lease.effect_intent().unwrap();
        let mismatch = lease
            .effect_started(POLICY_FENCE + 1, CANCELLATION_FENCE)
            .unwrap_err();
        assert_eq!(
            mismatch,
            LeaseError::FenceMismatch {
                expected_policy_fence: POLICY_FENCE,
                actual_policy_fence: POLICY_FENCE + 1,
                expected_cancellation_fence: CANCELLATION_FENCE,
                actual_cancellation_fence: CANCELLATION_FENCE,
            }
        );
        assert_eq!(lease.state(), LeaseState::EffectInFlight);

        lease
            .effect_started(POLICY_FENCE, CANCELLATION_FENCE)
            .unwrap();
        let mismatch = lease
            .commit(POLICY_FENCE, CANCELLATION_FENCE + 1)
            .unwrap_err();
        assert!(matches!(mismatch, LeaseError::FenceMismatch { .. }));
        assert_eq!(lease.state(), LeaseState::EffectStarted);
        lease.commit(POLICY_FENCE, CANCELLATION_FENCE).unwrap();
    }

    #[test]
    fn release_is_allowed_only_from_terminal_effect_states() {
        for state in [
            LeaseState::Active,
            LeaseState::InUse,
            LeaseState::EffectInFlight,
        ] {
            let mut lease = lease();
            if state != LeaseState::Active {
                lease.acquire().unwrap();
            }
            if state == LeaseState::EffectInFlight {
                lease.effect_intent().unwrap();
            }
            assert!(matches!(
                lease.release(),
                Err(LeaseError::InvalidTransition {
                    operation: "release",
                    ..
                })
            ));
            assert_eq!(lease.state(), state);
        }

        let mut revoking = lease();
        revoking.begin_revoke().unwrap();
        assert!(revoking.release().is_err());
        assert_eq!(revoking.state(), LeaseState::Revoking);

        let mut committed = lease();
        committed.acquire().unwrap();
        committed.effect_intent().unwrap();
        committed
            .effect_started(POLICY_FENCE, CANCELLATION_FENCE)
            .unwrap();
        committed.commit(POLICY_FENCE, CANCELLATION_FENCE).unwrap();
        committed.release().unwrap();
        assert!(committed.release().is_err());
    }

    #[test]
    fn snapshot_captures_identity_and_fences_without_aliasing_state() {
        let mut lease = lease();
        let snapshot = lease.snapshot();
        assert_eq!(snapshot.invocation_id(), 7);
        assert_eq!(snapshot.policy_fence(), POLICY_FENCE);
        assert_eq!(snapshot.cancellation_fence(), CANCELLATION_FENCE);
        assert_eq!(snapshot.state(), LeaseState::Active);

        lease.acquire().unwrap();
        assert_eq!(snapshot.state(), LeaseState::Active);
        assert_eq!(lease.snapshot().state(), LeaseState::InUse);
    }

    #[test]
    fn zero_invocation_identity_is_rejected() {
        assert!(matches!(
            EphemeralCapabilityLease::new(0, POLICY_FENCE, CANCELLATION_FENCE),
            Err(LeaseError::ZeroInvocationId)
        ));
    }
}
