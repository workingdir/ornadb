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
//! - `manifest.tsv`: one header line, one `snapshot` line naming the primary
//!   member, one `archive` line per carried snapshot plus that member's `object`
//!   lines (kind, object ID, size), and one `dependency` line per recorded
//!   gitlink (path, pinned commit, origin, bundle file, owning member).
//! - `snapshots/<commit>.bundle`: one self-contained Git bundle per carried
//!   snapshot, holding that commit's complete reachable object closure.
//! - `dependencies/<commit>.bundle`: one self-contained bundle per pinned
//!   dependency commit.
//!
//! A header-1 archive — one snapshot, `superproject.bundle`, no `archive` lines,
//! no owning member on a dependency line — is still read and reconstructed
//! exactly, so an archive written before this format keeps working.
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

// Two incompatible `digest` major versions are in the graph: `sha2` 0.11 uses
// `digest` 0.11 while `sha1` 0.10 uses `digest` 0.10. Name each trait through
// its own crate so both hashers implement the trait this module calls.
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::{Digest as Sha2Digest, Sha256};

use crate::{scrub_git_routing_environment, CommittedTreeEntryKind, GitCommitRef, Repository};

const MANIFEST_FILE: &str = "manifest.tsv";
const SUPERPROJECT_BUNDLE: &str = "superproject.bundle";
const DEPENDENCIES_DIR: &str = "dependencies";
const SNAPSHOTS_DIR: &str = "snapshots";
/// Prefix of the ref names a reconstructed copy uses for non-primary members.
const MEMBER_REF_PREFIX: &str = "orna-complete-copy-member-";
const HEADER: &str = "orna-complete-copy 2";
/// Header of an archive written before members existed: one snapshot, one
/// closure, one bundle. It stays readable and is never written again.
const LEGACY_HEADER: &str = "orna-complete-copy 1";
/// Ref name created inside a reconstructed copy. It is an ordinary branch of
/// that copy and unrelated to anything the source repository publishes.
const RESTORED_BRANCH: &str = "orna-complete-copy";

/// Largest number of objects one export records and verifies.
pub const MAX_EXPORT_OBJECTS: usize = 1_000_000;
/// Largest number of snapshots one archive may carry.
pub const MAX_EXPORT_SNAPSHOTS: usize = 64;
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

/// One snapshot carried by a complete copy.
///
/// `CompleteCopyManifest::snapshot` names the primary (first exported) member;
/// every member, the primary included, also appears in `members` with the
/// bundle that carries its closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompleteCopyMember {
    /// The pinned commit this member reproduces.
    pub commit: String,
    /// Archive-relative bundle file carrying this member's closure.
    pub bundle: String,
    /// Every object of this member's closure, ordered by object ID.
    pub objects: Vec<CompleteCopyObject>,
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
    /// Commit of the archive member whose gitlink pins this dependency.
    pub member: String,
}

/// The verified contents of one complete-copy manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompleteCopyManifest {
    /// The pinned superproject commit this archive reproduces.
    pub snapshot: String,
    /// Every snapshot this archive carries, primary member first.
    pub members: Vec<CompleteCopyMember>,
    /// Every object of the primary member's closure, ordered by object ID.
    ///
    /// A single-snapshot export records each object exactly once. An export
    /// carrying several snapshots records their *union* here, so an object
    /// reachable from two members appears once and each member's own `objects`
    /// stays the exact closure of that member.
    pub objects: Vec<CompleteCopyObject>,
    /// Every pinned dependency, ordered by worktree path.
    pub dependencies: Vec<CompleteCopyDependency>,
}

impl CompleteCopyManifest {
    /// The recorded object and dependency counts.
    pub fn counts(&self) -> (usize, usize) {
        (self.objects.len(), self.dependencies.len())
    }

    /// The recorded member commits.
    pub fn commits(&self) -> impl Iterator<Item = &str> {
        self.members.iter().map(|member| member.commit.as_str())
    }

    /// The recorded closure of one member.
    pub fn member(&self, commit: &str) -> Option<&CompleteCopyMember> {
        self.members
            .iter()
            .find(|member| member.commit.eq_ignore_ascii_case(commit))
    }

