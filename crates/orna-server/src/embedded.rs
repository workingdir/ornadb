//! The embedded PostgreSQL instance boundary.

use std::{
    collections::BTreeSet,
    ffi::{CString, OsString},
    fmt, fs,
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicI32, Ordering},
    time::Duration,
};

use nix::{
    errno::Errno,
    sys::{
        signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, kill, sigaction},
        wait::{WaitPidFlag, WaitStatus, waitpid},
    },
    unistd::{ForkResult, Group, Pid, User, fork, getegid, geteuid, getgroups},
};
use orna_postgres_engine::{
    AbsolutePath, ENGINE_MANIFEST, EmbeddedEngine, EngineError, LinkedArguments, SUPPORT_ARCHIVE,
    SUPPORT_MANIFEST,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio_postgres::{Config, NoTls};

use crate::{OpenStandardDatabaseError, open_standard_database};

const INSTANCE_NAME: &str = "default";
const STATE_ROOT: &str = "/var/lib/orna/instances/default";
const RUNTIME_ROOT: &str = "/run/orna/default";
const SUPPORT_DIRECTORY: &str = "embedded-postgresql";
const CONFIGURATION_PATH: &str = "/etc/orna/instances/default.toml";
const PACKAGE_LOCK_PATH: &str = "/var/lib/orna/package.lock";
const PACKAGE_STATE_PATH: &str = "/var/lib/orna/package-state.toml";
const INSTANCE_PARENT: &str = "/var/lib/orna/instances";
const INSTANCE_LOCK_NAME: &str = "lock";
const INSTANCE_MANIFEST_NAME: &str = "instance.toml";
const READY_NAME: &str = "ready";
const GENERATION_NAME: &str = "0000000000000001";
const CONFIGURATION_BYTES: &[u8] = b"format = 1\ninstance = \"default\"\n";
const PACKAGE_STATE_BYTES: &[u8] = b"format = 1\nstate = \"ready\"\n";
const BOOTSTRAP_HBA_BYTES: &[u8] =
    b"local postgres orna_kernel peer map=orna_default\nlocal all all reject\n";
const NORMAL_HBA_BYTES: &[u8] =
    b"local orna orna_kernel peer map=orna_default\nlocal all all reject\n";
const IDENT_BYTES: &[u8] = b"orna_default orna orna_kernel\n";
const POSTGRES_PORT: u16 = 5432;
const STARTUP_ATTEMPTS: usize = 600;
const STARTUP_INTERVAL: Duration = Duration::from_millis(50);
const FAST_STOP_ATTEMPTS: usize = 600;
const IMMEDIATE_STOP_ATTEMPTS: usize = 300;
static SHUTDOWN_SIGNAL: AtomicI32 = AtomicI32::new(0);

/// The immutable identity of the PostgreSQL engine embedded in this executable.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EmbeddedEngineIdentity(String);

impl EmbeddedEngineIdentity {
    /// Computes the identity from the exact embedded engine manifest bytes.
    pub fn current() -> Self {
        Self(hex_digest(ENGINE_MANIFEST))
    }

    /// Returns the lowercase SHA-256 identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The fixed paths for the first managed Orna instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddedHostPaths {
    state_root: PathBuf,
    runtime_root: PathBuf,
    socket_directory: PathBuf,
    support_root: PathBuf,
}

impl EmbeddedHostPaths {
    /// Selects the fixed production paths for the embedded engine in this executable.
    pub fn production() -> Self {
        let identity = EmbeddedEngineIdentity::current();
        let runtime_root = PathBuf::from(RUNTIME_ROOT);
        Self {
            state_root: PathBuf::from(STATE_ROOT),
            socket_directory: runtime_root.join("postgres"),
            support_root: runtime_root.join(SUPPORT_DIRECTORY).join(identity.as_str()),
            runtime_root,
        }
    }

    /// Returns the fixed managed instance name.
    pub fn instance_name(&self) -> &'static str {
        INSTANCE_NAME
    }

    /// Returns the durable state root for the default instance.
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    /// Returns the private runtime root for the default instance.
    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }

    /// Returns the private Unix-socket directory.
    pub fn socket_directory(&self) -> &Path {
        &self.socket_directory
    }

    /// Returns the data-only support root selected by the engine identity.
    pub fn support_root(&self) -> &Path {
        &self.support_root
    }
}

struct ServiceIdentity {
    uid: u32,
    gid: u32,
}

#[derive(Clone, Copy)]
enum LockKind {
    Package,
    Instance,
}

impl LockKind {
    fn error(self) -> EmbeddedHostError {
        match self {
            Self::Package => EmbeddedHostError::InvalidPackageState,
            Self::Instance => EmbeddedHostError::InvalidInstanceState,
        }
    }
}

struct PreparedInstance {
    paths: EmbeddedHostPaths,
    identity: EmbeddedEngineIdentity,
    data_directory: PathBuf,
    is_new: bool,
    _package_lock: fs::File,
    _instance_lock: fs::File,
    _support: MaterialisedSupport,
}

/// A verified live embedded host retained for one private client lifetime.
pub struct ReadyEmbeddedHost {
    config: Config,
    _package_lock: fs::File,
    _instance_lock: fs::File,
}

impl ReadyEmbeddedHost {
    /// Returns the fixed peer-authenticated database connection configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }
}

impl fmt::Debug for ReadyEmbeddedHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadyEmbeddedHost")
            .field("config", &"private Unix socket")
            .finish_non_exhaustive()
    }
}

