//! Attached read-only mount views for the VFS-1 host contract.
//!
//! `orna mount DIR --at COMMIT_OR_REF` (profiles/vfs-1.md) resolves its
//! selector exactly once and is *always* read-only. Attaching is recorded in
//! the per-repository runtime directory so `mount status --json` can report
//! every attached view, and every view keeps its own snapshot identity: a
//! format-1/2 migration reader and a format-3 workspace view share neither the
//! mount record nor the format context, consumer or checkpoint identity behind
//! it. Two mounts of different snapshots therefore never alias.
//!
//! These views address the host-visible names; they do not yet project the
//! FUSE tree, which remains the VFS-1 host slice.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use fs2::FileExt;


use sha2::{Digest, Sha256};

use crate::{HistoricalFormatContext, Repository, RuntimePaths};

/// Domain separation for one attached view's identity within one repository
/// runtime, so a view identifier never collides with a snapshot or store digest.
const MOUNT_VIEW_DOMAIN: &[u8] = b"orna.repository.mount.view.v1\0";
/// First line of every mount-view record.
const MOUNT_RECORD_MAGIC: &str = "orna-mount-view 2";
const MOUNT_REGISTRY_OWNER_ID_FILE: &str = "mount-registry-owner-id-v1";

/// Serializes mount records and their snapshot-pin updates across processes.
fn mount_registry_lock(paths: &RuntimePaths, exclusive: bool) -> Result<fs::File, MountError> {
    let locks = paths.locks();
    fs::create_dir_all(&locks).map_err(|_| MountError::HostFilesystem)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(locks.join("mount-registry.lock"))
        .map_err(|_| MountError::HostFilesystem)?;
    if exclusive {
        file.lock_exclusive()
    } else {
        file.lock_shared()
    }
    .map_err(|_| MountError::HostFilesystem)?;
    Ok(file)
}

/// Why a mount view could not be attached or reported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MountError {
    /// The host filesystem refused a mountpoint or registry operation.
    HostFilesystem,
    /// The selector did not resolve, or its metadata could not be admitted.
    Format(crate::FormatContextError),
    /// The mountpoint exists and is not empty.
    MountpointNotEmpty,
    /// A mount view is already recorded at this mountpoint.
    MountpointAlreadyAttached,
    /// The mountpoint is the backing worktree or inside/above it.
    MountpointOverlapsWorktree,
    /// A recorded mount view was malformed and cannot be reported as truth.
    RecordInvalid,
    /// No view is attached at that mountpoint.
    NotAttached,
    /// The immutable commit could not be durably pinned for this view.
    SnapshotPinUnavailable,
    /// Attachment rollback could not clean up its pin or staging record.
    AttachRollbackFailed,
}

impl std::fmt::Display for MountError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HostFilesystem => formatter.write_str("mountpoint or mount registry unavailable"),
            Self::Format(error) => write!(formatter, "snapshot not mountable: {error}"),
            Self::MountpointNotEmpty => formatter.write_str("mountpoint is not empty"),
            Self::MountpointAlreadyAttached => formatter.write_str("a mount view is already attached there"),
            Self::MountpointOverlapsWorktree => {
                formatter.write_str("mountpoint overlaps the backing worktree")
            }
            Self::RecordInvalid => formatter.write_str("retained mount-view record is invalid"),
            Self::NotAttached => formatter.write_str("no mount view is attached there"),
            Self::SnapshotPinUnavailable => formatter.write_str("snapshot could not be pinned for the mount"),
            Self::AttachRollbackFailed => formatter.write_str(
                "mount attachment failed and rollback could not clean up its pin or staging record",
            ),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        text.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    text
}

