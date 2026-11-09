//! Private pin refs of one graph owner: their names, their durability and
//! their reconciliation after an owner dies.
//!
//! Capture, promotion and row-candidate preparation root OGB-2 closures under
//! `refs/orna/pins/<owner>/...`. These refs are the only durable evidence that
//! a closure is still reachable, so every mutation here is followed by an
//! explicit directory sync, and reconciliation deletes nothing until it has
//! proven the ref is neither accepted nor owned by a publisher.

use super::*;

/// Runs Git plumbing for one repository worktree on behalf of the private pin
/// refs this module manages, with object discovery pinned to that worktree.
#[derive(Debug)]
pub(super) struct PrivateRefCleanupOwner {
    pub(super) repository: crate::Repository,
}

impl PrivateRefCleanupOwner {
    pub(super) fn git_command(&self) -> Command {
        let mut command = crate::git_command_in(self.repository.worktree());
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_OPTIONAL_LOCKS", "0");
        command
    }

    pub(super) fn git_output(&self, args: &[&str]) -> Result<Vec<u8>, GraphError> {
        let output = self
            .git_command()
            .args(args)
            .output()
            .map_err(|_| GraphError::GitCommandFailed)?;
        if !output.status.success() {
            // A `cat-file` invocation that names one object reports an object
            // that is absent locally. Name it, and say whether a partial clone
            // defers it, so a promised object is never an anonymous
            // availability failure (ORNA-GIT-002, ORNA-ROW-007).
            return Err(match cat_file_object_id(args) {
                Some(object_id) => absent_object_error(&self.repository, object_id),
                None => GraphError::GitCommandFailed,
            });
        }
        Ok(output.stdout)
    }

    pub(super) fn create_protected_ref(
        &self,
        reference: &str,
        oid: &NativeOid,
    ) -> Result<(), GraphError> {
        let zero = "0".repeat(oid.algorithm().width() * 2);
        self.git_output(&["update-ref", reference, &oid.to_hex(), &zero])?;
        self.sync_ref_file(reference)
    }

    pub(super) fn sync_ref_file(&self, reference: &str) -> Result<(), GraphError> {
        let output = self.git_output(&["rev-parse", "--git-path", reference])?;
        let path = resolve_git_path(self.repository.worktree(), &output)?;
        let metadata = fs::symlink_metadata(&path).map_err(|_| GraphError::DurabilityFailed)?;
        if !metadata.file_type().is_file() {
            return Err(GraphError::DurabilityFailed);
        }
        File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(|_| GraphError::DurabilityFailed)?;
        self.sync_ref_directories(reference)
    }

    pub(super) fn delete_protected_ref(
        &self,
        reference: &str,
        oid: &NativeOid,
    ) -> Result<(), GraphError> {
        self.git_output(&["update-ref", "-d", reference, &oid.to_hex()])?;
        self.sync_ref_directories(reference)
    }

    pub(super) fn sync_ref_directories(&self, reference: &str) -> Result<(), GraphError> {
        let common_dir_output = self.git_output(&["rev-parse", "--git-common-dir"])?;
        let common_dir = resolve_git_path(self.repository.worktree(), &common_dir_output)?;
        let common_dir = fs::canonicalize(common_dir).map_err(|_| GraphError::DurabilityFailed)?;
        let ref_path_output = self.git_output(&["rev-parse", "--git-path", reference])?;
        let ref_path = resolve_git_path(self.repository.worktree(), &ref_path_output)?;
        let mut directory = ref_path
            .parent()
            .ok_or(GraphError::DurabilityFailed)?
            .to_path_buf();
        let directory = loop {
            match fs::canonicalize(&directory) {
                Ok(directory) => break directory,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if !directory.pop() {
                        return Err(GraphError::DurabilityFailed);
                    }
                }
                Err(_) => return Err(GraphError::DurabilityFailed),
            }
        };
        if !directory.starts_with(&common_dir) {
            return Err(GraphError::DurabilityFailed);
        }
        let mut current = directory;
        loop {
            sync_directory(&current)?;
            if current == common_dir || !current.pop() {
                break;
            }
        }
        let packed_refs = common_dir.join("packed-refs");
        match fs::symlink_metadata(&packed_refs) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(GraphError::DurabilityFailed);
                }
                File::open(&packed_refs)
                    .and_then(|file| file.sync_all())
                    .map_err(|_| GraphError::DurabilityFailed)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(GraphError::DurabilityFailed),
        }
        Ok(())
    }
}