fn prepare_instance() -> Result<PreparedInstance, EmbeddedHostError> {
    let service = require_service_identity()?;
    require_file_bytes(
        Path::new(CONFIGURATION_PATH),
        0,
        0,
        0o644,
        CONFIGURATION_BYTES,
    )?;
    let package_lock = open_verified_lock(
        Path::new(PACKAGE_LOCK_PATH),
        0,
        service.gid,
        0o640,
        nix::libc::F_RDLCK as i16,
        LockKind::Package,
    )?;
    require_file_bytes(
        Path::new(PACKAGE_STATE_PATH),
        0,
        service.gid,
        0o640,
        PACKAGE_STATE_BYTES,
    )
    .map_err(|_| EmbeddedHostError::InvalidPackageState)?;

    let paths = EmbeddedHostPaths::production();
    require_directory(Path::new(INSTANCE_PARENT), service.uid, service.gid, 0o700)?;
    require_directory(paths.runtime_root(), service.uid, service.gid, 0o700)?;
    let state_metadata = fs::symlink_metadata(paths.state_root());
    let is_new = match state_metadata {
        Ok(_) => {
            require_directory(paths.state_root(), service.uid, service.gid, 0o700)?;
            false
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_owned_directory(paths.state_root(), 0o700)?;
            sync_directory(Path::new(INSTANCE_PARENT))?;
            true
        }
        Err(error) => return Err(error.into()),
    };
    let lock_path = paths.state_root().join(INSTANCE_LOCK_NAME);
    if is_new {
        create_lock_file(&lock_path)?;
    }
    let instance_lock = open_verified_lock(
        &lock_path,
        service.uid,
        service.gid,
        0o600,
        nix::libc::F_WRLCK as i16,
        LockKind::Instance,
    )?;

    let generation = paths.state_root().join("generations").join(GENERATION_NAME);
    let data_directory = generation.join("data");
    let identity = EmbeddedEngineIdentity::current();
    if is_new {
        create_owned_directory(&paths.state_root().join("generations"), 0o700)?;
        create_owned_directory(&generation, 0o700)?;
        create_owned_directory(&data_directory, 0o700)?;
        sync_directory(&generation)?;
        sync_directory(
            generation
                .parent()
                .ok_or(EmbeddedHostError::InvalidInstanceState)?,
        )?;
        sync_directory(paths.state_root())?;
    } else {
        require_directory(&data_directory, service.uid, service.gid, 0o700)?;
        let manifest_path = paths.state_root().join(INSTANCE_MANIFEST_NAME);
        let bytes = read_regular_file(&manifest_path, service.uid, service.gid, 0o600)?;
        let active = instance_manifest_bytes(&identity, true);
        let pending = instance_manifest_bytes(&identity, false);
        if bytes != active && bytes != pending {
            return Err(EmbeddedHostError::InvalidInstanceState);
        }
    }

    let support_parent = paths.runtime_root().join(SUPPORT_DIRECTORY);
    match fs::symlink_metadata(&support_parent) {
        Ok(_) => require_directory(&support_parent, service.uid, service.gid, 0o700)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_owned_directory(&support_parent, 0o700)?;
            sync_directory(paths.runtime_root())?;
        }
        Err(error) => return Err(error.into()),
    }
    let support = materialise_support_data(paths.support_root())?;
    recreate_socket_directory(paths.socket_directory(), &service)?;
    remove_stale_ready(paths.runtime_root().join(READY_NAME), &service)?;

    Ok(PreparedInstance {
        paths,
        identity,
        data_directory,
        is_new,
        _package_lock: package_lock,
        _instance_lock: instance_lock,
        _support: support,
    })
}

/// Verifies and retains the package and instance facts for a private host client.
pub fn inspect_ready_embedded_host() -> Result<ReadyEmbeddedHost, EmbeddedHostError> {
    let service = require_service_identity()?;
    require_file_bytes(
        Path::new(CONFIGURATION_PATH),
        0,
        0,
        0o644,
        CONFIGURATION_BYTES,
    )?;
    let package_lock = open_verified_lock(
        Path::new(PACKAGE_LOCK_PATH),
        0,
        service.gid,
        0o640,
        nix::libc::F_RDLCK as i16,
        LockKind::Package,
    )?;
    require_file_bytes(
        Path::new(PACKAGE_STATE_PATH),
        0,
        service.gid,
        0o640,
        PACKAGE_STATE_BYTES,
    )
    .map_err(|_| EmbeddedHostError::InvalidPackageState)?;

    let paths = EmbeddedHostPaths::production();
    require_directory(paths.state_root(), service.uid, service.gid, 0o700)?;
    require_directory(paths.runtime_root(), service.uid, service.gid, 0o700)?;
    require_directory(paths.socket_directory(), service.uid, service.gid, 0o700)?;
    let instance_lock = open_verified_file(
        &paths.state_root().join(INSTANCE_LOCK_NAME),
        service.uid,
        service.gid,
        0o600,
        true,
        LockKind::Instance,
    )?;
    let ready = read_regular_file(
        &paths.runtime_root().join(READY_NAME),
        service.uid,
        service.gid,
        0o600,
    )?;
    let ready = parse_ready_record(&ready)?;
    if ready.engine != EmbeddedEngineIdentity::current().as_str()
        || ready.generation != GENERATION_NAME
        || ready.executable_sha256 != hex_digest(&fs::read("/proc/self/exe")?)
        || !process_exists(ready.server_pid)
        || !process_exists(ready.postmaster_pid)
    {
        return Err(EmbeddedHostError::InvalidInstanceState);
    }
    require_lock_holder(&instance_lock, ready.server_pid)?;
    let manifest = read_regular_file(
        &paths.state_root().join(INSTANCE_MANIFEST_NAME),
        service.uid,
        service.gid,
        0o600,
    )?;
    if manifest != instance_manifest_bytes(&EmbeddedEngineIdentity::current(), true)
        || hex_digest(&manifest) != ready.instance_manifest_sha256
    {
        return Err(EmbeddedHostError::InvalidInstanceState);
    }

    Ok(ReadyEmbeddedHost {
        config: private_database_config(paths.socket_directory(), "orna"),
        _package_lock: package_lock,
        _instance_lock: instance_lock,
    })
}

struct ReadyRecord<'a> {
    server_pid: i32,
    postmaster_pid: i32,
    generation: &'a str,
    engine: &'a str,
    executable_sha256: &'a str,
    instance_manifest_sha256: &'a str,
}

fn parse_ready_record(bytes: &[u8]) -> Result<ReadyRecord<'_>, EmbeddedHostError> {
    let text = std::str::from_utf8(bytes).map_err(|_| EmbeddedHostError::InvalidInstanceState)?;
    let mut lines = text.lines();
    if lines.next() != Some("format = 1") || lines.next() != Some("instance = \"default\"") {
        return Err(EmbeddedHostError::InvalidInstanceState);
    }
    let server_pid = parse_ready_number(lines.next(), "server_pid = ")?;
    let postmaster_pid = parse_ready_number(lines.next(), "postmaster_pid = ")?;
    let generation = parse_ready_string(lines.next(), "generation = \"")?;
    let engine = parse_ready_string(lines.next(), "engine = \"")?;
    let executable = parse_ready_string(lines.next(), "executable_sha256 = \"")?;
    let instance_manifest_sha256 =
        parse_ready_string(lines.next(), "instance_manifest_sha256 = \"")?;
    if lines.next().is_some()
        || !text.ends_with('\n')
        || !is_sha256(engine)
        || !is_sha256(executable)
        || !is_sha256(instance_manifest_sha256)
    {
        return Err(EmbeddedHostError::InvalidInstanceState);
    }
    Ok(ReadyRecord {
        server_pid,
        postmaster_pid,
        generation,
        engine,
        executable_sha256: executable,
        instance_manifest_sha256,
    })
}

fn parse_ready_number(line: Option<&str>, prefix: &str) -> Result<i32, EmbeddedHostError> {
    line.and_then(|line| line.strip_prefix(prefix))
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| *value > 1)
        .ok_or(EmbeddedHostError::InvalidInstanceState)
}

