//! Canonical tracked repository metadata initialization.
//!
//! The record spelling in this module is an implementation-defined encoding:
//! the repository chapters require the information, but do not prescribe exact
//! bytes.  We deliberately use ordinary Orna record expressions and validate
//! every persisted record through `orna-syntax-v1`; this module is not a
//! second parser for an ad hoc metadata language.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
};

use fs2::FileExt;
use tempfile::TempDir;
use uuid::{Uuid, Version};

use orna_syntax_v1::{Expr, LiteralKind, parse_row};

use crate::{Repository, RepositoryError, scrub_git_routing_environment};

const FORMAT_DIRECTORY: &str = ".orna";
const FORMAT_FILE: &str = "format.orna";
const DATABASE_FILE: &str = "database.orna";
const MAIN_FILE: &str = "main.orna";
const MAX_METADATA_BYTES: u64 = 64 * 1024;
const REPOSITORY_FORMAT: i64 = 1;
const STORAGE_PROFILE: &str = "compact-storage-v1";

/// A stable repository identity, represented by UUIDv4 bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DatabaseId(Uuid);

impl DatabaseId {
    fn new_v4() -> Self {
        Self(Uuid::new_v4())
    }

    /// The canonical UUIDv4 bytes used by runtime and storage adapters.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl fmt::Display for DatabaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The parsed canonical tracked metadata for one repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryMetadata {
    database_id: DatabaseId,
}

impl RepositoryMetadata {
    /// The stable identity persisted in `.orna/database.orna`.
    pub const fn database_id(&self) -> DatabaseId {
        self.database_id
    }

    /// The supported repository metadata format version.
    pub const fn repository_format(&self) -> i64 {
        REPOSITORY_FORMAT
    }

    /// The supported physical storage profile.
    pub const fn storage_profile(&self) -> &'static str {
        STORAGE_PROFILE
    }
}

/// Successful repository metadata initialization or reinitialization.
#[derive(Clone, Debug)]
pub struct RepositoryInitialization {
    repository: Repository,
    metadata: RepositoryMetadata,
    created: bool,
}

impl RepositoryInitialization {
    /// The selected Git repository and worktree.
    pub fn repository(&self) -> &Repository {
        &self.repository
    }

    /// The canonical tracked repository metadata.
    pub fn metadata(&self) -> &RepositoryMetadata {
        &self.metadata
    }

    /// Whether this call created the metadata identity.
    pub const fn created(&self) -> bool {
        self.created
    }

    /// Consumes this result and returns the owned repository boundary.
    pub fn into_repository(self) -> Repository {
        self.repository
    }
}

/// Redacted failures from repository initialization.
#[derive(Debug)]
pub enum RepositoryInitError {
    GitUnavailable,
    GitOperationFailed,
    LocalStateUnavailable,
    UnsafeMetadataPath,
    MetadataIncomplete,
    MetadataMalformed,
    MetadataUnsupported,
    MetadataChanged,
    RepositoryBusy,
    PlatformUnsupported,
}

impl RepositoryInitError {
    /// Stable machine-readable code, without host paths or Git output.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::GitUnavailable => "ORNA-REPO-INIT-001",
            Self::GitOperationFailed => "ORNA-REPO-INIT-002",
            Self::LocalStateUnavailable => "ORNA-REPO-INIT-003",
            Self::UnsafeMetadataPath => "ORNA-REPO-INIT-004",
            Self::MetadataIncomplete => "ORNA-REPO-INIT-005",
            Self::MetadataMalformed => "ORNA-REPO-INIT-006",
            Self::MetadataUnsupported => "ORNA-REPO-INIT-007",
            Self::MetadataChanged => "ORNA-REPO-INIT-008",
            Self::RepositoryBusy => "ORNA-REPO-INIT-009",
            Self::PlatformUnsupported => "ORNA-REPO-INIT-010",
        }
    }
}

impl fmt::Display for RepositoryInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::GitUnavailable => "Git is unavailable",
            Self::GitOperationFailed => "Git repository initialization failed",
            Self::LocalStateUnavailable => "local repository state is unavailable",
            Self::UnsafeMetadataPath => "unsafe repository metadata path",
            Self::MetadataIncomplete => "repository metadata is incomplete",
            Self::MetadataMalformed => "repository metadata is malformed",
            Self::MetadataUnsupported => "repository metadata is unsupported",
            Self::MetadataChanged => "repository metadata changed during initialization",
            Self::RepositoryBusy => "repository initialization is already in progress",
            Self::PlatformUnsupported => {
                "repository initialization is unsupported on this platform"
            }
        };
        f.write_str(message)
    }
}