    /// Decodes one manifest document.
    fn decode(manifest: &str) -> Result<Self, CompleteCopyError> {
        let mut lines = manifest.lines();
        let header = lines.next();
        if header != Some(HEADER) && header != Some(LEGACY_HEADER) {
            return Err(CompleteCopyError::InvalidArchive("unknown header"));
        }
        let legacy = header == Some(LEGACY_HEADER);
        let mut snapshot = None;
        let mut members: Vec<CompleteCopyMember> = Vec::new();
        let mut objects = Vec::new();
        let mut dependencies = Vec::new();
        // Objects and archive members are recorded grouped by member, so the
        // decoder appends objects to the member named by the last `archive`
        // line and keeps one union list for the whole archive.
        let mut current = 0_usize;
        for line in lines {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["snapshot", commit] => {
                    if snapshot.is_some() || !valid_object_id(commit) {
                        return Err(CompleteCopyError::InvalidArchive("snapshot line"));
                    }
                    snapshot = Some((*commit).to_owned());
                }
                ["archive", commit, bundle] => {
                    if !valid_object_id(commit) || !valid_snapshot_bundle(bundle) {
                        return Err(CompleteCopyError::InvalidArchive("archive line"));
                    }
                    if members.len() == MAX_EXPORT_SNAPSHOTS {
                        return Err(CompleteCopyError::ClosureTooLarge);
                    }
                    if members
                        .iter()
                        .any(|member| member.commit.eq_ignore_ascii_case(commit))
                    {
                        return Err(CompleteCopyError::InvalidArchive("repeated archive line"));
                    }
                    members.push(CompleteCopyMember {
                        commit: (*commit).to_owned(),
                        bundle: (*bundle).to_owned(),
                        objects: Vec::new(),
                    });
                    current = members.len() - 1;
                }
                ["object", kind, oid, size] => {
                    let object = decode_object(kind, oid, size)?;
                    if objects.len() == MAX_EXPORT_OBJECTS {
                        return Err(CompleteCopyError::ClosureTooLarge);
                    }
                    if let Some(member) = members.get_mut(current) {
                        member.objects.push(object.clone());
                    }
                    objects.push(object);
                }
                ["dependency", path, commit, origin, bundle] => {
                    if !valid_object_id(commit)
                        || path.is_empty()
                        || origin.is_empty()
                        || !valid_dependency_bundle(bundle)
                    {
                        return Err(CompleteCopyError::InvalidArchive("dependency line"));
                    }
                    // A header-1 archive had one member, so its dependencies
                    // belong to that member.
                    let member = if legacy {
                        snapshot.clone().ok_or(CompleteCopyError::InvalidArchive(
                            "dependency before snapshot line",
                        ))?
                    } else {
                        members
                            .get(current)
                            .map(|member| member.commit.clone())
                            .ok_or(CompleteCopyError::InvalidArchive(
                                "dependency before archive line",
                            ))?
                    };
                    push_dependency(
                        &mut dependencies,
                        path,
                        commit,
                        origin,
                        bundle,
                        member,
                    )?;
                }
                // A current archive names the member that pins each dependency,
                // because two members may pin the same path differently.
                ["dependency", path, commit, origin, bundle, member] => {
                    if !valid_object_id(commit)
                        || path.is_empty()
                        || origin.is_empty()
                        || !valid_dependency_bundle(bundle)
                        || !valid_object_id(member)
                    {
                        return Err(CompleteCopyError::InvalidArchive("dependency line"));
                    }
                    if !members
                        .iter()
                        .any(|candidate| candidate.commit.eq_ignore_ascii_case(member))
                    {
                        return Err(CompleteCopyError::InvalidArchive(
                            "dependency names no member",
                        ));
                    }
                    push_dependency(
                        &mut dependencies,
                        path,
                        commit,
                        origin,
                        bundle,
                        (*member).to_owned(),
                    )?;
                }
                _ => return Err(CompleteCopyError::InvalidArchive("unrecognised line")),
            }
        }
        let snapshot = snapshot.ok_or(CompleteCopyError::InvalidArchive("missing snapshot"))?;
        // The archive's own object list is the union of its members, so an
        // object reachable from two members is recorded once. Grouping by
        // member appends it once per member, so fold the union back to a set
        // here: a read manifest then equals the manifest that was written.
        let objects: Vec<CompleteCopyObject> = objects
            .into_iter()
            .collect::<BTreeSet<CompleteCopyObject>>()
            .into_iter()
            .collect();
        if members.is_empty() {
            if !legacy {
                return Err(CompleteCopyError::InvalidArchive("missing archive members"));
            }
            // A header-1 archive named exactly one closure with one bundle and
            // recorded no per-member line. It decodes as that single member, so
            // an archive written before members existed stays readable.
            members.push(CompleteCopyMember {
                commit: snapshot.clone(),
                bundle: SUPERPROJECT_BUNDLE.to_owned(),
                objects: objects.clone(),
            });
        } else if members[0].commit != snapshot {
            return Err(CompleteCopyError::InvalidArchive(
                "primary snapshot is not the first member",
            ));
        }
        Ok(Self {
            snapshot,
            members,
            objects,
            dependencies,
        })
    }
}

