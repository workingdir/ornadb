//! Extracted from `lib.rs` along its seams; contents are moved verbatim.

use super::*;

/// Version marker of the Git index lock this repository claims.
pub(crate) const GIT_INDEX_LOCK_MAGIC: &[u8] = b"ORNA-GIT-INDEX-LOCK\0";

pub(crate) struct GitIndexLock {
    path: PathBuf,
    marker: Vec<u8>,
    _file: fs::File,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum GitIndexLockReclaimPoint {
    BeforeTransfer,
    AfterTransfer,
    BeforePrivateClaimDisposal,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Eq, PartialEq)]
struct GitIndexLockFile {
    bytes: Vec<u8>,
    metadata: GitIndexLockFileMetadata,
}

#[cfg(target_os = "linux")]
impl GitIndexLockFile {
    fn matches_after_transfer(&self, other: &Self) -> bool {
        self.bytes == other.bytes && self.metadata.matches_after_rename(other.metadata)
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Eq, PartialEq)]
struct GitIndexLockFileMetadata {
    device: u64,
    inode: u64,
    mode: u32,
    size: i64,
    modified_seconds: i64,
    modified_nanoseconds: u64,
    changed_seconds: i64,
    changed_nanoseconds: u64,
}

#[cfg(target_os = "linux")]
impl GitIndexLockFileMetadata {
    fn from_stat(stat: &rustix::fs::Stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            size: stat.st_size,
            modified_seconds: stat.mtime(),
            modified_nanoseconds: stat.st_mtime_nsec,
            changed_seconds: stat.ctime(),
            changed_nanoseconds: stat.st_ctime_nsec,
        }
    }

    fn is_regular(stat: &rustix::fs::Stat) -> bool {
        rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::RegularFile
    }

    // rename(2) updates ctime, so provenance across the transfer excludes it.
    fn matches_after_rename(self, other: Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.mode == other.mode
            && self.size == other.size
            && self.modified_seconds == other.modified_seconds
            && self.modified_nanoseconds == other.modified_nanoseconds
    }
}

#[cfg(target_os = "linux")]
struct GitIndexLockDirectory {
    fd: rustix::fd::OwnedFd,
}

#[cfg(target_os = "linux")]
impl GitIndexLockDirectory {
    fn open(path: &Path) -> Result<(Self, OsString), RepositoryError> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let name = path
            .file_name()
            .ok_or(RepositoryError::LocalStateUnavailable)?
            .to_os_string();
        let fd = rustix::fs::openat(
            rustix::fs::CWD,
            parent,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok((Self { fd }, name))
    }

    fn name_exists(&self, name: &OsStr) -> Result<bool, RepositoryError> {
        match rustix::fs::statat(&self.fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Ok(true),
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(_) => Err(RepositoryError::LocalStateUnavailable),
        }
    }

    fn metadata_matches(
        &self,
        name: &OsStr,
        expected: GitIndexLockFileMetadata,
    ) -> Result<bool, RepositoryError> {
        match rustix::fs::statat(&self.fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(GitIndexLockFileMetadata::is_regular(&stat)
                && GitIndexLockFileMetadata::from_stat(&stat) == expected),
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(_) => Err(RepositoryError::LocalStateUnavailable),
        }
    }