/// A private pin ref that is deleted when this handle drops, unless the ref
/// was disarmed because something durable now owns the closure.
#[derive(Debug)]
pub(super) struct PendingPrivateRefCleanup {
    pub(super) owner: Arc<PrivateRefCleanupOwner>,
    pub(super) reference: String,
    pub(super) oid: NativeOid,
    armed: bool,
}

impl PendingPrivateRefCleanup {
    pub(super) fn new(
        owner: Arc<PrivateRefCleanupOwner>,
        reference: String,
        oid: NativeOid,
    ) -> Self {
        Self {
            owner,
            reference,
            oid,
            armed: true,
        }
    }

    pub(super) fn cleanup(&mut self) -> Result<(), GraphError> {
        self.cleanup_with_identity().map_err(|(_, error)| error)
    }

    pub(super) fn cleanup_with_identity(&mut self) -> Result<(), (String, GraphError)> {
        if self.armed {
            self.owner
                .delete_protected_ref(&self.reference, &self.oid)
                .map_err(|error| (self.reference.clone(), error))?;
            self.armed = false;
        }
        Ok(())
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingPrivateRefCleanup {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

/// Scratch ref a capture writes its closure under before the content is
/// accepted as a row value.
pub(super) fn scratch_content_ref(owner_id: &[u8; 16], pin_id: &[u8; 16]) -> String {
    format!(
        "refs/orna/pins/{}/scratch/{}",
        hex_encode(owner_id),
        hex_encode(pin_id)
    )
}

/// Pending ref an accepted content pin holds its closure under.
pub(super) fn accepted_content_ref(owner_id: &[u8; 16], pin_id: &[u8; 16]) -> String {
    format!(
        "refs/orna/pins/{}/pending/{}",
        hex_encode(owner_id),
        hex_encode(pin_id)
    )
}

/// Native hash algorithm for one hexadecimal object id, by its exact width.
fn algorithm_for_hex(hex: &str) -> Option<GitHashAlgorithm> {
    match hex.len() {
        40 => Some(GitHashAlgorithm::Sha1),
        64 => Some(GitHashAlgorithm::Sha256),
        _ => None,
    }
}

/// RAII handle for the same coordination lock used by repository publishers.
/// Reconciliation must hold this lock from the journal check through the last
/// private-ref deletion, otherwise a publisher can journal a root between them.
pub(super) struct NativeGraphCoordinationLock {
    pub(super) _file: File,
}

impl NativeGraphCoordinationLock {
    pub(super) fn acquire(repository: &crate::Repository) -> Result<Self, GraphError> {
        let locks = repository.runtime_paths().locks();
        fs::create_dir_all(&locks).map_err(|_| GraphError::DurabilityFailed)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(locks.join("coordination.lock"))
            .map_err(|_| GraphError::DurabilityFailed)?;
        file.try_lock_exclusive()
            .map_err(|_| GraphError::PublicationNotTerminal)?;
        Ok(Self { _file: file })
    }
}

impl Drop for NativeGraphCoordinationLock {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}

/// Reclaims a departed runtime owner's private refs after durable runtime
/// state has been recovered. Accepted pending pins are retained only when the
/// recovered manifest names both their pin IDs and descriptor OIDs.
///
/// Capture, promotion and row-candidate preparation root OGB-2 closures under
/// `refs/orna/pins/<owner>/...`. A killed owner never runs drop-time cleanup.
/// Recovery must therefore reconcile those refs against durable accepted-pin
/// evidence before deleting anything; an absent in-memory handle is not proof
/// that a pending pin is orphaned.
///
/// Only refs under the departed owner's namespace are considered. Every
/// retained pending ref must still resolve to its manifest's descriptor OID;
/// other refs are deleted only after their native object IDs are resolved.
pub(crate) fn reclaim_dead_owner_pin_refs(
    repository: &crate::Repository,
    dead_owner: [u8; 16],
    accepted_pending: &BTreeMap<[u8; 16], NativeOid>,
) -> Result<usize, GraphError> {
    reclaim_departed_owner_pin_refs(repository, &[(dead_owner, accepted_pending)])
}

/// One departed owner and the pin candidates its transaction accepted.
type DepartedOwnerPins<'a> = ([u8; 16], &'a BTreeMap<[u8; 16], NativeOid>);

/// Reclaims every listed departed owner in one pass under one coordination
/// lock. The journal check and each owner's deletions all run while that lock
/// is held, so a publisher cannot journal a root between owners. A pass that
/// cannot take the lock deletes nothing.
pub(crate) fn reclaim_departed_owner_pin_refs(
    repository: &crate::Repository,
    departed: &[DepartedOwnerPins<'_>],
) -> Result<usize, GraphError> {
    let _coordination_lock = NativeGraphCoordinationLock::acquire(repository)?;
    if repository
        .read_publication_journal_locked()
        .map_err(|_| GraphError::DurabilityFailed)?
        .is_some()
    {
        return Err(GraphError::PublicationNotTerminal);
    }
    let owner = PrivateRefCleanupOwner {
        repository: repository.clone(),
    };
    let mut reclaimed = 0;
    for (dead_owner, accepted_pending) in departed {
        reclaimed += reclaim_owner_pin_refs_locked(&owner, *dead_owner, accepted_pending)?;
    }
    Ok(reclaimed)
}

/// Deletes one departed owner's unaccepted private pin refs. The caller must
/// hold the coordination lock and have confirmed that no publication journal
/// is pending.
fn reclaim_owner_pin_refs_locked(
    owner: &PrivateRefCleanupOwner,
    dead_owner: [u8; 16],
    accepted_pending: &BTreeMap<[u8; 16], NativeOid>,
) -> Result<usize, GraphError> {
    let prefix = format!("refs/orna/pins/{}/", hex_encode(&dead_owner));
    let listing = owner.git_output(&["for-each-ref", "--format=%(refname)", &prefix])?;
    let listing = String::from_utf8(listing).map_err(|_| GraphError::MalformedReferenceName)?;

    let accepted_refs = accepted_pending
        .iter()
        .map(|(pin_id, oid)| (accepted_content_ref(&dead_owner, pin_id), oid))
        .collect::<BTreeMap<_, _>>();
    for (reference, expected_oid) in &accepted_refs {
        let resolved = owner
            .git_output(&["rev-parse", "--verify", reference])
            .map_err(|_| GraphError::InvalidProtectedRef)?;
        let resolved =
            String::from_utf8(resolved).map_err(|_| GraphError::MalformedReferenceName)?;
        let resolved = resolved.trim();
        let algorithm = algorithm_for_hex(resolved).ok_or(GraphError::MalformedReferenceName)?;
        let oid = NativeOid::from_hex(algorithm, resolved)?;
        if &oid != *expected_oid {
            return Err(GraphError::InvalidProtectedRef);
        }
    }

    let mut reclaimed = 0;
    for name in listing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if !name.starts_with(&prefix) {
            continue;
        }
        if accepted_refs.contains_key(name) {
            continue;
        }
        let resolved = owner.git_output(&["rev-parse", "--verify", name])?;
        let resolved =
            String::from_utf8(resolved).map_err(|_| GraphError::MalformedReferenceName)?;
        let resolved = resolved.trim();
        let algorithm = algorithm_for_hex(resolved).ok_or(GraphError::MalformedReferenceName)?;
        let oid = NativeOid::from_hex(algorithm, resolved)?;
        owner.delete_protected_ref(name, &oid)?;
        reclaimed += 1;
    }
    Ok(reclaimed)
}

fn mount_snapshot_pin_ref(
    registry_owner_id: &[u8; 16],
    owner_id: &[u8; 16],
    view_id: &[u8; 32],
) -> String {
    format!(
        "refs/orna/pins/{}/mounts/{}/{}",
        hex_encode(registry_owner_id),
        hex_encode(owner_id),
        hex_encode(view_id)
    )
}

pub(crate) struct MountSnapshotPin {
    pub(crate) owner_id: [u8; 16],
    pub(crate) view_id: [u8; 32],
    pub(crate) oid_hex: String,
}

pub(crate) fn list_mount_snapshot_pins(
    repository: &crate::Repository,
    registry_owner_id: &[u8; 16],
) -> Result<Vec<MountSnapshotPin>, GraphError> {
    let owner = PrivateRefCleanupOwner {
        repository: repository.clone(),
    };
    let prefix = format!("refs/orna/pins/{}/mounts/", hex_encode(registry_owner_id));
    let listing =
        owner.git_output(&["for-each-ref", "--format=%(refname) %(objectname)", &prefix])?;
    let listing = String::from_utf8(listing).map_err(|_| GraphError::MalformedReferenceName)?;
    let mut pins = Vec::new();
    for line in listing.lines() {
        let (reference, oid_hex) = line
            .split_once(' ')
            .ok_or(GraphError::MalformedReferenceName)?;
        let Some(suffix) = reference.strip_prefix(&prefix) else {
            continue;
        };
        let Some((owner_hex, view_hex)) = suffix.split_once('/') else {
            return Err(GraphError::MalformedReferenceName);
        };
        if view_hex.contains('/') {
            return Err(GraphError::MalformedReferenceName);
        }
        let owner_id = decode_mount_pin_component::<16>(owner_hex)?;
        let view_id = decode_mount_pin_component::<32>(view_hex)?;
        let algorithm = algorithm_for_hex(oid_hex).ok_or(GraphError::MalformedReferenceName)?;
        let oid = NativeOid::from_hex(algorithm, oid_hex)?;
        pins.push(MountSnapshotPin {
            owner_id,
            view_id,
            oid_hex: oid.to_hex(),
        });
    }
    Ok(pins)
}

pub(crate) fn mount_snapshot_pin_oid(
    repository: &crate::Repository,
    registry_owner_id: &[u8; 16],
    owner_id: &[u8; 16],
    view_id: &[u8; 32],
) -> Result<Option<String>, GraphError> {
    let owner = PrivateRefCleanupOwner {
        repository: repository.clone(),
    };
    let reference = mount_snapshot_pin_ref(registry_owner_id, owner_id, view_id);
    let listing = owner.git_output(&[
        "for-each-ref",
        "--format=%(refname) %(objectname)",
        &reference,
    ])?;
    let listing = String::from_utf8(listing).map_err(|_| GraphError::MalformedReferenceName)?;
    let Some(oid_hex) = listing.lines().find_map(|line| {
        let (name, value) = line.split_once(' ')?;
        (name == reference).then_some(value)
    }) else {
        return Ok(None);
    };
    let algorithm = algorithm_for_hex(oid_hex).ok_or(GraphError::MalformedReferenceName)?;
    let oid = NativeOid::from_hex(algorithm, oid_hex)?;
    Ok(Some(oid.to_hex()))
}

fn decode_mount_pin_component<const N: usize>(value: &str) -> Result<[u8; N], GraphError> {
    if value.len() != N * 2 {
        return Err(GraphError::MalformedReferenceName);
    }
    let mut bytes = [0; N];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| GraphError::MalformedReferenceName)?;
    }
    Ok(bytes)
}