fn parse_ready_string<'a>(
    line: Option<&'a str>,
    prefix: &str,
) -> Result<&'a str, EmbeddedHostError> {
    line.and_then(|line| line.strip_prefix(prefix))
        .and_then(|value| value.strip_suffix('"'))
        .filter(|value| !value.is_empty())
        .ok_or(EmbeddedHostError::InvalidInstanceState)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn process_exists(pid: i32) -> bool {
    // SAFETY: signal zero performs only process existence and permission checks.
    unsafe { nix::libc::kill(pid, 0) == 0 }
}

fn require_lock_holder(file: &fs::File, server_pid: i32) -> Result<(), EmbeddedHostError> {
    let mut lock = nix::libc::flock {
        l_type: nix::libc::F_WRLCK as i16,
        l_whence: nix::libc::SEEK_SET as i16,
        l_start: 0,
        l_len: 1,
        l_pid: 0,
    };
    // SAFETY: the descriptor and lock pointer are valid for this fcntl call.
    if unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_GETLK, &mut lock) } != 0
        || lock.l_type != nix::libc::F_WRLCK as i16
        || lock.l_pid != server_pid
    {
        return Err(EmbeddedHostError::InvalidInstanceState);
    }
    Ok(())
}

fn require_service_identity() -> Result<ServiceIdentity, EmbeddedHostError> {
    let user = User::from_name("orna")
        .map_err(|_| EmbeddedHostError::InvalidServiceIdentity)?
        .ok_or(EmbeddedHostError::InvalidServiceIdentity)?;
    let group = Group::from_name("orna")
        .map_err(|_| EmbeddedHostError::InvalidServiceIdentity)?
        .ok_or(EmbeddedHostError::InvalidServiceIdentity)?;
    let groups = getgroups().map_err(|_| EmbeddedHostError::InvalidServiceIdentity)?;
    if user.uid.is_root()
        || user.gid != group.gid
        || user.shell != Path::new("/usr/sbin/nologin")
        || geteuid() != user.uid
        || getegid() != group.gid
        || groups.iter().any(|gid| *gid != group.gid)
        || group.mem.iter().any(|member| member != "orna")
    {
        return Err(EmbeddedHostError::InvalidServiceIdentity);
    }
    Ok(ServiceIdentity {
        uid: user.uid.as_raw(),
        gid: group.gid.as_raw(),
    })
}

fn create_owned_directory(path: &Path, mode: u32) -> Result<(), EmbeddedHostError> {
    fs::create_dir(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    sync_directory(path)?;
    Ok(())
}

fn create_lock_file(path: &Path) -> Result<(), EmbeddedHostError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(b"\n")?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.sync_all()?;
    sync_directory(
        path.parent()
            .ok_or(EmbeddedHostError::InvalidInstanceState)?,
    )?;
    Ok(())
}

fn open_verified_lock(
    path: &Path,
    uid: u32,
    gid: u32,
    mode: u32,
    lock_type: i16,
    kind: LockKind,
) -> Result<fs::File, EmbeddedHostError> {
    let file = open_verified_file(
        path,
        uid,
        gid,
        mode,
        lock_type == nix::libc::F_WRLCK as i16,
        kind,
    )?;
    let mut lock = nix::libc::flock {
        l_type: lock_type,
        l_whence: nix::libc::SEEK_SET as i16,
        l_start: 0,
        l_len: 1,
        l_pid: 0,
    };
    // SAFETY: the descriptor and lock pointer are valid for this fcntl call.
    if unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_SETLK, &mut lock) } != 0 {
        return Err(kind.error());
    }
    Ok(file)
}

fn open_verified_file(
    path: &Path,
    uid: u32,
    gid: u32,
    mode: u32,
    write: bool,
    kind: LockKind,
) -> Result<fs::File, EmbeddedHostError> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(write)
        .custom_flags(libc_o_nofollow() | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| kind.error())?;
    require_metadata(&file.metadata().map_err(|_| kind.error())?, uid, gid, mode)
        .map_err(|_| kind.error())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|_| kind.error())?;
    if bytes != b"\n" {
        return Err(kind.error());
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| kind.error())?;
    Ok(file)
}

fn require_file_bytes(
    path: &Path,
    uid: u32,
    gid: u32,
    mode: u32,
    expected: &[u8],
) -> Result<(), EmbeddedHostError> {
    let bytes = read_regular_file(path, uid, gid, mode)?;
    if bytes != expected {
        return Err(EmbeddedHostError::InvalidInstanceState);
    }
    Ok(())
}

fn read_regular_file(
    path: &Path,
    uid: u32,
    gid: u32,
    mode: u32,
) -> Result<Vec<u8>, EmbeddedHostError> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc_o_nofollow() | nix::libc::O_CLOEXEC)
        .open(path)?;
    require_metadata(&file.metadata()?, uid, gid, mode)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn require_metadata(
    metadata: &fs::Metadata,
    uid: u32,
    gid: u32,
    mode: u32,
) -> Result<(), EmbeddedHostError> {
    if !metadata.file_type().is_file()
        || metadata.nlink() != 1
        || metadata.uid() != uid
        || metadata.gid() != gid
        || metadata.mode() & 0o7777 != mode
    {
        return Err(EmbeddedHostError::InvalidInstanceState);
    }
    Ok(())
}

fn require_directory(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<(), EmbeddedHostError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != uid
        || metadata.gid() != gid
        || metadata.mode() & 0o7777 != mode
    {
        return Err(EmbeddedHostError::InvalidInstanceState);
    }
    Ok(())
}

fn recreate_socket_directory(
    path: &Path,
    service: &ServiceIdentity,
) -> Result<(), EmbeddedHostError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            require_directory(path, service.uid, service.gid, 0o700)?;
            fs::remove_dir(path).map_err(|_| EmbeddedHostError::InvalidInstanceState)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    create_owned_directory(path, 0o700)?;
    sync_directory(
        path.parent()
            .ok_or(EmbeddedHostError::InvalidInstanceState)?,
    )?;
    Ok(())
}

