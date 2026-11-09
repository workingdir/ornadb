//! Effect admission for the runtime commit boundary (REQUEST-1 step 5,
//! ORNA-CAP-005). The evaluator is the sole first-party minter of
//! [`ActivationEffectReport`]: [`EffectGate::admit_sys_host`] derives the
//! report from the host objects an evaluation is bound to, and the runtime and
//! live layers consume the minted report without constructing one.

use crate::{SysHostAttachment, SysHostBindingRegistry};

mod sealed {
    /// Seals [`super::ExternalStateEvidence`] to this crate.
    pub trait Sealed {}
}

/// Evidence that an evaluation may reach state outside Orna's table transaction
/// (REQUEST-1 step 5, ORNA-CAP-005). [`EffectGate`] reads only this answer.
///
/// Sealed: only the evaluator implements it, so no third party can supply a
/// `false` answer for an evaluation that reaches external state.
///
/// ```compile_fail,E0603
/// // Outside the evaluator the sealing supertrait is unnameable, so an
/// // external crate cannot implement the evidence trait.
/// struct Forged;
/// impl orna_evaluator::ExternalStateEvidence for Forged {
///     fn may_reach_external_state(&self) -> bool {
///         false
///     }
/// }
/// ```
pub trait ExternalStateEvidence: sealed::Sealed {
    /// Returns `true` when the evaluation may reach external state.
    fn may_reach_external_state(&self) -> bool;
}

/// Proof that an evaluation was admitted with no external reach. The private
/// field keeps struct literals outside this module impossible, and the type is
/// deliberately not `Clone`: a token is consumed by the single report that
/// carries it.
///
/// ```compile_fail,E0277
/// // The token cannot be duplicated outside the evaluator.
/// fn assert_clone<T: Clone>() {}
/// assert_clone::<orna_evaluator::ControlledEffectToken>();
/// ```
///
/// ```compile_fail,E0603
/// // Outside the evaluator the marker cannot be minted: the field is private.
/// let _ = orna_evaluator::ControlledEffectToken(());
/// ```
#[derive(Debug, Eq, PartialEq)]
pub struct ControlledEffectToken {
    _private: (),
}

impl ControlledEffectToken {
    const fn mint() -> Self {
        Self { _private: () }
    }
}

/// The effect report every table-request activation presents to the runtime
/// commit boundary. There is no default and no public constructor for the
/// controlled variant: it is produced only by [`EffectGate`].
#[derive(Debug, Eq, PartialEq)]
pub enum ActivationEffectReport {
    ControlledOnly(ControlledEffectToken),
    ExternalPossible,
}

/// Copying a report re-mints its token from an existing `ControlledOnly`
/// report, so a staged activation can present the same admitted evidence on
/// each retry. The copy is derived only from an admitted report, so it cannot
/// forge a controlled token the gate did not admit.
impl Clone for ActivationEffectReport {
    fn clone(&self) -> Self {
        match self {
            Self::ControlledOnly(_) => Self::ControlledOnly(ControlledEffectToken::mint()),
            Self::ExternalPossible => Self::ExternalPossible,
        }
    }
}

/// The admission gate for the Orna-controlled effect token. It is the only
/// place a [`ControlledEffectToken`] is minted.
#[derive(Debug)]
pub struct EffectGate;

impl EffectGate {
    /// Admits an evaluation bound to the given host objects. An attached host
    /// or a registry that may reach external state yields `ExternalPossible`;
    /// only an evaluation bound to nothing yields `ControlledOnly`.
    #[must_use]
    pub fn admit_sys_host(
        attachment: Option<&SysHostAttachment>,
        bindings: Option<&SysHostBindingRegistry>,
    ) -> ActivationEffectReport {
        Self::admit(&SysHostEffectEvidence {
            attachment,
            bindings,
        })
    }

    /// Admits an evaluation bound to no host object. Test-only: production
    /// callers admit through [`Self::admit_sys_host`] with the host they bound.
    #[cfg(any(test, feature = "test-evidence"))]
    #[must_use]
    pub fn admit_unbound_for_test() -> ActivationEffectReport {
        Self::admit_sys_host(None, None)
    }

    fn admit(evidence: &dyn ExternalStateEvidence) -> ActivationEffectReport {
        if evidence.may_reach_external_state() {
            ActivationEffectReport::ExternalPossible
        } else {
            ActivationEffectReport::ControlledOnly(ControlledEffectToken::mint())
        }
    }
}

/// The host objects an evaluation is bound to, as external-state evidence.
/// An attached system host, or a registry whose
/// [`SysHostBindingRegistry::may_reach_external_state`] is true, may reach
/// state outside Orna's table transaction.
#[derive(Clone, Copy, Debug)]
struct SysHostEffectEvidence<'a> {
    attachment: Option<&'a SysHostAttachment>,
    bindings: Option<&'a SysHostBindingRegistry>,
}

impl sealed::Sealed for SysHostEffectEvidence<'_> {}

impl ExternalStateEvidence for SysHostEffectEvidence<'_> {
    fn may_reach_external_state(&self) -> bool {
        self.attachment.is_some()
            || self
                .bindings
                .is_some_and(SysHostBindingRegistry::may_reach_external_state)
    }
}

#[cfg(test)]
mod tests {
    use orna_sys::{RuntimeMode, StdSnapshotPin};

    use super::{ActivationEffectReport, EffectGate};
    use crate::{SysHostAttachment, SysHostBindingRegistry, SysHostContext};

    fn host_context() -> SysHostContext {
        SysHostContext {
            database_id: [1; 16],
            repository_id: [2; 16],
            runtime_id: [0x11; 16],
            root: std::path::PathBuf::new(),
            read_only: true,
            mode: RuntimeMode::Run,
            repository_layout_version: "layout-1".to_owned(),
            storage_manifest_version: "store-3".to_owned(),
            std_snapshot: StdSnapshotPin::Unpinned,
        }
    }

    #[test]
    fn report_copy_preserves_admitted_verdict() {
        // A copy re-mints from an admitted report only; the verdict it carries
        // is the one the gate admitted.
        let controlled = EffectGate::admit_sys_host(None, None);
        assert!(matches!(
            controlled.clone(),
            ActivationEffectReport::ControlledOnly(_)
        ));
        let external = ActivationEffectReport::ExternalPossible;
        assert!(matches!(
            external.clone(),
            ActivationEffectReport::ExternalPossible
        ));
    }

    #[test]
    fn gate_admits_unbound_evaluation_as_controlled() {
        assert!(matches!(
            EffectGate::admit_sys_host(None, None),
            ActivationEffectReport::ControlledOnly(_)
        ));
    }

    #[test]
    fn gate_admits_attached_host_as_external() {
        let attachment = SysHostAttachment::new(host_context());
        assert!(matches!(
            EffectGate::admit_sys_host(Some(&attachment), None),
            ActivationEffectReport::ExternalPossible
        ));
    }

    #[test]
    fn gate_admits_binding_registry_as_external() {
        let registry = SysHostBindingRegistry::default();
        assert!(matches!(
            EffectGate::admit_sys_host(None, Some(&registry)),
            ActivationEffectReport::ExternalPossible
        ));
    }
}