fn mount_registry_owner_id(
    paths: &RuntimePaths,
    create: bool,
) -> Result<[u8; 16], MountError> {
    let path = paths.root().join(MOUNT_REGISTRY_OWNER_ID_FILE);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                return Err(MountError::RecordInvalid);
            }
            let bytes = fs::read(&path).map_err(|_| MountError::HostFilesystem)?;
            let value = std::str::from_utf8(&bytes).map_err(|_| MountError::RecordInvalid)?;
            let owner_id = parse_hex16(value)?;
            if owner_id == [0; 16] {
                return Err(MountError::RecordInvalid);
            }
            Ok(owner_id)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => {
            Err(MountError::RecordInvalid)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let owner_id = *crate::Uuid::new_v4().as_bytes();
            if owner_id == [0; 16] {
                return Err(MountError::HostFilesystem);
            }
            let staging = paths.root().join(format!(
                ".{MOUNT_REGISTRY_OWNER_ID_FILE}-{}.tmp",
                hex(&owner_id)
            ));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staging)
                .map_err(|_| MountError::HostFilesystem)?;
            if file
                .write_all(hex(&owner_id).as_bytes())
                .and_then(|()| file.sync_all())
                .is_err()
            {
                let _ = fs::remove_file(&staging);
                return Err(MountError::HostFilesystem);
            }
            drop(file);
            match fs::hard_link(&staging, &path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let _ = fs::remove_file(&staging);
                    return mount_registry_owner_id(paths, false);
                }
                Err(_) => {
                    let _ = fs::remove_file(&staging);
                    return Err(MountError::HostFilesystem);
                }
            }
            let _ = fs::remove_file(&staging);
            Ok(owner_id)
        }
        Err(_) => Err(MountError::HostFilesystem),
    }
}

fn mount_view_identity(mountpoint: &Path, snapshot_id: &[u8; 32]) -> [u8; 32] {
    let mut identity = Sha256::new();
    identity.update(MOUNT_VIEW_DOMAIN);
    identity.update(mountpoint.as_os_str().as_encoded_bytes());
    identity.update([0u8]);
    identity.update(snapshot_id);
    identity.finalize().into()
}

fn remove_registry_file(path: &Path) -> Result<(), MountError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(MountError::HostFilesystem),
    }
}

/// One attached read-only view of an exactly-resolved snapshot.
///
/// Reads go through the pinned historical context, so schema, row map, native
/// graph and Blob ranges all resolve against the attached commit and never the
/// workspace `HEAD`. There is deliberately no write seam: an `--at` view is
/// read-only (ORNA-VFS-1), and dropping the view releases its record without
/// touching the workspace.
pub struct MountView {
    repository: Repository,
    mountpoint: PathBuf,
    selector: String,
    registry_owner_id: [u8; 16],
    owner_id: [u8; 16],
    pin_oid_hex: String,
    view_id: [u8; 32],
    context: HistoricalFormatContext,
}

impl MountView {
    /// Attaches one read-only view of `selector` at `mountpoint`.
    ///
    /// The selector resolves exactly once; later workspace movement cannot
    /// retarget the view. The mountpoint must be empty and must not be the
    /// backing worktree, or inside/above it.
    pub fn attach(
        repository: &Repository,
        mountpoint: impl AsRef<Path>,
        selector: &str,
    ) -> Result<Self, MountError> {
        let context = repository
            .open_format_context_at_selector(selector)
            .map_err(MountError::Format)?;
        let mountpoint =
            std::path::absolute(mountpoint.as_ref()).map_err(|_| MountError::HostFilesystem)?;
        let worktree = repository
            .worktree()
            .canonicalize()
            .map_err(|_| MountError::HostFilesystem)?;
        let resolved_mountpoint = mountpoint
            .canonicalize()
            .unwrap_or_else(|_| mountpoint.clone());
        if resolved_mountpoint == worktree
            || resolved_mountpoint.starts_with(&worktree)
            || worktree.starts_with(&resolved_mountpoint)
        {
            return Err(MountError::MountpointOverlapsWorktree);
        }
        let _registry_lock = mount_registry_lock(repository.runtime_paths(), true)?;
        let registry_owner_id = reconcile_mount_registry_locked(repository)?;
        let record_path = mount_record_path(repository.runtime_paths(), &resolved_mountpoint);
        match fs::symlink_metadata(&record_path) {
            Ok(_) => return Err(MountError::MountpointAlreadyAttached),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(MountError::HostFilesystem),
        }
        if resolved_mountpoint.is_dir()
            && fs::read_dir(&resolved_mountpoint)
                .map_err(|_| MountError::HostFilesystem)?
                .next()
                .is_some()
        {
            return Err(MountError::MountpointNotEmpty);
        }

        let view_id = mount_view_identity(&resolved_mountpoint, context.snapshot_id());
        let owner_id = *crate::Uuid::new_v4().as_bytes();
        let pin_oid_hex = match crate::native_graph::create_mount_snapshot_pin(
            repository,
            &registry_owner_id,
            &owner_id,
            &view_id,
            context.commit_oid_hex(),
        ) {
            Ok(pin_oid_hex) => pin_oid_hex,
            Err(_) => {
                if crate::native_graph::delete_mount_snapshot_pin(
                    repository,
                    &registry_owner_id,
                    &owner_id,
                    &view_id,
                    context.commit_oid_hex(),
                )
                .is_err()
                {
                    return Err(MountError::AttachRollbackFailed);
                }
                return Err(MountError::SnapshotPinUnavailable);
            }
        };

        let view = Self {
            repository: repository.clone(),
            mountpoint: resolved_mountpoint,
            selector: selector.to_owned(),
            registry_owner_id,
            owner_id,
            pin_oid_hex,
            view_id,
            context,
        };
        if let Err(error) = view.write_record() {
            let pin_cleanup = crate::native_graph::delete_mount_snapshot_pin(
                repository,
                &view.registry_owner_id,
                &view.owner_id,
                &view.view_id,
                &view.pin_oid_hex,
            );
            let staging_cleanup = remove_registry_file(&view.staging_path());
            if pin_cleanup.is_err() || staging_cleanup.is_err() {
                return Err(MountError::AttachRollbackFailed);
            }
            return Err(error);
        }
        Ok(view)
    }