/// Records one decoded dependency line, bounded by the archive limits.
fn push_dependency(
    dependencies: &mut Vec<CompleteCopyDependency>,
    path: &str,
    commit: &str,
    origin: &str,
    bundle: &str,
    member: String,
) -> Result<(), CompleteCopyError> {
    if dependencies.len() == MAX_EXPORT_DEPENDENCIES {
        return Err(CompleteCopyError::ClosureTooLarge);
    }
    dependencies.push(CompleteCopyDependency {
        path: path.to_owned(),
        commit: commit.to_owned(),
        origin: origin.to_owned(),
        bundle: bundle.to_owned(),
        member,
    });
    Ok(())
}

fn decode_object(kind: &str, oid: &str, size: &str) -> Result<CompleteCopyObject, CompleteCopyError> {
    let kind = CompleteCopyObjectKind::parse(kind)
        .ok_or(CompleteCopyError::InvalidArchive("object kind"))?;
    if !valid_object_id(oid) {
        return Err(CompleteCopyError::InvalidArchive("object id"));
    }
    let size = size
        .parse::<u64>()
        .map_err(|_| CompleteCopyError::InvalidArchive("object size"))?;
    Ok(CompleteCopyObject {
        kind,
        oid: oid.to_owned(),
        size,
    })
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
    MissingDependencyOrigin {
        path: String,
    },
    /// A pinned dependency repository could not be read for the export.
    DependencyUnavailable {
        path: String,
    },
    /// The closure exceeds the recorded object or dependency bound.
    ClosureTooLarge,
    /// A recorded integrity check failed. `scope` names what is unavailable.
    IntegrityMismatch {
        scope: String,
    },
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
///
/// This is the single-snapshot spelling of [`export_complete_copy_of`].
pub fn export_complete_copy(
    repository: &Repository,
    selector: &str,
    destination: &Path,
    sources: &[DependencySource],
) -> Result<CompleteCopyManifest, CompleteCopyError> {
    export_complete_copy_of(repository, std::slice::from_ref(&selector), destination, sources)
}

/// Writes a complete copy carrying every snapshot in `selectors`.
///
/// The first selector is the primary member: [`CompleteCopyManifest::snapshot`]
/// names it, and it is the member a reconstruction materialises into the
/// copy's own worktree. The remaining selectors are carried alongside it, each
/// with its own bundle and its own recorded closure, so a later commit and the
/// commit it replaced can both be read offline from one archive (Gate F: both
/// snapshots reconstruct with the original remotes disabled).
///
/// Every member must be an admitted format-3 snapshot of the same repository.
/// A gitlink whose pinned commit is not reachable from the primary member is
/// resolved from the member that does pin it, so a snapshot that predates a
/// dependency bump can still be exported with its own dependency commit.
pub fn export_complete_copy_of(
    repository: &Repository,
    selectors: &[&str],
    destination: &Path,
    sources: &[DependencySource],
) -> Result<CompleteCopyManifest, CompleteCopyError> {
    if selectors.is_empty() || selectors.len() > MAX_EXPORT_SNAPSHOTS {
        return Err(CompleteCopyError::ClosureTooLarge);
    }
    require_empty(destination)?;
    let source_root = repository.worktree().to_path_buf();

    let mut exported: Vec<ExportedMember> = Vec::with_capacity(selectors.len());
    for selector in selectors {
        let commit = repository
            .resolve_snapshot(selector)
            .map_err(|_| CompleteCopyError::UnresolvedSnapshot)?;
        // Proving the pinned snapshot admits format-3 metadata proves its schema
        // and committed store roots before any object is copied, so a text tree
        // without its row/graph closure cannot be exported as a complete copy.
        let pin = repository
            .pin_snapshot(commit.as_str())
            .map_err(|_| CompleteCopyError::UnresolvedSnapshot)?;
        let format = repository
            .open_format_context_at(&pin)
            .map_err(|_| CompleteCopyError::NotAFormat3Snapshot)?;
        if format.validate_schema_root().is_err() || format.validate_store_root().is_err() {
            return Err(CompleteCopyError::NotAFormat3Snapshot);
        }
        let name = commit.as_str().to_owned();
        if exported.iter().any(|member| member.name == name) {
            return Err(CompleteCopyError::InvalidArchive("repeated snapshot"));
        }
        exported.push(ExportedMember {
            objects: closure_objects(&source_root, &name)?,
            bundle: snapshot_bundle(&name),
            snapshot: commit,
            name,
        });
    }

    let dependencies = resolve_dependencies_of(repository, &exported, sources)?;

    fs::create_dir_all(destination)?;
    // Every member bundle lives under `snapshots/`, so the directory must exist
    // before the first `git bundle create` writes into it.
    fs::create_dir_all(destination.join(SNAPSHOTS_DIR))?;
    for member in &exported {
        write_bundle(
            &source_root,
            &member.name,
            &destination.join(&member.bundle),
        )?;
    }
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

    let members = exported
        .into_iter()
        .map(ExportedMember::into_member)
        .collect::<Vec<_>>();
    let mut document = format!("{HEADER}\n");
    document.push_str(&format!("snapshot\t{}\n", members[0].commit));
    for member in &members {
        document.push_str(&format!("archive\t{}\t{}\n", member.commit, member.bundle));
        for object in &member.objects {
            document.push_str(&format!(
                "object\t{}\t{}\t{}\n",
                object.kind.as_str(),
                object.oid,
                object.size
            ));
        }
    }
    for dependency in &dependencies {
        document.push_str(&format!(
            "dependency\t{}\t{}\t{}\t{}\t{}\n",
            dependency.path,
            dependency.commit,
            dependency.origin,
            dependency.bundle,
            dependency.member
        ));
    }
    // The manifest is written last, so an interrupted export leaves an archive
    // without a manifest: visibly incomplete rather than silently partial.
    let mut file = fs::File::create(destination.join(MANIFEST_FILE))?;
    file.write_all(document.as_bytes())?;
    file.sync_all()?;
    Ok(CompleteCopyManifest {
        snapshot: members[0].commit.clone(),
        objects: union_objects(&members),
        members,
        dependencies,
    })
}

/// One snapshot being exported: its resolved commit, recorded closure, and the
/// bundle that will carry that closure.
struct ExportedMember {
    snapshot: GitCommitRef,
    name: String,
    bundle: String,
    objects: Vec<CompleteCopyObject>,
}

impl ExportedMember {
    fn into_member(self) -> CompleteCopyMember {
        CompleteCopyMember {
            commit: self.name,
            bundle: self.bundle,
            objects: self.objects,
        }
    }
}

/// Every object reachable from any member, ordered by object ID.
fn union_objects(members: &[CompleteCopyMember]) -> Vec<CompleteCopyObject> {
    let mut union = BTreeSet::new();
    for member in members {
        union.extend(member.objects.iter().cloned());
    }
    union.into_iter().collect()
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
/// Every recorded object of every member is then re-verified against the
/// reconstructed object databases (see the module documentation).
pub fn restore_complete_copy(
    archive: &Path,
    destination: &Path,
) -> Result<CompleteCopyManifest, CompleteCopyError> {
    require_empty(destination)?;
    let manifest = read_complete_copy_manifest(archive)?;
    fs::create_dir_all(destination)?;

    // The primary member is fetched first so the copy's own branch and HEAD
    // name it; the remaining members are fetched into the same object database
    // under their own bundle refs, so both snapshots are readable from one copy.
    fetch_bundle_into(
        &archive.join(&manifest.members[0].bundle),
        destination,
        &manifest.members[0].commit,
    )?;
    for member in manifest.members.iter().skip(1) {
        fetch_member_into(
            &archive.join(&member.bundle),
            destination,
            &member.commit,
            &member_bundle_ref(&member.commit),
        )?;
    }
    for member in &manifest.members {
        verify_closure(
            destination,
            &member.objects,
            &format!("snapshot {}", member.commit),
        )?;
    }

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
/// object comes from the copy itself, so no source remote is contacted. When
/// the archive carries more than one snapshot, the worktree materialises the
/// primary member; every member's objects stay readable in the copy regardless.
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
        // A dependency pinned differently by two members exists under one path:
        // the primary member's pin is the one the worktree shows.
        if !dependency.member.eq_ignore_ascii_case(&manifest.snapshot) {
            continue;
        }
        checkout(&copy.join(&dependency.path), &dependency.commit)?;
    }
    Ok(())
}

fn checkout(repository: &Path, commit: &str) -> Result<(), CompleteCopyError> {
    git_output(repository, &["checkout", "--quiet", "--detach", commit])?;
    Ok(())
}

/// Resolves the gitlinks of every exported member into recorded dependencies.
///
/// Each distinct worktree path is recorded once. The pinned commit comes from
/// the primary member that records it, so a member that predates a dependency
/// bump still exports that member's own dependency commit rather than the
/// primary member's newer one.
fn resolve_dependencies_of(
    repository: &Repository,
    members: &[ExportedMember],
    sources: &[DependencySource],
) -> Result<Vec<CompleteCopyDependency>, CompleteCopyError> {
    let mut dependencies: Vec<CompleteCopyDependency> = Vec::new();
    for exported in members {
        let entries = repository
            .list_committed_tree(&exported.snapshot, MAX_EXPORT_OBJECTS)
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
            continue;
        }
        let origins = read_gitmodules(repository, &exported.snapshot)?;
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
                .committed_submodule_commit(&exported.snapshot, &path)
                .map_err(|_| CompleteCopyError::DependencyUnavailable { path: path.clone() })?;
            let commit = commit.as_str().to_owned();
            // The pinned commit must be present in the dependency's own object
            // database before this export may claim to carry it.
            if object_at(&source, &commit)?.is_none() {
                return Err(CompleteCopyError::DependencyUnavailable { path });
            }
            // Two members may pin the same dependency differently; both commits
            // are carried, each under its own bundle.
            if dependencies
                .iter()
                .any(|existing| existing.path == path && existing.commit == commit)
            {
                continue;
            }
            if dependencies.len() == MAX_EXPORT_DEPENDENCIES {
                return Err(CompleteCopyError::ClosureTooLarge);
            }
            dependencies.push(CompleteCopyDependency {
                path,
                bundle: format!("{DEPENDENCIES_DIR}/{commit}.bundle"),
                commit,
                origin,
                member: exported.name.clone(),
            });
        }
        if dependencies.len() > MAX_EXPORT_DEPENDENCIES {
            return Err(CompleteCopyError::ClosureTooLarge);
        }
    }
    dependencies.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.commit.cmp(&right.commit))
    });
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
    let bytes = match repository.read_committed_file(snapshot, ".gitmodules", MAX_GITMODULES_BYTES)
    {
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
    let flush = |path: &mut Option<String>, url: &mut Option<String>, pairs: &mut Vec<_>| {
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
        &[
            "cat-file",
            "--batch-check=%(objectname) %(objecttype) %(objectsize)",
        ],
        Some(stdin.as_bytes()),
    )?;
    let text = std::str::from_utf8(&output).map_err(|_| CompleteCopyError::GitUnavailable)?;
    let mut results = Vec::with_capacity(oids.len());
    for (line, oid) in text.lines().zip(oids) {
        let mut fields = line.split_ascii_whitespace();
        let (Some(name), Some(kind), Some(size)) = (fields.next(), fields.next(), fields.next())
        else {
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
    let mut child = command
        .spawn()
        .map_err(|_| CompleteCopyError::GitUnavailable)?;
    let mut hasher = object_hasher(&oid)?;
    hasher.update(format!("blob {size}\0").as_bytes());
    let mut stream = child
        .stdout
        .take()
        .ok_or(CompleteCopyError::GitUnavailable)?;
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
    let status = child
        .wait()
        .map_err(|_| CompleteCopyError::GitUnavailable)?;
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
            Self::Sha1(hasher) => Sha1Digest::update(hasher, bytes),
            Self::Sha256(hasher) => Sha2Digest::update(hasher, bytes),
        }
    }

    fn finalize(self) -> Vec<u8> {
        match self {
            Self::Sha1(hasher) => Sha1Digest::finalize(hasher).to_vec(),
            Self::Sha256(hasher) => Sha2Digest::finalize(hasher).to_vec(),
        }
    }
}

