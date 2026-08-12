//! Root-only package readiness protocol for Debian maintainer scripts.

use std::{
    env,
    ffi::OsStr,
    fmt, fs,
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::Path,
};

use nix::unistd::{Gid, Group, Uid, User, chown, geteuid, getuid};

const SELECTOR: &str = "ORNA_PACKAGE_MAINTENANCE";
const PACKAGE_ROOT: &str = "/var/lib/orna";
const PACKAGE_LOCK: &str = "/var/lib/orna/package.lock";
const PACKAGE_STATE: &str = "/var/lib/orna/package-state.toml";
const INSTANCE_PARENT: &str = "/var/lib/orna/instances";
const CONFIGURATION: &str = "/etc/orna/instances/default.toml";
const INSTALLED_EXECUTABLE: &str = "/usr/bin/orna";
const CONFIGURATION_BYTES: &[u8] = b"format = 1\ninstance = \"default\"\n";
const READY_BYTES: &[u8] = b"format = 1\nstate = \"ready\"\n";
const INCOMPLETE_BYTES: &[u8] = b"format = 1\nstate = \"incomplete\"\n";

#[derive(Clone, Copy)]
enum Operation {
    Begin,
    Complete,
}

/// A selected package operation that cannot reach its durable commit point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageMaintenanceError {
    /// The exact private selector was supplied by a non-root process.
    RootRequired,
    /// The selected operation could not commit the accepted package state.
    Failed,
}

impl fmt::Display for PackageMaintenanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::RootRequired => "orna: package maintenance requires root",
            Self::Failed => "orna: package maintenance did not complete",
        })
    }
}

impl std::error::Error for PackageMaintenanceError {}

/// Runs an exact private maintenance request when the process has no public arguments.
///
/// This function reads the private selector only when `argument_count` is one, including
/// `argv[0]`. Invalid values return `None` so ordinary public usage remains authoritative.
pub fn run_if_selected(argument_count: usize) -> Option<Result<(), PackageMaintenanceError>> {
    if argument_count != 1 {
        return None;
    }
    let operation = match env::var_os(SELECTOR).as_deref() {
        Some(value) if value == OsStr::new("begin") => Operation::Begin,
        Some(value) if value == OsStr::new("complete") => Operation::Complete,
        _ => return None,
    };
    Some(run_selected(operation))
}

fn run_selected(operation: Operation) -> Result<(), PackageMaintenanceError> {
    let mut environment = env::vars_os();
    let exact = environment
        .next()
        .is_some_and(|(name, value)| name == SELECTOR && operation.matches(&value))
        && environment.next().is_none();
    if !exact {
        return Err(PackageMaintenanceError::Failed);
    }
    // SAFETY: package dispatch runs before any thread exists and removes its sole variable.
    unsafe { env::remove_var(SELECTOR) };
    if !root_identity_is_exact() {
        return Err(PackageMaintenanceError::RootRequired);
    }
    run_protocol(operation).map_err(|_| PackageMaintenanceError::Failed)
}

impl Operation {
    fn matches(self, value: &OsStr) -> bool {
        match self {
            Self::Begin => value == OsStr::new("begin"),
            Self::Complete => value == OsStr::new("complete"),
        }
    }
}

fn root_identity_is_exact() -> bool {
    getuid().as_raw() == 0 && geteuid().as_raw() == 0
}

fn run_protocol(operation: Operation) -> io::Result<()> {
    let service = match operation {
        Operation::Begin => require_service_group()?,
        Operation::Complete => require_service_identity()?,
    };
    if matches!(operation, Operation::Complete) {
        prepare_package_paths(service)?;
        verify_installed_package()?;
    }
    let mut lock = open_package_lock(service)?;
    acquire_write_lock(&lock)?;
    require_state_for(operation, service)?;
    write_state(
        &mut lock,
        service,
        match operation {
            Operation::Begin => INCOMPLETE_BYTES,
            Operation::Complete => READY_BYTES,
        },
    )
}

fn require_service_group() -> io::Result<Gid> {
    let group = Group::from_name("orna")?
        .filter(|group| group.gid.as_raw() != 0)
        .ok_or_else(invalid_data)?;
    if Group::from_gid(group.gid)? != Some(group.clone())
        || group.mem.iter().any(|member| member.as_bytes() != b"orna")
    {
        return Err(invalid_data());
    }
    Ok(group.gid)
}

fn require_service_identity() -> io::Result<Gid> {
    let group = require_service_group()?;
    let user = User::from_name("orna")?
        .filter(|user| !user.uid.is_root() && user.gid == group)
        .ok_or_else(invalid_data)?;
    if User::from_uid(user.uid)? != Some(user.clone())
        || user.shell != Path::new("/usr/sbin/nologin")
    {
        return Err(invalid_data());
    }
    Ok(group)
}