fn remove_stale_ready(path: PathBuf, service: &ServiceIdentity) -> Result<(), EmbeddedHostError> {
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            require_metadata(&metadata, service.uid, service.gid, 0o600)?;
            fs::remove_file(&path)?;
            sync_directory(
                path.parent()
                    .ok_or(EmbeddedHostError::InvalidInstanceState)?,
            )?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn instance_manifest_bytes(identity: &EmbeddedEngineIdentity, activation: bool) -> Vec<u8> {
    format!(
        "format = 1\ninstance = \"default\"\ngeneration = \"{GENERATION_NAME}\"\npostgresql_major = 18\nengine = \"{}\"\nactivation_committed = {activation}\n",
        identity.as_str()
    )
    .into_bytes()
}

/// Runs the fixed default embedded PostgreSQL instance in the foreground.
pub fn run_embedded_server() -> Result<(), EmbeddedHostError> {
    install_shutdown_handlers()?;
    let instance = prepare_instance()?;
    if instance.is_new {
        bootstrap_new_instance(&instance)?;
    } else {
        verify_normal_configuration(&instance)?;
    }

    let mut postmaster = start_embedded_postmaster(
        instance.paths.support_root(),
        &instance.data_directory,
        instance.paths.socket_directory(),
    )?;
    let runtime = current_thread_runtime()?;
    runtime.block_on(async {
        postmaster.wait_until_ready("orna").await?;
        let kernel = orna_kernel_postgres::PostgresKernel::new(private_database_config(
            instance.paths.socket_directory(),
            "orna",
        ));
        let _kernel = open_standard_database(kernel).await?;
        Ok::<(), EmbeddedHostError>(())
    })?;
    drop(runtime);

    let manifest = instance_manifest_bytes(&instance.identity, true);
    atomic_write(
        &instance.paths.state_root().join(INSTANCE_MANIFEST_NAME),
        &manifest,
        0o600,
    )?;
    let ready_path = instance.paths.runtime_root().join(READY_NAME);
    let ready = ready_record_bytes(&instance, postmaster.pid(), &manifest)?;
    atomic_write(&ready_path, &ready, 0o600)?;

    let supervision = supervise_until_shutdown(&mut postmaster);
    let removal = remove_ready_record(&ready_path);
    supervision?;
    removal?;
    current_thread_runtime()?.block_on(postmaster.stop())
}

fn bootstrap_new_instance(instance: &PreparedInstance) -> Result<(), EmbeddedHostError> {
    initialise_embedded_cluster(instance.paths.support_root(), &instance.data_directory)?;
    write_authentication(&instance.data_directory, BOOTSTRAP_HBA_BYTES)?;
    require_empty_auto_configuration(&instance.data_directory)?;

    let mut postmaster = start_embedded_postmaster(
        instance.paths.support_root(),
        &instance.data_directory,
        instance.paths.socket_directory(),
    )?;
    let runtime = current_thread_runtime()?;
    runtime.block_on(async {
        postmaster.wait_until_ready("postgres").await?;
        create_private_database(instance.paths.socket_directory()).await?;
        postmaster.stop().await
    })?;
    drop(runtime);

    write_authentication(&instance.data_directory, NORMAL_HBA_BYTES)?;
    atomic_write(
        &instance.paths.state_root().join(INSTANCE_MANIFEST_NAME),
        &instance_manifest_bytes(&instance.identity, false),
        0o600,
    )?;
    Ok(())
}

fn verify_normal_configuration(instance: &PreparedInstance) -> Result<(), EmbeddedHostError> {
    let owner = effective_identity();
    require_file_bytes(
        &instance.data_directory.join("pg_hba.conf"),
        owner.0,
        owner.1,
        0o600,
        NORMAL_HBA_BYTES,
    )?;
    require_file_bytes(
        &instance.data_directory.join("pg_ident.conf"),
        owner.0,
        owner.1,
        0o600,
        IDENT_BYTES,
    )?;
    require_empty_auto_configuration(&instance.data_directory)
}

fn write_authentication(data_directory: &Path, hba: &[u8]) -> Result<(), EmbeddedHostError> {
    atomic_write(&data_directory.join("pg_hba.conf"), hba, 0o600)?;
    atomic_write(&data_directory.join("pg_ident.conf"), IDENT_BYTES, 0o600)
}

fn require_empty_auto_configuration(data_directory: &Path) -> Result<(), EmbeddedHostError> {
    let owner = effective_identity();
    require_file_bytes(
        &data_directory.join("postgresql.auto.conf"),
        owner.0,
        owner.1,
        0o600,
        b"",
    )
}

async fn create_private_database(socket_directory: &Path) -> Result<(), EmbeddedHostError> {
    let (client, connection) = private_database_config(socket_directory, "postgres")
        .connect(NoTls)
        .await?;
    let driver = tokio::spawn(connection);
    let result = client
        .batch_execute("CREATE DATABASE orna TEMPLATE template0")
        .await;
    drop(client);
    let driver_result = driver
        .await
        .map_err(|_| EmbeddedHostError::InvalidInstanceState)?;
    result?;
    driver_result?;
    Ok(())
}

fn current_thread_runtime() -> Result<tokio::runtime::Runtime, EmbeddedHostError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(EmbeddedHostError::Runtime)
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), EmbeddedHostError> {
    let parent = path
        .parent()
        .ok_or(EmbeddedHostError::InvalidInstanceState)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(EmbeddedHostError::InvalidInstanceState)?;
    let temporary = path.with_file_name(format!(".{name}.orna-tmp"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    sync_directory(parent)
}

fn ready_record_bytes(
    instance: &PreparedInstance,
    postmaster_pid: u32,
    manifest: &[u8],
) -> Result<Vec<u8>, EmbeddedHostError> {
    let executable = fs::read("/proc/self/exe")?;
    Ok(format!(
        "format = 1\ninstance = \"default\"\nserver_pid = {}\npostmaster_pid = {postmaster_pid}\ngeneration = \"{GENERATION_NAME}\"\nengine = \"{}\"\nexecutable_sha256 = \"{}\"\ninstance_manifest_sha256 = \"{}\"\n",
        std::process::id(),
        instance.identity.as_str(),
        hex_digest(&executable),
        hex_digest(manifest),
    )
    .into_bytes())
}

fn install_shutdown_handlers() -> Result<(), EmbeddedHostError> {
    SHUTDOWN_SIGNAL.store(0, Ordering::SeqCst);
    let action = SigAction::new(
        SigHandler::Handler(record_shutdown_signal),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    // SAFETY: the handler only stores one integer in a lock-free atomic.
    unsafe {
        sigaction(Signal::SIGINT, &action).map_err(|_| EmbeddedHostError::Signal)?;
        sigaction(Signal::SIGTERM, &action).map_err(|_| EmbeddedHostError::Signal)?;
    }
    Ok(())
}

extern "C" fn record_shutdown_signal(signal: i32) {
    let _ = SHUTDOWN_SIGNAL.compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst);
}

fn supervise_until_shutdown(postmaster: &mut EmbeddedPostmaster) -> Result<(), EmbeddedHostError> {
    loop {
        if SHUTDOWN_SIGNAL.load(Ordering::SeqCst) != 0 {
            return Ok(());
        }
        postmaster.require_running()?;
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn remove_ready_record(path: &Path) -> Result<(), EmbeddedHostError> {
    fs::remove_file(path)?;
    sync_directory(
        path.parent()
            .ok_or(EmbeddedHostError::InvalidInstanceState)?,
    )
}

/// A verified materialised copy of the embedded support-data bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterialisedSupport {
    root: PathBuf,
    member_count: usize,
}

impl MaterialisedSupport {
    /// Returns the verified runtime support root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the number of verified support members.
    pub const fn member_count(&self) -> usize {
        self.member_count
    }
}

/// A failure while selecting or materialising the embedded server host.
#[derive(Debug)]
#[non_exhaustive]
pub enum EmbeddedHostError {
    /// A private PostgreSQL connection failed.
    Database(tokio_postgres::Error),
    /// The linked embedded-engine boundary rejected an input.
    Engine(EngineError),
    /// The process does not have the exact Orna service identity.
    InvalidServiceIdentity,
    /// Package state or its lock does not match the accepted shape.
    InvalidPackageState,
    /// Instance state, readiness, or its lock does not match the accepted shape.
    InvalidInstanceState,
    /// The embedded support manifest is malformed or internally inconsistent.
    InvalidSupportManifest,
    /// A support member path is not a safe relative path.
    InvalidSupportPath,
    /// A linked PostgreSQL entry was requested after another thread existed.
    MultipleThreads,
    /// The linked initialiser returned a non-zero status.
    InitialiserExited(i32),
    /// A signal stopped the linked initialiser.
    InitialiserSignalled(i32),
    /// The supervisor could not reap the linked initialiser.
    InitialiserWait,
    /// The linked postmaster returned a non-zero status.
    PostmasterExited(i32),
    /// A signal stopped the linked postmaster unexpectedly.
    PostmasterSignalled(i32),
    /// The supervisor could not control or reap the linked postmaster.
    PostmasterWait,
    /// The linked postmaster did not accept a complete private query in time.
    ReadinessTimeout,
    /// The private asynchronous runtime could not start or complete.
    Runtime(io::Error),
    /// Supervisor signal setup failed.
    Signal,
    /// Kernel bootstrap or accepted-standard recovery failed.
    Standard(OpenStandardDatabaseError),
    /// Materialised support data differs from its embedded manifest.
    SupportMismatch(&'static str),
    /// A host filesystem operation failed.
    Io(io::Error),
}

impl fmt::Display for EmbeddedHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(source) => source.fmt(formatter),
            Self::Engine(source) => source.fmt(formatter),
            Self::InvalidServiceIdentity => formatter.write_str("Orna service identity is invalid"),
            Self::InvalidPackageState => {
                formatter.write_str("orna: package maintenance is incomplete")
            }
            Self::InvalidInstanceState => {
                formatter.write_str("embedded PostgreSQL instance state is invalid")
            }
            Self::InvalidSupportManifest => {
                formatter.write_str("embedded PostgreSQL support manifest is invalid")
            }
            Self::InvalidSupportPath => {
                formatter.write_str("embedded PostgreSQL support path is invalid")
            }
            Self::MultipleThreads => formatter.write_str(
                "embedded PostgreSQL cannot fork while the Orna supervisor has another thread",
            ),
            Self::InitialiserExited(status) => {
                write!(
                    formatter,
                    "embedded PostgreSQL initialisation exited with status {status}"
                )
            }
            Self::InitialiserSignalled(signal) => write!(
                formatter,
                "embedded PostgreSQL initialisation was stopped by signal {signal}"
            ),
            Self::InitialiserWait => {
                formatter.write_str("embedded PostgreSQL initialisation could not be reaped")
            }
            Self::PostmasterExited(status) => {
                write!(formatter, "embedded PostgreSQL exited with status {status}")
            }
            Self::PostmasterSignalled(signal) => {
                write!(
                    formatter,
                    "embedded PostgreSQL was stopped by signal {signal}"
                )
            }
            Self::PostmasterWait => formatter.write_str("embedded PostgreSQL could not be reaped"),
            Self::ReadinessTimeout => {
                formatter.write_str("embedded PostgreSQL did not become ready")
            }
            Self::Runtime(source) => write!(formatter, "Orna server runtime failed: {source}"),
            Self::Signal => formatter.write_str("Orna server signal handling failed"),
            Self::Standard(source) => source.fmt(formatter),
            Self::SupportMismatch(reason) => {
                write!(
                    formatter,
                    "embedded PostgreSQL support data differs: {reason}"
                )
            }
            Self::Io(source) => write!(
                formatter,
                "embedded PostgreSQL support I/O failed: {source}"
            ),
        }
    }
}

impl std::error::Error for EmbeddedHostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(source) => Some(source),
            Self::Engine(source) => Some(source),
            Self::Io(source) => Some(source),
            Self::Runtime(source) => Some(source),
            Self::Standard(source) => Some(source),
            _ => None,
        }
    }
}