    /// The host path this view is attached at.
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    /// The attached snapshot's canonical identity.
    pub const fn snapshot_id(&self) -> &[u8; 32] {
        self.context.snapshot_id()
    }

    /// This view's own identity within one repository runtime. Two views of the
    /// same snapshot at different mountpoints have distinct view identities and
    /// neither aliases the other's record.
    pub const fn view_id(&self) -> &[u8; 32] {
        &self.view_id
    }

    /// The numeric repository-format coordinate at the attached commit.
    pub const fn repository_format_number(&self) -> u8 {
        self.context.repository_format_number()
    }

    /// Whether the attached commit is a format-1/2 migration reader input.
    pub const fn is_legacy_format(&self) -> bool {
        self.context.is_legacy_format()
    }

    /// An `--at` view is always read-only.
    pub const fn is_read_only(&self) -> bool {
        true
    }

    /// The canonical Git commit resolved for this view's historical snapshot.
    pub fn commit(&self) -> &crate::GitCommitRef {
        self.context.commit()
    }

    /// The pinned historical context, for schema/row/graph reads.
    pub fn context(&self) -> &HistoricalFormatContext {
        &self.context
    }

    /// The accepted-row baselines this view's saves persist across processes.
    pub fn row_baselines(&self) -> MountRowBaselines {
        MountRowBaselines {
            repository: self.repository.clone(),
            view_id: self.view_id,
        }
    }

    fn record_path(&self) -> PathBuf {
        mount_record_path(self.repository.runtime_paths(), &self.mountpoint)
    }

    fn staging_path(&self) -> PathBuf {
        self.record_path().with_extension("staging")
    }

    fn write_record(&self) -> Result<(), MountError> {
        let paths = self.repository.runtime_paths();
        let directory = mount_registry_dir(paths);
        fs::create_dir_all(&directory).map_err(|_| MountError::HostFilesystem)?;
        let record = format!(
            "{MOUNT_RECORD_MAGIC}
mountpoint {}
selector {}
snapshot {}
view {}
owner {}
pin {}
format {}
legacy {}
read_only true
",
            self.mountpoint.display(),
            self.selector,
            hex(self.context.snapshot_id()),
            hex(&self.view_id),
            hex(&self.owner_id),
            self.pin_oid_hex.as_str(),
            self.context.repository_format_number(),
            self.context.is_legacy_format(),
        );
        let path = self.record_path();
        let staging = self.staging_path();
        fs::write(&staging, record.as_bytes()).map_err(|_| MountError::HostFilesystem)?;
        fs::rename(&staging, &path).map_err(|_| MountError::HostFilesystem)
    }

