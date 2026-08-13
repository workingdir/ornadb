//! Test harness for system-level package tests.
//!
//! This crate freezes a package artifact at a point in time. Construction
//! streams the source bytes into a private snapshot below the repository
//! `target/orna-system-tests/` directory and records the SHA-256 digest of
//! those exact bytes. Later changes to the source file do not change the
//! snapshot bytes or the digest.
//!
//! Consumers must not reopen the snapshot by path after construction, and
//! this crate does not claim the snapshot is immutable on disk. The safe
//! consumption model is [`FrozenPackageArtifact::open_verified`], which pins
//! an open descriptor to the exact inode it hashed and returns a
//! [`VerifiedPackageArtifact`]. The consumer installs or copies from that
//! pinned handle, never from a path.

use std::error::Error as StdError;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// The distribution format of a package artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageFormat {
    /// A Debian binary package (`.deb`).
    Debian,
}

impl PackageFormat {
    /// The exact lowercase file suffix for this format, without the dot.
    ///
    /// All suffix validation and suffix display must use this value so that
    /// a future format cannot bypass format-specific rules. Only Debian is
    /// currently public.
    pub fn expected_suffix(self) -> &'static str {
        match self {
            PackageFormat::Debian => "deb",
        }
    }
}

/// A package artifact with a frozen byte snapshot and SHA-256 digest.
///
/// Construction opens the source once with `O_NOFOLLOW | O_NONBLOCK`,
/// verifies it is a regular file, and streams it through a fixed-size buffer
/// into a private snapshot file below the repository
/// `target/orna-system-tests/` directory. The private root has exact mode
/// `0700`, forced after creation so the process umask cannot weaken it, and
/// the snapshot file has exact mode `0400`. The whole package is never
/// loaded into memory.
///
/// The object owns its private snapshot directory and removes only that
/// directory on drop. It is not `Clone`: a copy would split ownership of the
/// cleanup and could leave one instance pointing at a removed path.
///
/// This object does not make the snapshot immutable on disk. Use
/// [`Self::open_verified`] to pin a verified open handle to the exact inode,
/// and read only through that handle.
#[derive(Debug)]
pub struct FrozenPackageArtifact {
    format: PackageFormat,
    /// The private snapshot directory owned by this artifact.
    root: PathBuf,
    /// The snapshot file inside `root`. This is what [`Self::path`] returns.
    snapshot: PathBuf,
    sha256: String,
}

/// The fixed-size buffer used to stream the source into the snapshot.
const SNAPSHOT_BUFFER_SIZE: usize = 64 * 1024;

/// A monotonic counter that makes snapshot directory names unique.
static SNAPSHOT_COUNTER: AtomicU64 = AtomicU64::new(0);

impl FrozenPackageArtifact {
    /// Freeze the package artifact at `path`.
    ///
    /// # Requirements
    ///
    /// - `path` must be absolute.
    /// - The final path entry must end with the exact lowercase suffix from
    ///   [`PackageFormat::expected_suffix`].
    /// - The final path entry must not be a symbolic link.
    /// - The final path entry must be a regular file.
    ///
    /// The source is opened once with `O_NOFOLLOW | O_NONBLOCK` and streamed
    /// through a fixed-size buffer into a private snapshot below the
    /// repository `target/orna-system-tests/` directory. The private root is
    /// created with exact mode `0700`; the mode is forced through an open
    /// directory descriptor after creation so the process umask cannot
    /// weaken it. The snapshot file is created with exclusive creation and
    /// mode `0600`, synced, then tightened to exact mode `0400` and synced
    /// again before the artifact is returned. If snapshot creation, writing,
    /// or syncing fails, the constructor removes the partial snapshot and
    /// reports [`Error::SnapshotIo`].
    ///
    /// On non-unix platforms this constructor returns [`Error::SourceIo`]
    /// with `ErrorKind::Unsupported` because the `O_NOFOLLOW` freeze is
    /// unix-only.
    pub fn new(format: PackageFormat, path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();

        if !path.is_absolute() {
            return Err(Error::RelativePath);
        }

        let file_name = path.file_name().ok_or(Error::WrongSuffix { format })?;
        if Path::new(file_name).extension() != Some(OsStr::new(format.expected_suffix())) {
            return Err(Error::WrongSuffix { format });
        }

        let source = open_regular_source(path)?;
        let root = create_snapshot_root()?;
        match build_snapshot(format, root.clone(), source) {
            Ok(artifact) => Ok(artifact),
            Err(error) => {
                // Fail closed: remove the partial private snapshot.
                let _ = fs::remove_dir_all(&root);
                Err(error)
            }
        }
    }

    /// The package format.
    pub fn format(&self) -> PackageFormat {
        self.format
    }