impl From<io::Error> for EmbeddedHostError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}

impl From<EngineError> for EmbeddedHostError {
    fn from(source: EngineError) -> Self {
        Self::Engine(source)
    }
}

impl From<tokio_postgres::Error> for EmbeddedHostError {
    fn from(source: tokio_postgres::Error) -> Self {
        Self::Database(source)
    }
}

impl From<OpenStandardDatabaseError> for EmbeddedHostError {
    fn from(source: OpenStandardDatabaseError) -> Self {
        Self::Standard(source)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupportManifest {
    format: u32,
    members: Vec<SupportMember>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupportMember {
    length: u64,
    mode: String,
    path: String,
    sha256: String,
    #[serde(rename = "type")]
    member_type: String,
}

/// Materialises the support bundle when `root` is absent, or verifies an existing tree.
///
/// The caller owns the parent directory and the instance lock. This function never removes an
/// existing tree. It accepts only the exact data inventory embedded in this executable.
pub fn materialise_support_data(root: &Path) -> Result<MaterialisedSupport, EmbeddedHostError> {
    let manifest = parse_support_manifest()?;
    match fs::symlink_metadata(root) {
        Ok(_) => verify_materialised_tree(root, &manifest.members)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            materialise_new_tree(root, &manifest.members)?;
            verify_materialised_tree(root, &manifest.members)?;
        }
        Err(error) => return Err(error.into()),
    }

    Ok(MaterialisedSupport {
        root: root.to_owned(),
        member_count: manifest.members.len(),
    })
}

/// Initialises one new, empty PostgreSQL data directory through the linked engine entry.
///
/// The caller must own the instance lock and must call this before it creates an asynchronous
/// runtime or another operating-system thread.
pub fn initialise_embedded_cluster(
    support_root: &Path,
    data_directory: &Path,
) -> Result<(), EmbeddedHostError> {
    require_single_thread()?;
    let support_root = AbsolutePath::new(support_root)?;
    let data_directory = AbsolutePath::new(data_directory)?;

    // SAFETY: the thread gate ran immediately before this call. The child uses only prepared
    // values, resets its signal state, and enters process-global PostgreSQL code. The parent does
    // not call PostgreSQL and waits for the exact child.
    match unsafe { fork() }.map_err(|_| EmbeddedHostError::InitialiserWait)? {
        ForkResult::Child => {
            if reset_child_signals().is_err() {
                process_exit(126);
            }
            // SAFETY: this is the fresh, single-threaded child selected above.
            let engine = match unsafe { EmbeddedEngine::configure_process(&support_root) } {
                Ok(engine) => engine,
                Err(_) => process_exit(124),
            };
            // SAFETY: the child owns PostgreSQL process-global state and exits after this call.
            let status = unsafe { engine.initialise_process(&data_directory) };
            process_exit(status);
        }
        ForkResult::Parent { child } => wait_for_initialiser(child),
    }
}

fn require_single_thread() -> Result<(), EmbeddedHostError> {
    let mut count = 0_usize;
    for task in fs::read_dir("/proc/self/task")? {
        task?;
        count += 1;
        if count > 1 {
            return Err(EmbeddedHostError::MultipleThreads);
        }
    }
    if count == 1 {
        Ok(())
    } else {
        Err(EmbeddedHostError::MultipleThreads)
    }
}

fn wait_for_initialiser(child: Pid) -> Result<(), EmbeddedHostError> {
    loop {
        match waitpid(child, None) {
            Ok(WaitStatus::Exited(waited, 0)) if waited == child => return Ok(()),
            Ok(WaitStatus::Exited(waited, status)) if waited == child => {
                return Err(EmbeddedHostError::InitialiserExited(status));
            }
            Ok(WaitStatus::Signaled(waited, signal, _)) if waited == child => {
                return Err(EmbeddedHostError::InitialiserSignalled(signal as i32));
            }
            Ok(
                WaitStatus::Continued(_)
                | WaitStatus::Stopped(_, _)
                | WaitStatus::PtraceEvent(_, _, _)
                | WaitStatus::PtraceSyscall(_),
            ) => continue,
            Ok(_) => return Err(EmbeddedHostError::InitialiserWait),
            Err(Errno::EINTR) => continue,
            Err(_) => return Err(EmbeddedHostError::InitialiserWait),
        }
    }
}

/// One direct linked PostgreSQL postmaster child owned by the Rust supervisor.
#[derive(Debug)]
pub struct EmbeddedPostmaster {
    child: Option<Pid>,
    socket_directory: PathBuf,
}

impl EmbeddedPostmaster {
    /// Returns the direct child process identifier while the postmaster is live.
    pub fn pid(&self) -> u32 {
        self.child
            .expect("a stopped postmaster has no public lifetime")
            .as_raw() as u32
    }

    /// Waits until the private PostgreSQL database accepts a complete query.
    pub async fn wait_until_ready(&mut self, database: &str) -> Result<(), EmbeddedHostError> {
        let config = private_database_config(&self.socket_directory, database);
        for _ in 0..STARTUP_ATTEMPTS {
            self.require_running()?;
            if let Ok((client, connection)) = config.connect(NoTls).await {
                let driver = tokio::spawn(connection);
                let result = client.simple_query("SELECT 1").await;
                drop(client);
                let driver_result = driver.await;
                if result.is_ok() && matches!(driver_result, Ok(Ok(()))) {
                    return Ok(());
                }
            }
            tokio::time::sleep(STARTUP_INTERVAL).await;
        }
        self.require_running()?;
        Err(EmbeddedHostError::ReadinessTimeout)
    }

    /// Requests fast shutdown, with a bounded immediate-shutdown escalation.
    pub async fn stop(mut self) -> Result<(), EmbeddedHostError> {
        let child = self.child.ok_or(EmbeddedHostError::PostmasterWait)?;
        kill(child, Signal::SIGINT).map_err(|_| EmbeddedHostError::PostmasterWait)?;
        if let Some(result) = wait_for_postmaster(child, FAST_STOP_ATTEMPTS).await? {
            self.child = None;
            return result;
        }
        kill(child, Signal::SIGQUIT).map_err(|_| EmbeddedHostError::PostmasterWait)?;
        if let Some(result) = wait_for_postmaster(child, IMMEDIATE_STOP_ATTEMPTS).await? {
            self.child = None;
            return result;
        }
        Err(EmbeddedHostError::PostmasterWait)
    }

    fn require_running(&mut self) -> Result<(), EmbeddedHostError> {
        let child = self.child.ok_or(EmbeddedHostError::PostmasterWait)?;
        match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => Ok(()),
            Ok(status) => {
                self.child = None;
                postmaster_status(status)
            }
            Err(Errno::EINTR) => self.require_running(),
            Err(_) => Err(EmbeddedHostError::PostmasterWait),
        }
    }
}

impl Drop for EmbeddedPostmaster {
    fn drop(&mut self) {
        let Some(child) = self.child.take() else {
            return;
        };
        let _ = kill(child, Signal::SIGQUIT);
        for _ in 0..300 {
            match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) | Err(Errno::EINTR) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(_) | Err(Errno::ECHILD) => return,
                Err(_) => return,
            }
        }
        let _ = kill(child, Signal::SIGKILL);
        loop {
            match waitpid(child, None) {
                Ok(_) | Err(Errno::ECHILD) => return,
                Err(Errno::EINTR) => continue,
                Err(_) => return,
            }
        }
    }
}

