//! Complete, portable offline copies of one pinned format-3 snapshot.
//!
//! [`crate::offline_copy`] copies committed *rows* as verified payload files.
//! This module copies the *snapshot*: the whole native Git object closure of one
//! resolved commit, plus every recursively pinned dependency (gitlink) named by
//! that snapshot, with each dependency recorded as an explicit map entry. It is
//! the export and reconstruction side of ORNA-GIT-003 and ORNA-GIT-007: a
//! pointer-only clone, a mounted text tree, or a superproject mirror without its
//! dependencies never counts as an offline-complete copy.
//!
//! ## Archive format
//!
//! An archive is a plain directory:
//!
//! - `manifest.tsv`: one header line, one `snapshot` line, one `object` line per
//!   object in the exported closure (kind, object ID, size), and one
//!   `dependency` line per recorded gitlink (path, pinned commit, origin,
//!   bundle file).
//! - `superproject.bundle`: a self-contained Git bundle carrying the complete
//!   object closure reachable from the pinned commit.
//! - `dependencies/<commit>.bundle`: one self-contained bundle per pinned
//!   dependency commit.
//!
//! ## Reconstruction independence
//!
//! [`restore_complete_copy`] reconstructs an archive into a fresh directory
//! without configuring a remote and without retaining any object from the
//! source: the archive's bundles are resolved by ordinary Git transfer (which
//! verifies every object ID as it indexes it), never through shared storage.
//! The reconstructed object databases are then checked against the manifest:
//! every recorded object must be present at the recorded kind and size, and
//! every recorded blob's bytes are read back and re-hashed by this module's own
//! SHA-1/SHA-256 implementation. An absent object, a changed object ID, or
//! tampered bytes fails the reconstruction instead of reporting a complete copy.
//!
//! Recovery is also readable: a reconstructed copy is an ordinary Git
//! repository for its pinned commit and can be materialised into a CWD with
//! [`materialize_complete_copy`], which needs no source remote because both
//! the superproject and its dependencies live inside the copy.
//!
//! ORNA-GIT-012: a failure names the unavailable scope (`snapshot` or
//! `dependency <path>`) and never substitutes a host file or another snapshot.

use std::{
    collections::BTreeSet,
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::{
    CommittedTreeEntryKind, GitCommitRef, Repository, scrub_git_routing_environment,
};

const MANIFEST_FILE: &str = "manifest.tsv";
const SUPERPROJECT_BUNDLE: &str = "superproject.bundle";
const DEPENDENCIES_DIR: &str = "dependencies";
const HEADER: &str = "orna-complete-copy 1";
/// Ref name created inside a reconstructed copy. It is an ordinary branch of
/// that copy and unrelated to anything the source repository publishes.
const RESTORED_BRANCH: &str = "orna-complete-copy";

/// Largest number of objects one export records and verifies.
pub const MAX_EXPORT_OBJECTS: usize = 1_000_000;
/// Largest number of pinned dependencies one snapshot may record.
pub const MAX_EXPORT_DEPENDENCIES: usize = 4096;
/// Largest `.gitmodules` file read while resolving dependency origins.
pub const MAX_GITMODULES_BYTES: usize = 1 << 20;
/// Largest single payload read back during verification.
pub const MAX_VERIFY_PAYLOAD_BYTES: u64 = 1 << 33;

/// The kind of one native Git object in an exported closure.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CompleteCopyObjectKind {
    Blob,
    Tree,
    Commit,
    Tag,
}

impl CompleteCopyObjectKind {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "blob" => Self::Blob,
            "tree" => Self::Tree,
            "commit" => Self::Commit,
            "tag" => Self::Tag,
            _ => return None,
        })
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
            Self::Commit => "commit",
            Self::Tag => "tag",
        }
    }
}

/// One object recorded in an exported closure.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CompleteCopyObject {
    pub kind: CompleteCopyObjectKind,
    pub oid: String,
    pub size: u64,
}

/// One recursively pinned dependency recorded by a gitlink in the snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompleteCopyDependency {
    /// Worktree-relative submodule path recorded by the gitlink.
    pub path: String,
    /// Exact commit pinned by the gitlink.
    pub commit: String,
    /// Recorded `.gitmodules` origin for this path.
    pub origin: String,
    /// Archive-relative bundle file carrying this dependency's closure.
    pub bundle: String,
}

