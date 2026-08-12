//! The embedded PostgreSQL instance boundary.

use std::{
    collections::BTreeSet,
    fmt, fs,
    io::{self, Cursor, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

use orna_postgres_engine::{ENGINE_MANIFEST, SUPPORT_ARCHIVE, SUPPORT_MANIFEST};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const INSTANCE_NAME: &str = "default";
const STATE_ROOT: &str = "/var/lib/orna/instances/default";
const RUNTIME_ROOT: &str = "/run/orna/default";
const SUPPORT_DIRECTORY: &str = "embedded-postgresql";

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

    pub fn instance_name(&self) -> &'static str {
        INSTANCE_NAME
    }

    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }

    pub fn socket_directory(&self) -> &Path {
        &self.socket_directory
    }

    pub fn support_root(&self) -> &Path {
        &self.support_root
    }
}

/// A verified materialised copy of the embedded support-data bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterialisedSupport {
    root: PathBuf,
    member_count: usize,
}

impl MaterialisedSupport {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn member_count(&self) -> usize {
        self.member_count
    }
}

/// A failure while selecting or materialising the embedded server host.
#[derive(Debug)]
#[non_exhaustive]
pub enum EmbeddedHostError {
    InvalidSupportManifest,
    InvalidSupportPath,
    SupportMismatch(&'static str),
    Io(io::Error),
}

impl fmt::Display for EmbeddedHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSupportManifest => {
                formatter.write_str("embedded PostgreSQL support manifest is invalid")
            }
            Self::InvalidSupportPath => {
                formatter.write_str("embedded PostgreSQL support path is invalid")
            }
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
            Self::Io(source) => Some(source),
            _ => None,
        }
    }
}

impl From<io::Error> for EmbeddedHostError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
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
}