/// Starts one linked PostgreSQL postmaster on the private Unix socket.
pub fn start_embedded_postmaster(
    support_root: &Path,
    data_directory: &Path,
    socket_directory: &Path,
) -> Result<EmbeddedPostmaster, EmbeddedHostError> {
    require_single_thread()?;
    let support_root = AbsolutePath::new(support_root)?;
    let values = [
        OsString::from("/usr/bin/orna"),
        OsString::from("-D"),
        data_directory.as_os_str().to_owned(),
        OsString::from("-k"),
        socket_directory.as_os_str().to_owned(),
        OsString::from("-h"),
        OsString::new(),
        OsString::from("-p"),
        OsString::from("5432"),
        OsString::from("-c"),
        OsString::from("unix_socket_permissions=0700"),
        OsString::from("-c"),
        OsString::from("ssl=off"),
        OsString::from("-c"),
        OsString::from("allow_alter_system=off"),
        OsString::from("-c"),
        OsString::from("shared_preload_libraries="),
        OsString::from("-c"),
        OsString::from("session_preload_libraries="),
        OsString::from("-c"),
        OsString::from("local_preload_libraries="),
        OsString::from("-c"),
        OsString::from("archive_mode=off"),
        OsString::from("-c"),
        OsString::from("archive_command="),
        OsString::from("-c"),
        OsString::from("archive_library="),
    ];
    let mut arguments = LinkedArguments::new(values)?;
    let environment = FixedChildEnvironment::new();

    // SAFETY: the thread gate ran immediately before this call. All child arguments and
    // environment values are prepared and remain live in the selected branch.
    match unsafe { fork() }.map_err(|_| EmbeddedHostError::PostmasterWait)? {
        ForkResult::Child => {
            if reset_child_signals().is_err() || environment.install().is_err() {
                process_exit(126);
            }
            // SAFETY: this is the fresh, single-threaded child selected above.
            let engine = match unsafe { EmbeddedEngine::configure_process(&support_root) } {
                Ok(engine) => engine,
                Err(_) => process_exit(124),
            };
            // SAFETY: the child owns PostgreSQL process-global state and its writable arguments.
            let status = unsafe { engine.run_process(&mut arguments) };
            process_exit(status);
        }
        ForkResult::Parent { child } => Ok(EmbeddedPostmaster {
            child: Some(child),
            socket_directory: socket_directory.to_owned(),
        }),
    }
}