fn prepare_package_paths(service: Gid) -> io::Result<()> {
    require_directory(Path::new("/var/lib"), root_uid(), root_gid(), 0o755)?;
    create_or_verify_directory(Path::new(PACKAGE_ROOT), root_uid(), root_gid(), 0o755)?;
    create_or_verify_directory(
        Path::new(INSTANCE_PARENT),
        User::from_name("orna")?.ok_or_else(invalid_data)?.uid,
        service,
        0o700,
    )?;
    match fs::symlink_metadata(PACKAGE_LOCK) {
        Ok(_) => require_file(
            Path::new(PACKAGE_LOCK),
            root_uid(),
            service,
            0o640,
            Some(b"\n"),
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o640)
                .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
                .open(PACKAGE_LOCK)?;
            file.write_all(b"\n")?;
            file.set_permissions(fs::Permissions::from_mode(0o640))?;
            chown(PACKAGE_LOCK, Some(root_uid()), Some(service))?;
            file.sync_all()?;
            sync_directory(Path::new(PACKAGE_ROOT))
        }
        Err(error) => Err(error),
    }
}

fn verify_installed_package() -> io::Result<()> {
    require_file(
        Path::new(CONFIGURATION),
        root_uid(),
        root_gid(),
        0o644,
        Some(CONFIGURATION_BYTES),
    )?;
    require_file(
        Path::new(INSTALLED_EXECUTABLE),
        root_uid(),
        root_gid(),
        0o755,
        None,
    )?;
    let installed = fs::metadata(INSTALLED_EXECUTABLE)?;
    let running = fs::metadata("/proc/self/exe")?;
    if (installed.dev(), installed.ino()) != (running.dev(), running.ino()) {
        return Err(invalid_data());
    }
    Ok(())
}

fn create_or_verify_directory(path: &Path, uid: Uid, gid: Gid, mode: u32) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => require_directory(path, uid, gid, mode),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
            chown(path, Some(uid), Some(gid))?;
            sync_directory(path.parent().ok_or_else(invalid_data)?)?;
            require_directory(path, uid, gid, mode)
        }
        Err(error) => Err(error),
    }
}

fn open_package_lock(service: Gid) -> io::Result<fs::File> {
    require_file(
        Path::new(PACKAGE_LOCK),
        root_uid(),
        service,
        0o640,
        Some(b"\n"),
    )?;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(PACKAGE_LOCK)
}

fn acquire_write_lock(file: &fs::File) -> io::Result<()> {
    let lock = nix::libc::flock {
        l_type: nix::libc::F_WRLCK as i16,
        l_whence: nix::libc::SEEK_SET as i16,
        l_start: 0,
        l_len: 1,
        l_pid: 0,
    };
    // SAFETY: the descriptor and lock pointer are valid for this non-blocking fcntl call.
    if unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_SETLK, &lock) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn require_state_for(operation: Operation, service: Gid) -> io::Result<()> {
    match fs::symlink_metadata(PACKAGE_STATE) {
        Ok(_) => {
            let bytes = read_verified_file(Path::new(PACKAGE_STATE), root_uid(), service, 0o640)?;
            if bytes == READY_BYTES || bytes == INCOMPLETE_BYTES {
                Ok(())
            } else {
                Err(invalid_data())
            }
        }
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && matches!(operation, Operation::Complete) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn write_state(lock: &mut fs::File, service: Gid, bytes: &[u8]) -> io::Result<()> {
    let temporary = Path::new(PACKAGE_ROOT).join(".package-state.toml.orna-tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o640)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.set_permissions(fs::Permissions::from_mode(0o640))?;
    chown(&temporary, Some(root_uid()), Some(service))?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, PACKAGE_STATE)?;
    sync_directory(Path::new(PACKAGE_ROOT))?;
    require_file(
        Path::new(PACKAGE_STATE),
        root_uid(),
        service,
        0o640,
        Some(bytes),
    )?;
    lock.sync_all()
}

fn require_directory(path: &Path, uid: Uid, gid: Gid, mode: u32) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_dir()
        && metadata.uid() == uid.as_raw()
        && metadata.gid() == gid.as_raw()
        && metadata.mode() & 0o7777 == mode
    {
        Ok(())
    } else {
        Err(invalid_data())
    }
}

fn require_file(
    path: &Path,
    uid: Uid,
    gid: Gid,
    mode: u32,
    expected: Option<&[u8]>,
) -> io::Result<()> {
    let bytes = read_verified_file(path, uid, gid, mode)?;
    if expected.is_none_or(|expected| bytes == expected) {
        Ok(())
    } else {
        Err(invalid_data())
    }
}

fn read_verified_file(path: &Path, uid: Uid, gid: Gid, mode: u32) -> io::Result<Vec<u8>> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.nlink() != 1
        || metadata.uid() != uid.as_raw()
        || metadata.gid() != gid.as_raw()
        || metadata.mode() & 0o7777 != mode
    {
        return Err(invalid_data());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

fn root_uid() -> Uid {
    Uid::from_raw(0)
}

fn root_gid() -> Gid {
    Gid::from_raw(0)
}

fn invalid_data() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "package state is invalid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_are_exact() {
        assert_eq!(
            PackageMaintenanceError::RootRequired.to_string(),
            "orna: package maintenance requires root"
        );
        assert_eq!(
            PackageMaintenanceError::Failed.to_string(),
            "orna: package maintenance did not complete"
        );
    }

    #[test]
    fn operations_match_only_their_exact_selector_values() {
        assert!(Operation::Begin.matches(OsStr::new("begin")));
        assert!(Operation::Complete.matches(OsStr::new("complete")));
        assert!(!Operation::Begin.matches(OsStr::new("complete")));
        assert!(!Operation::Complete.matches(OsStr::new("Complete")));
    }
}