impl std::error::Error for RepositoryInitError {}

impl From<RepositoryError> for RepositoryInitError {
    fn from(error: RepositoryError) -> Self {
        match error {
            RepositoryError::GitUnavailable => Self::GitUnavailable,
            RepositoryError::GitOperationFailed | RepositoryError::NotAWorktree => {
                Self::GitOperationFailed
            }
            RepositoryError::RepositoryBusy => Self::MetadataChanged,
            RepositoryError::UnsafeManagedPath => Self::UnsafeMetadataPath,
            _ => Self::LocalStateUnavailable,
        }
    }
}

/// Initializes Git at `target` with Git's default options, then creates the
/// tracked Orna metadata when it is absent.
///
/// This never stages or commits files and does not initialize runtime/storage
/// services. Existing canonical metadata is read unchanged and retains its
/// original database identity.
pub fn initialize_repository(
    target: impl AsRef<Path>,
) -> Result<RepositoryInitialization, RepositoryInitError> {
    let target = target.as_ref();
    initialize_git(target)?;
    let repository = Repository::discover(target)?;
    let _lock = acquire_init_lock(&repository)?;

    preflight_main_file(&repository)?;
    let (metadata, created) = match inspect_metadata_unlocked(&repository)? {
        Some(metadata) => (metadata, false),
        None => {
            let metadata = RepositoryMetadata {
                database_id: DatabaseId::new_v4(),
            };
            create_metadata_unlocked(&repository, &metadata)?;
            (metadata, true)
        }
    };
    ensure_main_file_unlocked(&repository)?;
    Ok(RepositoryInitialization {
        repository,
        metadata,
        created,
    })
}

/// Reads existing canonical metadata without modifying the repository.
pub fn inspect_metadata(
    repository: &Repository,
) -> Result<Option<RepositoryMetadata>, RepositoryInitError> {
    inspect_metadata_unlocked(repository)
}

fn initialize_git(target: &Path) -> Result<(), RepositoryInitError> {
    let mut command = Command::new("git");
    command.arg("init").arg("--").arg(target);
    scrub_git_routing_environment(&mut command);
    let output = command
        .output()
        .map_err(|_| RepositoryInitError::GitUnavailable)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(RepositoryInitError::GitOperationFailed)
    }
}

fn inspect_metadata_unlocked(
    repository: &Repository,
) -> Result<Option<RepositoryMetadata>, RepositoryInitError> {
    let metadata_root = metadata_root(repository);
    match fs::symlink_metadata(&metadata_root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(RepositoryInitError::LocalStateUnavailable),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(RepositoryInitError::UnsafeMetadataPath);
        }
        Ok(_) => {}
    }

    let format = read_regular_file(&metadata_root.join(FORMAT_FILE))?;
    let database = read_regular_file(&metadata_root.join(DATABASE_FILE))?;
    match (format, database) {
        (None, None) => Err(RepositoryInitError::MetadataIncomplete),
        (Some(format), Some(database)) => {
            parse_format(&format)?;
            let database_id = parse_database(&database)?;
            Ok(Some(RepositoryMetadata { database_id }))
        }
        _ => Err(RepositoryInitError::MetadataIncomplete),
    }
}

fn create_metadata_unlocked(
    repository: &Repository,
    metadata: &RepositoryMetadata,
) -> Result<(), RepositoryInitError> {
    let root = metadata_root(repository);
    match fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(RepositoryInitError::UnsafeMetadataPath);
        }
        Ok(_) => return Err(RepositoryInitError::MetadataChanged),
        Err(_) => return Err(RepositoryInitError::LocalStateUnavailable),
    }
    let staging = tempfile::Builder::new()
        .prefix(".orna-init-")
        .tempdir_in(repository.worktree())
        .map_err(|_| RepositoryInitError::LocalStateUnavailable)?;
    stage_metadata(&staging, metadata)?;
    publish_staging_directory(repository, &staging)
}

fn stage_metadata(
    staging: &TempDir,
    metadata: &RepositoryMetadata,
) -> Result<(), RepositoryInitError> {
    create_new_file(&staging.path().join(FORMAT_FILE), format_bytes())?;
    create_new_file(
        &staging.path().join(DATABASE_FILE),
        &database_bytes(metadata),
    )?;
    sync_directory(staging.path())?;
    sync_directory(
        staging
            .path()
            .parent()
            .ok_or(RepositoryInitError::LocalStateUnavailable)?,
    )
}