/// Returns the private peer-authenticated connection configuration for one embedded database.
pub fn private_database_config(socket_directory: &Path, database: &str) -> Config {
    let mut config = Config::new();
    config
        .host_path(socket_directory)
        .port(POSTGRES_PORT)
        .user("orna_kernel")
        .dbname(database);
    config
}

struct FixedChildEnvironment {
    values: [(CString, CString); 3],
}

impl FixedChildEnvironment {
    fn new() -> Self {
        Self {
            values: [
                (cstring("LANG"), cstring("C.UTF-8")),
                (cstring("LC_ALL"), cstring("C.UTF-8")),
                (cstring("TZ"), cstring("UTC0")),
            ],
        }
    }

    fn install(&self) -> Result<(), ()> {
        // SAFETY: every pointer is a live NUL-terminated string. The caller is the fresh child.
        unsafe {
            if nix::libc::clearenv() != 0 {
                return Err(());
            }
            for (name, value) in &self.values {
                if nix::libc::setenv(name.as_ptr(), value.as_ptr(), 1) != 0 {
                    return Err(());
                }
            }
        }
        Ok(())
    }
}

async fn wait_for_postmaster(
    child: Pid,
    attempts: usize,
) -> Result<Option<Result<(), EmbeddedHostError>>, EmbeddedHostError> {
    for _ in 0..attempts {
        match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => tokio::time::sleep(STARTUP_INTERVAL).await,
            Ok(status) => return Ok(Some(postmaster_status(status))),
            Err(Errno::EINTR) => continue,
            Err(_) => return Err(EmbeddedHostError::PostmasterWait),
        }
    }
    Ok(None)
}

fn postmaster_status(status: WaitStatus) -> Result<(), EmbeddedHostError> {
    match status {
        WaitStatus::Exited(_, 0) => Ok(()),
        WaitStatus::Exited(_, status) => Err(EmbeddedHostError::PostmasterExited(status)),
        WaitStatus::Signaled(_, signal, _) => {
            Err(EmbeddedHostError::PostmasterSignalled(signal as i32))
        }
        _ => Err(EmbeddedHostError::PostmasterWait),
    }
}

fn cstring(value: &str) -> CString {
    CString::new(value).expect("fixed PostgreSQL process input is a C string")
}

fn reset_child_signals() -> Result<(), ()> {
    // SAFETY: all pointers refer to live local values. The caller is the fresh child process.
    unsafe {
        let mut action: nix::libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = nix::libc::SIG_DFL;
        if nix::libc::sigemptyset(&mut action.sa_mask) != 0 {
            return Err(());
        }
        for signal in 1..=nix::libc::SIGRTMAX() {
            if signal != nix::libc::SIGKILL && signal != nix::libc::SIGSTOP {
                let _ = nix::libc::sigaction(signal, &action, std::ptr::null_mut());
            }
        }
        let mut mask: nix::libc::sigset_t = std::mem::zeroed();
        if nix::libc::sigemptyset(&mut mask) != 0
            || nix::libc::sigprocmask(nix::libc::SIG_SETMASK, &mask, std::ptr::null_mut()) != 0
        {
            return Err(());
        }
    }
    Ok(())
}

fn process_exit(status: i32) -> ! {
    // SAFETY: `_exit` terminates only this forked child and does not run inherited Rust destructors.
    unsafe { nix::libc::_exit(status) }
}

fn parse_support_manifest() -> Result<SupportManifest, EmbeddedHostError> {
    let manifest: SupportManifest = serde_json::from_slice(SUPPORT_MANIFEST)
        .map_err(|_| EmbeddedHostError::InvalidSupportManifest)?;
    if manifest.format != 1 || manifest.members.is_empty() {
        return Err(EmbeddedHostError::InvalidSupportManifest);
    }

    let mut paths = BTreeSet::new();
    let mut folded_paths = BTreeSet::new();
    for member in &manifest.members {
        validate_member(member)?;
        if !paths.insert(member.path.clone())
            || !folded_paths.insert(member.path.to_ascii_lowercase())
        {
            return Err(EmbeddedHostError::InvalidSupportManifest);
        }
    }
    Ok(manifest)
}

fn validate_member(member: &SupportMember) -> Result<(), EmbeddedHostError> {
    let path = Path::new(&member.path);
    if member.mode != "0600"
        || member.member_type != "file"
        || member.sha256.len() != 64
        || !member
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || member.path.contains(".orna-support-tmp")
        || member.path.bytes().any(|byte| {
            byte.is_ascii_control() || matches!(byte, b'\\' | b'*' | b'?' | b'[' | b']')
        })
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(EmbeddedHostError::InvalidSupportManifest);
    }
    Ok(())
}

fn materialise_new_tree(root: &Path, members: &[SupportMember]) -> Result<(), EmbeddedHostError> {
    let parent = root.parent().ok_or(EmbeddedHostError::InvalidSupportPath)?;
    require_private_directory(parent)?;
    fs::create_dir(root)?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;

    let mut archive = tar::Archive::new(Cursor::new(SUPPORT_ARCHIVE));
    let mut entries = archive.entries()?;
    for expected in members {
        let mut entry = entries
            .next()
            .ok_or(EmbeddedHostError::SupportMismatch("bundle is incomplete"))??;
        let header = entry.header();
        let path = entry
            .path()?
            .to_str()
            .ok_or(EmbeddedHostError::InvalidSupportManifest)?
            .to_owned();
        if path != expected.path
            || !header.entry_type().is_file()
            || header.mode()? != 0o600
            || header.uid()? != 0
            || header.gid()? != 0
            || header.size()? != expected.length
        {
            return Err(EmbeddedHostError::SupportMismatch(
                "bundle metadata is not accepted",
            ));
        }

        let destination = root.join(&expected.path);
        let directory = destination
            .parent()
            .ok_or(EmbeddedHostError::InvalidSupportPath)?;
        create_private_directories(root, directory)?;
        let temporary = destination.with_file_name(format!(
            "{}.orna-support-tmp",
            destination
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(EmbeddedHostError::InvalidSupportManifest)?
        ));
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let (length, digest) = stream_digest(&mut entry, Some(&mut output))?;
        output.set_permissions(fs::Permissions::from_mode(0o600))?;
        output.sync_all()?;
        drop(output);
        if length != expected.length || digest != expected.sha256 {
            return Err(EmbeddedHostError::SupportMismatch(
                "bundle member bytes are not accepted",
            ));
        }
        fs::rename(&temporary, &destination)?;
        sync_directory(directory)?;
    }
    if entries.next().transpose()?.is_some() {
        return Err(EmbeddedHostError::SupportMismatch(
            "bundle has an additional member",
        ));
    }
    sync_directory(root)?;
    sync_directory(parent)?;
    Ok(())
}