fn object_hasher(oid: &str) -> Result<ObjectHasher, CompleteCopyError> {
    match oid.len() {
        40 => Ok(ObjectHasher::Sha1(<Sha1 as Sha1Digest>::new())),
        64 => Ok(ObjectHasher::Sha256(<Sha256 as Sha2Digest>::new())),
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
        &[
            "init",
            "--quiet",
            "--bare",
            "--template=",
            path_text(&stage)?,
        ],
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

/// Fetches the primary member out of one bundle into `destination` without
/// configuring a remote. The bundle carries its own ref name, so the pinned
/// commit is named explicitly and no branch of the source is assumed.
///
/// The fetched commit becomes the copy's own branch and HEAD, so a
/// reconstruction names its pinned commit as an ordinary repository does.
fn fetch_bundle_into(
    bundle: &Path,
    destination: &Path,
    commit: &str,
) -> Result<(), CompleteCopyError> {
    fetch_member_into(bundle, destination, commit, RESTORED_BRANCH)?;
    // A fresh `git init` leaves HEAD on an unborn branch, so the copy would not
    // name its pinned commit until something checked it out. Point HEAD at the
    // reconstructed branch here: the copy is then an ordinary repository whose
    // HEAD is the pinned commit, which is what a recovery read needs.
    git_output(
        destination,
        &["symbolic-ref", "HEAD", &format!("refs/heads/{RESTORED_BRANCH}")],
    )?;
    Ok(())
}

/// Fetches one archive member into the copy's object database under `reference`.
///
/// Every member shares one object database, so an object reachable from two
/// members is stored once; only the ref under which a member is named differs.
fn fetch_member_into(
    bundle: &Path,
    destination: &Path,
    commit: &str,
    reference: &str,
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
            &format!("+refs/heads/{RESTORED_BRANCH}:refs/heads/{reference}"),
        ],
    )
    .map_err(|_| CompleteCopyError::IntegrityMismatch {
        scope: format!("bundle {} does not carry commit {commit}", bundle.display()),
    })?;
    let resolved = git_output(
        destination,
        &["rev-parse", &format!("refs/heads/{reference}")],
    )?;
    let resolved = std::str::from_utf8(&resolved)
        .map_err(|_| CompleteCopyError::GitUnavailable)?
        .trim();
    if !resolved.eq_ignore_ascii_case(commit) {
        return Err(CompleteCopyError::IntegrityMismatch {
            scope: format!("bundle {} carries {resolved}", bundle.display()),
        });
    }
    // A fresh `git init` leaves HEAD on an unborn branch, so the copy would not
    // name its pinned commit until something checked it out. Point HEAD at the
    // reconstructed branch here: the copy is then an ordinary repository whose
    // HEAD is the pinned commit, which is what a recovery read needs.
    git_output(
        destination,
        &["symbolic-ref", "HEAD", &format!("refs/heads/{RESTORED_BRANCH}")],
    )?;
    Ok(())
}