    /// The path of the private snapshot file.
    ///
    /// Private by design: consumers must not reopen the snapshot by path,
    /// because a path-based reopen would reintroduce a time-of-check to
    /// time-of-use window. Use [`Self::open_verified`] and read from the
    /// pinned handle instead. Internal code and tests use this path only to
    /// inspect the harness-owned bytes on disk.
    #[cfg(test)]
    fn path(&self) -> &Path {
        &self.snapshot
    }

    /// The SHA-256 digest of the frozen bytes, as lowercase hex.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Open and verify the frozen snapshot, returning a pinned handle.
    ///
    /// The snapshot is reopened with the same nonblocking nofollow
    /// regular-file guarantee used at construction. The open descriptor is
    /// `fstat`-ed on the same inode: the file must be regular with exact
    /// mode `0400`, and its bytes must stream-hash to [`Self::sha256`]. The
    /// descriptor is rewound to the start of the file before it is returned.
    ///
    /// The returned [`VerifiedPackageArtifact`] pins the verified inode with
    /// an open file descriptor. The consumer installs or copies from that
    /// handle and never reopens the snapshot path, so a concurrent path
    /// replacement cannot change which bytes are read. A mode or digest
    /// mismatch returns [`Error::SnapshotChanged`] without a source. IO
    /// failures while opening, stat-ing, reading, or rewinding return
    /// [`Error::VerifyIo`].
    pub fn open_verified(&self) -> Result<VerifiedPackageArtifact, Error> {
        let mut file = match open_regular_source(&self.snapshot) {
            Ok(file) => file,
            Err(Error::Symlink) | Err(Error::NonRegularFile) => return Err(Error::SnapshotChanged),
            Err(Error::SourceIo(source)) | Err(Error::MetadataIo(source)) => {
                return Err(Error::VerifyIo(source));
            }
            Err(_) => return Err(Error::SnapshotChanged),
        };

        #[cfg(unix)]
        verify_pinned_mode_0400(&file)?;

        let mut hasher = Sha256::new();
        let mut buffer = [0u8; SNAPSHOT_BUFFER_SIZE];
        loop {
            let count = file.read(&mut buffer).map_err(Error::VerifyIo)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        let digest: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if digest != self.sha256 {
            return Err(Error::SnapshotChanged);
        }
        file.seek(SeekFrom::Start(0)).map_err(Error::VerifyIo)?;

        Ok(VerifiedPackageArtifact {
            format: self.format,
            file,
            sha256: self.sha256.clone(),
        })
    }
}

impl Drop for FrozenPackageArtifact {
    fn drop(&mut self) {
        // Remove only the exact private root created at construction. The
        // guard makes the ownership boundary visible and prevents a corrupted
        // path from removing anything outside the snapshot base.
        if self.root.starts_with(snapshot_root_base()) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// A pinned, verified handle to the frozen snapshot bytes.
///
/// Returned by [`FrozenPackageArtifact::open_verified`]. The handle owns an
/// open regular file descriptor with `O_NOFOLLOW | O_NONBLOCK` semantics.
/// Verification `fstat`-ed exact mode `0400` and stream-hashed that same
/// open inode, then rewound the descriptor before returning it.
///
/// The open descriptor pins the verified inode. A concurrent rename or
/// replacement of the snapshot path cannot change which bytes are read
/// through this handle. The consumer installs or copies from [`Self::file`]
/// or [`Self::into_file`] and never reopens the snapshot path.
///
/// The handle is not tied to the lifetime of the [`FrozenPackageArtifact`]
/// that created it. On unix, the open descriptor keeps the verified bytes
/// readable even if the owning artifact is dropped and its private snapshot
/// directory is removed.
#[derive(Debug)]
pub struct VerifiedPackageArtifact {
    format: PackageFormat,
    file: File,
    sha256: String,
}

impl VerifiedPackageArtifact {
    /// The package format.
    pub fn format(&self) -> PackageFormat {
        self.format
    }

    /// The SHA-256 digest of the verified bytes, as lowercase hex.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// The pinned open file descriptor holding the verified bytes.
    ///
    /// The descriptor is rewound to the start of the file. Read or copy from
    /// this handle; do not reopen the snapshot path.
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Consume the handle and return the pinned open file descriptor.
    ///
    /// The descriptor is rewound to the start of the file and remains pinned
    /// to the verified inode after this call.
    pub fn into_file(self) -> File {
        self.file
    }
}

/// Open a path read-only with `O_NOFOLLOW | O_NONBLOCK` and verify it is a
/// regular file.
///
/// The single descriptor pins the inode, so a concurrent rename or replace
/// of the path cannot change which bytes are read. `O_NONBLOCK` is harmless
/// for regular files but stops a FIFO or device open from blocking forever.
#[cfg(unix)]
fn open_regular_source(path: &Path) -> Result<File, Error> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};