fn ensure_main_file_unlocked(repository: &Repository) -> Result<(), RepositoryInitError> {
    let main = repository.worktree().join(MAIN_FILE);
    preflight_main_path(&main)?;
    match fs::symlink_metadata(&main) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_new_file(&main, b"")?;
            sync_directory(repository.worktree())
        }
        Err(_) => Err(RepositoryInitError::LocalStateUnavailable),
    }
}

fn preflight_main_file(repository: &Repository) -> Result<(), RepositoryInitError> {
    preflight_main_path(&repository.worktree().join(MAIN_FILE))
}

fn preflight_main_path(main: &Path) -> Result<(), RepositoryInitError> {
    match fs::symlink_metadata(main) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(RepositoryInitError::UnsafeMetadataPath)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(RepositoryInitError::LocalStateUnavailable),
    }
}

fn metadata_root(repository: &Repository) -> PathBuf {
    repository.worktree().join(FORMAT_DIRECTORY)
}

#[cfg(target_os = "linux")]
fn publish_staging_directory(
    repository: &Repository,
    staging: &TempDir,
) -> Result<(), RepositoryInitError> {
    use rustix::fs::{RenameFlags, renameat_with};

    let parent = repository.worktree();
    let parent_file = File::open(parent).map_err(|_| RepositoryInitError::LocalStateUnavailable)?;
    let staging_name = staging
        .path()
        .file_name()
        .ok_or(RepositoryInitError::LocalStateUnavailable)?;
    renameat_with(
        &parent_file,
        Path::new(staging_name),
        &parent_file,
        FORMAT_DIRECTORY,
        RenameFlags::NOREPLACE,
    )
    .map_err(|error| match error.raw_os_error() {
        libc::EEXIST | libc::ENOTEMPTY => RepositoryInitError::MetadataChanged,
        libc::ENOSYS | libc::EINVAL => RepositoryInitError::PlatformUnsupported,
        _ => RepositoryInitError::LocalStateUnavailable,
    })?;
    sync_directory(parent)
}

#[cfg(not(target_os = "linux"))]
fn publish_staging_directory(
    _repository: &Repository,
    _staging: &TempDir,
) -> Result<(), RepositoryInitError> {
    Err(RepositoryInitError::PlatformUnsupported)
}

fn validate_directory(path: &Path) -> Result<(), RepositoryInitError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => Ok(()),
        Ok(_) => Err(RepositoryInitError::UnsafeMetadataPath),
        Err(_) => Err(RepositoryInitError::LocalStateUnavailable),
    }
}

fn read_regular_file(path: &Path) -> Result<Option<Vec<u8>>, RepositoryInitError> {
    validate_directory(
        path.parent()
            .ok_or(RepositoryInitError::UnsafeMetadataPath)?,
    )?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        options.custom_flags(libc::O_NOFOLLOW);
        let mut file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                return Err(RepositoryInitError::UnsafeMetadataPath);
            }
            Err(_) => return Err(RepositoryInitError::LocalStateUnavailable),
        };
        let descriptor = file
            .metadata()
            .map_err(|_| RepositoryInitError::LocalStateUnavailable)?;
        if !descriptor.is_file() || descriptor.nlink() != 1 {
            return Err(RepositoryInitError::UnsafeMetadataPath);
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_METADATA_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| RepositoryInitError::LocalStateUnavailable)?;
        if bytes.len() as u64 > MAX_METADATA_BYTES {
            return Err(RepositoryInitError::MetadataMalformed);
        }
        Ok(Some(bytes))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(RepositoryInitError::PlatformUnsupported)
    }
}

fn create_new_file(path: &Path, bytes: &[u8]) -> Result<(), RepositoryInitError> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                RepositoryInitError::MetadataChanged
            } else {
                RepositoryInitError::LocalStateUnavailable
            }
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| RepositoryInitError::LocalStateUnavailable)
}

fn sync_directory(path: &Path) -> Result<(), RepositoryInitError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| RepositoryInitError::LocalStateUnavailable)
}

struct InitLock {
    file: File,
}

impl Drop for InitLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn acquire_init_lock(repository: &Repository) -> Result<InitLock, RepositoryInitError> {
    ensure_directory_tree(repository.runtime_paths().root())?;
    let directory = repository.runtime_paths().locks();
    ensure_directory_tree(&directory)?;
    let path = directory.join("coordination.lock");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(RepositoryInitError::UnsafeMetadataPath);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(RepositoryInitError::LocalStateUnavailable),
    }
    let file = open_lock_file(&path)?;
    file.try_lock_exclusive()
        .map_err(|_| RepositoryInitError::RepositoryBusy)?;
    Ok(InitLock { file })
}