fn require_empty(destination: &Path) -> Result<(), CompleteCopyError> {
    if destination.exists() && fs::read_dir(destination)?.next().is_some() {
        return Err(CompleteCopyError::TargetNotEmpty);
    }
    Ok(())
}

/// The archive-relative bundle file carrying one snapshot's closure.
fn snapshot_bundle(commit: &str) -> String {
    format!("{SNAPSHOTS_DIR}/{commit}.bundle")
}

/// The ref name a reconstructed copy uses for a non-primary member.
///
/// The copy's own branch carries the primary member; every other member needs
/// its own name so both snapshots stay readable in one object database.
fn member_bundle_ref(commit: &str) -> String {
    format!("{MEMBER_REF_PREFIX}{commit}")
}

fn valid_object_id(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_dependency_bundle(bundle: &str) -> bool {
    let Some(name) = bundle.strip_prefix(&format!("{DEPENDENCIES_DIR}/")) else {
        return false;
    };
    valid_bundle_name(name)
}

fn valid_snapshot_bundle(bundle: &str) -> bool {
    let Some(name) = bundle.strip_prefix(&format!("{SNAPSHOTS_DIR}/")) else {
        // A header-1 archive recorded its one closure under this fixed name.
        return bundle == SUPERPROJECT_BUNDLE;
    };
    valid_bundle_name(name)
}

/// One bundle file directly under an archive directory, named after the commit
/// it carries, so a manifest can never redirect a read elsewhere.
fn valid_bundle_name(name: &str) -> bool {
    let Some(commit) = name.strip_suffix(".bundle") else {
        return false;
    };
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
    let mut child = command
        .spawn()
        .map_err(|_| CompleteCopyError::GitUnavailable)?;
    if let Some(bytes) = stdin {
        let mut handle = child
            .stdin
            .take()
            .ok_or(CompleteCopyError::GitUnavailable)?;
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
        parse_gitmodules, valid_dependency_bundle, valid_object_id, valid_snapshot_bundle,
        CompleteCopyManifest,
    };

    #[test]
    fn gitmodules_pairs_are_read_one_section_at_a_time() {
        let text =
            "[submodule \"std\"]\n\tpath = stdlib/std\n\turl = https://example.invalid/std.git\n\
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
    fn manifest_round_trips_every_member_and_its_own_objects() {
        let document = "orna-complete-copy 2\n\
             snapshot\t0123456789012345678901234567890123456789\n\
             archive\t0123456789012345678901234567890123456789\tsnapshots/0123456789012345678901234567890123456789.bundle\n\
             object\tcommit\t0123456789012345678901234567890123456789\t120\n\
             object\tblob\t89abcdef0123456789abcdef0123456789abcdef\t7\n\
             archive\tabcdef0123456789abcdef0123456789abcdef01\tsnapshots/abcdef0123456789abcdef0123456789abcdef01.bundle\n\
             object\tcommit\tabcdef0123456789abcdef0123456789abcdef01\t121\n\
             object\tblob\t89abcdef0123456789abcdef0123456789abcdef\t7\n\
             dependency\tstdlib/std\tabcdef0123456789abcdef0123456789abcdef01\thttps://example.invalid/std.git\tdependencies/abcdef0123456789abcdef0123456789abcdef01.bundle\tabcdef0123456789abcdef0123456789abcdef01\n";
        let decoded = CompleteCopyManifest::decode(document).unwrap();
        assert_eq!(decoded.snapshot, "0123456789012345678901234567890123456789");
        assert_eq!(decoded.members.len(), 2);
        assert_eq!(
            decoded.commits().collect::<Vec<_>>(),
            vec![
                "0123456789012345678901234567890123456789",
                "abcdef0123456789abcdef0123456789abcdef01"
            ]
        );
        // Each member keeps its own closure; the shared blob is recorded once
        // in the archive's union and once in each member.
        assert_eq!(decoded.member("abcdef0123456789abcdef0123456789abcdef01").unwrap().objects.len(), 2);
        assert_eq!(decoded.member("0123456789012345678901234567890123456789").unwrap().objects.len(), 2);
        assert_eq!(decoded.objects.len(), 3);
        assert_eq!(decoded.dependencies[0].path, "stdlib/std");
        assert_eq!(
            decoded.dependencies[0].member,
            "abcdef0123456789abcdef0123456789abcdef01"
        );
        assert_eq!(decoded.counts(), (3, 1));
    }

    #[test]
    fn header_1_archives_still_decode_as_one_member() {
        // An archive written before members existed carries one closure under a
        // fixed bundle name and names no member on its dependency line.
        let document = "orna-complete-copy 1\n\
             snapshot\t0123456789012345678901234567890123456789\n\
             object\tcommit\t0123456789012345678901234567890123456789\t120\n\
             object\tblob\t89abcdef0123456789abcdef0123456789abcdef\t7\n\
             dependency\tstdlib/std\tabcdef0123456789abcdef0123456789abcdef01\thttps://example.invalid/std.git\tdependencies/abcdef0123456789abcdef0123456789abcdef01.bundle\n";
        let decoded = CompleteCopyManifest::decode(document).unwrap();
        assert_eq!(decoded.snapshot, "0123456789012345678901234567890123456789");
        assert_eq!(decoded.members.len(), 1);
        assert_eq!(decoded.members[0].bundle, "superproject.bundle");
        assert_eq!(decoded.members[0].objects.len(), 2);
        assert_eq!(
            decoded.dependencies[0].member,
            "0123456789012345678901234567890123456789"
        );
    }

    #[test]
    fn manifest_rejects_malformed_documents() {
        assert!(
            CompleteCopyManifest::decode("orna-complete-copy 1\nobject\tblob\tzz\t1\n").is_err()
        );
        assert!(CompleteCopyManifest::decode(
            "orna-complete-copy 1\nsnapshot\t0123456789012345678901234567890123456789\nrow\tx\n"
        )
        .is_err());
        // A current archive names its members; a missing one is not a copy.
        assert!(CompleteCopyManifest::decode(
            "orna-complete-copy 2\nsnapshot\t0123456789012345678901234567890123456789\n\
             object\tcommit\t0123456789012345678901234567890123456789\t120\n"
        )
        .is_err());
        // The primary member must be the snapshot the header line names.
        assert!(CompleteCopyManifest::decode(
            "orna-complete-copy 2\nsnapshot\t0123456789012345678901234567890123456789\n\
             archive\tabcdef0123456789abcdef0123456789abcdef01\tsnapshots/abcdef0123456789abcdef0123456789abcdef01.bundle\n"
        )
        .is_err());
        // Two members may not reuse one commit.
        assert!(CompleteCopyManifest::decode(
            "orna-complete-copy 2\nsnapshot\t0123456789012345678901234567890123456789\n\
             archive\t0123456789012345678901234567890123456789\tsnapshots/0123456789012345678901234567890123456789.bundle\n\
             archive\t0123456789012345678901234567890123456789\tsnapshots/0123456789012345678901234567890123456789.bundle\n"
        )
        .is_err());
        // A member bundle may not point outside the archive.
        assert!(CompleteCopyManifest::decode(
            "orna-complete-copy 2\nsnapshot\t0123456789012345678901234567890123456789\n\
             archive\t0123456789012345678901234567890123456789\t../../escape.bundle\n"
        )
        .is_err());
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

    #[test]
    fn snapshot_bundles_stay_inside_the_archive() {
        assert!(valid_snapshot_bundle(
            "snapshots/0123456789012345678901234567890123456789.bundle"
        ));
        // The one closure a header-1 archive recorded lives under this name.
        assert!(valid_snapshot_bundle("superproject.bundle"));
        assert!(!valid_snapshot_bundle("snapshots/../../escape.bundle"));
        assert!(!valid_snapshot_bundle("snapshots/nested/ok.bundle"));
        assert!(!valid_snapshot_bundle("/etc/passwd"));
    }
}