    let c_path = match std::ffi::CString::new(path.as_os_str().as_bytes()) {
        Ok(c_path) => c_path,
        Err(_) => {
            return Err(Error::SourceIo(io::Error::new(
                io::ErrorKind::InvalidInput,
                "package path contains a NUL byte",
            )));
        }
    };

    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let source = io::Error::last_os_error();
        if source.raw_os_error() == Some(libc::ELOOP) {
            return Err(Error::Symlink);
        }
        return Err(Error::SourceIo(source));
    }
    // The File now owns the descriptor and closes it on drop.
    let file = unsafe { File::from_raw_fd(fd) };

    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut stat) } != 0 {
        return Err(Error::MetadataIo(io::Error::last_os_error()));
    }
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(Error::NonRegularFile);
    }
    Ok(file)
}

/// Non-unix platforms cannot enforce the `O_NOFOLLOW` freeze, so fail closed.
#[cfg(not(unix))]
fn open_regular_source(_path: &Path) -> Result<File, Error> {
    Err(Error::SourceIo(io::Error::new(
        io::ErrorKind::Unsupported,
        "byte-frozen package snapshots require a unix platform",
    )))
}

/// The repository `target/orna-system-tests/` directory.
///
/// All snapshots live below this base, and never below `/tmp`.
fn snapshot_root_base() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .expect("canonical manifest directory");
    manifest
        .parent()
        .expect("crates directory")
        .parent()
        .expect("repository root")
        .join("target")
        .join("orna-system-tests")
}

/// Create a fresh private snapshot directory with exact mode `0700`.
fn create_snapshot_root() -> Result<PathBuf, Error> {
    let base = snapshot_root_base();
    fs::create_dir_all(&base).map_err(Error::SnapshotIo)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the unix epoch")
        .as_nanos();
    let sequence = SNAPSHOT_COUNTER.fetch_add(1, Ordering::SeqCst);
    let root = base.join(format!(
        "snapshot-{}-{sequence}-{nanos}",
        std::process::id()
    ));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&root).map_err(Error::SnapshotIo)?;
    // DirBuilderExt::mode is filtered by the process umask, so the created
    // directory can be weaker than 0700. Force the exact mode through an
    // open directory descriptor and verify it before returning.
    #[cfg(unix)]
    force_exact_root_mode(&root)?;
    Ok(root)
}

/// Force exact mode `0700` on a freshly created private root directory.
///
/// `DirBuilderExt::mode(0700)` is filtered by the process umask, so a
/// restrictive umask can leave the root with fewer permissions than the
/// snapshot contract requires. `chmod` is not filtered by the umask, so
/// [`fs::set_permissions`] restores the exact mode even when the created
/// directory has no permissions at all. The mode is then verified with
/// `fstat` on an open descriptor of the same directory before returning.
#[cfg(unix)]
fn force_exact_root_mode(root: &Path) -> Result<(), Error> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};

    let mut permissions = fs::metadata(root).map_err(Error::SnapshotIo)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(root, permissions).map_err(Error::SnapshotIo)?;

    let c_path = match std::ffi::CString::new(root.as_os_str().as_bytes()) {
        Ok(c_path) => c_path,
        Err(_) => {
            return Err(Error::SnapshotIo(io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot root path contains a NUL byte",
            )));
        }
    };

    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
        )
    };
    if fd < 0 {
        return Err(Error::SnapshotIo(io::Error::last_os_error()));
    }
    // The File now owns the descriptor and closes it on drop.
    let file = unsafe { File::from_raw_fd(fd) };

    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut stat) } != 0 {
        return Err(Error::SnapshotIo(io::Error::last_os_error()));
    }
    if stat.st_mode & 0o7777 != 0o700 {
        return Err(Error::SnapshotIo(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "snapshot root mode is {:o}, expected 0700",
                stat.st_mode & 0o7777
            ),
        )));
    }
    Ok(())
}

/// `fstat` the pinned descriptor and require exact mode `0400`.
///
/// `File::metadata` uses `fstat` on the already-open descriptor, so the mode
/// check applies to the exact inode that will be hashed.
#[cfg(unix)]
fn verify_pinned_mode_0400(file: &File) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    let mode = file
        .metadata()
        .map_err(Error::VerifyIo)?
        .permissions()
        .mode();
    if mode & 0o7777 != 0o400 {
        return Err(Error::SnapshotChanged);
    }
    Ok(())
}

