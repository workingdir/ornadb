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
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::{HistoricalFormatContext, Repository, RuntimePaths};

/// Domain separation for one attached view's identity within one repository
/// runtime, so a view identifier never collides with a snapshot or store digest.
const MOUNT_VIEW_DOMAIN: &[u8] = b"orna.repository.mount.view.v1\0";
/// First line of every mount-view record.
const MOUNT_RECORD_MAGIC: &str = "orna-mount-view 1";

/// Why a mount view could not be attached or reported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MountError {
    /// The host filesystem refused a mountpoint or registry operation.
    HostFilesystem,
    /// The selector did not resolve, or its metadata could not be admitted.
    Format(crate::FormatContextError),
    /// The mountpoint exists and is not empty.
    MountpointNotEmpty,
    /// The mountpoint is the backing worktree or inside/above it.
    MountpointOverlapsWorktree,
    /// A recorded mount view was malformed and cannot be reported as truth.
    RecordInvalid,
    /// No view is attached at that mountpoint.
    NotAttached,
}

impl std::fmt::Display for MountError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HostFilesystem => formatter.write_str("mountpoint or mount registry unavailable"),
            Self::Format(error) => write!(formatter, "snapshot not mountable: {error}"),
            Self::MountpointNotEmpty => formatter.write_str("mountpoint is not empty"),
            Self::MountpointOverlapsWorktree => {
                formatter.write_str("mountpoint overlaps the backing worktree")
            }
            Self::RecordInvalid => formatter.write_str("retained mount-view record is invalid"),
            Self::NotAttached => formatter.write_str("no mount view is attached there"),
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
        if resolved_mountpoint.is_dir()
            && fs::read_dir(&resolved_mountpoint)
                .map_err(|_| MountError::HostFilesystem)?
                .next()
                .is_some()
        {
            return Err(MountError::MountpointNotEmpty);
        }

        let mut identity = Sha256::new();
        identity.update(MOUNT_VIEW_DOMAIN);
        identity.update(resolved_mountpoint.as_os_str().as_encoded_bytes());
        identity.update([0u8]);
        identity.update(context.snapshot_id());
        let view_id: [u8; 32] = identity.finalize().into();

        let view = Self {
            repository: repository.clone(),
            mountpoint: resolved_mountpoint,
            selector: selector.to_owned(),
            view_id,
            context,
        };
        view.write_record()?;
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

    /// The pinned historical context, for schema/row/graph reads.
    pub fn context(&self) -> &HistoricalFormatContext {
        &self.context
    }

    fn record_path(&self) -> PathBuf {
        mount_record_path(self.repository.runtime_paths(), &self.mountpoint)
    }

    fn write_record(&self) -> Result<(), MountError> {
        let paths = self.repository.runtime_paths();
        let directory = mount_registry_dir(paths);
        fs::create_dir_all(&directory).map_err(|_| MountError::HostFilesystem)?;
        let record = format!(
            "{MOUNT_RECORD_MAGIC}\nmountpoint {}\nselector {}\nsnapshot {}\nview {}\nformat {}\nlegacy {}\nread_only true\n",
            self.mountpoint.display(),
            self.selector,
            hex(self.context.snapshot_id()),
            hex(&self.view_id),
            self.context.repository_format_number(),
            self.context.is_legacy_format(),
        );
        let path = self.record_path();
        let staging = path.with_extension("staging");
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
        let bytes = fs::read(self.record_path()).map_err(|_| MountError::HostFilesystem)?;
        let record = MountRecord::parse(&bytes)?;
        if record.view != self.view_id
            || record.snapshot != *self.context.snapshot_id()
            || record.format != self.context.repository_format_number()
            || record.legacy != self.context.is_legacy_format()
        {
            return Err(MountError::RecordInvalid);
        }
        Ok(MountStatus {
            mountpoint: record.mountpoint,
            snapshot: record.snapshot,
            view: record.view,
            format: record.format,
            legacy: record.legacy,
        })
    }

    fn release(&mut self) -> Result<(), MountError> {
        let path = self.record_path();
        let Ok(bytes) = fs::read(&path) else {
            return Ok(());
        };
        // Only the view that owns the current record removes it: a replacement
        // view attached at the same mountpoint keeps its own evidence.
        if MountRecord::parse(&bytes)?.view != self.view_id {
            return Ok(());
        }
        fs::remove_file(&path).map_err(|_| MountError::HostFilesystem)
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

fn mount_registry_dir(paths: &RuntimePaths) -> PathBuf {
    paths.root().join("mounts")
}

fn mount_record_path(paths: &RuntimePaths, mountpoint: &Path) -> PathBuf {
    let mut digest = Sha256::new();
    digest.update(MOUNT_VIEW_DOMAIN);
    digest.update(b"record\0");
    digest.update(mountpoint.as_os_str().as_encoded_bytes());
    let name: [u8; 32] = digest.finalize().into();
    mount_registry_dir(paths).join(format!("mount-{}.orna", hex(&name)))
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
        let directory = mount_registry_dir(self.runtime_paths());
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(MountError::HostFilesystem),
        };
        let mut statuses = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| MountError::HostFilesystem)?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "staging")
            {
                continue;
            }
            let bytes = fs::read(&path).map_err(|_| MountError::HostFilesystem)?;
            let record = MountRecord::parse(&bytes)?;
            statuses.push(MountStatus {
                mountpoint: record.mountpoint,
                snapshot: record.snapshot,
                view: record.view,
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
        let record = self
            .mount_status()?
            .into_iter()
            .find(|status| status.mountpoint == resolved)
            .ok_or(MountError::NotAttached)?;
        debug_assert_eq!(record.mountpoint, resolved);
        // The record names one immutable commit selector. Resolving that
        // selector again — never a branch tip or the workspace HEAD — restores
        // the attached view, and the resolved snapshot must match the logical
        // identity the record reported.
        let bytes = fs::read(mount_record_path(self.runtime_paths(), &resolved))
            .map_err(|_| MountError::HostFilesystem)?;
        let parsed = MountRecord::parse(&bytes)?;
        let context = self
            .open_format_context_at_selector(&parsed.selector)
            .map_err(MountError::Format)?;
        // The recorded coordinate is re-verified, not trusted: a record whose
        // format or legacy flag no longer describes the resolved commit is
        // corrupt and must not be served as an attached view.
        if context.snapshot_id() != &parsed.snapshot
            || context.repository_format_number() != parsed.format
            || context.is_legacy_format() != parsed.legacy
        {
            return Err(MountError::RecordInvalid);
        }
        Ok(MountView {
            repository: self.clone(),
            mountpoint: resolved,
            selector: parsed.selector,
            view_id: parsed.view,
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
        let status = self
            .mount_status()?
            .into_iter()
            .find(|status| status.mountpoint == resolved)
            .ok_or(MountError::NotAttached)?;
        fs::remove_file(mount_record_path(self.runtime_paths(), &resolved))
            .map_err(|_| MountError::HostFilesystem)?;
        Ok(status)
    }
}