    fn read_regular(&self, name: &OsStr) -> Result<GitIndexLockFile, RepositoryError> {
        let before = rustix::fs::statat(&self.fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| RepositoryError::GitIndexLockPresent)?;
        if !GitIndexLockFileMetadata::is_regular(&before) {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        let fd = rustix::fs::openat(
            &self.fd,
            name,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| RepositoryError::GitIndexLockPresent)?;
        let opened = rustix::fs::fstat(&fd).map_err(|_| RepositoryError::GitIndexLockPresent)?;
        if !GitIndexLockFileMetadata::is_regular(&opened)
            || GitIndexLockFileMetadata::from_stat(&before)
                != GitIndexLockFileMetadata::from_stat(&opened)
        {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        let mut file = fs::File::from(fd);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|_| RepositoryError::GitIndexLockPresent)?;
        let after_open =
            rustix::fs::fstat(&file).map_err(|_| RepositoryError::GitIndexLockPresent)?;
        let after_name = rustix::fs::statat(&self.fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| RepositoryError::GitIndexLockPresent)?;
        let metadata = GitIndexLockFileMetadata::from_stat(&opened);
        if !GitIndexLockFileMetadata::is_regular(&after_open)
            || !GitIndexLockFileMetadata::is_regular(&after_name)
            || GitIndexLockFileMetadata::from_stat(&after_open) != metadata
            || GitIndexLockFileMetadata::from_stat(&after_name) != metadata
        {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        Ok(GitIndexLockFile { bytes, metadata })
    }

    fn abandoned_marker(
        &self,
        name: &OsStr,
        journal_binding: Option<[u8; 32]>,
    ) -> Result<GitIndexLockFile, RepositoryError> {
        let file = self.read_regular(name)?;
        let Some(marker) = GitIndexLockMarker::decode(&file.bytes) else {
            return Err(RepositoryError::GitIndexLockPresent);
        };
        if !GitIndexLockMarker::binding_permits_reclaim(marker.journal_binding, journal_binding)
            || marker.owner_liveness() != GitIndexLockOwnerLiveness::ProvenDead
        {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        Ok(file)
    }

    fn rename_no_replace(&self, from: &OsStr, to: &OsStr) -> Result<(), RepositoryError> {
        rustix::fs::renameat_with(
            &self.fd,
            from,
            &self.fd,
            to,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|_| RepositoryError::GitIndexLockPresent)
    }

    fn unlink(&self, name: &OsStr) -> Result<(), RepositoryError> {
        rustix::fs::unlinkat(&self.fd, name, rustix::fs::AtFlags::empty())
            .map_err(|_| RepositoryError::GitIndexLockPresent)
    }

    fn private_entries(&self, canonical: &OsStr) -> Result<Vec<OsString>, RepositoryError> {
        rustix::fs::seek(&self.fd, rustix::fs::SeekFrom::Start(0))
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let prefix = GitIndexLock::private_prefix(canonical);
        let mut entries = Vec::new();
        let mut buffer = [MaybeUninit::uninit(); 4096];
        let mut directory = rustix::fs::RawDir::new(&self.fd, &mut buffer);
        while let Some(entry) = directory.next() {
            let entry = entry.map_err(|_| RepositoryError::LocalStateUnavailable)?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes()).to_os_string();
            if name.as_bytes().starts_with(prefix.as_bytes()) {
                entries.push(name);
            }
        }
        Ok(entries)
    }
}

impl GitIndexLock {
    pub(crate) fn acquire(path: PathBuf) -> Result<Self, RepositoryError> {
        Self::acquire_with_binding(path, [0; 32])
    }

    pub(crate) fn acquire_owned(
        path: PathBuf,
        journal_binding: [u8; 32],
    ) -> Result<Self, RepositoryError> {
        Self::acquire_with_binding(path, journal_binding)
    }

    fn acquire_with_binding(
        path: PathBuf,
        journal_binding: [u8; 32],
    ) -> Result<Self, RepositoryError> {
        let marker = GitIndexLockMarker::new(journal_binding)?.encode();
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        let mut file = options.open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                // Never remove an existing Git lock: it may belong to a live
                // ordinary Git writer or require Git's own recovery.
                RepositoryError::GitIndexLockPresent
            } else {
                RepositoryError::LocalStateUnavailable
            }
        })?;
        if file
            .write_all(&marker)
            .and_then(|_| file.sync_all())
            .is_err()
        {
            let _ = fs::remove_file(&path);
            return Err(RepositoryError::LocalStateUnavailable);
        }
        Ok(Self {
            path,
            marker,
            _file: file,
        })
    }

    pub(crate) fn reclaim_abandoned(
        path: PathBuf,
        journal_binding: [u8; 32],
    ) -> Result<(), RepositoryError> {
        Self::reclaim_bound(path, Some(journal_binding))
    }

    /// Reclaims an Orna-marked lock whose publisher stopped before writing its
    /// journal, so no journal binding exists to check. The marker must carry a
    /// non-zero publisher binding and the owner must be proven dead.
    pub(crate) fn reclaim_unbound_orphan(path: PathBuf) -> Result<(), RepositoryError> {
        Self::reclaim_bound(path, None)
    }

    fn reclaim_bound(
        path: PathBuf,
        journal_binding: Option<[u8; 32]>,
    ) -> Result<(), RepositoryError> {
        #[cfg(target_os = "linux")]
        {
            Self::reclaim_abandoned_linux(path, journal_binding, |_, _| {})
        }

        #[cfg(not(target_os = "linux"))]
        Self::reclaim_abandoned_unavailable(path, journal_binding)
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn reclaim_abandoned_with_hook(
        path: PathBuf,
        journal_binding: [u8; 32],
        hook: impl FnMut(GitIndexLockReclaimPoint, &Path),
    ) -> Result<(), RepositoryError> {
        Self::reclaim_abandoned_linux(path, Some(journal_binding), hook)
    }

    #[cfg(target_os = "linux")]
    fn reclaim_abandoned_linux(
        path: PathBuf,
        journal_binding: Option<[u8; 32]>,
        mut hook: impl FnMut(GitIndexLockReclaimPoint, &Path),
    ) -> Result<(), RepositoryError> {
        let (directory, canonical) = GitIndexLockDirectory::open(&path)?;
        let private_entries = directory.private_entries(&canonical)?;
        if private_entries.len() > 1 {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        if let Some(private_entry) = private_entries.first() {
            if directory.name_exists(&canonical)? {
                return Err(RepositoryError::GitIndexLockPresent);
            }
            return Self::recover_private_entry(
                &directory,
                &canonical,
                private_entry,
                journal_binding,
                &path,
            );
        }
        if !directory.name_exists(&canonical)? {
            return Ok(());
        }

        let original = directory.abandoned_marker(&canonical, journal_binding)?;
        let claim = Self::claim_name(&canonical, &original.bytes);
        hook(GitIndexLockReclaimPoint::BeforeTransfer, &path);
        directory.rename_no_replace(&canonical, &claim)?;
        hook(GitIndexLockReclaimPoint::AfterTransfer, &path);

        let transferred = match directory.abandoned_marker(&claim, journal_binding) {
            Ok(transferred) => transferred,
            Err(_) => {
                let transferred = match directory.read_regular(&claim) {
                    Ok(transferred) => transferred,
                    Err(_) => return Err(RepositoryError::GitIndexLockPresent),
                };
                return Self::restore_claim(&directory, &claim, &canonical, &transferred);
            }
        };
        if !original.matches_after_transfer(&transferred) {
            return Self::restore_claim(&directory, &claim, &canonical, &transferred);
        }

        let claim_path = path.with_file_name(&claim);
        Self::dispose_private_claim(
            &directory,
            &canonical,
            &claim,
            &transferred,
            journal_binding,
            &mut hook,
            &claim_path,
        )?;
        if directory.name_exists(&canonical)? {
            Err(RepositoryError::GitIndexLockPresent)
        } else {
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    fn recover_private_entry(
        directory: &GitIndexLockDirectory,
        canonical: &OsStr,
        private_entry: &OsStr,
        journal_binding: Option<[u8; 32]>,
        path: &Path,
    ) -> Result<(), RepositoryError> {
        let file = directory.abandoned_marker(private_entry, journal_binding)?;
        let claim = Self::claim_name(canonical, &file.bytes);
        let disposal = Self::disposal_name(canonical, &file.bytes);
        if private_entry == claim {
            let claim_path = path.with_file_name(&claim);
            return Self::dispose_private_claim(
                directory,
                canonical,
                &claim,
                &file,
                journal_binding,
                &mut |_, _| {},
                &claim_path,
            );
        }
        if private_entry == disposal {
            return Self::dispose_tombstone(
                directory,
                canonical,
                &claim,
                &disposal,
                &file,
                journal_binding,
            );
        }
        Err(RepositoryError::GitIndexLockPresent)
    }

    #[cfg(target_os = "linux")]
    fn dispose_private_claim(
        directory: &GitIndexLockDirectory,
        canonical: &OsStr,
        claim: &OsStr,
        expected: &GitIndexLockFile,
        journal_binding: Option<[u8; 32]>,
        hook: &mut impl FnMut(GitIndexLockReclaimPoint, &Path),
        claim_path: &Path,
    ) -> Result<(), RepositoryError> {
        hook(
            GitIndexLockReclaimPoint::BeforePrivateClaimDisposal,
            claim_path,
        );
        let current = directory.abandoned_marker(claim, journal_binding)?;
        if current != *expected
            || directory.private_entries(canonical)?.as_slice() != [claim.to_os_string()]
            || !directory.metadata_matches(claim, current.metadata)?
        {
            return Err(RepositoryError::GitIndexLockPresent);
        }

        let disposal = Self::disposal_name(canonical, &current.bytes);
        directory.rename_no_replace(claim, &disposal)?;
        let transferred = match directory.abandoned_marker(&disposal, journal_binding) {
            Ok(transferred) => transferred,
            Err(_) => {
                let transferred = match directory.read_regular(&disposal) {
                    Ok(transferred) => transferred,
                    Err(_) => return Err(RepositoryError::GitIndexLockPresent),
                };
                return Self::restore_claim(directory, &disposal, claim, &transferred);
            }
        };
        if !current.matches_after_transfer(&transferred) {
            return Self::restore_claim(directory, &disposal, claim, &transferred);
        }
        Self::dispose_tombstone(
            directory,
            canonical,
            claim,
            &disposal,
            &transferred,
            journal_binding,
        )
    }

    #[cfg(target_os = "linux")]
    fn dispose_tombstone(
        directory: &GitIndexLockDirectory,
        canonical: &OsStr,
        claim: &OsStr,
        disposal: &OsStr,
        expected: &GitIndexLockFile,
        journal_binding: Option<[u8; 32]>,
    ) -> Result<(), RepositoryError> {
        let current = directory.abandoned_marker(disposal, journal_binding)?;
        if current != *expected || !directory.metadata_matches(disposal, current.metadata)? {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        let external_before = directory.name_exists(canonical)? || directory.name_exists(claim)?;
        let current = directory.abandoned_marker(disposal, journal_binding)?;
        if current != *expected {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        // Only the private tombstone is unlinked after revalidation. A new
        // canonical or claim name is retained and reported as a conflict.
        directory.unlink(disposal)?;
        let external_after = directory.name_exists(canonical)? || directory.name_exists(claim)?;
        if external_before || external_after || !directory.private_entries(canonical)?.is_empty() {
            Err(RepositoryError::GitIndexLockPresent)
        } else {
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    fn restore_claim(
        directory: &GitIndexLockDirectory,
        claim: &OsStr,
        canonical: &OsStr,
        expected: &GitIndexLockFile,
    ) -> Result<(), RepositoryError> {
        let current = match directory.read_regular(claim) {
            Ok(current) => current,
            Err(_) => return Err(RepositoryError::GitIndexLockPresent),
        };
        if current != *expected || !directory.metadata_matches(claim, current.metadata)? {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        let _ = directory.rename_no_replace(claim, canonical);
        Err(RepositoryError::GitIndexLockPresent)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn private_prefix(canonical: &OsStr) -> OsString {
        let mut prefix = OsString::from(".orna-reclaim-");
        prefix.push(Self::digest_hex(canonical.as_bytes()));
        prefix.push("-");
        prefix
    }

    #[cfg(target_os = "linux")]
    fn claim_name(canonical: &OsStr, bytes: &[u8]) -> OsString {
        let mut claim = Self::private_prefix(canonical);
        claim.push(Self::digest_hex(bytes));
        claim
    }

    #[cfg(target_os = "linux")]
    fn digest_hex(bytes: &[u8]) -> String {
        orna_base::hex(bytes)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn disposal_name(canonical: &OsStr, bytes: &[u8]) -> OsString {
        let mut disposal = Self::claim_name(canonical, bytes);
        disposal.push(".dispose");
        disposal
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn claim_path(path: &Path, bytes: &[u8]) -> Result<PathBuf, RepositoryError> {
        let name = path
            .file_name()
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        Ok(path.with_file_name(Self::claim_name(name, bytes)))
    }

    #[cfg(not(target_os = "linux"))]
    fn reclaim_abandoned_unavailable(
        path: PathBuf,
        journal_binding: Option<[u8; 32]>,
    ) -> Result<(), RepositoryError> {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        let bytes = fs::read(&path).map_err(|_| RepositoryError::GitIndexLockPresent)?;
        let Some(marker) = GitIndexLockMarker::decode(&bytes) else {
            return Err(RepositoryError::GitIndexLockPresent);
        };
        if !GitIndexLockMarker::binding_permits_reclaim(marker.journal_binding, journal_binding) {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        Err(RepositoryError::GitIndexLockPresent)
    }
}
impl Drop for GitIndexLock {
    fn drop(&mut self) {
        if fs::read(&self.path).ok().as_deref() == Some(self.marker.as_slice()) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct GitIndexLockMarker {
    pid: u32,
    pub(crate) process_start: u64,
    pub(crate) nonce: [u8; 16],
    journal_binding: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GitIndexLockOwnerLiveness {
    ProvenDead,
    Live,
    Unknown,
}

#[cfg(target_os = "linux")]
pub(crate) enum ProcessStartObservation {
    Observed(u64),
    Missing,
    Unknown,
}

#[cfg(target_os = "linux")]
pub(crate) fn process_owner_liveness(
    expected_start: u64,
    observation: ProcessStartObservation,
) -> GitIndexLockOwnerLiveness {
    match observation {
        ProcessStartObservation::Observed(process_start) if process_start == expected_start => {
            GitIndexLockOwnerLiveness::Live
        }
        ProcessStartObservation::Observed(_) | ProcessStartObservation::Missing => {
            GitIndexLockOwnerLiveness::ProvenDead
        }
        ProcessStartObservation::Unknown => GitIndexLockOwnerLiveness::Unknown,
    }
}

impl GitIndexLockMarker {
    /// Journal-bound reclaim requires the exact binding. Unbound reclaim requires
    /// a publisher binding; the all-zero binding marks ordinary Orna writers,
    /// whose locks publication recovery never reclaims.
    fn binding_permits_reclaim(
        marker_binding: [u8; 32],
        journal_binding: Option<[u8; 32]>,
    ) -> bool {
        match journal_binding {
            Some(expected) => marker_binding == expected,
            None => marker_binding != [0; 32],
        }
    }

    pub(crate) fn new(journal_binding: [u8; 32]) -> Result<Self, RepositoryError> {
        let pid = std::process::id();
        #[cfg(target_os = "linux")]
        let process_start = match process_start_time(pid) {
            ProcessStartObservation::Observed(process_start) => process_start,
            ProcessStartObservation::Missing | ProcessStartObservation::Unknown => {
                return Err(RepositoryError::LocalStateUnavailable);
            }
        };
        #[cfg(not(target_os = "linux"))]
        let process_start = 0;
        Ok(Self {
            pid,
            process_start,
            nonce: Uuid::new_v4().into_bytes(),
            journal_binding,
        })
    }

    pub(crate) fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(GIT_INDEX_LOCK_MAGIC.len() + 1 + 4 + 8 + 16 + 32);
        bytes.extend_from_slice(GIT_INDEX_LOCK_MAGIC);
        bytes.push(1);
        bytes.extend_from_slice(&self.pid.to_be_bytes());
        bytes.extend_from_slice(&self.process_start.to_be_bytes());
        bytes.extend_from_slice(&self.nonce);
        bytes.extend_from_slice(&self.journal_binding);
        bytes
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut cursor = GIT_INDEX_LOCK_MAGIC.len();
        if !bytes.starts_with(GIT_INDEX_LOCK_MAGIC) || bytes.get(cursor)? != &1 {
            return None;
        }
        cursor += 1;
        let pid = u32::from_be_bytes(bytes.get(cursor..cursor + 4)?.try_into().ok()?);
        cursor += 4;
        let process_start = u64::from_be_bytes(bytes.get(cursor..cursor + 8)?.try_into().ok()?);
        cursor += 8;
        let nonce = bytes.get(cursor..cursor + 16)?.try_into().ok()?;
        cursor += 16;
        let journal_binding = bytes.get(cursor..cursor + 32)?.try_into().ok()?;
        cursor += 32;
        (pid != 0 && cursor == bytes.len()).then_some(Self {
            pid,
            process_start,
            nonce,
            journal_binding,
        })
    }

    fn owner_liveness(self) -> GitIndexLockOwnerLiveness {
        #[cfg(target_os = "linux")]
        {
            process_owner_liveness(self.process_start, process_start_time(self.pid))
        }
        #[cfg(not(target_os = "linux"))]
        {
            GitIndexLockOwnerLiveness::Unknown
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn process_start_time(pid: u32) -> ProcessStartObservation {
    // A missing target PID proves death only after a readable, parseable procfs
    // record for this process confirms that procfs itself is available.
    if fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|contents| parse_process_start_time(&contents))
        .is_none()
    {
        return ProcessStartObservation::Unknown;
    }
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(contents) => parse_process_start_time(&contents)
            .map(ProcessStartObservation::Observed)
            .unwrap_or(ProcessStartObservation::Unknown),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ProcessStartObservation::Missing
        }
        Err(_) => ProcessStartObservation::Unknown,
    }
}

#[cfg(target_os = "linux")]
fn parse_process_start_time(contents: &str) -> Option<u64> {
    let fields = contents
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    fields.get(19)?.parse().ok()
}