/// Open the snapshot file with `O_CREAT | O_EXCL` and mode `0600`.
fn open_snapshot_file(path: &Path) -> Result<File, Error> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(Error::SnapshotIo)
}

/// Stream the source into the private snapshot while updating the digest.
///
/// The snapshot file is created with mode `0600` so the bytes can be
/// written, synced, tightened to exact mode `0400`, and synced again before
/// the artifact is returned.
fn build_snapshot(
    format: PackageFormat,
    root: PathBuf,
    mut source: File,
) -> Result<FrozenPackageArtifact, Error> {
    let snapshot = root.join(format!("snapshot.{}", format.expected_suffix()));
    let mut snapshot_file = open_snapshot_file(&snapshot)?;

    let mut hasher = Sha256::new();
    let mut buffer = [0u8; SNAPSHOT_BUFFER_SIZE];
    loop {
        let count = source.read(&mut buffer).map_err(Error::SourceIo)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        snapshot_file
            .write_all(&buffer[..count])
            .map_err(Error::SnapshotIo)?;
    }
    snapshot_file.sync_all().map_err(Error::SnapshotIo)?;

    // Tighten the snapshot to read-only for its owner, then sync the mode
    // change. The write handle stays usable for the duration of the sync.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = snapshot_file
            .metadata()
            .map_err(Error::SnapshotIo)?
            .permissions();
        permissions.set_mode(0o400);
        snapshot_file
            .set_permissions(permissions)
            .map_err(Error::SnapshotIo)?;
    }
    snapshot_file.sync_all().map_err(Error::SnapshotIo)?;

    let sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    Ok(FrozenPackageArtifact {
        format,
        root,
        snapshot,
        sha256,
    })
}