fn ensure_directory_tree(path: &Path) -> Result<(), RepositoryInitError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(RepositoryInitError::UnsafeMetadataPath)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or(RepositoryInitError::UnsafeMetadataPath)?;
            ensure_directory_tree(parent)?;
            match fs::create_dir(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    validate_directory(path)
                }
                Err(_) => Err(RepositoryInitError::LocalStateUnavailable),
            }
        }
        Err(_) => Err(RepositoryInitError::LocalStateUnavailable),
    }
}

fn open_lock_file(path: &Path) -> Result<File, RepositoryInitError> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .map_err(|_| RepositoryInitError::LocalStateUnavailable)
}

fn format_bytes() -> &'static [u8] {
    b"{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n"
}

fn database_bytes(metadata: &RepositoryMetadata) -> Vec<u8> {
    format!("{{database_id: \"{}\"}}\n", metadata.database_id).into_bytes()
}

fn parse_format(bytes: &[u8]) -> Result<(), RepositoryInitError> {
    let fields = parse_record(bytes)?;
    if fields.len() != 2 {
        return Err(RepositoryInitError::MetadataMalformed);
    }
    let Some(repository_format) = fields
        .iter()
        .find(|field| field.name == "repository_format")
    else {
        return Err(RepositoryInitError::MetadataMalformed);
    };
    let Some(storage_profile) = fields.iter().find(|field| field.name == "storage_profile") else {
        return Err(RepositoryInitError::MetadataMalformed);
    };
    if fields
        .iter()
        .any(|field| field.name != "repository_format" && field.name != "storage_profile")
    {
        return Err(RepositoryInitError::MetadataMalformed);
    }
    if integer_literal(&repository_format.value) != Some(REPOSITORY_FORMAT)
        || string_literal(&storage_profile.value) != Some(STORAGE_PROFILE)
    {
        return Err(RepositoryInitError::MetadataUnsupported);
    }
    Ok(())
}

fn parse_database(bytes: &[u8]) -> Result<DatabaseId, RepositoryInitError> {
    let fields = parse_record(bytes)?;
    if fields.len() != 1 || fields[0].name != "database_id" {
        return Err(RepositoryInitError::MetadataMalformed);
    }
    let value = string_literal(&fields[0].value).ok_or(RepositoryInitError::MetadataMalformed)?;
    DatabaseId::from_str(value).map_err(|_| RepositoryInitError::MetadataMalformed)
}

fn parse_record(bytes: &[u8]) -> Result<Vec<orna_syntax_v1::RecordField>, RepositoryInitError> {
    let source = std::str::from_utf8(bytes).map_err(|_| RepositoryInitError::MetadataMalformed)?;
    let parsed = parse_row(source);
    if !parsed.is_ok() {
        return Err(RepositoryInitError::MetadataMalformed);
    }
    match parsed.value {
        Expr::Record { fields, .. } => Ok(fields),
        _ => Err(RepositoryInitError::MetadataMalformed),
    }
}

fn integer_literal(value: &Expr) -> Option<i64> {
    let Expr::Literal {
        text,
        kind: LiteralKind::Integer,
        ..
    } = value
    else {
        return None;
    };
    text.parse().ok()
}

fn string_literal(value: &Expr) -> Option<&str> {
    let Expr::Literal {
        text,
        kind: LiteralKind::String,
        ..
    } = value
    else {
        return None;
    };
    text.strip_prefix('"')?.strip_suffix('"')
}

impl FromStr for DatabaseId {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let uuid = Uuid::parse_str(value).map_err(|_| ())?;
        if uuid.get_version() != Some(Version::Random) || uuid.to_string() != value {
            return Err(());
        }
        Ok(Self(uuid))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use super::{
        DATABASE_FILE, FORMAT_DIRECTORY, FORMAT_FILE, Repository, RepositoryInitError,
        initialize_repository, inspect_metadata,
    };

    fn git(directory: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(directory)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn initializes_canonical_records_and_reuses_the_database_identity() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("database");
        let initialized = initialize_repository(&target).unwrap();
        let database_id = initialized.metadata().database_id().to_string();
        assert!(initialized.created());
        assert_eq!(
            fs::read_to_string(target.join(FORMAT_DIRECTORY).join(FORMAT_FILE)).unwrap(),
            "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n"
        );
        assert_eq!(
            fs::read_to_string(target.join(FORMAT_DIRECTORY).join(DATABASE_FILE)).unwrap(),
            format!("{{database_id: \"{database_id}\"}}\n")
        );
        assert_eq!(fs::read(target.join("main.orna")).unwrap(), b"");
        assert_eq!(
            inspect_metadata(initialized.repository())
                .unwrap()
                .unwrap()
                .database_id()
                .to_string(),
            database_id
        );

        let reinitialized = initialize_repository(&target).unwrap();
        assert!(!reinitialized.created());
        assert_eq!(
            reinitialized.metadata().database_id().to_string(),
            database_id
        );
    }