    /// Releases this view's record. The attached snapshot and the workspace are
    /// untouched, so an earlier view of the same snapshot stays valid.
    ///
    /// Release is explicit: dropping a view never removes the record, so the
    /// short-lived CLI process that attached it cannot silently unmount it.
    pub fn detach(mut self) -> Result<(), MountError> {
        self.release()
    }

    /// Re-reads the durable record this view wrote, proving the attached view
    /// is still reported exactly as it was admitted.
    pub fn verify_record(&self) -> Result<MountStatus, MountError> {
        let _registry_lock = mount_registry_lock(self.repository.runtime_paths(), false)?;
        let registry_owner_id = mount_registry_owner_id(self.repository.runtime_paths(), false)?;
        if registry_owner_id != self.registry_owner_id {
            return Err(MountError::RecordInvalid);
        }
        let bytes = fs::read(self.record_path()).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                MountError::NotAttached
            } else {
                MountError::HostFilesystem
            }
        })?;
        let record = MountRecord::parse(&bytes)?;
        validate_mount_record_location(
            self.repository.runtime_paths(),
            &self.record_path(),
            &record,
        )?;
        if record.mountpoint != self.mountpoint
            || record.view != self.view_id
            || record.snapshot != *self.context.snapshot_id()
            || record.format != self.context.repository_format_number()
            || record.legacy != self.context.is_legacy_format()
            || record.owner_id != self.owner_id
            || record.pin_oid_hex != self.pin_oid_hex
        {
            return Err(MountError::RecordInvalid);
        }
        crate::native_graph::verify_mount_snapshot_pin(
            &self.repository,
            &registry_owner_id,
            &self.owner_id,
            &self.view_id,
            &self.pin_oid_hex,
        )
        .map_err(|_| MountError::RecordInvalid)?;
        Ok(MountStatus {
            mountpoint: record.mountpoint,
            snapshot: record.snapshot,
            view: record.view,
            owner_id: record.owner_id,
            format: record.format,
            legacy: record.legacy,
        })
    }
    fn release(&mut self) -> Result<(), MountError> {
        let _registry_lock = mount_registry_lock(self.repository.runtime_paths(), true)?;
        let registry_owner_id = reconcile_mount_registry_locked(&self.repository)?;
        if registry_owner_id != self.registry_owner_id {
            return Err(MountError::RecordInvalid);
        }
        let path = self.record_path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(MountError::HostFilesystem),
        };
        let record = MountRecord::parse(&bytes)?;
        if record.view != self.view_id
            || record.owner_id != self.owner_id
            || record.pin_oid_hex != self.pin_oid_hex
        {
            return Ok(());
        }
        if record.mountpoint != self.mountpoint
            || mount_view_identity(&record.mountpoint, &record.snapshot) != record.view
        {
            return Err(MountError::RecordInvalid);
        }
        crate::native_graph::delete_mount_snapshot_pin(
            &self.repository,
            &registry_owner_id,
            &self.owner_id,
            &self.view_id,
            &self.pin_oid_hex,
        )
        .map_err(|_| MountError::SnapshotPinUnavailable)?;
        remove_registry_file(&path)?;
        // Accepted-row baselines belong to this view alone; a released view
        // leaves nothing a later attach could inherit.
        let _ = fs::remove_dir_all(mount_baseline_dir(
            self.repository.runtime_paths(),
            &self.view_id,
        ));
        Ok(())
    }
}

impl std::fmt::Debug for MountView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MountView")
            .field("mountpoint", &self.mountpoint)
            .field("snapshot", &hex(self.context.snapshot_id()))
            .field("format", &self.context.repository_format_number())
            .field("legacy", &self.context.is_legacy_format())
            .finish()
    }
}

/// One attached view as reported by `mount status`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountStatus {
    mountpoint: PathBuf,
    snapshot: [u8; 32],
    view: [u8; 32],
    owner_id: [u8; 16],
    format: u8,
    legacy: bool,
}

impl MountStatus {
    /// The host path this view is attached at.
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    /// The attached snapshot's canonical identity.
    pub const fn snapshot(&self) -> &[u8; 32] {
        &self.snapshot
    }

    /// The attached commit's numeric repository-format coordinate.
    pub const fn repository_format_number(&self) -> u8 {
        self.format
    }

