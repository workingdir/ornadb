//! Builds the `sys.rt` host context that CLI-owned runtimes attach to their
//! evaluation (ORNA-SYS-006, ORNA-SYS-022..030).

use std::path::Path;

use orna_project::LoadedProject;
use orna_repository::{
    GitCommitRef, GitHashAlgorithm, NativeOid, REPOSITORY_LAYOUT_VERSION, STORAGE_PROFILE,
};
use orna_runtime::RuntimeIdentity;
use orna_sys::{RuntimeMode, StdSnapshotPin, SysHostContext, new_runtime_id};

/// Describes one writable runtime attachment rooted at the repository worktree.
/// Every CLI attachment is writable, so the carrier always reports
/// `read_only: false`.
///
/// `head` is the commit every reference the activation serves from the
/// repository's Git state (`sys.git.branches`) is pinned to. Callers pass the
/// commit the activation's catalogue rows are pinned to, so one activation
/// carries one pin; `None` (an unborn or unreadable `HEAD`) leaves those rows
/// unservable rather than pinned to a commit the activation does not read.
pub(crate) fn sys_host_context(
    root: &Path,
    identity: RuntimeIdentity,
    mode: RuntimeMode,
    std_snapshot: StdSnapshotPin,
    head: Option<&GitCommitRef>,
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
        head_commit: head.and_then(commit_object_id),
    }
}

/// The raw Git object id of `commit`, or `None` when its text is not one.
fn commit_object_id(commit: &GitCommitRef) -> Option<Vec<u8>> {
    let algorithm = match commit.as_str().len() {
        40 => GitHashAlgorithm::Sha1,
        64 => GitHashAlgorithm::Sha256,
        _ => return None,
    };
    NativeOid::from_hex(algorithm, commit.as_str())
        .ok()
        .map(|oid| oid.as_bytes().to_vec())
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
    use orna_check::Catalogue;
    use orna_evaluator::Limits;
    use orna_live::LiveApplication;
    use orna_runtime::RuntimeIdentity;
    use orna_sys::{RuntimeMode, StdSnapshotPin};
    use std::path::Path;

    fn identity() -> RuntimeIdentity {
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        }
    }

    #[test]
    fn detached_adapter_holds_first_party_effect_trust() {
        let adapter = ApplicationLiveAdapter::new(ApplicationAuthority::new(
            Catalogue::authoritative_core(),
            Limits::default(),
        ));
        assert!(
            adapter.first_party_effect_trust().is_some(),
            "an adapter with no bound sys host reports only its own controlled effects"
        );
    }

    #[test]
    fn host_bound_adapter_never_holds_first_party_effect_trust() {
        let context = sys_host_context(
            Path::new("."),
            identity(),
            RuntimeMode::Serve,
            StdSnapshotPin::Unpinned,
            None,
        );
        let adapter = ApplicationLiveAdapter::new(
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default())
                .with_sys_host_context(context),
        );
        assert!(
            adapter.first_party_effect_trust().is_none(),
            "a bound sys host can reach state the handler does not observe, so its reports are downgraded"
        );
    }
}
