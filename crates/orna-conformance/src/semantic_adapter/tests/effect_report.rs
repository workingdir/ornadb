use super::{ActivationEffectReport, SysHostAttachment, SysHostContext, declared_effect_report};
use orna_sys::{RuntimeMode, StdSnapshotPin};

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
fn attached_system_host_declares_external_effects() {
    let attachment = SysHostAttachment::new(host_context());
    assert!(matches!(
        declared_effect_report(Some(&attachment)),
        ActivationEffectReport::ExternalPossible
    ));
}

#[test]
fn detached_activation_declares_controlled_effects() {
    assert!(matches!(
        declared_effect_report(None),
        ActivationEffectReport::ControlledOnly(_)
    ));
}