fn create_private_directories(root: &Path, directory: &Path) -> Result<(), EmbeddedHostError> {
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| EmbeddedHostError::InvalidSupportPath)?;
    let mut current = root.to_owned();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(EmbeddedHostError::InvalidSupportPath);
        };
        current.push(component);
        match fs::create_dir(&current) {
            Ok(()) => {
                fs::set_permissions(&current, fs::Permissions::from_mode(0o700))?;
                sync_directory(
                    current
                        .parent()
                        .ok_or(EmbeddedHostError::InvalidSupportPath)?,
                )?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                require_private_directory(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn verify_materialised_tree(
    root: &Path,
    members: &[SupportMember],
) -> Result<(), EmbeddedHostError> {
    require_private_directory(root)?;
    let expected = members
        .iter()
        .map(|member| member.path.clone())
        .collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    collect_tree_paths(root, root, &mut actual)?;
    if actual != expected {
        return Err(EmbeddedHostError::SupportMismatch(
            "materialised inventory is not accepted",
        ));
    }

    let owner = effective_identity();
    for member in members {
        let path = root.join(&member.path);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o7777 != 0o600
            || (metadata.uid(), metadata.gid()) != owner
            || metadata.len() != member.length
        {
            return Err(EmbeddedHostError::SupportMismatch(
                "materialised member metadata is not accepted",
            ));
        }
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc_o_nofollow())
            .open(path)?;
        let (_, digest) = stream_digest(&mut file, None)?;
        if digest != member.sha256 {
            return Err(EmbeddedHostError::SupportMismatch(
                "materialised member digest is not accepted",
            ));
        }
    }
    Ok(())
}

fn collect_tree_paths(
    root: &Path,
    directory: &Path,
    paths: &mut BTreeSet<String>,
) -> Result<(), EmbeddedHostError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_dir() {
            require_private_directory(&path)?;
            collect_tree_paths(root, &path, paths)?;
        } else if metadata.file_type().is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| EmbeddedHostError::InvalidSupportPath)?
                .to_str()
                .ok_or(EmbeddedHostError::InvalidSupportPath)?
                .to_owned();
            paths.insert(relative);
        } else {
            return Err(EmbeddedHostError::SupportMismatch(
                "materialised tree contains a link or special file",
            ));
        }
    }
    Ok(())
}

fn require_private_directory(path: &Path) -> Result<(), EmbeddedHostError> {
    let metadata = fs::symlink_metadata(path)?;
    let owner = effective_identity();
    if !metadata.file_type().is_dir()
        || metadata.mode() & 0o7777 != 0o700
        || (metadata.uid(), metadata.gid()) != owner
    {
        return Err(EmbeddedHostError::SupportMismatch(
            "support directory metadata is not accepted",
        ));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), EmbeddedHostError> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn stream_digest(
    input: &mut impl Read,
    mut output: Option<&mut fs::File>,
) -> Result<(u64, String), EmbeddedHostError> {
    let mut digest = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if let Some(output) = output.as_deref_mut() {
            output.write_all(&buffer[..count])?;
        }
        digest.update(&buffer[..count]);
        length = length
            .checked_add(count as u64)
            .ok_or(EmbeddedHostError::SupportMismatch("member is too large"))?;
    }
    Ok((length, digest_hex(digest.finalize())))
}

fn hex_digest(bytes: &[u8]) -> String {
    digest_hex(Sha256::digest(bytes))
}

fn digest_hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn effective_identity() -> (u32, u32) {
    // SAFETY: these libc identity getters have no preconditions.
    unsafe { (nix::libc::geteuid(), nix::libc::getegid()) }
}

fn libc_o_nofollow() -> i32 {
    nix::libc::O_NOFOLLOW
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .expect("server crate remains below crates");
            let path = repository.join("target").join(format!(
                "embedded-support-test-{}-{}",
                std::process::id(),
                NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create private test parent");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .expect("make test parent private");
            Self(path)
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn selects_fixed_production_paths() {
        let paths = EmbeddedHostPaths::production();
        assert_eq!(paths.instance_name(), "default");
        assert_eq!(paths.state_root(), Path::new(STATE_ROOT));
        assert_eq!(paths.runtime_root(), Path::new(RUNTIME_ROOT));
        assert_eq!(
            paths.socket_directory(),
            Path::new(RUNTIME_ROOT).join("postgres")
        );
        assert_eq!(
            paths.support_root(),
            Path::new(RUNTIME_ROOT)
                .join(SUPPORT_DIRECTORY)
                .join(EmbeddedEngineIdentity::current().as_str())
        );
    }

    #[test]
    fn materialises_and_reverifies_the_embedded_support_inventory() {
        let parent = TestRoot::new();
        let root = parent.0.join("support");
        let first = materialise_support_data(&root).expect("materialise support");
        let second = materialise_support_data(&root).expect("reverify support");
        assert_eq!(first, second);
        assert_eq!(first.member_count(), 620);
    }

    #[test]
    fn rejects_changed_materialised_support() {
        let parent = TestRoot::new();
        let root = parent.0.join("support");
        materialise_support_data(&root).expect("materialise support");
        fs::write(root.join("postgres.bki"), b"changed").expect("tamper test member");
        assert!(matches!(
            materialise_support_data(&root),
            Err(EmbeddedHostError::SupportMismatch(_))
        ));
    }

    #[test]
    fn parses_only_the_exact_ready_record_shape() {
        let digest = "a".repeat(64);
        let record = format!(
            "format = 1\ninstance = \"default\"\nserver_pid = 10\npostmaster_pid = 11\ngeneration = \"{GENERATION_NAME}\"\nengine = \"{digest}\"\nexecutable_sha256 = \"{digest}\"\ninstance_manifest_sha256 = \"{digest}\"\n"
        );
        let parsed = parse_ready_record(record.as_bytes()).expect("valid ready record");
        assert_eq!(parsed.server_pid, 10);
        assert_eq!(parsed.postmaster_pid, 11);
        assert_eq!(parsed.generation, GENERATION_NAME);
        assert!(parse_ready_record(format!("{record}extra = true\n").as_bytes()).is_err());
    }
}