    /// Whether the attached commit is a format-1/2 migration reader input.
    pub const fn is_legacy_format(&self) -> bool {
        self.legacy
    }

    /// Every attached view is read-only.
    pub const fn is_read_only(&self) -> bool {
        true
    }

    /// Hex rendering of the view identity, for stable JSON output.
    pub fn view_hex(&self) -> String {
        hex(&self.view)
    }

    /// Hex rendering of the snapshot identity, for stable JSON output.
    pub fn snapshot_hex(&self) -> String {
        hex(&self.snapshot)
    }
}

struct MountRecord {
    mountpoint: PathBuf,
    selector: String,
    snapshot: [u8; 32],
    owner_id: [u8; 16],
    pin_oid_hex: String,
    view: [u8; 32],
    format: u8,
    legacy: bool,
}

impl MountRecord {
    fn parse(bytes: &[u8]) -> Result<Self, MountError> {
        let text = std::str::from_utf8(bytes).map_err(|_| MountError::RecordInvalid)?;
        let mut lines = text.lines();
        if lines.next() != Some(MOUNT_RECORD_MAGIC) {
            return Err(MountError::RecordInvalid);
        }
        let mut mountpoint = None;
        let mut selector = None;
        let mut snapshot = None;
        let mut view = None;
        let mut owner_id = None;
        let mut pin_oid_hex = None;
        let mut format = None;
        let mut legacy = None;
        for line in lines {
            let Some((key, value)) = line.split_once(' ') else {
                return Err(MountError::RecordInvalid);
            };
            match key {
                "mountpoint" => mountpoint = Some(PathBuf::from(value)),
                "selector" => selector = Some(value.to_owned()),
                "snapshot" => snapshot = Some(parse_hex32(value)?),
                "view" => view = Some(parse_hex32(value)?),
                "owner" => owner_id = Some(parse_hex16(value)?),
                "pin" => pin_oid_hex = Some(parse_mount_pin_oid(value)?),
                "format" => {
                    format = Some(value.parse::<u8>().map_err(|_| MountError::RecordInvalid)?)
                }
                "legacy" => {
                    legacy = Some(match value {
                        "true" => true,
                        "false" => false,
                        _ => return Err(MountError::RecordInvalid),
                    })
                }
                "read_only" if value == "true" => {}
                _ => return Err(MountError::RecordInvalid),
            }
        }
        Ok(Self {
            mountpoint: mountpoint.ok_or(MountError::RecordInvalid)?,
            selector: selector.ok_or(MountError::RecordInvalid)?,
            snapshot: snapshot.ok_or(MountError::RecordInvalid)?,
            view: view.ok_or(MountError::RecordInvalid)?,
            owner_id: owner_id.ok_or(MountError::RecordInvalid)?,
            pin_oid_hex: pin_oid_hex.ok_or(MountError::RecordInvalid)?,
            format: format.ok_or(MountError::RecordInvalid)?,
            legacy: legacy.ok_or(MountError::RecordInvalid)?,
        })
    }
}

fn parse_hex32(value: &str) -> Result<[u8; 32], MountError> {
    if value.len() != 64 {
        return Err(MountError::RecordInvalid);
    }
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| MountError::RecordInvalid)?;
    }
    Ok(bytes)
}

fn parse_hex16(value: &str) -> Result<[u8; 16], MountError> {
    if value.len() != 32 {
        return Err(MountError::RecordInvalid);
    }
    let mut bytes = [0u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| MountError::RecordInvalid)?;
    }
    Ok(bytes)
}

fn parse_mount_pin_oid(value: &str) -> Result<String, MountError> {
    let algorithm = match value.len() {
        40 => crate::native_graph::GitHashAlgorithm::Sha1,
        64 => crate::native_graph::GitHashAlgorithm::Sha256,
        _ => return Err(MountError::RecordInvalid),
    };
    crate::native_graph::NativeOid::from_hex(algorithm, value)
        .map(|oid| oid.to_hex())
        .map_err(|_| MountError::RecordInvalid)
}
fn mount_registry_dir(paths: &RuntimePaths) -> PathBuf {
    paths.root().join("mounts")
}