/// The verified contents of one complete-copy manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompleteCopyManifest {
    /// The pinned superproject commit this archive reproduces.
    pub snapshot: String,
    /// Every object of the superproject closure, ordered by object ID.
    pub objects: Vec<CompleteCopyObject>,
    /// Every pinned dependency, ordered by worktree path.
    pub dependencies: Vec<CompleteCopyDependency>,
}

impl CompleteCopyManifest {
    /// The recorded object and dependency counts.
    pub fn counts(&self) -> (usize, usize) {
        (self.objects.len(), self.dependencies.len())
    }

    /// Decodes one manifest document.
    fn decode(manifest: &str) -> Result<Self, CompleteCopyError> {
        let mut lines = manifest.lines();
        if lines.next() != Some(HEADER) {
            return Err(CompleteCopyError::InvalidArchive("unknown header"));
        }
        let mut snapshot = None;
        let mut objects = Vec::new();
        let mut dependencies = Vec::new();
        for line in lines {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["snapshot", commit] => {
                    if snapshot.is_some() || !valid_object_id(commit) {
                        return Err(CompleteCopyError::InvalidArchive("snapshot line"));
                    }
                    snapshot = Some((*commit).to_owned());
                }
                ["object", kind, oid, size] => {
                    let kind = CompleteCopyObjectKind::parse(kind)
                        .ok_or(CompleteCopyError::InvalidArchive("object kind"))?;
                    if !valid_object_id(oid) {
                        return Err(CompleteCopyError::InvalidArchive("object id"));
                    }
                    let size = size
                        .parse::<u64>()
                        .map_err(|_| CompleteCopyError::InvalidArchive("object size"))?;
                    if objects.len() == MAX_EXPORT_OBJECTS {
                        return Err(CompleteCopyError::ClosureTooLarge);
                    }
                    objects.push(CompleteCopyObject {
                        kind,
                        oid: (*oid).to_owned(),
                        size,
                    });
                }
                ["dependency", path, commit, origin, bundle] => {
                    if !valid_object_id(commit)
                        || path.is_empty()
                        || origin.is_empty()
                        || !valid_dependency_bundle(bundle)
                    {
                        return Err(CompleteCopyError::InvalidArchive("dependency line"));
                    }
                    if dependencies.len() == MAX_EXPORT_DEPENDENCIES {
                        return Err(CompleteCopyError::ClosureTooLarge);
                    }
                    dependencies.push(CompleteCopyDependency {
                        path: (*path).to_owned(),
                        commit: (*commit).to_owned(),
                        origin: (*origin).to_owned(),
                        bundle: (*bundle).to_owned(),
                    });
                }
                _ => return Err(CompleteCopyError::InvalidArchive("unrecognised line")),
            }
        }
        let snapshot = snapshot.ok_or(CompleteCopyError::InvalidArchive("missing snapshot"))?;
        Ok(Self {
            snapshot,
            objects,
            dependencies,
        })
    }
}

/// Why a complete copy could not be written, read, or reconstructed.
#[derive(Debug)]
pub enum CompleteCopyError {
    Io(std::io::Error),
    /// The archive or reconstruction directory already holds files.
    TargetNotEmpty,
    /// The selector did not resolve to a reachable commit.
    UnresolvedSnapshot,
    /// The pinned snapshot is not an admitted format-3 repository snapshot.
    NotAFormat3Snapshot,
    /// A gitlink names a dependency with no recorded origin.
    MissingDependencyOrigin { path: String },
    /// A pinned dependency repository could not be read for the export.
    DependencyUnavailable { path: String },
    /// The closure exceeds the recorded object or dependency bound.
    ClosureTooLarge,
    /// A recorded integrity check failed. `scope` names what is unavailable.
    IntegrityMismatch { scope: String },
    /// Git could not be run or refused an operation.
    GitUnavailable,
    /// A recorded archive field could not be parsed.
    InvalidArchive(&'static str),
}

impl fmt::Display for CompleteCopyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "complete copy I/O failed: {error}"),
            Self::TargetNotEmpty => write!(formatter, "target directory is not empty"),
            Self::UnresolvedSnapshot => {
                write!(formatter, "snapshot selector did not resolve to a commit")
            }
            Self::NotAFormat3Snapshot => {
                write!(formatter, "snapshot is not an admitted format-3 repository")
            }
            Self::MissingDependencyOrigin { path } => {
                write!(formatter, "dependency {path} has no recorded origin")
            }
            Self::DependencyUnavailable { path } => {
                write!(formatter, "dependency {path} is unavailable")
            }
            Self::ClosureTooLarge => write!(formatter, "snapshot closure exceeds the export bound"),
            Self::IntegrityMismatch { scope } => {
                write!(formatter, "integrity check failed for {scope}")
            }
            Self::GitUnavailable => write!(formatter, "Git is unavailable or refused the request"),
            Self::InvalidArchive(reason) => write!(formatter, "invalid complete copy: {reason}"),
        }
    }
}

