//! External-state evidence for the runtime effect gate (REQUEST-1 step 5,
//! ORNA-CAP-005). This is the first-party producer of
//! [`orna_runtime::ExternalStateEvidence`]: it reports the host objects an
//! evaluation is bound to, and [`orna_runtime::EffectGate::admit`] reads only
//! that answer.

use orna_runtime::ExternalStateEvidence;

use crate::{SysHostAttachment, SysHostBindingRegistry};

/// The host objects an evaluation is bound to, as external-state evidence.
///
/// An attached system host, or a registry whose
/// [`SysHostBindingRegistry::may_reach_external_state`] is true, may reach
/// state outside Orna's table transaction. Only [`Self::unbound`] reports
/// none.
#[derive(Clone, Copy, Debug)]
pub struct SysHostEffectEvidence<'a> {
    attachment: Option<&'a SysHostAttachment>,
    bindings: Option<&'a SysHostBindingRegistry>,
}

impl<'a> SysHostEffectEvidence<'a> {
    /// Evidence for an evaluation bound to no host object.
    #[must_use]
    pub const fn unbound() -> Self {
        Self {
            attachment: None,
            bindings: None,
        }
    }

    /// Evidence for an evaluation bound to the given host objects.
    #[must_use]
    pub const fn bound(
        attachment: Option<&'a SysHostAttachment>,
        bindings: Option<&'a SysHostBindingRegistry>,
    ) -> Self {
        Self {
            attachment,
            bindings,
        }
    }
}

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
    use orna_runtime::{ActivationEffectReport, EffectGate, ExternalStateEvidence};
    use orna_sys::{RuntimeMode, StdSnapshotPin};

    use super::SysHostEffectEvidence;
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
    fn unbound_evidence_reports_no_external_reach() {
        assert!(!SysHostEffectEvidence::unbound().may_reach_external_state());
    }

    #[test]
    fn evidence_bound_to_nothing_reports_no_external_reach() {
        assert!(!SysHostEffectEvidence::bound(None, None).may_reach_external_state());
    }

    #[test]
    fn attached_host_reports_external_reach() {
        let attachment = SysHostAttachment::new(host_context());
        assert!(SysHostEffectEvidence::bound(Some(&attachment), None).may_reach_external_state());
    }

    #[test]
    fn binding_registry_reports_external_reach() {
        let registry = SysHostBindingRegistry::default();
        assert!(SysHostEffectEvidence::bound(None, Some(&registry)).may_reach_external_state());
    }

    #[test]
    fn gate_admits_unbound_evaluation_as_controlled() {
        assert!(matches!(
            EffectGate::admit(&SysHostEffectEvidence::unbound()),
            ActivationEffectReport::ControlledOnly(_)
        ));
    }

    #[test]
    fn gate_admits_attached_host_as_external() {
        let attachment = SysHostAttachment::new(host_context());
        assert!(matches!(
            EffectGate::admit(&SysHostEffectEvidence::bound(Some(&attachment), None)),
            ActivationEffectReport::ExternalPossible
        ));
    }
}