pub(crate) fn create_mount_snapshot_pin(
    repository: &crate::Repository,
    registry_owner_id: &[u8; 16],
    owner_id: &[u8; 16],
    view_id: &[u8; 32],
    commit_oid_hex: &str,
) -> Result<String, GraphError> {
    let algorithm = algorithm_for_hex(commit_oid_hex).ok_or(GraphError::MalformedOid)?;
    let oid = NativeOid::from_hex(algorithm, commit_oid_hex)?;
    let reference = mount_snapshot_pin_ref(registry_owner_id, owner_id, view_id);
    let owner = PrivateRefCleanupOwner {
        repository: repository.clone(),
    };
    owner.create_protected_ref(&reference, &oid)?;
    Ok(oid.to_hex())
}

pub(crate) fn verify_mount_snapshot_pin(
    repository: &crate::Repository,
    registry_owner_id: &[u8; 16],
    owner_id: &[u8; 16],
    view_id: &[u8; 32],
    commit_oid_hex: &str,
) -> Result<(), GraphError> {
    let algorithm = algorithm_for_hex(commit_oid_hex).ok_or(GraphError::MalformedOid)?;
    let expected = NativeOid::from_hex(algorithm, commit_oid_hex)?;
    let actual_hex = mount_snapshot_pin_oid(repository, registry_owner_id, owner_id, view_id)?
        .ok_or(GraphError::InvalidProtectedRef)?;
    let algorithm = algorithm_for_hex(&actual_hex).ok_or(GraphError::MalformedReferenceName)?;
    let actual = NativeOid::from_hex(algorithm, &actual_hex)?;
    if actual != expected {
        return Err(GraphError::InvalidProtectedRef);
    }
    Ok(())
}

pub(crate) fn delete_mount_snapshot_pin(
    repository: &crate::Repository,
    registry_owner_id: &[u8; 16],
    owner_id: &[u8; 16],
    view_id: &[u8; 32],
    commit_oid_hex: &str,
) -> Result<(), GraphError> {
    let algorithm = algorithm_for_hex(commit_oid_hex).ok_or(GraphError::MalformedOid)?;
    let oid = NativeOid::from_hex(algorithm, commit_oid_hex)?;
    let Some(actual) = mount_snapshot_pin_oid(repository, registry_owner_id, owner_id, view_id)?
    else {
        return Ok(());
    };
    if actual != oid.to_hex() {
        return Err(GraphError::InvalidProtectedRef);
    }
    let owner = PrivateRefCleanupOwner {
        repository: repository.clone(),
    };
    owner.delete_protected_ref(
        &mount_snapshot_pin_ref(registry_owner_id, owner_id, view_id),
        &oid,
    )
}