/// Accepted-row baselines one mounted view persists across the processes that
/// save through it. A row with no record keeps the caller's own baseline.
#[derive(Clone)]
pub struct MountRowBaselines {
    repository: Repository,
    view_id: [u8; 32],
}

impl MountRowBaselines {
    /// The row bytes this view last accepted for `table` and `key`, if any.
    pub fn accepted_row(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, MountError> {
        match fs::read(self.path(table, key)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(MountError::HostFilesystem),
        }
    }

    /// Persists `row` as the baseline the next save of `table` and `key`
    /// through this view must match.
    pub fn record_accepted_row(
        &self,
        table: &str,
        key: &[u8],
        row: &[u8],
    ) -> Result<(), MountError> {
        let path = self.path(table, key);
        let directory = path.parent().ok_or(MountError::HostFilesystem)?;
        fs::create_dir_all(directory).map_err(|_| MountError::HostFilesystem)?;
        let staging = path.with_extension(format!("staging-{}", std::process::id()));
        fs::write(&staging, row).map_err(|_| MountError::HostFilesystem)?;
        fs::rename(&staging, &path).map_err(|_| MountError::HostFilesystem)
    }

    fn path(&self, table: &str, key: &[u8]) -> PathBuf {
        let mut digest = Sha256::new();
        digest.update(MOUNT_VIEW_DOMAIN);
        digest.update(b"row-baseline\0");
        digest.update((table.len() as u64).to_le_bytes());
        digest.update(table.as_bytes());
        digest.update((key.len() as u64).to_le_bytes());
        digest.update(key);
        mount_baseline_dir(self.repository.runtime_paths(), &self.view_id)
            .join(hex(&digest.finalize()))
    }
}

fn mount_baseline_dir(paths: &RuntimePaths, view_id: &[u8; 32]) -> PathBuf {
    paths.root().join("mount-baselines").join(hex(view_id))
}

fn mount_record_path(paths: &RuntimePaths, mountpoint: &Path) -> PathBuf {
    let mut digest = Sha256::new();
    digest.update(MOUNT_VIEW_DOMAIN);
    digest.update(b"record\0");
    digest.update(mountpoint.as_os_str().as_encoded_bytes());
    let name: [u8; 32] = digest.finalize().into();
    mount_registry_dir(paths).join(format!("mount-{}.orna", hex(&name)))
}

fn validate_mount_record_location(
    paths: &RuntimePaths,
    path: &Path,
    record: &MountRecord,
) -> Result<(), MountError> {
    if mount_record_path(paths, &record.mountpoint) != path
        || mount_view_identity(&record.mountpoint, &record.snapshot) != record.view
    {
        return Err(MountError::RecordInvalid);
    }
    Ok(())
}

fn reconcile_mount_registry_locked(
    repository: &Repository,
) -> Result<[u8; 16], MountError> {
    let paths = repository.runtime_paths();
    let registry_owner_id = mount_registry_owner_id(paths, true)?;
    let mut pins = BTreeMap::new();
    for pin in crate::native_graph::list_mount_snapshot_pins(repository, &registry_owner_id)
        .map_err(|_| MountError::SnapshotPinUnavailable)?
    {
        let key = (pin.owner_id, pin.view_id);
        if pins.insert(key, pin.oid_hex).is_some() {
            return Err(MountError::RecordInvalid);
        }
    }

    let directory = mount_registry_dir(paths);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            for ((owner_id, view_id), pin_oid_hex) in pins {
                crate::native_graph::delete_mount_snapshot_pin(
                    repository,
                    &registry_owner_id,
                    &owner_id,
                    &view_id,
                    &pin_oid_hex,
                )
                .map_err(|_| MountError::SnapshotPinUnavailable)?;
            }
            return Ok(registry_owner_id);
        }
        Err(_) => return Err(MountError::HostFilesystem),
    };

    let mut seen = BTreeSet::new();
    let mut stale_records = Vec::new();
    let mut transient_files = Vec::new();
    for entry in entries {
        let path = entry.map_err(|_| MountError::HostFilesystem)?.path();
        if path.extension().is_some_and(|extension| {
            extension == "staging" || extension == "tmp"
        }) {
            transient_files.push(path);
            continue;
        }
        let bytes = fs::read(&path).map_err(|_| MountError::HostFilesystem)?;
        let record = MountRecord::parse(&bytes)?;
        validate_mount_record_location(paths, &path, &record)?;
        let key = (record.owner_id, record.view);
        if !seen.insert(key) {
            return Err(MountError::RecordInvalid);
        }
        match pins.get(&key) {
            Some(pin_oid) if pin_oid == &record.pin_oid_hex => {}
            Some(_) => return Err(MountError::RecordInvalid),
            None => stale_records.push(path),
        }
    }

    for ((owner_id, view_id), pin_oid_hex) in pins {
        if !seen.contains(&(owner_id, view_id)) {
            crate::native_graph::delete_mount_snapshot_pin(
                repository,
                &registry_owner_id,
                &owner_id,
                &view_id,
                &pin_oid_hex,
            )
            .map_err(|_| MountError::SnapshotPinUnavailable)?;
        }
    }
    for path in stale_records.into_iter().chain(transient_files) {
        remove_registry_file(&path)?;
    }
    Ok(registry_owner_id)
}