impl std::error::Error for CompleteCopyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for CompleteCopyError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// One dependency repository the exporter may read from: the pinned gitlink
/// path and the local repository directory holding that dependency's objects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencySource {
    /// Worktree-relative submodule path, matching the gitlink.
    pub path: String,
    /// Local directory containing the dependency repository.
    pub directory: PathBuf,
}

/// Writes a complete copy of `selector` into `destination`.
///
/// `destination` must be absent or empty. `sources` names the local
/// directories for the snapshot's pinned dependencies; every gitlink in the
/// snapshot must have a matching source whose repository carries the pinned
/// commit, otherwise the export fails without writing a claimed-complete
/// archive (ORNA-GIT-007, ORNA-GIT-012).
///
/// The source repository's refs, index, and worktree are not modified.
pub fn export_complete_copy(
    repository: &Repository,
    selector: &str,
    destination: &Path,
    sources: &[DependencySource],
) -> Result<CompleteCopyManifest, CompleteCopyError> {
    require_empty(destination)?;
    let snapshot = repository
        .resolve_snapshot(selector)
        .map_err(|_| CompleteCopyError::UnresolvedSnapshot)?;
    // Proving the pinned snapshot admits format-3 metadata proves its schema and
    // committed store roots before any object is copied, so a text tree without
    // its row/graph closure cannot be exported as a complete copy.
    let source_root = repository.worktree().to_path_buf();
    let pin = repository
        .pin_snapshot(snapshot.as_str())
        .map_err(|_| CompleteCopyError::UnresolvedSnapshot)?;
    let format = repository
        .open_format_context_at(&pin)
        .map_err(|_| CompleteCopyError::NotAFormat3Snapshot)?;
    if format.validate_schema_root().is_err() || format.validate_store_root().is_err() {
        return Err(CompleteCopyError::NotAFormat3Snapshot);
    }

    let objects = closure_objects(&source_root, snapshot.as_str())?;
    let dependencies = resolve_dependencies(repository, &snapshot, sources)?;

    fs::create_dir_all(destination)?;
    write_bundle(&source_root, snapshot.as_str(), &destination.join(SUPERPROJECT_BUNDLE))?;
    if !dependencies.is_empty() {
        fs::create_dir_all(destination.join(DEPENDENCIES_DIR))?;
    }
    for dependency in &dependencies {
        let source = dependency_directory(repository, sources, &dependency.path);
        write_bundle(
            &source,
            &dependency.commit,
            &destination.join(&dependency.bundle),
        )?;
    }

    let mut document = format!("{HEADER}\n");
    document.push_str(&format!("snapshot\t{}\n", snapshot.as_str()));
    for object in &objects {
        document.push_str(&format!(
            "object\t{}\t{}\t{}\n",
            object.kind.as_str(),
            object.oid,
            object.size
        ));
    }
    for dependency in &dependencies {
        document.push_str(&format!(
            "dependency\t{}\t{}\t{}\t{}\n",
            dependency.path, dependency.commit, dependency.origin, dependency.bundle
        ));
    }
    // The manifest is written last, so an interrupted export leaves an archive
    // without a manifest: visibly incomplete rather than silently partial.
    let mut file = fs::File::create(destination.join(MANIFEST_FILE))?;
    file.write_all(document.as_bytes())?;
    file.sync_all()?;
    Ok(CompleteCopyManifest {
        snapshot: snapshot.as_str().to_owned(),
        objects,
        dependencies,
    })
}

/// Reads an archive manifest without reading any bundle.
pub fn read_complete_copy_manifest(
    archive: &Path,
) -> Result<CompleteCopyManifest, CompleteCopyError> {
    let document = fs::read_to_string(archive.join(MANIFEST_FILE))?;
    CompleteCopyManifest::decode(&document)
}

