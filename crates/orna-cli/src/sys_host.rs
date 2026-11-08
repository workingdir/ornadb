//! Builds the `sys.rt` host context that CLI-owned runtimes attach to their
//! evaluation (ORNA-SYS-006, ORNA-SYS-022..030).

use std::path::Path;

use orna_project::LoadedProject;
use orna_repository::{GitHashAlgorithm, NativeOid, REPOSITORY_LAYOUT_VERSION, STORAGE_PROFILE};
use orna_runtime::RuntimeIdentity;
use orna_sys::{RuntimeMode, StdSnapshotPin, SysHostContext, new_runtime_id};

/// Describes one writable runtime attachment rooted at the repository worktree.
/// Every CLI attachment is writable, so the carrier always reports
/// `read_only: false`.
pub(crate) fn sys_host_context(
    root: &Path,
    identity: RuntimeIdentity,
    mode: RuntimeMode,
    std_snapshot: StdSnapshotPin,
) -> SysHostContext {
    SysHostContext {
        database_id: identity.database_id,
        repository_id: identity.repository_id,
        runtime_id: new_runtime_id(),
        root: root.to_path_buf(),
        read_only: false,
        mode,
        repository_layout_version: REPOSITORY_LAYOUT_VERSION.to_owned(),
        storage_manifest_version: STORAGE_PROFILE.to_owned(),
        std_snapshot,
    }
}

/// Pins the selected `std` package to its retained source snapshot. A project
/// without a standard dependency profile is unpinned; a snapshot that is not a
/// valid commit object id is unreadable and fails closed in `sys.rt.info`.
pub(crate) fn std_snapshot_pin(project: &LoadedProject) -> StdSnapshotPin {
    let Some(profile) = project.standard_profile() else {
        return StdSnapshotPin::Unpinned;
    };
    let snapshot = profile.snapshot();
    let algorithm = match snapshot.len() {
        40 => GitHashAlgorithm::Sha1,
        64 => GitHashAlgorithm::Sha256,
        _ => return StdSnapshotPin::Unreadable,
    };
    match NativeOid::from_hex(algorithm, snapshot) {
        Ok(oid) => StdSnapshotPin::Commit(oid.as_bytes().to_vec()),
        Err(_) => StdSnapshotPin::Unreadable,
    }
}

#[cfg(test)]
mod tests {
    use super::sys_host_context;
    use orna_application::{ApplicationAuthority, ApplicationLiveAdapter};
    use orna_evaluator::Limits;
    use orna_live::LiveApplication;
    use orna_runtime::RuntimeIdentity;
    use orna_semantic::Catalogue;
    use orna_sys::{RuntimeMode, StdSnapshotPin};
    use std::path::Path;

    fn identity() -> RuntimeIdentity {
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        }
    }

    #[test]
    fn detached_adapter_trusts_controlled_effect_reports() {
        let adapter = ApplicationLiveAdapter::new(ApplicationAuthority::new(
            Catalogue::authoritative_core(),
            Limits::default(),
        ));
        assert!(
            adapter.trusts_controlled_effect_reports(),
            "an adapter with no bound sys host reports only its own controlled effects"
        );
    }

    #[test]
    fn host_bound_adapter_never_trusts_controlled_effect_reports() {
        let context = sys_host_context(
            Path::new("."),
            identity(),
            RuntimeMode::Serve,
            StdSnapshotPin::Unpinned,
        );
        let adapter = ApplicationLiveAdapter::new(
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default())
                .with_sys_host_context(context),
        );
        assert!(
            !adapter.trusts_controlled_effect_reports(),
            "a bound sys host can reach state the handler does not observe, so its reports are downgraded"
        );
    }
}