    #[test]
    fn leaves_existing_head_index_and_user_worktree_content_unchanged() {
        let target = tempfile::tempdir().unwrap();
        git(target.path(), &["init", "-b", "main"]);
        git(
            target.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(
            target.path(),
            &["config", "user.name", "Repository init test"],
        );
        fs::write(target.path().join("ordinary.txt"), "base\n").unwrap();
        fs::write(target.path().join("main.orna"), "module main;\n").unwrap();
        git(target.path(), &["add", "."]);
        git(target.path(), &["commit", "-m", "base"]);
        fs::write(target.path().join("ordinary.txt"), "staged\n").unwrap();
        git(target.path(), &["add", "ordinary.txt"]);
        fs::write(target.path().join("main.orna"), "unstaged\n").unwrap();
        let head = git(target.path(), &["rev-parse", "HEAD"]);
        let index = git(target.path(), &["ls-files", "--stage"]);

        initialize_repository(target.path()).unwrap();

        assert_eq!(git(target.path(), &["rev-parse", "HEAD"]), head);
        assert_eq!(git(target.path(), &["ls-files", "--stage"]), index);
        assert_eq!(
            fs::read_to_string(target.path().join("ordinary.txt")).unwrap(),
            "staged\n"
        );
        assert_eq!(
            fs::read_to_string(target.path().join("main.orna")).unwrap(),
            "unstaged\n"
        );
    }

    #[test]
    fn existing_empty_metadata_directory_fails_closed_without_writes() {
        let target = tempfile::tempdir().unwrap();
        git(target.path(), &["init", "-b", "main"]);
        fs::create_dir(target.path().join(FORMAT_DIRECTORY)).unwrap();

        let error = initialize_repository(target.path()).unwrap_err();
        assert_eq!(error.code(), "ORNA-REPO-INIT-005");
        assert!(matches!(error, RepositoryInitError::MetadataIncomplete));
        assert!(
            !target
                .path()
                .join(FORMAT_DIRECTORY)
                .join(FORMAT_FILE)
                .exists()
        );
        assert!(!target.path().join("main.orna").exists());
    }

    #[test]
    fn treats_a_dash_prefixed_target_as_a_path() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("--bare");
        let initialized = initialize_repository(&target).unwrap();
        assert_eq!(initialized.repository().worktree(), target);
        assert!(target.join(".git").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlinked_runtime_root_before_opening_its_existing_lock() {
        use std::os::unix::fs::symlink;

        let target = tempfile::tempdir().unwrap();
        git(target.path(), &["init", "-b", "main"]);
        let repository = Repository::discover(target.path()).unwrap();
        let runtime = repository.runtime_paths().root().to_owned();
        let outside = target.path().join("outside");
        fs::create_dir_all(outside.join("locks")).unwrap();
        symlink(&outside, &runtime).unwrap();

        let error = initialize_repository(target.path()).unwrap_err();
        assert_eq!(error.code(), "ORNA-REPO-INIT-004");
        assert!(!outside.join("locks/coordination.lock").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn no_replace_publication_preserves_an_existing_metadata_directory() {
        let target = tempfile::tempdir().unwrap();
        git(target.path(), &["init", "-b", "main"]);
        let repository = Repository::discover(target.path()).unwrap();
        let root = target.path().join(FORMAT_DIRECTORY);
        fs::create_dir(&root).unwrap();
        fs::write(root.join("marker"), "preserve me\n").unwrap();
        let metadata = super::RepositoryMetadata {
            database_id: super::DatabaseId::new_v4(),
        };
        let staging = tempfile::Builder::new()
            .prefix(".orna-init-")
            .tempdir_in(target.path())
            .unwrap();
        super::stage_metadata(&staging, &metadata).unwrap();

        let error = super::publish_staging_directory(&repository, &staging).unwrap_err();
        assert!(matches!(error, RepositoryInitError::MetadataChanged));
        assert_eq!(
            fs::read_to_string(root.join("marker")).unwrap(),
            "preserve me\n"
        );
    }
}