/// Reconstructs an archive into `destination` and verifies the result.
///
/// `destination` must be absent or empty. No remote is configured and no
/// source-repository path is consulted; only the archive's bundles are read.
/// Every recorded object is then re-verified against the reconstructed object
/// databases (see the module documentation).
pub fn restore_complete_copy(
    archive: &Path,
    destination: &Path,
) -> Result<CompleteCopyManifest, CompleteCopyError> {
    require_empty(destination)?;
    let manifest = read_complete_copy_manifest(archive)?;
    fs::create_dir_all(destination)?;

    fetch_bundle_into(
        &archive.join(SUPERPROJECT_BUNDLE),
        destination,
        &manifest.snapshot,
    )?;
    verify_closure(
        destination,
        &manifest.objects,
        &format!("snapshot {}", manifest.snapshot),
    )?;

    for dependency in &manifest.dependencies {
        let directory = destination.join(&dependency.path);
        fs::create_dir_all(&directory)?;
        fetch_bundle_into(
            &archive.join(&dependency.bundle),
            &directory,
            &dependency.commit,
        )?;
        let objects = closure_objects(&directory, &dependency.commit)?;
        verify_closure(
            &directory,
            &objects,
            &format!("dependency {}", dependency.path),
        )?;
    }
    Ok(manifest)
}

/// Materialises the reconstructed snapshot of `copy` into the copy's own
/// worktree, including each dependency at its gitlink path.
///
/// The pin and code are the only things a caller needs to mount or run: every
/// object comes from the copy itself, so no source remote is contacted.
pub fn materialize_complete_copy(
    copy: &Path,
    manifest: &CompleteCopyManifest,
) -> Result<(), CompleteCopyError> {
    // The superproject is materialised first: a checkout deletes and recreates
    // the directory at a gitlink path, so a dependency populated before this
    // step would be discarded. Dependencies are materialised afterwards, each
    // into its own path, which is why the manifest records them separately.
    checkout(copy, &manifest.snapshot)?;
    for dependency in &manifest.dependencies {
        checkout(&copy.join(&dependency.path), &dependency.commit)?;
    }
    Ok(())
}

fn checkout(repository: &Path, commit: &str) -> Result<(), CompleteCopyError> {
    git_output(
        repository,
        &["checkout", "--quiet", "--detach", commit],
    )?;
    Ok(())
}

/// Resolves the gitlinks of one snapshot into recorded dependencies.
fn resolve_dependencies(
    repository: &Repository,
    snapshot: &GitCommitRef,
    sources: &[DependencySource],
) -> Result<Vec<CompleteCopyDependency>, CompleteCopyError> {
    let entries = repository
        .list_committed_tree(snapshot, MAX_EXPORT_OBJECTS)
        .map_err(|_| CompleteCopyError::GitUnavailable)?;
    let mut gitlink_paths = Vec::new();
    for entry in &entries {
        if entry.kind() == CommittedTreeEntryKind::Submodule {
            let path = entry
                .path()
                .as_path()
                .to_str()
                .ok_or(CompleteCopyError::InvalidArchive(
                    "dependency path is not UTF-8",
                ))?
                .to_owned();
            gitlink_paths.push(path);
        }
    }
    if gitlink_paths.is_empty() {
        return Ok(Vec::new());
    }
    if gitlink_paths.len() > MAX_EXPORT_DEPENDENCIES {
        return Err(CompleteCopyError::ClosureTooLarge);
    }
    let origins = read_gitmodules(repository, snapshot)?;
    let mut dependencies = Vec::new();
    for path in gitlink_paths {
        let origin = origins
            .iter()
            .find(|(candidate, _)| *candidate == path)
            .map(|(_, origin)| origin.clone())
            .ok_or_else(|| CompleteCopyError::MissingDependencyOrigin { path: path.clone() })?;
        let source = dependency_directory(repository, sources, &path);
        if !is_repository(&source) {
            return Err(CompleteCopyError::DependencyUnavailable { path });
        }
        let commit = repository
            .committed_submodule_commit(snapshot, &path)
            .map_err(|_| CompleteCopyError::DependencyUnavailable { path: path.clone() })?;
        let commit = commit.as_str().to_owned();
        // The pinned commit must be present in the dependency's own object
        // database before this export may claim to carry it.
        if object_at(&source, &commit)?.is_none() {
            return Err(CompleteCopyError::DependencyUnavailable { path });
        }
        dependencies.push(CompleteCopyDependency {
            path,
            bundle: format!("{DEPENDENCIES_DIR}/{commit}.bundle"),
            commit,
            origin,
        });
    }
    dependencies.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(dependencies)
}