/// Errors produced when freezing a package artifact.
///
/// The variants are not exhaustive. Callers must handle unknown future
/// variants with a wildcard arm instead of matching exhaustively.
#[non_exhaustive]
#[derive(Debug)]
pub enum Error {
    /// The package path is not absolute.
    RelativePath,
    /// The final path entry does not end with the exact lowercase suffix
    /// required by the format. The suffix comes from
    /// [`PackageFormat::expected_suffix`].
    WrongSuffix {
        /// The format whose suffix was required.
        format: PackageFormat,
    },
    /// The package file cannot be opened or read.
    ///
    /// This also covers a path that contains a NUL byte and, on non-unix
    /// platforms, the whole byte-freeze operation.
    SourceIo(io::Error),
    /// The open source file cannot be stat-ed.
    MetadataIo(io::Error),
    /// The final path entry is a symbolic link. The source open fails with
    /// `ELOOP` when `O_NOFOLLOW` is set.
    Symlink,
    /// The final path entry is not a regular file.
    NonRegularFile,
    /// The snapshot no longer matches the frozen state.
    ///
    /// [`FrozenPackageArtifact::open_verified`] reopens the snapshot and
    /// requires exact mode `0400` and the recorded SHA-256 digest. A
    /// mismatch means the harness-owned bytes or permissions changed after
    /// construction.
    SnapshotChanged,
    /// The private snapshot cannot be created, written, or synced.
    ///
    /// The constructor fails closed: it reports this error and removes the
    /// partial snapshot instead of returning a half-written artifact.
    SnapshotIo(io::Error),
    /// The verified snapshot cannot be opened, stat-ed, read, or rewound.
    ///
    /// [`FrozenPackageArtifact::open_verified`] reports this error for IO
    /// failures while pinning and hashing the verified snapshot descriptor.
    /// A mode or digest mismatch is [`Error::SnapshotChanged`], not this
    /// variant.
    VerifyIo(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::RelativePath => write!(f, "package path must be absolute"),
            Error::WrongSuffix { format } => write!(
                f,
                "package file name must end with the exact lowercase .{} suffix",
                format.expected_suffix()
            ),
            Error::SourceIo(_) => write!(f, "package file cannot be opened or read"),
            Error::MetadataIo(_) => write!(f, "package file metadata cannot be read"),
            Error::Symlink => write!(f, "package path must not be a symbolic link"),
            Error::NonRegularFile => write!(f, "package path must be a regular file"),
            Error::SnapshotChanged => {
                write!(
                    f,
                    "package snapshot no longer matches the frozen digest or mode"
                )
            }
            Error::SnapshotIo(_) => {
                write!(f, "package snapshot cannot be created, written, or synced")
            }
            Error::VerifyIo(_) => {
                write!(
                    f,
                    "package snapshot cannot be opened, read, or rewound for verification"
                )
            }
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::SourceIo(source)
            | Error::MetadataIo(source)
            | Error::SnapshotIo(source)
            | Error::VerifyIo(source) => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const SHA256_DEF: &str = "cb8379ac2098aa165029e3938a51da0bcecfc008fd6795f401178647f96c5b34";

    static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique scratch directory below the repository `target/` directory.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new() -> Self {
            let root = repository_target();
            fs::create_dir_all(&root).expect("create repository target directory");
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after the unix epoch")
                .as_nanos();
            let sequence = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = root.join(format!(
                "orna-system-tests-{}-{sequence}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create scratch directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, bytes).expect("write scratch file");
            path
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn repository_target() -> PathBuf {
        snapshot_root_base()
            .parent()
            .expect("repository target directory")
            .to_path_buf()
    }

    /// The lower permission bits of `path`'s mode, including setuid,
    /// setgid, and sticky bits.
    #[cfg(unix)]
    fn perm_bits(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).expect("stat path").permissions().mode() & 0o7777
    }

    #[test]
    fn expected_suffix_is_deb_for_debian() {
        assert_eq!(PackageFormat::Debian.expected_suffix(), "deb");
    }

    #[test]
    fn accepts_absolute_regular_deb_with_expected_digest_and_private_snapshot() {
        let scratch = ScratchDir::new();
        let source = scratch.write("package.deb", b"abc");

        let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &source)
            .expect("absolute regular .deb file is accepted");

        assert_eq!(artifact.format(), PackageFormat::Debian);
        assert_eq!(artifact.sha256(), SHA256_ABC);
        let snapshot = artifact.path();
        assert_ne!(
            snapshot,
            source.as_path(),
            "snapshot must be a private copy, not the source"
        );
        assert!(
            snapshot.starts_with(snapshot_root_base()),
            "snapshot must live below repository target/orna-system-tests"
        );
        assert!(snapshot.is_file(), "snapshot must exist on disk");
        assert_eq!(
            fs::read(snapshot).expect("read snapshot"),
            b"abc",
            "snapshot must contain the frozen bytes"
        );
        assert!(source.is_file(), "constructor must retain the source file");

        #[cfg(unix)]
        assert_eq!(perm_bits(snapshot), 0o400, "snapshot must have mode 0400");
    }

    #[test]
    fn rejects_relative_path() {
        let result = FrozenPackageArtifact::new(PackageFormat::Debian, "package.deb");
        assert!(matches!(result, Err(Error::RelativePath)));
    }

    #[test]
    fn rejects_wrong_suffix() {
        let scratch = ScratchDir::new();
        let txt = scratch.write("package.txt", b"abc");
        assert!(matches!(
            FrozenPackageArtifact::new(PackageFormat::Debian, &txt),
            Err(Error::WrongSuffix {
                format: PackageFormat::Debian
            })
        ));

        let upper = scratch.write("package.DEB", b"abc");
        assert!(matches!(
            FrozenPackageArtifact::new(PackageFormat::Debian, &upper),
            Err(Error::WrongSuffix {
                format: PackageFormat::Debian
            })
        ));
    }

    #[test]
    fn rejects_missing_path() {
        let scratch = ScratchDir::new();
        let missing = scratch.path().join("missing.deb");
        match FrozenPackageArtifact::new(PackageFormat::Debian, &missing) {
            Err(Error::SourceIo(source)) => assert_eq!(source.kind(), io::ErrorKind::NotFound),
            other => panic!("expected SourceIo(NotFound), got {other:?}"),
        }
    }

    #[test]
    fn rejects_directory() {
        let scratch = ScratchDir::new();
        let dir = scratch.path().join("dir.deb");
        fs::create_dir(&dir).expect("create scratch directory");
        assert!(matches!(
            FrozenPackageArtifact::new(PackageFormat::Debian, &dir),
            Err(Error::NonRegularFile)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink() {
        use std::os::unix::fs::symlink;

        let scratch = ScratchDir::new();
        let target = scratch.write("target.deb", b"abc");
        let link = scratch.path().join("link.deb");
        symlink(&target, &link).expect("create symlink");
        assert!(matches!(
            FrozenPackageArtifact::new(PackageFormat::Debian, &link),
            Err(Error::Symlink)
        ));
    }

    #[test]
    fn snapshot_is_frozen_after_source_mutation() {
        let scratch = ScratchDir::new();
        let source = scratch.write("mutable.deb", b"abc");

        let frozen = FrozenPackageArtifact::new(PackageFormat::Debian, &source)
            .expect("first freeze succeeds");
        fs::write(&source, b"def").expect("mutate source file");

        let snapshot = frozen.path();
        assert_ne!(
            snapshot,
            source.as_path(),
            "snapshot path must differ from the source path"
        );
        assert_eq!(
            fs::read(snapshot).expect("read snapshot"),
            b"abc",
            "snapshot keeps the original bytes"
        );
        assert_eq!(
            frozen.sha256(),
            SHA256_ABC,
            "old instance keeps the old digest"
        );

        let fresh = FrozenPackageArtifact::new(PackageFormat::Debian, &source)
            .expect("second freeze succeeds");
        assert_eq!(
            fresh.sha256(),
            SHA256_DEF,
            "fresh instance sees the new bytes"
        );
        assert_ne!(fresh.sha256(), frozen.sha256());
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_root_has_exact_mode_0700() {
        let scratch = ScratchDir::new();
        let source = scratch.write("package.deb", b"abc");
        let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &source)
            .expect("absolute regular .deb file is accepted");

        assert_eq!(
            perm_bits(&artifact.root),
            0o700,
            "snapshot root must have mode 0700"
        );
    }

    /// The environment variable that selects the dedicated umask child
    /// branch in the regression test below.
    #[cfg(unix)]
    const UMASK_CHILD_ENV: &str = "ORNA_SYSTEM_TESTS_UMASK_CHILD";

    #[cfg(unix)]
    #[test]
    fn snapshot_root_has_exact_mode_0700_under_restrictive_umask() {
        use std::process::{Command, exit};

        // Child branch: a dedicated subprocess sets a restrictive umask and
        // proves the exact 0700 root survives it. The child process is the
        // only place the process-global umask is touched, so the test
        // runner's umask is never mutated concurrently with other tests.
        if std::env::var_os(UMASK_CHILD_ENV).is_some() {
            let ok = run_umask_child();
            exit(if ok { 0 } else { 1 });
        }

        let test_name = concat!(
            "tests::",
            stringify!(snapshot_root_has_exact_mode_0700_under_restrictive_umask)
        );
        let output = Command::new(std::env::current_exe().expect("test binary path"))
            .env(UMASK_CHILD_ENV, "1")
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .output()
            .expect("spawn umask child process");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "umask child failed with {}: {stderr}",
            output.status
        );
        assert!(
            stderr.contains("umask child:"),
            "umask child must report its run: {stderr}"
        );
    }

    /// Run the private-root creation under a fully restrictive umask.
    ///
    /// Only called from the dedicated umask child subprocess, where no other
    /// test is running and the process-global umask can be changed safely.
    /// The snapshot base is created before the umask changes so the root
    /// can be created inside a searchable base.
    #[cfg(unix)]
    fn run_umask_child() -> bool {
        fs::create_dir_all(snapshot_root_base()).expect("create snapshot base");

        // A fully restrictive umask strips the owner bits too. Without the
        // exact-mode fix, a fresh root would be created with mode 0000 and
        // the snapshot contract would be broken.
        unsafe {
            libc::umask(0o777);
        }
        eprintln!("umask child: running with umask 0777");

        let root = match create_snapshot_root() {
            Ok(root) => root,
            Err(error) => {
                eprintln!("umask child: create_snapshot_root failed: {error}");
                return false;
            }
        };
        let root_mode = perm_bits(&root);
        eprintln!("umask child: root mode {root_mode:o}");
        let ok = root_mode == 0o700;
        if !ok {
            eprintln!("umask child: expected exact root 0700");
        }
        let _ = fs::remove_dir_all(&root);
        ok
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_has_exact_mode_0400_and_open_verified_returns_rewound_handle() {
        let scratch = ScratchDir::new();
        let source = scratch.write("package.deb", b"abc");
        let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &source)
            .expect("absolute regular .deb file is accepted");

        assert_eq!(
            perm_bits(artifact.path()),
            0o400,
            "snapshot must have mode 0400"
        );

        let verified = artifact
            .open_verified()
            .expect("untampered snapshot verifies");
        assert_eq!(verified.format(), PackageFormat::Debian);
        assert_eq!(verified.sha256(), SHA256_ABC);
        // The descriptor is rewound before return, so the first read yields
        // the full frozen bytes.
        assert_eq!(
            io::read_to_string(&mut verified.file()).expect("read pinned handle"),
            "abc",
            "open_verified must rewind the handle to the start"
        );

        // Verification checks the frozen snapshot, not the source: mutating
        // the source after construction must not break verification.
        fs::write(&source, b"def").expect("mutate source file");
        artifact
            .open_verified()
            .expect("frozen snapshot still verifies after source mutation");
    }

    #[cfg(unix)]
    #[test]
    fn open_verified_pins_bytes_across_path_replacement() {
        let scratch = ScratchDir::new();
        let source = scratch.write("package.deb", b"abc");
        let artifact =
            FrozenPackageArtifact::new(PackageFormat::Debian, &source).expect("freeze succeeds");
        let snapshot = artifact.path().to_path_buf();

        let mut handle = artifact
            .open_verified()
            .expect("untampered snapshot verifies")
            .into_file();
        let mut first = Vec::new();
        io::copy(&mut handle, &mut first).expect("first read from pinned handle");
        assert_eq!(first, b"abc", "handle must be rewound and readable");

        // Replace the snapshot path with different bytes. The private root
        // is owned by this process with mode 0700, so the swap succeeds.
        let backup = scratch.path().join("backup.deb");
        fs::rename(&snapshot, &backup).expect("move verified snapshot aside");
        fs::write(&snapshot, b"tampered").expect("write replacement at snapshot path");
        assert_eq!(
            fs::read(&snapshot).expect("read replacement"),
            b"tampered",
            "the on-disk path now holds the replacement bytes"
        );

        // The pinned descriptor still reads the original verified inode.
        handle
            .seek(SeekFrom::Start(0))
            .expect("rewind pinned handle");
        let mut second = Vec::new();
        io::copy(&mut handle, &mut second).expect("second read from pinned handle");
        assert_eq!(
            second, b"abc",
            "path replacement must not change bytes read from the pinned handle"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_fifo_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::FileTypeExt;
        use std::sync::mpsc;

        let scratch = ScratchDir::new();
        let fifo = scratch.path().join("pipe.deb");
        let c_path = CString::new(fifo.as_os_str().as_bytes()).expect("no NUL in path");
        assert_eq!(
            unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) },
            0,
            "mkfifo must succeed"
        );
        assert!(
            fs::metadata(&fifo)
                .expect("stat fifo")
                .file_type()
                .is_fifo(),
            "scratch entry must be a fifo"
        );

        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = FrozenPackageArtifact::new(PackageFormat::Debian, &fifo);
            let _ = sender.send(result);
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("freeze must not block on a fifo");
        worker.join().expect("freeze worker must terminate");

        assert!(
            matches!(result, Err(Error::NonRegularFile)),
            "a fifo must be rejected as a non-regular file, got {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_write_to_snapshot_is_rejected_or_detected() {
        let scratch = ScratchDir::new();
        let source = scratch.write("package.deb", b"abc");
        let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &source)
            .expect("absolute regular .deb file is accepted");
        let snapshot = artifact.path().to_path_buf();

        assert_eq!(
            perm_bits(&snapshot),
            0o400,
            "snapshot must be read-only for its owner"
        );

        match fs::write(&snapshot, b"xyz") {
            Ok(()) => {
                // Root bypasses the 0400 owner mode, so the write lands. The
                // digest changes and verification must detect it.
                assert!(matches!(
                    artifact.open_verified(),
                    Err(Error::SnapshotChanged)
                ));
            }
            Err(error) => {
                assert_eq!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied,
                    "non-root direct write must be rejected"
                );
                artifact
                    .open_verified()
                    .expect("untampered snapshot still verifies");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn chmod_and_write_tampering_fails_open_verified() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = ScratchDir::new();
        let source = scratch.write("package.deb", b"abc");
        let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &source)
            .expect("absolute regular .deb file is accepted");
        let snapshot = artifact.path();

        let mut permissions = fs::metadata(snapshot).expect("stat snapshot").permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(snapshot, permissions).expect("chmod snapshot to 0600");
        fs::write(snapshot, b"def").expect("write through owner-writable mode");

        let error = artifact
            .open_verified()
            .expect_err("tampered snapshot must fail");
        assert!(
            matches!(error, Error::SnapshotChanged),
            "expected SnapshotChanged, got {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "package snapshot no longer matches the frozen digest or mode"
        );
        assert!(error.source().is_none(), "SnapshotChanged has no source");
    }

    #[cfg(unix)]
    #[test]
    fn mode_only_tampering_fails_open_verified() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = ScratchDir::new();
        let source = scratch.write("package.deb", b"abc");
        let artifact = FrozenPackageArtifact::new(PackageFormat::Debian, &source)
            .expect("absolute regular .deb file is accepted");
        let snapshot = artifact.path();

        // Change only the mode: bytes are untouched, but the 0400 contract
        // is broken and open_verified must fail on the fstat check.
        let mut permissions = fs::metadata(snapshot).expect("stat snapshot").permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(snapshot, permissions).expect("chmod snapshot to 0600");
        assert_eq!(
            fs::read(snapshot).expect("read snapshot"),
            b"abc",
            "bytes are untouched by mode-only tampering"
        );

        assert!(
            matches!(artifact.open_verified(), Err(Error::SnapshotChanged)),
            "mode-only tampering must fail open_verified"
        );
    }

    #[test]
    fn drop_removes_only_the_private_snapshot() {
        let scratch = ScratchDir::new();
        let first_source = scratch.write("first.deb", b"abc");
        let second_source = scratch.write("second.deb", b"def");

        let first = FrozenPackageArtifact::new(PackageFormat::Debian, &first_source)
            .expect("first freeze succeeds");
        let second = FrozenPackageArtifact::new(PackageFormat::Debian, &second_source)
            .expect("second freeze succeeds");
        let first_snapshot = first.path().to_path_buf();
        let second_snapshot = second.path().to_path_buf();
        assert_ne!(first_snapshot, second_snapshot);

        drop(first);

        assert!(
            !first_snapshot.exists(),
            "drop must remove the dropped artifact's private snapshot"
        );
        assert_eq!(
            fs::read(&second_snapshot).expect("read surviving snapshot"),
            b"def",
            "the other artifact's snapshot must survive"
        );
    }

    #[test]
    fn error_display_and_source_are_exact() {
        let scratch = ScratchDir::new();

        let relative =
            FrozenPackageArtifact::new(PackageFormat::Debian, "package.deb").unwrap_err();
        assert_eq!(relative.to_string(), "package path must be absolute");
        assert!(relative.source().is_none());

        let txt = scratch.write("package.txt", b"abc");
        let wrong_suffix = FrozenPackageArtifact::new(PackageFormat::Debian, &txt).unwrap_err();
        assert_eq!(
            wrong_suffix.to_string(),
            "package file name must end with the exact lowercase .deb suffix"
        );
        assert!(wrong_suffix.source().is_none());

        let missing = scratch.path().join("missing.deb");
        let missing_path = FrozenPackageArtifact::new(PackageFormat::Debian, &missing).unwrap_err();
        assert_eq!(
            missing_path.to_string(),
            "package file cannot be opened or read"
        );
        match missing_path.source() {
            Some(source) => {
                let inner = source.downcast_ref::<io::Error>().expect("io error source");
                assert_eq!(inner.kind(), io::ErrorKind::NotFound);
            }
            None => panic!("SourceIo must expose its source"),
        }

        let dir = scratch.path().join("dir.deb");
        fs::create_dir(&dir).expect("create scratch directory");
        let non_regular = FrozenPackageArtifact::new(PackageFormat::Debian, &dir).unwrap_err();
        assert_eq!(
            non_regular.to_string(),
            "package path must be a regular file"
        );
        assert!(non_regular.source().is_none());

        let symlink = Error::Symlink;
        assert_eq!(
            symlink.to_string(),
            "package path must not be a symbolic link"
        );
        assert!(symlink.source().is_none());

        let source_io = Error::SourceIo(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "open denied",
        ));
        assert_eq!(
            source_io.to_string(),
            "package file cannot be opened or read"
        );
        match source_io.source() {
            Some(source) => {
                let inner = source.downcast_ref::<io::Error>().expect("io error source");
                assert_eq!(inner.kind(), io::ErrorKind::PermissionDenied);
                assert_eq!(inner.to_string(), "open denied");
            }
            None => panic!("SourceIo must expose its source"),
        }

        let metadata_io = Error::MetadataIo(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "stat denied",
        ));
        assert_eq!(
            metadata_io.to_string(),
            "package file metadata cannot be read"
        );
        match metadata_io.source() {
            Some(source) => {
                let inner = source.downcast_ref::<io::Error>().expect("io error source");
                assert_eq!(inner.kind(), io::ErrorKind::PermissionDenied);
                assert_eq!(inner.to_string(), "stat denied");
            }
            None => panic!("MetadataIo must expose its source"),
        }

        let snapshot_io =
            Error::SnapshotIo(io::Error::new(io::ErrorKind::UnexpectedEof, "sync failed"));
        assert_eq!(
            snapshot_io.to_string(),
            "package snapshot cannot be created, written, or synced"
        );
        match snapshot_io.source() {
            Some(source) => {
                let inner = source.downcast_ref::<io::Error>().expect("io error source");
                assert_eq!(inner.kind(), io::ErrorKind::UnexpectedEof);
                assert_eq!(inner.to_string(), "sync failed");
            }
            None => panic!("SnapshotIo must expose its source"),
        }

        let verify_io =
            Error::VerifyIo(io::Error::new(io::ErrorKind::UnexpectedEof, "read failed"));
        assert_eq!(
            verify_io.to_string(),
            "package snapshot cannot be opened, read, or rewound for verification"
        );
        match verify_io.source() {
            Some(source) => {
                let inner = source.downcast_ref::<io::Error>().expect("io error source");
                assert_eq!(inner.kind(), io::ErrorKind::UnexpectedEof);
                assert_eq!(inner.to_string(), "read failed");
            }
            None => panic!("VerifyIo must expose its source"),
        }
    }
}