impl Repository {
    /// Reports every currently attached read-only mount view, ordered by
    /// mountpoint.
    ///
    /// The report is read from the repository's own runtime directory, so it
    /// describes the views this checkout has attached, whatever format each one
    /// resolved to. A malformed record is reported rather than skipped: a
    /// retained record that cannot be read is not absence of a mount.
    pub fn mount_status(&self) -> Result<Vec<MountStatus>, MountError> {
        let _registry_lock = mount_registry_lock(self.runtime_paths(), true)?;
        let registry_owner_id = reconcile_mount_registry_locked(self)?;
        let directory = mount_registry_dir(self.runtime_paths());
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(MountError::HostFilesystem),
        };
        let mut statuses = Vec::new();
        for entry in entries {
            let path = entry.map_err(|_| MountError::HostFilesystem)?.path();
            if path.extension().is_some_and(|extension| {
                extension == "staging" || extension == "tmp"
            }) {
                continue;
            }
            let bytes = fs::read(&path).map_err(|_| MountError::HostFilesystem)?;
            let record = MountRecord::parse(&bytes)?;
            validate_mount_record_location(self.runtime_paths(), &path, &record)?;
            crate::native_graph::verify_mount_snapshot_pin(
                self,
                &registry_owner_id,
                &record.owner_id,
                &record.view,
                &record.pin_oid_hex,
            )
            .map_err(|_| MountError::RecordInvalid)?;
            statuses.push(MountStatus {
                mountpoint: record.mountpoint,
                snapshot: record.snapshot,
                view: record.view,
                owner_id: record.owner_id,
                format: record.format,
                legacy: record.legacy,
            });
        }
        statuses.sort_by(|left, right| left.mountpoint.cmp(&right.mountpoint));
        Ok(statuses)
    }

    /// Re-attaches to the view already recorded at `mountpoint`, without
    /// resolving a new selector.
    ///
    /// The durable record is the authority: the recorded snapshot is resolved
    /// again into its own pinned context, so a caller can read through the
    /// attached view while the workspace `HEAD` keeps moving. A mountpoint with
    /// no record is refused rather than silently mounted at `HEAD`.
    pub fn open_mount_view(&self, mountpoint: impl AsRef<Path>) -> Result<MountView, MountError> {
        let mountpoint =
            std::path::absolute(mountpoint.as_ref()).map_err(|_| MountError::HostFilesystem)?;
        let resolved = mountpoint
            .canonicalize()
            .unwrap_or_else(|_| mountpoint.clone());
        let _registry_lock = mount_registry_lock(self.runtime_paths(), true)?;
        let registry_owner_id = reconcile_mount_registry_locked(self)?;
        let record_path = mount_record_path(self.runtime_paths(), &resolved);
        let bytes = fs::read(&record_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                MountError::NotAttached
            } else {
                MountError::HostFilesystem
            }
        })?;
        let record = MountRecord::parse(&bytes)?;
        if record.mountpoint != resolved {
            return Err(MountError::RecordInvalid);
        }
        validate_mount_record_location(self.runtime_paths(), &record_path, &record)?;
        self.open_mount_record(record, &registry_owner_id)
    }

    /// Re-attaches to a recorded view using the view identity carried by its
    /// FUSE source, independent of the registry's canonical mountpoint path.
    pub fn open_mount_view_by_id(&self, view_id_hex: &str) -> Result<MountView, MountError> {
        let view_id = parse_hex32(view_id_hex)?;
        let _registry_lock = mount_registry_lock(self.runtime_paths(), true)?;
        let registry_owner_id = reconcile_mount_registry_locked(self)?;
        let directory = mount_registry_dir(self.runtime_paths());
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(MountError::NotAttached);
            }
            Err(_) => return Err(MountError::HostFilesystem),
        };
        let mut found = None;
        for entry in entries {
            let path = entry.map_err(|_| MountError::HostFilesystem)?.path();
            if path.extension().is_some_and(|extension| {
                extension == "staging" || extension == "tmp"
            }) {
                continue;
            }
            let bytes = fs::read(&path).map_err(|_| MountError::HostFilesystem)?;
            let record = MountRecord::parse(&bytes)?;
            validate_mount_record_location(self.runtime_paths(), &path, &record)?;
            if record.view == view_id {
                if found.is_some() {
                    return Err(MountError::RecordInvalid);
                }
                found = Some(record);
            }
        }
        self.open_mount_record(found.ok_or(MountError::NotAttached)?, &registry_owner_id)
    }

    fn open_mount_record(
        &self,
        record: MountRecord,
        registry_owner_id: &[u8; 16],
    ) -> Result<MountView, MountError> {
        crate::native_graph::verify_mount_snapshot_pin(
            self,
            registry_owner_id,
            &record.owner_id,
            &record.view,
            &record.pin_oid_hex,
        )
        .map_err(|_| MountError::RecordInvalid)?;
        let context = self
            .open_format_context_at_selector(&record.pin_oid_hex)
            .map_err(MountError::Format)?;
        if context.snapshot_id() != &record.snapshot
            || context.repository_format_number() != record.format
            || context.is_legacy_format() != record.legacy
        {
            return Err(MountError::RecordInvalid);
        }
        Ok(MountView {
            repository: self.clone(),
            mountpoint: record.mountpoint,
            selector: record.selector,
            registry_owner_id: *registry_owner_id,
            owner_id: record.owner_id,
            pin_oid_hex: record.pin_oid_hex,
            view_id: record.view,
            context,
        })
    }

    /// Removes the recorded view at `mountpoint`.
    ///
    /// Only the mount record is removed. The attached snapshot, its objects and
    /// the workspace are untouched, so an earlier view of the same snapshot and
    /// a format-3 workspace view keep working. An unrecognised mountpoint is
    /// reported rather than ignored.
    pub fn unmount_view(&self, mountpoint: impl AsRef<Path>) -> Result<MountStatus, MountError> {
        let mountpoint =
            std::path::absolute(mountpoint.as_ref()).map_err(|_| MountError::HostFilesystem)?;
        let resolved = mountpoint
            .canonicalize()
            .unwrap_or_else(|_| mountpoint.clone());
        let _registry_lock = mount_registry_lock(self.runtime_paths(), true)?;
        let registry_owner_id = reconcile_mount_registry_locked(self)?;
        let record_path = mount_record_path(self.runtime_paths(), &resolved);
        let bytes = fs::read(&record_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                MountError::NotAttached
            } else {
                MountError::HostFilesystem
            }
        })?;
        let record = MountRecord::parse(&bytes)?;
        if record.mountpoint != resolved {
            return Err(MountError::RecordInvalid);
        }
        validate_mount_record_location(self.runtime_paths(), &record_path, &record)?;
        let status = MountStatus {
            mountpoint: record.mountpoint,
            snapshot: record.snapshot,
            view: record.view,
            owner_id: record.owner_id,
            format: record.format,
            legacy: record.legacy,
        };
        crate::native_graph::delete_mount_snapshot_pin(
            self,
            &registry_owner_id,
            &record.owner_id,
            &record.view,
            &record.pin_oid_hex,
        )
        .map_err(|_| MountError::SnapshotPinUnavailable)?;
        remove_registry_file(&record_path)?;
        Ok(status)
    }
}