/// The local repository directory for one gitlink path.
///
/// The pinned commit is read through the worktree, but a dependency's objects
/// are read from a local repository: the exported source when the caller names
/// one, otherwise the dependency checked out at that very gitlink path, which
/// is where a repository with populated dependencies keeps it.
fn dependency_directory(
    repository: &Repository,
    sources: &[DependencySource],
    path: &str,
) -> PathBuf {
    sources
        .iter()
        .find(|source| source.path == path)
        .map(|source| source.directory.clone())
        .unwrap_or_else(|| repository.worktree().join(path))
}

fn is_repository(directory: &Path) -> bool {
    if directory.join(".git").exists() {
        return true;
    }
    let mut command = Command::new("git");
    command
        .current_dir(directory)
        .args(["rev-parse", "--git-dir"]);
    scrub_git_routing_environment(&mut command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Reads the committed `.gitmodules` file as `path`/`url` pairs. A snapshot
/// without `.gitmodules` records no origins.
fn read_gitmodules(
    repository: &Repository,
    snapshot: &GitCommitRef,
) -> Result<Vec<(String, String)>, CompleteCopyError> {
    let bytes = match repository.read_committed_file(snapshot, ".gitmodules", MAX_GITMODULES_BYTES) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(Vec::new()),
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| CompleteCopyError::InvalidArchive("`.gitmodules` is not UTF-8"))?;
    parse_gitmodules(text)
}

/// Parses `[submodule ...]` sections, one `path`/`url` pair each. A value is a
/// single token; a value containing a control character is refused rather than
/// truncated into the manifest.
fn parse_gitmodules(text: &str) -> Result<Vec<(String, String)>, CompleteCopyError> {
    let mut pairs = Vec::new();
    let mut in_section = false;
    let mut path: Option<String> = None;
    let mut url: Option<String> = None;
    let mut flush = |path: &mut Option<String>, url: &mut Option<String>, pairs: &mut Vec<_>| {
        if let (Some(path), Some(url)) = (path.take(), url.take()) {
            pairs.push((path, url));
        }
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if in_section {
                flush(&mut path, &mut url, &mut pairs);
            }
            in_section = line.starts_with("[submodule ");
            path = None;
            url = None;
            continue;
        }
        if !in_section || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(CompleteCopyError::InvalidArchive("`.gitmodules` value"));
        }
        match key.trim() {
            "path" => path = Some(value.to_owned()),
            "url" => url = Some(value.to_owned()),
            _ => {}
        }
    }
    if in_section {
        flush(&mut path, &mut url, &mut pairs);
    }
    Ok(pairs)
}

/// Lists every object reachable from `commit`, ordered by object ID.
fn closure_objects(
    directory: &Path,
    commit: &str,
) -> Result<Vec<CompleteCopyObject>, CompleteCopyError> {
    let listing = git_output(
        directory,
        &["rev-list", "--objects", "--no-object-names", commit],
    )?;
    let text = std::str::from_utf8(&listing).map_err(|_| CompleteCopyError::GitUnavailable)?;
    let mut unique = BTreeSet::new();
    for oid in text.split_ascii_whitespace() {
        if !valid_object_id(oid) {
            return Err(CompleteCopyError::GitUnavailable);
        }
        unique.insert(oid.to_owned());
    }
    if unique.len() > MAX_EXPORT_OBJECTS {
        return Err(CompleteCopyError::ClosureTooLarge);
    }
    let mut objects = Vec::with_capacity(unique.len());
    for oid in unique {
        let Some((kind, size)) = object_at(directory, &oid)? else {
            return Err(CompleteCopyError::IntegrityMismatch {
                scope: format!("object {oid}"),
            });
        };
        objects.push(CompleteCopyObject { kind, oid, size });
    }
    Ok(objects)
}

/// Reports one object's kind and size without reading its body.
fn object_at(
    directory: &Path,
    oid: &str,
) -> Result<Option<(CompleteCopyObjectKind, u64)>, CompleteCopyError> {
    Ok(objects_at(directory, &[oid])?.pop().flatten())
}

/// Looks up many object headers in one Git process.
fn objects_at(
    directory: &Path,
    oids: &[&str],
) -> Result<Vec<Option<(CompleteCopyObjectKind, u64)>>, CompleteCopyError> {
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let mut stdin = String::with_capacity(oids.len() * 41);
    for oid in oids {
        stdin.push_str(oid);
        stdin.push('\n');
    }
    let output = git_output_stdin(
        directory,
        &["cat-file", "--batch-check=%(objectname) %(objecttype) %(objectsize)"],
        Some(stdin.as_bytes()),
    )?;
    let text = std::str::from_utf8(&output).map_err(|_| CompleteCopyError::GitUnavailable)?;
    let mut results = Vec::with_capacity(oids.len());
    for (line, oid) in text.lines().zip(oids) {
        let mut fields = line.split_ascii_whitespace();
        let (Some(name), Some(kind), Some(size)) = (fields.next(), fields.next(), fields.next()) else {
            results.push(None);
            continue;
        };
        let parsed = CompleteCopyObjectKind::parse(kind)
            .zip(size.parse::<u64>().ok())
            .filter(|_| name.eq_ignore_ascii_case(oid));
        results.push(parsed);
    }
    results.resize(oids.len(), None);
    Ok(results)
}

/// Verifies the recorded closure against the repository at `directory`.
///
/// Every recorded object must be present at its recorded kind and size, every
/// blob must exist at its recorded length, and blob bytes must hash back to the
/// recorded object ID by this module's own digest, so a tampered payload fails.
fn verify_closure(
    directory: &Path,
    expected: &[CompleteCopyObject],
    scope: &str,
) -> Result<(), CompleteCopyError> {
    let oids: Vec<&str> = expected.iter().map(|object| object.oid.as_str()).collect();
    let headers = objects_at(directory, &oids)?;
    for (object, header) in expected.iter().zip(headers) {
        let Some((kind, size)) = header else {
            return Err(CompleteCopyError::IntegrityMismatch {
                scope: format!("{scope}: missing object {}", object.oid),
            });
        };
        if kind != object.kind || size != object.size {
            return Err(CompleteCopyError::IntegrityMismatch {
                scope: format!("{scope}: object {}", object.oid),
            });
        }
    }
    for object in expected
        .iter()
        .filter(|object| object.kind == CompleteCopyObjectKind::Blob)
    {
        verify_blob(directory, &object.oid, object.size).map_err(|error| match error {
            CompleteCopyError::IntegrityMismatch { .. } => CompleteCopyError::IntegrityMismatch {
                scope: format!("{scope}: blob {}", object.oid),
            },
            other => other,
        })?;
    }
    Ok(())
}

/// Streams one blob out of the object database and re-hashes it.
fn verify_blob(directory: &Path, oid: &str, size: u64) -> Result<(), CompleteCopyError> {
    if size > MAX_VERIFY_PAYLOAD_BYTES {
        return Err(CompleteCopyError::ClosureTooLarge);
    }
    let mut command = Command::new("git");
    command
        .current_dir(directory)
        .args(["cat-file", "blob", oid]);
    scrub_git_routing_environment(&mut command);
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = command.spawn().map_err(|_| CompleteCopyError::GitUnavailable)?;
    let mut hasher = object_hasher(&oid)?;
    hasher.update(format!("blob {size}\0").as_bytes());
    let mut stream = child.stdout.take().ok_or(CompleteCopyError::GitUnavailable)?;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut read = 0_u64;
    loop {
        let count = stream
            .read(&mut buffer)
            .map_err(|_| CompleteCopyError::GitUnavailable)?;
        if count == 0 {
            break;
        }
        read += count as u64;
        if read > size {
            return Err(CompleteCopyError::IntegrityMismatch {
                scope: format!("blob {oid}"),
            });
        }
        hasher.update(&buffer[..count]);
    }
    drop(stream);
    let status = child.wait().map_err(|_| CompleteCopyError::GitUnavailable)?;
    if !status.success() || read != size {
        return Err(CompleteCopyError::IntegrityMismatch {
            scope: format!("blob {oid}"),
        });
    }
    if encode_oid(&hasher.finalize()) != oid {
        return Err(CompleteCopyError::IntegrityMismatch {
            scope: format!("blob {oid}"),
        });
    }
    Ok(())
}

enum ObjectHasher {
    Sha1(Sha1),
    Sha256(Sha256),
}

impl ObjectHasher {
    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha1(hasher) => hasher.update(bytes),
            Self::Sha256(hasher) => hasher.update(bytes),
        }
    }

    fn finalize(self) -> Vec<u8> {
        match self {
            Self::Sha1(hasher) => hasher.finalize().to_vec(),
            Self::Sha256(hasher) => hasher.finalize().to_vec(),
        }
    }
}

fn object_hasher(oid: &str) -> Result<ObjectHasher, CompleteCopyError> {
    match oid.len() {
        40 => Ok(ObjectHasher::Sha1(Sha1::new())),
        64 => Ok(ObjectHasher::Sha256(Sha256::new())),
        _ => Err(CompleteCopyError::InvalidArchive("object id length")),
    }
}

fn encode_oid(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Writes one self-contained bundle of the closure reachable from `commit`.
///
/// The export is staged through a private temporary repository whose object
/// database is linked read-only to `source`'s objects. The source repository's
/// refs, index, and worktree are never written, so an export cannot move a
/// branch, disturb an edit, or create a pin (ORNA-GIT-008).
fn write_bundle(source: &Path, commit: &str, bundle: &Path) -> Result<(), CompleteCopyError> {
    // Fetching a temporary branch into the staging repository both creates the
    // ref Git needs for bundling and pulls any objects the source's own
    // database holds only through a promisor or an alternate.
    let staging = tempfile::TempDir::new()?;
    let stage = staging.path().join("bundle.git");
    git_output(
        source,
        &["init", "--quiet", "--bare", "--template=", path_text(&stage)?],
    )?;
    git_output(
        source,
        &[
            "--git-dir",
            path_text(&stage)?,
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            path_text(source)?,
            &format!("+{commit}:refs/heads/{RESTORED_BRANCH}"),
        ],
    )?;
    git_output(
        source,
        &[
            "--git-dir",
            path_text(&stage)?,
            "bundle",
            "create",
            path_text(bundle)?,
            &format!("refs/heads/{RESTORED_BRANCH}"),
        ],
    )?;
    Ok(())
}

/// Fetches `commit` out of one bundle into `destination` without configuring a
/// remote. The bundle carries its own ref name, so the pinned commit is named
/// explicitly and no branch of the source is assumed.
fn fetch_bundle_into(
    bundle: &Path,
    destination: &Path,
    commit: &str,
) -> Result<(), CompleteCopyError> {
    if !destination.join(".git").exists() {
        fs::create_dir_all(destination)?;
        git_output(
            destination,
            &["init", "--quiet", "--template=", path_text(destination)?],
        )?;
    }
    // `fetch.recurseSubmodules=false` keeps a gitlink in the transferred tree
    // from triggering a submodule fetch against the origin recorded in
    // `.gitmodules`: the dependencies travel in their own bundles.
    git_output(
        destination,
        &[
            "-c",
            "fetch.recurseSubmodules=false",
            "fetch",
            "--no-tags",
            path_text(bundle)?,
            &format!("+refs/heads/{RESTORED_BRANCH}:refs/heads/{RESTORED_BRANCH}"),
        ],
    )
    .map_err(|_| CompleteCopyError::IntegrityMismatch {
        scope: format!("bundle {} does not carry commit {commit}", bundle.display()),
    })?;
    let resolved = git_output(
        destination,
        &["rev-parse", &format!("refs/heads/{RESTORED_BRANCH}")],
    )?;
    let resolved = std::str::from_utf8(&resolved)
        .map_err(|_| CompleteCopyError::GitUnavailable)?
        .trim();
    if !resolved.eq_ignore_ascii_case(commit) {
        return Err(CompleteCopyError::IntegrityMismatch {
            scope: format!("bundle {} carries {resolved}", bundle.display()),
        });
    }
    Ok(())
}

fn require_empty(destination: &Path) -> Result<(), CompleteCopyError> {
    if destination.exists() && fs::read_dir(destination)?.next().is_some() {
        return Err(CompleteCopyError::TargetNotEmpty);
    }
    Ok(())
}

fn valid_object_id(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_dependency_bundle(bundle: &str) -> bool {
    let Some(name) = bundle.strip_prefix("dependencies/") else {
        return false;
    };
    let Some(commit) = name.strip_suffix(".bundle") else {
        return false;
    };
    // A dependency bundle path is always one file directly under the archive's
    // dependency directory, so a manifest can never redirect a read elsewhere.
    !name.contains('/') && valid_object_id(commit)
}

fn path_text(path: &Path) -> Result<&str, CompleteCopyError> {
    path.to_str()
        .ok_or(CompleteCopyError::InvalidArchive("path is not UTF-8"))
}

/// Runs Git in `directory`, failing closed when Git is unavailable or refuses.
fn git_output(directory: &Path, args: &[&str]) -> Result<Vec<u8>, CompleteCopyError> {
    git_output_stdin(directory, args, None)
}

fn git_output_stdin(
    directory: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<Vec<u8>, CompleteCopyError> {
    let mut command = Command::new("git");
    command.current_dir(directory).args(args);
    scrub_git_routing_environment(&mut command);
    command.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = command.spawn().map_err(|_| CompleteCopyError::GitUnavailable)?;
    if let Some(bytes) = stdin {
        let mut handle = child.stdin.take().ok_or(CompleteCopyError::GitUnavailable)?;
        handle
            .write_all(bytes)
            .map_err(|_| CompleteCopyError::GitUnavailable)?;
        drop(handle);
    }
    let output = child
        .wait_with_output()
        .map_err(|_| CompleteCopyError::GitUnavailable)?;
    if !output.status.success() {
        return Err(CompleteCopyError::GitUnavailable);
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::{
        CompleteCopyManifest, CompleteCopyObjectKind, parse_gitmodules, valid_dependency_bundle,
        valid_object_id,
    };

    #[test]
    fn gitmodules_pairs_are_read_one_section_at_a_time() {
        let text = "[submodule \"std\"]\n\tpath = stdlib/std\n\turl = https://example.invalid/std.git\n\
                    [submodule \"tools\"]\n\tpath = tools\n\turl = ../tools.git\n\tbranch = main\n";
        assert_eq!(
            parse_gitmodules(text).unwrap(),
            vec![
                (
                    "stdlib/std".to_owned(),
                    "https://example.invalid/std.git".to_owned()
                ),
                ("tools".to_owned(), "../tools.git".to_owned()),
            ]
        );
    }

    #[test]
    fn gitmodules_values_with_control_characters_are_refused() {
        assert!(parse_gitmodules("[submodule \"a\"]\n\tpath = a\n\turl = oops\ttab\n").is_err());
        assert!(parse_gitmodules("[submodule \"a\"]\n\tpath = a\n\turl = \n").is_err());
    }

    #[test]
    fn manifest_round_trips_snapshot_objects_and_dependencies() {
        let document = "orna-complete-copy 1\n\
             snapshot\t0123456789012345678901234567890123456789\n\
             object\tcommit\t0123456789012345678901234567890123456789\t120\n\
             object\tblob\t89abcdef0123456789abcdef0123456789abcdef\t7\n\
             dependency\tstdlib/std\tabcdef0123456789abcdef0123456789abcdef01\thttps://example.invalid/std.git\tdependencies/abcdef0123456789abcdef0123456789abcdef01.bundle\n";
        let decoded = CompleteCopyManifest::decode(document).unwrap();
        assert_eq!(decoded.snapshot, "0123456789012345678901234567890123456789");
        assert_eq!(decoded.objects[0].kind, CompleteCopyObjectKind::Commit);
        assert_eq!(decoded.objects[1].size, 7);
        assert_eq!(decoded.dependencies[0].path, "stdlib/std");
        assert_eq!(decoded.counts(), (2, 1));
    }

    #[test]
    fn manifest_rejects_malformed_documents() {
        assert!(CompleteCopyManifest::decode("orna-complete-copy 1\nobject\tblob\tzz\t1\n").is_err());
        assert!(
            CompleteCopyManifest::decode(
                "orna-complete-copy 1\nsnapshot\t0123456789012345678901234567890123456789\nrow\tx\n"
            )
            .is_err()
        );
        assert!(
            CompleteCopyManifest::decode(
                "orna-complete-copy 1\nsnapshot\t0123456789012345678901234567890123456789\n"
            )
            .is_err()
            == false
        );
        assert!(!valid_object_id("0123456789"));
        assert!(valid_object_id("0123456789012345678901234567890123456789"));
    }

    #[test]
    fn dependency_bundles_stay_inside_the_archive() {
        assert!(valid_dependency_bundle(
            "dependencies/0123456789012345678901234567890123456789.bundle"
        ));
        assert!(!valid_dependency_bundle("dependencies/../../escape.bundle"));
        assert!(!valid_dependency_bundle(
            "dependencies/nested/0123456789012345678901234567890123456789.bundle"
        ));
        assert!(!valid_dependency_bundle("/etc/passwd"));
    }
}
