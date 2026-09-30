//! Snapshot-pinned database attachments and repository-resolved packages.
//!
//! A session reads each database from one exact Git commit. The small
//! `.orna/packages` file is committed with the parent snapshot and contains
//! only `name <commit-oid>` rows; hosts map those names to repositories they
//! already have open. This keeps historical resolution deterministic without
//! adding a registry, semantic-version solver, or separate lockfile.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    path::Path,
};

use orna_repository_v1::{
    CommittedTreeEntryKind, GitCommitRef, Repository, RepositoryError,
};
use orna_semantic_v1::ModuleInput;
use orna_syntax_v1::{Declaration, Keyword, TokenKind, lex, parse_module};

use crate::{LoadedProject, LooseRowCandidate, ProjectLoadError, ProjectLoader};

/// The committed file containing exact package attachment pins.
pub const PACKAGE_PIN_MANIFEST_PATH: &str = ".orna/packages";
const MAX_PACKAGE_PIN_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_PACKAGE_PINS: usize = 256;
const MAX_PACKAGE_PARENT_TREE_ENTRIES: usize = 4_096;

/// One unverified exact commit row from `.orna/packages`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackagePinSpec {
    name: String,
    object_id: String,
}

impl PackagePinSpec {
    /// Package or database alias recorded in the parent snapshot.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Exact native Git object ID recorded in the parent snapshot.
    pub fn object_id(&self) -> &str {
        &self.object_id
    }
}

/// Parsed, bounded package pins from one immutable parent snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackagePinManifest {
    pins: Vec<PackagePinSpec>,
}

impl PackagePinManifest {
    /// Parses rows of `name <40-or-64-character-lowercase-git-oid>`.
    /// Blank lines and whole-line comments are ignored.
    pub fn parse(source: &str) -> Result<Self, AttachmentError> {
        if source.len() > MAX_PACKAGE_PIN_MANIFEST_BYTES {
            return Err(AttachmentError::ManifestTooLarge);
        }
        let mut pins = Vec::new();
        let mut names = BTreeSet::new();
        for line in source.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_ascii_whitespace();
            let Some(name) = fields.next() else {
                continue;
            };
            let Some(object_id) = fields.next() else {
                return Err(AttachmentError::MalformedManifest);
            };
            if fields.next().is_some()
                || !valid_attachment_name(name)
                || name == "sys"
                || !valid_full_git_oid(object_id)
                || !names.insert(name.to_owned())
            {
                return Err(AttachmentError::MalformedManifest);
            }
            pins.push(PackagePinSpec {
                name: name.to_owned(),
                object_id: object_id.to_owned(),
            });
            if pins.len() > MAX_PACKAGE_PINS {
                return Err(AttachmentError::TooManyPackages);
            }
        }
        Ok(Self { pins })
    }

    /// Reads the manifest from the exact committed parent snapshot.
    pub fn load(
        repository: &Repository,
        parent: &GitCommitRef,
    ) -> Result<Self, AttachmentError> {
        let entries = repository
            .list_committed_tree(parent, MAX_PACKAGE_PARENT_TREE_ENTRIES)
            .map_err(AttachmentError::Repository)?;
        let path = Path::new(PACKAGE_PIN_MANIFEST_PATH);
        let manifest = entries.iter().find(|entry| entry.path().as_path() == path);
        if let Some(entry) = manifest {
            if !matches!(entry.kind(), CommittedTreeEntryKind::File { .. }) {
                return Err(AttachmentError::MalformedManifest);
            }
        } else if entries
            .iter()
            .any(|entry| entry.path().as_path().starts_with(path))
        {
            // `.orna/packages` is a file boundary. A directory below that
            // name is malformed rather than an empty optional dependency set.
            return Err(AttachmentError::MalformedManifest);
        } else {
            // The reference makes `std` and other packages optional, but does
            // not prescribe a manifest format. In v1, omitting our exact-pin
            // manifest means the parent has no package attachments.
            return Ok(Self::default());
        }
        let bytes = repository
            .read_committed_file(parent, PACKAGE_PIN_MANIFEST_PATH, MAX_PACKAGE_PIN_MANIFEST_BYTES)
            .map_err(AttachmentError::Repository)?;
        let source = std::str::from_utf8(&bytes).map_err(|_| AttachmentError::MalformedManifest)?;
        Self::parse(source)
    }

    /// Exact pins in their committed order.
    pub fn pins(&self) -> &[PackagePinSpec] {
        &self.pins
    }
}

/// A package/database identity resolved to one immutable Git commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackagePin {
    name: String,
    commit: GitCommitRef,
}

impl PackagePin {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn commit(&self) -> &GitCommitRef {
        &self.commit
    }
}

/// A database loaded entirely from its pinned commit.
#[derive(Clone, Debug)]
pub struct PinnedDatabase {
    pin: PackagePin,
    repository: Repository,
    project: LoadedProject,
}

impl PinnedDatabase {
    /// Resolves a selector once, then loads the resulting immutable commit.
    /// A branch or tag therefore never remains live inside the session.
    pub fn resolve(
        name: impl Into<String>,
        repository: Repository,
        selector: &str,
        loader: ProjectLoader,
    ) -> Result<Self, AttachmentError> {
        let name = checked_name(name.into())?;
        let commit = repository
            .resolve_snapshot(selector)
            .map_err(AttachmentError::Repository)?;
        Self::load_at_commit(name, repository, commit, loader)
    }

    fn load_at_commit(
        name: String,
        repository: Repository,
        commit: GitCommitRef,
        loader: ProjectLoader,
    ) -> Result<Self, AttachmentError> {
        let project = loader
            .load_committed_snapshot(&repository, &commit)
            .map_err(AttachmentError::Project)?;
        Ok(Self {
            pin: PackagePin { name, commit },
            repository,
            project,
        })
    }

    pub fn pin(&self) -> &PackagePin {
        &self.pin
    }

    pub fn project(&self) -> &LoadedProject {
        &self.project
    }
}

/// Resolves aliases in a committed pin manifest against repositories already
/// opened by the host. It does not discover repositories from ambient paths.
#[derive(Clone, Debug)]
pub struct PackageResolver {
    repositories: BTreeMap<String, Repository>,
    loader: ProjectLoader,
}

impl PackageResolver {
    /// Builds the host's alias-to-repository mapping.
    pub fn new(
        repositories: impl IntoIterator<Item = (String, Repository)>,
        loader: ProjectLoader,
    ) -> Result<Self, AttachmentError> {
        let mut resolved = BTreeMap::new();
        for (name, repository) in repositories {
            let name = checked_name(name)?;
            if name == "sys" || resolved.insert(name, repository).is_some() {
                return Err(AttachmentError::DuplicateRepository);
            }
        }
        Ok(Self {
            repositories: resolved,
            loader,
        })
    }

    /// Loads the exact attachments declared by `primary`'s pinned snapshot.
    /// Parent history is authoritative: later edits to the primary manifest
    /// cannot move this session to newer package commits. Resolution is
    /// all-or-nothing: a failed pin never returns a partially attached session.
    pub fn resolve_for_parent(
        &self,
        primary: PinnedDatabase,
    ) -> Result<AttachedDatabaseSession, AttachmentError> {
        let manifest = PackagePinManifest::load(&primary.repository, &primary.pin.commit)?;
        // `stdlib/std` is the project import's source authority; `.orna/packages`
        // is the explicit host attachment map. If both pin the `std` alias,
        // require one immutable snapshot so the package map cannot retarget
        // `use std.*` away from the program's captured Git module.
        let standard_gitlink =
            captured_standard_gitlink(&primary.repository, &primary.pin.commit)?;
        AttachedDatabaseSession::validate_primary_name(&primary.pin.name)?;
        let mut resolved_databases = Vec::with_capacity(manifest.pins().len());
        for spec in manifest.pins() {
            if spec.name() == primary.pin.name {
                return Err(AttachmentError::DuplicateAttachment);
            }
            if spec.name() == "std"
                && standard_gitlink
                    .as_ref()
                    .is_some_and(|commit| commit.as_str() != spec.object_id())
            {
                return Err(AttachmentError::PinUnavailable);
            }
            let repository = self
                .repositories
                .get(spec.name())
                .cloned()
                .ok_or(AttachmentError::RepositoryUnavailable)?;
            let commit = repository
                .resolve_snapshot(spec.object_id())
                .map_err(|_| AttachmentError::PinUnavailable)?;
            if commit.as_str() != spec.object_id() {
                return Err(AttachmentError::PinUnavailable);
            }
            let database = PinnedDatabase::load_at_commit(
                spec.name().to_owned(),
                repository,
                commit,
                self.loader,
            )
            .map_err(|error| match error {
                // The normative contract fixes exact commit identity but not
                // package-load diagnostics. Keep host paths and parser details
                // out of resolver errors while distinguishing a present pin
                // whose contents are not a loadable database.
                AttachmentError::Project(_) => AttachmentError::PinnedPackageInvalid,
                error => error,
            })?;
            resolved_databases.push(database);
        }

        // The reference requires each attachment to use its exact pin but
        // leaves batch failure visibility unspecified. Resolve every package
        // before creating the session so callers can observe no partial set.
        let mut session = AttachedDatabaseSession::new(primary)?;
        for database in resolved_databases {
            session.attach_database(database)?;
        }
        Ok(session)
    }
}

/// A primary database and zero or more read-only, commit-pinned attachments.
#[derive(Clone, Debug)]
pub struct AttachedDatabaseSession {
    primary: PinnedDatabase,
    attached: BTreeMap<String, PinnedDatabase>,
}

impl AttachedDatabaseSession {
    pub fn new(primary: PinnedDatabase) -> Result<Self, AttachmentError> {
        Self::validate_primary_name(&primary.pin.name)?;
        Ok(Self {
            primary,
            attached: BTreeMap::new(),
        })
    }

    fn validate_primary_name(name: &str) -> Result<(), AttachmentError> {
        if name == "sys" || name == "std" {
            return Err(AttachmentError::InvalidName);
        }
        Ok(())
    }

    /// Adds a secondary snapshot. The source and rows remain read-only for the
    /// lifetime of the session; writes are admitted only against the primary.
    pub fn attach_database(
        &mut self,
        database: PinnedDatabase,
    ) -> Result<(), AttachmentError> {
        let name = database.pin.name.clone();
        if name == "sys" {
            return Err(AttachmentError::SystemDatabaseCannotAttach);
        }
        if name == self.primary.pin.name {
            return Err(AttachmentError::DuplicateAttachment);
        }
        if self.attached.contains_key(&name) {
            return Err(AttachmentError::DuplicateAttachment);
        }
        self.attached.insert(name, database);
        Ok(())
    }

    /// Detaches an optional database alias from subsequent session lookups.
    /// The primary and implementation-provided `sys` facility are not
    /// detachable. The reference does not define live detach timing; v1 drops
    /// the alias immediately, so callers must revalidate module admission
    /// before the next evaluation. A detached alias is unavailable as both a
    /// read and write target; reattaching it admits only the new read-only pin.
    /// Matching is by exact alias name; a prefix does not select or detach a
    /// neighboring alias.
    /// Each clone keeps its own alias map: later detach or reattach operations
    /// on one value do not update another, and a clone made while detached
    /// stays detached. Module and relation routing use the same per-instance
    /// map as read and write validation.
    pub fn detach_database(&mut self, name: &str) -> Result<(), AttachmentError> {
        let name = checked_name(name.to_owned())?;
        if name == self.primary.pin.name {
            return Err(AttachmentError::PrimaryDatabaseCannotDetach);
        }
        if name == "sys" {
            return Err(AttachmentError::SystemDatabaseCannotDetach);
        }
        self.attached
            .remove(&name)
            .map(drop)
            .ok_or(AttachmentError::AttachmentNotFound)
    }

    pub fn primary(&self) -> &PinnedDatabase {
        &self.primary
    }

    pub fn attached(&self) -> impl Iterator<Item = (&str, &PinnedDatabase)> {
        self.attached
            .iter()
            .map(|(name, database)| (name.as_str(), database))
    }

    pub fn database(&self, name: &str) -> Option<&PinnedDatabase> {
        if self.primary.pin.name == name {
            Some(&self.primary)
        } else {
            self.attached.get(name)
        }
    }

    /// Reports the one database whose transaction may be written by this
    /// session. Independent attached logs are never described as atomic.
    pub fn is_writable_database(&self, name: &str) -> bool {
        self.validate_write_target(name).is_ok()
    }

    /// Enforces the attach-layer write boundary before a caller starts a
    /// mutation. ORNA-ATTACH-002 and ORNA-HIST-003 require attached snapshots
    /// to stay read-only; v1 reports a stable refusal for attached aliases and
    /// the system namespace, while only the session's primary is writable.
    pub fn validate_write_target(&self, name: &str) -> Result<(), AttachmentError> {
        let name = checked_name(name.to_owned())?;
        if name == self.primary.pin.name {
            return Ok(());
        }
        if name == "sys" {
            return Err(AttachmentError::SystemDatabaseReadOnly);
        }
        if self.attached.contains_key(&name) {
            return Err(AttachmentError::AttachedSnapshotReadOnly);
        }
        Err(AttachmentError::DatabaseUnavailable)
    }

    /// Source modules for typed session admission. Attached module namespaces
    /// use each full exact alias, so prefix-related aliases stay independent.
    pub fn module_inputs(&self) -> Vec<ModuleInput> {
        let mut modules = self.primary.project.modules().to_vec();
        for (name, database) in &self.attached {
            if name == "std" {
                continue;
            }
            modules.extend(database.project.modules().iter().map(|module| {
                let logical_path = if module.logical_path == "main.orna" {
                    if name == "main" {
                        // Keep the attached `main` namespace distinct from the
                        // primary root module, which already owns `main.orna`.
                        "main/main.orna".to_owned()
                    } else {
                        format!("{name}.orna")
                    }
                } else {
                    format!("{name}/{}", module.logical_path)
                };
                ModuleInput::new(logical_path, module.source.clone())
            }));
        }
        modules
    }

    /// Source units supplied by the optional `std` attachment, if present.
    /// Core language and `sys` are not sourced from this package.
    pub fn standard_sources(&self) -> Vec<(String, String)> {
        self.attached.get("std").map_or_else(Vec::new, |database| {
            database
                .project
                .modules()
                .iter()
                .map(|module| {
                    let logical_path = if module.logical_path == "main.orna" {
                        "std/main.orna".to_owned()
                    } else {
                        format!("std/{}", module.logical_path)
                    };
                    (logical_path, module.source.clone())
                })
                .collect()
        })
    }

    /// Exact source snapshot used by an optional `std` package attachment.
    pub fn standard_snapshot(&self) -> Option<&GitCommitRef> {
        self.attached.get("std").map(|database| database.pin.commit())
    }

    /// Returns every row source for a table path together with the exact
    /// database snapshot that supplied it. Routing follows this session's own
    /// alias map, so clones retain distinct relation sources across detach and
    /// reattach. Detaching one alias leaves other aliases' rows routable. A
    /// query layer can compose these reads without pretending the separate
    /// write logs are atomic. V1 deliberately returns overlapping row paths
    /// from every alias without deduplicating them; consumers keep the alias
    /// and commit with each row and decide how their query treats that overlap.
    /// Aliases are exact names, so prefix overlap never selects or replaces a
    /// different route; `app`, `app_copy`, and `app_copy_archive` stay distinct.
    pub fn relation_sources<'a>(
        &'a self,
        table_path: &str,
    ) -> Vec<AttachedRelationSource<'a>> {
        let mut sources = Vec::new();
        for database in std::iter::once(&self.primary).chain(self.attached.values()) {
            for row in database
                .project
                .loose_rows()
                .iter()
                .filter(|row| row.table_path() == table_path)
            {
                sources.push(AttachedRelationSource {
                    database: database.pin.name(),
                    commit: database.pin.commit(),
                    row,
                });
            }
        }
        sources
    }

    /// Structural unit compatibility used at database boundaries. Names and
    /// source locations are excluded; dimension and definition tokens must
    /// otherwise match after the language lexer removes whitespace/comments.
    /// This conservative v1 rule never coerces on a name match alone.
    pub fn units_structurally_equivalent(
        &self,
        left_database: &str,
        left_unit: &str,
        right_database: &str,
        right_unit: &str,
    ) -> bool {
        let Some(left) = self
            .database(left_database)
            .and_then(|database| unit_signature(&database.project, left_unit))
        else {
            return false;
        };
        let Some(right) = self
            .database(right_database)
            .and_then(|database| unit_signature(&database.project, right_unit))
        else {
            return false;
        };
        left == right
    }
}

fn captured_standard_gitlink(
    repository: &Repository,
    parent: &GitCommitRef,
) -> Result<Option<GitCommitRef>, AttachmentError> {
    let tree = repository
        .list_committed_tree(parent, 4_096)
        .map_err(AttachmentError::Repository)?;
    let Some(entry) = tree
        .iter()
        .find(|entry| entry.path().as_path() == Path::new("stdlib/std"))
    else {
        return Ok(None);
    };
    if entry.kind() != CommittedTreeEntryKind::Submodule {
        return Err(AttachmentError::PinUnavailable);
    }
    repository
        .committed_submodule_commit(parent, "stdlib/std")
        .map(Some)
        .map_err(AttachmentError::Repository)
}

/// One row source from a pinned database snapshot.
#[derive(Clone, Copy, Debug)]
pub struct AttachedRelationSource<'a> {
    database: &'a str,
    commit: &'a GitCommitRef,
    row: &'a LooseRowCandidate,
}

impl<'a> AttachedRelationSource<'a> {
    pub fn database(&self) -> &'a str {
        self.database
    }

    pub fn commit(&self) -> &'a GitCommitRef {
        self.commit
    }

    pub fn row(&self) -> &'a LooseRowCandidate {
        self.row
    }
}

/// Redacted failures while resolving a committed attachment set.
#[derive(Debug)]
pub enum AttachmentError {
    InvalidName,
    MalformedManifest,
    ManifestTooLarge,
    TooManyPackages,
    DuplicateRepository,
    SystemDatabaseCannotAttach,
    RepositoryUnavailable,
    PinUnavailable,
    PinnedPackageInvalid,
    DuplicateAttachment,
    AttachedSnapshotReadOnly,
    SystemDatabaseReadOnly,
    DatabaseUnavailable,
    PrimaryDatabaseCannotDetach,
    SystemDatabaseCannotDetach,
    AttachmentNotFound,
    Repository(RepositoryError),
    Project(ProjectLoadError),
}

impl fmt::Display for AttachmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidName => "attachment name is invalid or reserved",
            Self::MalformedManifest => "package pin manifest is malformed",
            Self::ManifestTooLarge => "package pin manifest exceeds its size limit",
            Self::TooManyPackages => "package pin manifest exceeds its entry limit",
            Self::DuplicateRepository => "repository aliases are invalid or duplicated",
            Self::SystemDatabaseCannotAttach => "the system database is provided by the host",
            Self::RepositoryUnavailable => "pinned package repository is unavailable",
            Self::PinUnavailable => "pinned package commit is unavailable",
            Self::PinnedPackageInvalid => "pinned package is not a loadable database",
            Self::DuplicateAttachment => "database attachment name is already in use",
            Self::AttachedSnapshotReadOnly => "attached database snapshots are read-only",
            Self::SystemDatabaseReadOnly => "the system database cannot be written",
            Self::DatabaseUnavailable => "database is not available in this session",
            Self::PrimaryDatabaseCannotDetach => "the primary database cannot be detached",
            Self::SystemDatabaseCannotDetach => "the system database cannot be detached",
            Self::AttachmentNotFound => "database attachment does not exist",
            Self::Repository(_) => "repository snapshot could not be read",
            Self::Project(_) => "pinned database source could not be loaded",
        })
    }
}

impl Error for AttachmentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            Self::Project(error) => Some(error),
            _ => None,
        }
    }
}

fn checked_name(name: String) -> Result<String, AttachmentError> {
    if valid_attachment_name(&name) {
        Ok(name)
    } else {
        Err(AttachmentError::InvalidName)
    }
}

fn valid_attachment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_full_git_oid(object_id: &str) -> bool {
    matches!(object_id.len(), 40 | 64)
        && object_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn unit_signature(project: &LoadedProject, requested_name: &str) -> Option<String> {
    let mut found = None;
    for module in project.modules() {
        let parsed = parse_module(&module.source);
        if !parsed.is_ok() {
            return None;
        }
        let Ok(tokens) = lex(&module.source) else {
            return None;
        };
        for item in &parsed.value.items {
            let Declaration::Unit { name, .. } = &item.declaration else {
                continue;
            };
            if name != requested_name {
                continue;
            }
            let mut saw_unit = false;
            let mut saw_name = false;
            let signature = tokens
                .iter()
                .filter(|token| {
                    token.span.start >= item.span.start && token.span.end <= item.span.end
                })
                .filter_map(|token| {
                    if matches!(token.kind, TokenKind::Keyword(Keyword::Pub)) {
                        return None;
                    }
                    if !saw_unit && matches!(token.kind, TokenKind::Keyword(Keyword::Unit)) {
                        saw_unit = true;
                        return None;
                    }
                    if saw_unit && !saw_name {
                        saw_name = true;
                        return None;
                    }
                    Some(format!("{:?}:{}", token.kind, token.text))
                })
                .collect::<Vec<_>>()
                .join("\0");
            if found.as_ref().is_some_and(|previous| previous != &signature) {
                return None;
            }
            found = Some(signature);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};
    use tempfile::TempDir;

    fn git(directory: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn repository(source: &str) -> (TempDir, Repository, String) {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("main.orna"), source).unwrap();
        git(directory.path(), &["init", "--quiet"]);
        git(directory.path(), &["config", "user.name", "kierandrewett"]);
        git(directory.path(), &["config", "user.email", "kieran@drewett.dev"]);
        git(directory.path(), &["config", "commit.gpgsign", "false"]);
        git(directory.path(), &["add", "main.orna"]);
        git(directory.path(), &["commit", "--quiet", "-m", "snapshot"]);
        let commit = git(directory.path(), &["rev-parse", "HEAD"]);
        let repository = Repository::discover(directory.path()).unwrap();
        (directory, repository, commit)
    }

    fn write_commit(directory: &std::path::Path, path: &str, contents: &str) -> String {
        let path = directory.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        git(directory, &["add", "--all"]);
        git(directory, &["commit", "--quiet", "-m", "snapshot"]);
        git(directory, &["rev-parse", "HEAD"])
    }

    #[test]
    fn parent_snapshot_resolves_exact_package_commit_after_head_moves() {
        let package_dir = tempfile::tempdir().unwrap();
        git(package_dir.path(), &["init", "--quiet"]);
        git(package_dir.path(), &["config", "user.name", "kierandrewett"]);
        git(package_dir.path(), &["config", "user.email", "kieran@drewett.dev"]);
        git(package_dir.path(), &["config", "commit.gpgsign", "false"]);
        let package_commit = write_commit(
            package_dir.path(),
            "main.orna",
            include_str!("fixtures/attached-package-main.orna"),
        );
        let package_repository = Repository::discover(package_dir.path()).unwrap();

        let primary_dir = tempfile::tempdir().unwrap();
        git(primary_dir.path(), &["init", "--quiet"]);
        git(primary_dir.path(), &["config", "user.name", "kierandrewett"]);
        git(primary_dir.path(), &["config", "user.email", "kieran@drewett.dev"]);
        git(primary_dir.path(), &["config", "commit.gpgsign", "false"]);
        write_commit(
            primary_dir.path(),
            "main.orna",
            include_str!("fixtures/attached-primary-main.orna"),
        );
        let parent = write_commit(
            primary_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("widgets {package_commit}\n"),
        );
        let primary_repository = Repository::discover(primary_dir.path()).unwrap();
        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &parent,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [("widgets".to_owned(), package_repository.clone())],
            loader,
        )
        .unwrap();

        let session = resolver.resolve_for_parent(primary).unwrap();
        let pinned_widgets = session.database("widgets").unwrap();
        assert_eq!(pinned_widgets.pin().commit().as_str(), package_commit);
        assert!(session.is_writable_database("app"));
        assert!(!session.is_writable_database("widgets"));
        assert!(session
            .module_inputs()
            .iter()
            .any(|module| module.logical_path == "widgets.orna"));

        write_commit(
            package_dir.path(),
            "main.orna",
            &include_str!("fixtures/attached-package-main.orna").replace("42", "99"),
        );
        assert_eq!(
            session
                .database("widgets")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            package_commit
        );
        assert!(session
            .database("widgets")
            .unwrap()
            .project()
            .modules()
            .iter()
            .any(|module| module.source.contains("= 42")));
    }

    #[test]
    fn nested_archive_keeps_its_child_pin_and_unit_identity_scoped() {
        let (_source_dir, source_repository, source_commit) =
            repository(include_str!("../tests/fixtures/attached-incompatible-main.orna"));
        let (archive_dir, archive_repository, _) =
            repository(include_str!("../tests/fixtures/attached-equivalent-main.orna"));
        let archive_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("source {source_commit}\n"),
        );

        let primary_source =
            include_str!("../tests/fixtures/attached-equivalent-main.orna").replace("42", "7");
        let (primary_dir, primary_repository, _) = repository(&primary_source);
        let parent_commit = write_commit(
            primary_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &parent_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("archive".to_owned(), archive_repository),
                ("source".to_owned(), source_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(primary).unwrap();
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_commit
        );
        assert!(root_session.database("source").is_none());
        assert!(root_session.units_structurally_equivalent(
            "app", "meter", "archive", "meter"
        ));

        // The reference pins attachments from the selected parent but does not
        // prescribe recursive namespace flattening. V1 reads an archive's own
        // child manifest when that pinned archive is selected as a parent.
        let archive = root_session.database("archive").unwrap().clone();
        let nested_session = resolver.resolve_for_parent(archive).unwrap();
        assert_eq!(
            nested_session.primary().pin().commit().as_str(),
            archive_commit
        );
        assert_eq!(
            nested_session
                .database("source")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            source_commit
        );
        assert!(!nested_session.units_structurally_equivalent(
            "archive",
            "meter",
            "source",
            "meter"
        ));

        // Resolving the nested pin set does not retarget or flatten the root.
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_commit
        );
        assert!(root_session.database("source").is_none());
    }

    #[test]
    fn same_nested_alias_keeps_each_parent_snapshot_pin() {
        let (source_dir, source_repository, child_commit) =
            repository(include_str!("../tests/fixtures/attached-incompatible-main.orna"));
        let direct_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let direct_commit = write_commit(source_dir.path(), "main.orna", direct_source);
        assert_eq!(
            git(source_dir.path(), &["rev-parse", "HEAD"]),
            direct_commit
        );

        let (archive_dir, archive_repository, _) =
            repository(include_str!("../tests/fixtures/attached-equivalent-main.orna"));
        let archive_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("source {child_commit}\n"),
        );

        let primary_source = direct_source.replace("42", "7");
        let (primary_dir, primary_repository, _) = repository(&primary_source);
        let parent_commit = write_commit(
            primary_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\nsource {direct_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &parent_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("archive".to_owned(), archive_repository),
                ("source".to_owned(), source_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(primary).unwrap();
        assert_eq!(
            root_session
                .database("source")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            direct_commit
        );
        assert!(root_session.units_structurally_equivalent(
            "app", "meter", "source", "meter"
        ));

        // The reference does not define a flattened namespace for nested
        // manifests. V1 resolves the same alias independently for each pinned
        // parent, so the archive's `source` cannot replace the root's pin.
        let archive = root_session.database("archive").unwrap().clone();
        let nested_session = resolver.resolve_for_parent(archive).unwrap();
        assert_eq!(
            nested_session
                .database("source")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            child_commit
        );
        assert_ne!(direct_commit, child_commit);
        assert!(!nested_session.units_structurally_equivalent(
            "archive",
            "meter",
            "source",
            "meter"
        ));

        assert_eq!(
            root_session
                .database("source")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            direct_commit
        );
        assert!(root_session.units_structurally_equivalent(
            "app", "meter", "source", "meter"
        ));
    }

    #[test]
    fn nested_pin_chain_keeps_repeated_archive_alias_at_each_commit() {
        let equivalent_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_later_source = equivalent_source.replace("42", "99");
        let (archive_dir, archive_repository, archive_later_commit) =
            repository(&archive_later_source);

        let (source_dir, source_repository, _) =
            repository(include_str!("../tests/fixtures/attached-incompatible-main.orna"));
        let source_commit = write_commit(
            source_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_later_commit}\n"),
        );

        write_commit(archive_dir.path(), "main.orna", equivalent_source);
        let archive_parent_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("source {source_commit}\n"),
        );

        let primary_source = equivalent_source.replace("42", "7");
        let (primary_dir, primary_repository, _) = repository(&primary_source);
        let parent_commit = write_commit(
            primary_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_parent_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &parent_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("archive".to_owned(), archive_repository),
                ("source".to_owned(), source_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(primary).unwrap();
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_parent_commit
        );
        assert!(root_session.database("source").is_none());
        assert!(root_session.units_structurally_equivalent(
            "app", "meter", "archive", "meter"
        ));

        // The reference fixes each selected parent's exact pins but is silent
        // on recursive pin closure. V1 follows one manifest edge per session;
        // repeated aliases at later depths keep the commit selected there.
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        assert_eq!(
            archive_session.primary().pin().commit().as_str(),
            archive_parent_commit
        );
        assert_eq!(
            archive_session
                .database("source")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            source_commit
        );
        assert!(!archive_session.units_structurally_equivalent(
            "archive",
            "meter",
            "source",
            "meter"
        ));

        let source = archive_session.database("source").unwrap().clone();
        let source_session = resolver.resolve_for_parent(source).unwrap();
        assert_eq!(
            source_session.primary().pin().commit().as_str(),
            source_commit
        );
        assert_eq!(
            source_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_later_commit
        );
        assert_ne!(archive_parent_commit, archive_later_commit);
        assert!(!source_session.units_structurally_equivalent(
            "source",
            "meter",
            "archive",
            "meter"
        ));

        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_parent_commit
        );
    }

    #[test]
    fn nested_pin_cycle_closes_on_historical_parent_snapshot() {
        let app_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, app_historical_commit) = repository(app_source);
        let current_app_source = app_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &current_app_source);

        let (archive_dir, archive_repository, _) =
            repository(include_str!("../tests/fixtures/attached-incompatible-main.orna"));
        let archive_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );
        let current_app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &current_app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        assert_eq!(
            root_session.primary().pin().commit().as_str(),
            current_app_commit
        );
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_commit
        );
        assert!(!root_session.units_structurally_equivalent(
            "app",
            "meter",
            "archive",
            "meter"
        ));

        // The reference fixes each selected parent's exact pins but leaves
        // recursive closure unspecified. V1 advances one parent manifest at
        // a time, so this alias cycle terminates at the historical empty map.
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        assert_eq!(
            archive_session.primary().pin().commit().as_str(),
            archive_commit
        );
        assert_eq!(
            archive_session
                .database("app")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            app_historical_commit
        );
        assert!(!archive_session.units_structurally_equivalent(
            "archive",
            "meter",
            "app",
            "meter"
        ));

        let historical_app = archive_session.database("app").unwrap().clone();
        let closed_session = resolver.resolve_for_parent(historical_app).unwrap();
        assert_eq!(
            closed_session.primary().pin().commit().as_str(),
            app_historical_commit
        );
        assert_eq!(closed_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_commit
        );
    }

    #[test]
    fn nested_pin_cycle_keeps_repeated_aliases_distinct_to_the_terminal_edge() {
        let equivalent_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_historical_source = equivalent_source.replace("42", "99");
        let (archive_dir, archive_repository, _) =
            repository(&archive_historical_source);

        let (app_dir, app_repository, app_base_commit) = repository(equivalent_source);
        let archive_historical_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_base_commit}\n"),
        );
        let app_historical_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_historical_commit}\n"),
        );

        write_commit(
            archive_dir.path(),
            "main.orna",
            include_str!("../tests/fixtures/attached-incompatible-main.orna"),
        );
        let archive_current_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );

        let current_app_source = equivalent_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &current_app_source);
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        assert_eq!(
            root_session.primary().pin().commit().as_str(),
            app_current_commit
        );
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
        assert!(!root_session.units_structurally_equivalent(
            "app",
            "meter",
            "archive",
            "meter"
        ));

        // The reference fixes exact pins per selected parent but leaves
        // recursive cycle handling unspecified. V1 opens one manifest edge
        // per session; repeated aliases remain distinct by their pinned OID.
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        assert_eq!(
            archive_session.primary().pin().commit().as_str(),
            archive_current_commit
        );
        assert_eq!(
            archive_session
                .database("app")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            app_historical_commit
        );
        assert!(!archive_session.units_structurally_equivalent(
            "archive",
            "meter",
            "app",
            "meter"
        ));

        let historical_app = archive_session.database("app").unwrap().clone();
        let historical_app_session = resolver.resolve_for_parent(historical_app).unwrap();
        assert_eq!(
            historical_app_session.primary().pin().commit().as_str(),
            app_historical_commit
        );
        assert_eq!(
            historical_app_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_historical_commit
        );
        assert!(historical_app_session.units_structurally_equivalent(
            "app",
            "meter",
            "archive",
            "meter"
        ));

        let historical_archive = historical_app_session
            .database("archive")
            .unwrap()
            .clone();
        let closure_session = resolver.resolve_for_parent(historical_archive).unwrap();
        assert_eq!(
            closure_session.primary().pin().commit().as_str(),
            archive_historical_commit
        );
        assert_eq!(
            closure_session
                .database("app")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            app_base_commit
        );
        assert!(closure_session.units_structurally_equivalent(
            "archive",
            "meter",
            "app",
            "meter"
        ));

        let base_app = closure_session.database("app").unwrap().clone();
        let terminal_session = resolver.resolve_for_parent(base_app).unwrap();
        assert_eq!(
            terminal_session.primary().pin().commit().as_str(),
            app_base_commit
        );
        assert_eq!(terminal_session.attached().count(), 0);
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
    }

    #[test]
    fn nested_pin_closure_rejects_oid_present_only_in_sibling_repository() {
        let app_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, sibling_commit) = repository(archive_source);
        assert!(app_repository.resolve_snapshot(&sibling_commit).is_err());
        assert!(archive_repository.resolve_snapshot(&sibling_commit).is_ok());

        let archive_closure_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {sibling_commit}\n"),
        );
        let app_historical_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_closure_commit}\n"),
        );
        let archive_current_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );
        let app_current_source = app_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &app_current_source);
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        let historical_app = archive_session.database("app").unwrap().clone();
        let historical_app_session = resolver.resolve_for_parent(historical_app).unwrap();
        let archive_closure = historical_app_session
            .database("archive")
            .unwrap()
            .clone();

        // The reference requires the exact historical pin but leaves nested
        // closure diagnostics unspecified. V1 resolves each edge only in the
        // repository mapped to its alias, so a sibling OID is unavailable.
        assert!(matches!(
            resolver.resolve_for_parent(archive_closure),
            Err(AttachmentError::PinUnavailable)
        ));
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
        assert_eq!(
            archive_session
                .database("app")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            app_historical_commit
        );
        assert_eq!(
            historical_app_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_closure_commit
        );
    }

    #[test]
    fn nested_pin_closure_requires_a_repository_for_the_terminal_alias() {
        let app_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, archive_base_commit) = repository(archive_source);

        let archive_terminal_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("unmapped {archive_base_commit}\n"),
        );
        let app_historical_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_terminal_commit}\n"),
        );
        let archive_current_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );
        let app_current_source = app_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &app_current_source);
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );

        assert!(archive_repository
            .resolve_snapshot(&archive_base_commit)
            .is_ok());
        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        let historical_app = archive_session.database("app").unwrap().clone();
        let historical_app_session = resolver.resolve_for_parent(historical_app).unwrap();
        let archive_terminal = historical_app_session
            .database("archive")
            .unwrap()
            .clone();

        // The reference requires exact pins per selected parent but leaves
        // recursive closure diagnostics unspecified. V1 requires an explicit
        // alias mapping for every edge instead of searching by OID.
        assert!(matches!(
            resolver.resolve_for_parent(archive_terminal),
            Err(AttachmentError::RepositoryUnavailable)
        ));
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
        assert_eq!(
            archive_session
                .database("app")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            app_historical_commit
        );
        assert_eq!(
            historical_app_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_terminal_commit
        );
    }

    #[test]
    fn nested_pin_closure_keeps_shared_repository_alias_and_terminal_pin() {
        let app_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, archive_base_commit) = repository(archive_source);

        let archive_terminal_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("mirror {archive_base_commit}\n"),
        );
        let app_historical_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_terminal_commit}\n"),
        );
        let archive_current_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );
        let app_current_source = app_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &app_current_source);
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository.clone()),
                ("mirror".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        let historical_app = archive_session.database("app").unwrap().clone();
        let historical_app_session = resolver.resolve_for_parent(historical_app).unwrap();
        let archive_terminal = historical_app_session
            .database("archive")
            .unwrap()
            .clone();
        let terminal_session = resolver.resolve_for_parent(archive_terminal).unwrap();

        // The reference fixes exact pins but does not specify aliases sharing
        // one repository. V1 preserves the manifest alias and resolves its OID
        // in that alias's configured repository, even when another alias uses it.
        assert_eq!(
            terminal_session.primary().pin().name(),
            "archive"
        );
        assert_eq!(
            terminal_session.primary().pin().commit().as_str(),
            archive_terminal_commit
        );
        assert_eq!(
            terminal_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_base_commit
        );
        assert_ne!(archive_terminal_commit, archive_base_commit);
        assert!(terminal_session.units_structurally_equivalent(
            "archive",
            "meter",
            "mirror",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
    }

    #[test]
    fn nested_pin_closure_keeps_distinct_pins_for_shared_repository_aliases() {
        let app_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, archive_base_commit) = repository(archive_source);
        let mirror_commit = write_commit(
            archive_dir.path(),
            "main.orna",
            include_str!("../tests/fixtures/attached-equivalent-main.orna"),
        );
        let archive_terminal_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {mirror_commit}\nmirror {archive_base_commit}\n"),
        );
        let app_historical_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_terminal_commit}\n"),
        );
        let archive_current_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );
        let app_current_source = app_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &app_current_source);
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository.clone()),
                ("backup".to_owned(), archive_repository.clone()),
                ("mirror".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        let historical_app = archive_session.database("app").unwrap().clone();
        let historical_app_session = resolver.resolve_for_parent(historical_app).unwrap();
        let archive_terminal = historical_app_session
            .database("archive")
            .unwrap()
            .clone();
        let terminal_session = resolver.resolve_for_parent(archive_terminal).unwrap();

        // The reference fixes exact pins but does not define aliases sharing
        // one repository. V1 keeps each manifest alias and pin distinct.
        assert_eq!(terminal_session.attached().count(), 2);
        assert_eq!(
            terminal_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_base_commit
        );
        assert_eq!(
            terminal_session
                .database("backup")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            mirror_commit
        );
        assert_ne!(archive_base_commit, mirror_commit);
        assert!(!terminal_session.units_structurally_equivalent(
            "archive",
            "meter",
            "mirror",
            "meter"
        ));
        assert!(terminal_session.units_structurally_equivalent(
            "archive",
            "meter",
            "backup",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
    }

    #[test]
    fn nested_pin_closure_keeps_equal_pins_distinct_by_shared_repository_alias() {
        let app_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, archive_base_commit) = repository(archive_source);
        let archive_terminal_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {archive_base_commit}\nmirror {archive_base_commit}\n"),
        );
        let app_historical_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_terminal_commit}\n"),
        );
        let archive_current_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("app {app_historical_commit}\n"),
        );
        let app_current_source = app_source.replace("42", "7");
        write_commit(app_dir.path(), "main.orna", &app_current_source);
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository.clone()),
                ("backup".to_owned(), archive_repository.clone()),
                ("mirror".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_session = resolver.resolve_for_parent(archive).unwrap();
        let historical_app = archive_session.database("app").unwrap().clone();
        let historical_app_session = resolver.resolve_for_parent(historical_app).unwrap();
        let archive_terminal = historical_app_session
            .database("archive")
            .unwrap()
            .clone();
        let terminal_session = resolver.resolve_for_parent(archive_terminal).unwrap();

        // The reference does not define aliases sharing a repository or an
        // object ID. V1 keeps alias identity even when both pins resolve to
        // the same immutable package snapshot.
        assert_eq!(terminal_session.attached().count(), 2);
        let backup = terminal_session.database("backup").unwrap();
        let mirror = terminal_session.database("mirror").unwrap();
        assert_eq!(backup.pin().name(), "backup");
        assert_eq!(mirror.pin().name(), "mirror");
        assert_eq!(backup.pin().commit().as_str(), archive_base_commit);
        assert_eq!(mirror.pin().commit().as_str(), archive_base_commit);
        assert_ne!(backup.pin(), mirror.pin());
        assert!(terminal_session.units_structurally_equivalent(
            "backup",
            "meter",
            "mirror",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_current_commit
        );
    }

    #[test]
    fn nested_pin_closure_resolves_equal_shared_repository_aliases_independently() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);
        let (_leaf_dir, leaf_repository, leaf_commit) = repository(shared_source);
        let shared_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {shared_commit}\nmirror {shared_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("backup".to_owned(), shared_repository.clone()),
                ("mirror".to_owned(), shared_repository),
                ("leaf".to_owned(), leaf_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        assert_eq!(root_session.attached().count(), 2);
        let backup = root_session.database("backup").unwrap().clone();
        let mirror = root_session.database("mirror").unwrap().clone();
        assert_eq!(backup.pin().commit(), mirror.pin().commit());
        assert_ne!(backup.pin(), mirror.pin());
        // ORNA-UNIT-002 requires structural checks across database boundaries
        // but does not define alias-specific outcomes. V1 compares the pinned
        // unit structures, so equal snapshots stay compatible under each name.
        assert!(root_session.units_structurally_equivalent(
            "backup",
            "meter",
            "mirror",
            "meter"
        ));

        let backup_closure = resolver.resolve_for_parent(backup).unwrap();
        let mirror_closure = resolver.resolve_for_parent(mirror).unwrap();

        // The reference fixes exact pins but leaves recursive alias sharing
        // unspecified. V1 resolves each selected alias as its own parent and
        // follows the shared snapshot's child pin in both closures.
        assert_eq!(backup_closure.primary().pin().name(), "backup");
        assert_eq!(mirror_closure.primary().pin().name(), "mirror");
        assert_ne!(
            backup_closure.primary().pin(),
            mirror_closure.primary().pin()
        );
        assert_eq!(backup_closure.attached().count(), 1);
        assert_eq!(mirror_closure.attached().count(), 1);
        let backup_leaf = backup_closure.database("leaf").unwrap();
        let mirror_leaf = mirror_closure.database("leaf").unwrap();
        assert_eq!(backup_leaf.pin(), mirror_leaf.pin());
        assert_eq!(backup_leaf.pin().commit().as_str(), leaf_commit);
        assert!(backup_closure.units_structurally_equivalent(
            "backup",
            "meter",
            "leaf",
            "meter"
        ));
        assert!(mirror_closure.units_structurally_equivalent(
            "mirror",
            "meter",
            "leaf",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("backup")
                .unwrap()
                .pin()
                .commit(),
            root_session.database("mirror").unwrap().pin().commit()
        );
    }

    #[test]
    fn nested_pin_closure_keeps_equal_alias_edges_distinct_through_two_depths() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);
        let (leaf_dir, leaf_repository, _) = repository(shared_source);
        let (_end_dir, end_repository, end_commit) = repository(shared_source);

        let leaf_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("end {end_commit}\n"),
        );
        let shared_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {leaf_commit}\nterminal {leaf_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {shared_commit}\nmirror {shared_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("backup".to_owned(), shared_repository.clone()),
                ("mirror".to_owned(), shared_repository),
                ("archive".to_owned(), leaf_repository.clone()),
                ("terminal".to_owned(), leaf_repository),
                ("end".to_owned(), end_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let backup_closure = resolver
            .resolve_for_parent(root_session.database("backup").unwrap().clone())
            .unwrap();
        let mirror_closure = resolver
            .resolve_for_parent(root_session.database("mirror").unwrap().clone())
            .unwrap();

        // The reference fixes exact pins but does not specify recursive
        // namespace behavior for aliases that share a repository and OID.
        // V1 retains each alias edge as each selected parent is traversed.
        assert_eq!(backup_closure.primary().pin().name(), "backup");
        assert_eq!(mirror_closure.primary().pin().name(), "mirror");
        for closure in [&backup_closure, &mirror_closure] {
            assert_eq!(closure.attached().count(), 2);
            let archive = closure.database("archive").unwrap();
            let terminal = closure.database("terminal").unwrap();
            assert_eq!(archive.pin().commit().as_str(), leaf_commit);
            assert_eq!(terminal.pin().commit().as_str(), leaf_commit);
            assert_ne!(archive.pin(), terminal.pin());
            assert!(closure.units_structurally_equivalent(
                closure.primary().pin().name(),
                "meter",
                "archive",
                "meter"
            ));
        }

        let archive_tail = resolver
            .resolve_for_parent(backup_closure.database("archive").unwrap().clone())
            .unwrap();
        let terminal_tail = resolver
            .resolve_for_parent(mirror_closure.database("terminal").unwrap().clone())
            .unwrap();
        assert_eq!(archive_tail.primary().pin().name(), "archive");
        assert_eq!(terminal_tail.primary().pin().name(), "terminal");
        assert_ne!(archive_tail.primary().pin(), terminal_tail.primary().pin());
        let archive_end = archive_tail.database("end").unwrap();
        let terminal_end = terminal_tail.database("end").unwrap();
        assert_eq!(archive_end.pin(), terminal_end.pin());
        assert_eq!(archive_end.pin().commit().as_str(), end_commit);
        assert!(archive_tail.units_structurally_equivalent(
            "archive",
            "meter",
            "end",
            "meter"
        ));
        assert!(terminal_tail.units_structurally_equivalent(
            "terminal",
            "meter",
            "end",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("backup")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_commit
        );
        assert_eq!(
            root_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_commit
        );
    }

    #[test]
    fn nested_pin_closure_resolves_all_equal_alias_edge_routes_independently() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(shared_source);
        let (leaf_dir, leaf_repository, _) = repository(shared_source);
        let (_end_dir, end_repository, end_commit) = repository(shared_source);

        let leaf_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("end {end_commit}\n"),
        );
        let shared_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {leaf_commit}\nterminal {leaf_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {shared_commit}\nmirror {shared_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("backup".to_owned(), shared_repository.clone()),
                ("mirror".to_owned(), shared_repository),
                ("archive".to_owned(), leaf_repository.clone()),
                ("terminal".to_owned(), leaf_repository),
                ("end".to_owned(), end_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let backup = root_session.database("backup").unwrap();
        let mirror = root_session.database("mirror").unwrap();
        assert_eq!(backup.pin().commit(), mirror.pin().commit());
        assert_ne!(backup.pin(), mirror.pin());

        // The reference fixes exact pins but does not prescribe traversal
        // order for recursive shared-repository aliases. V1 resolves every
        // alias path independently while retaining each manifest edge name.
        let mut route_pins = Vec::new();
        let mut expected_end_pin = None;
        for outer_alias in ["mirror", "backup"] {
            let closure = resolver
                .resolve_for_parent(root_session.database(outer_alias).unwrap().clone())
                .unwrap();
            assert_eq!(closure.primary().pin().name(), outer_alias);

            for child_alias in ["terminal", "archive"] {
                let sibling_alias = if child_alias == "archive" {
                    "terminal"
                } else {
                    "archive"
                };
                let child = closure.database(child_alias).unwrap();
                let sibling = closure.database(sibling_alias).unwrap();
                assert_eq!(child.pin().commit().as_str(), leaf_commit);
                assert_eq!(sibling.pin().commit().as_str(), leaf_commit);
                assert_ne!(child.pin(), sibling.pin());

                let tail = resolver.resolve_for_parent(child.clone()).unwrap();
                assert_eq!(tail.primary().pin().name(), child_alias);
                assert_eq!(tail.primary().pin().commit().as_str(), leaf_commit);
                let end = tail.database("end").unwrap();
                assert_eq!(end.pin().commit().as_str(), end_commit);
                if let Some(expected) = &expected_end_pin {
                    assert_eq!(end.pin(), expected);
                } else {
                    expected_end_pin = Some(end.pin().clone());
                }
                assert!(tail.units_structurally_equivalent(
                    child_alias,
                    "meter",
                    "end",
                    "meter"
                ));
                route_pins.push(tail.primary().pin().clone());
            }
        }

        assert_eq!(route_pins[0], route_pins[2]);
        assert_eq!(route_pins[1], route_pins[3]);
        assert_ne!(route_pins[0], route_pins[1]);
        assert_eq!(route_pins[0].commit(), route_pins[1].commit());
        assert_eq!(
            root_session
                .database("backup")
                .unwrap()
                .pin()
                .commit(),
            root_session.database("mirror").unwrap().pin().commit()
        );
    }

    #[test]
    fn nested_pin_cycle_keeps_equal_alias_routes_distinct_to_historical_terminals() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let package_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, _) = repository(package_source);
        let (leaf_dir, leaf_repository, leaf_base_commit) = repository(package_source);

        let shared_historical_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {leaf_base_commit}\nterminal {leaf_base_commit}\n"),
        );
        let leaf_current_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {shared_historical_commit}\nmirror {shared_historical_commit}\n"),
        );
        let shared_current_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {leaf_current_commit}\nterminal {leaf_current_commit}\n"),
        );
        let app_current_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {shared_current_commit}\nmirror {shared_current_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_current_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("backup".to_owned(), shared_repository.clone()),
                ("mirror".to_owned(), shared_repository),
                ("archive".to_owned(), leaf_repository.clone()),
                ("terminal".to_owned(), leaf_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let backup = root_session.database("backup").unwrap();
        let mirror = root_session.database("mirror").unwrap();
        assert_eq!(backup.pin().commit(), mirror.pin().commit());
        assert_ne!(backup.pin(), mirror.pin());
        assert!(root_session.units_structurally_equivalent(
            "backup",
            "meter",
            "mirror",
            "meter"
        ));

        // The reference fixes each parent's exact pins but does not specify
        // recursive equal-alias cycle behavior. V1 follows one manifest edge
        // at a time and preserves the selected alias through historical pins.
        let mut terminal_pins = Vec::new();
        for (outer_alias, child_alias, repeated_alias, terminal_alias) in [
            ("backup", "archive", "mirror", "terminal"),
            ("mirror", "terminal", "backup", "archive"),
        ] {
            let current_closure = resolver
                .resolve_for_parent(root_session.database(outer_alias).unwrap().clone())
                .unwrap();
            assert_eq!(current_closure.primary().pin().name(), outer_alias);
            let child = current_closure.database(child_alias).unwrap();
            let sibling_alias = if child_alias == "archive" {
                "terminal"
            } else {
                "archive"
            };
            let sibling = current_closure.database(sibling_alias).unwrap();
            assert_eq!(child.pin().commit().as_str(), leaf_current_commit);
            assert_eq!(sibling.pin().commit().as_str(), leaf_current_commit);
            assert_ne!(child.pin(), sibling.pin());
            assert!(current_closure.units_structurally_equivalent(
                child_alias,
                "meter",
                sibling_alias,
                "meter"
            ));

            let leaf_closure = resolver.resolve_for_parent(child.clone()).unwrap();
            assert_eq!(leaf_closure.primary().pin().name(), child_alias);
            assert_eq!(leaf_closure.primary().pin().commit().as_str(), leaf_current_commit);
            let historical_shared = leaf_closure.database(repeated_alias).unwrap();
            let repeated_sibling_alias = if repeated_alias == "backup" {
                "mirror"
            } else {
                "backup"
            };
            let repeated_sibling = leaf_closure
                .database(repeated_sibling_alias)
                .unwrap();
            assert_eq!(
                historical_shared.pin().commit().as_str(),
                shared_historical_commit
            );
            assert_eq!(
                repeated_sibling.pin().commit(),
                historical_shared.pin().commit()
            );
            assert_ne!(historical_shared.pin(), repeated_sibling.pin());
            assert!(leaf_closure.units_structurally_equivalent(
                "backup",
                "meter",
                "mirror",
                "meter"
            ));

            let historical_closure =
                resolver.resolve_for_parent(historical_shared.clone()).unwrap();
            assert_eq!(historical_closure.primary().pin().name(), repeated_alias);
            assert_eq!(
                historical_closure.primary().pin().commit().as_str(),
                shared_historical_commit
            );
            let terminal = historical_closure
                .database(terminal_alias)
                .unwrap()
                .clone();
            let terminal_sibling_alias = if terminal_alias == "archive" {
                "terminal"
            } else {
                "archive"
            };
            let terminal_sibling = historical_closure
                .database(terminal_sibling_alias)
                .unwrap();
            assert_eq!(terminal.pin().commit().as_str(), leaf_base_commit);
            assert_eq!(terminal_sibling.pin().commit(), terminal.pin().commit());
            assert_ne!(terminal_sibling.pin(), terminal.pin());
            assert!(historical_closure.units_structurally_equivalent(
                "archive",
                "meter",
                "terminal",
                "meter"
            ));

            let closed = resolver.resolve_for_parent(terminal).unwrap();
            assert_eq!(closed.primary().pin().name(), terminal_alias);
            assert_eq!(closed.primary().pin().commit().as_str(), leaf_base_commit);
            assert_eq!(closed.attached().count(), 0);
            terminal_pins.push(closed.primary().pin().clone());
        }

        assert_ne!(terminal_pins[0], terminal_pins[1]);
        assert_eq!(terminal_pins[0].commit(), terminal_pins[1].commit());
        assert_eq!(
            root_session
                .database("backup")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_current_commit
        );
        assert_eq!(
            root_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_current_commit
        );
    }

    #[test]
    fn nested_pin_closure_keeps_equal_units_across_distinct_alias_commits() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let package_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (package_dir, package_repository, _) = repository(package_source);
        let (_leaf_dir, leaf_repository, leaf_commit) = repository(package_source);

        let historical_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_commit}\n# historical alias pin\n"),
        );
        let current_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_commit}\n# current alias pin\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {current_commit}\nmirror {historical_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("backup".to_owned(), package_repository.clone()),
                ("mirror".to_owned(), package_repository),
                ("leaf".to_owned(), leaf_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let backup = root_session.database("backup").unwrap();
        let mirror = root_session.database("mirror").unwrap();
        assert_ne!(backup.pin().commit(), mirror.pin().commit());
        assert_ne!(backup.pin(), mirror.pin());
        // The reference requires exact package pins and structural unit
        // checks, but does not define alias interaction across revisions. V1
        // compares loaded unit structures independently of alias and commit.
        assert!(root_session.units_structurally_equivalent(
            "backup",
            "meter",
            "mirror",
            "meter"
        ));

        let backup_closure = resolver.resolve_for_parent(backup.clone()).unwrap();
        let mirror_closure = resolver.resolve_for_parent(mirror.clone()).unwrap();
        assert_eq!(backup_closure.primary().pin().name(), "backup");
        assert_eq!(mirror_closure.primary().pin().name(), "mirror");
        assert_ne!(
            backup_closure.primary().pin().commit(),
            mirror_closure.primary().pin().commit()
        );
        let backup_leaf = backup_closure.database("leaf").unwrap();
        let mirror_leaf = mirror_closure.database("leaf").unwrap();
        assert_eq!(backup_leaf.pin(), mirror_leaf.pin());
        assert_eq!(backup_leaf.pin().commit().as_str(), leaf_commit);
        assert!(backup_closure.units_structurally_equivalent(
            "backup",
            "meter",
            "leaf",
            "meter"
        ));
        assert!(mirror_closure.units_structurally_equivalent(
            "mirror",
            "meter",
            "leaf",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("backup")
                .unwrap()
                .pin()
                .commit(),
            backup.pin().commit()
        );
        assert_eq!(
            root_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit(),
            mirror.pin().commit()
        );
    }

    #[test]
    fn nested_pin_closure_uses_each_alias_revision_for_unit_equivalence() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let compatible_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let incompatible_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (package_dir, package_repository, _) = repository(compatible_source);
        let (_leaf_dir, leaf_repository, leaf_commit) = repository(compatible_source);

        write_commit(package_dir.path(), "main.orna", incompatible_source);
        let mirror_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_commit}\n# incompatible mirror revision\n"),
        );
        write_commit(package_dir.path(), "main.orna", compatible_source);
        let backup_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_commit}\n# compatible backup revision\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("backup {backup_commit}\nmirror {mirror_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("backup".to_owned(), package_repository.clone()),
                ("mirror".to_owned(), package_repository),
                ("leaf".to_owned(), leaf_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let backup = root_session.database("backup").unwrap();
        let mirror = root_session.database("mirror").unwrap();
        assert_eq!(backup.pin().commit().as_str(), backup_commit);
        assert_eq!(mirror.pin().commit().as_str(), mirror_commit);
        assert_ne!(backup.pin().commit(), mirror.pin().commit());
        assert!(!root_session.units_structurally_equivalent(
            "backup",
            "meter",
            "mirror",
            "meter"
        ));

        // ORNA-PACKAGE-002 fixes each selected revision, and ORNA-UNIT-002
        // checks structural compatibility. The reference does not specify
        // shared-repository aliases in nested closures; V1 checks each pinned
        // alias revision against the same exact leaf independently.
        let backup_closure = resolver.resolve_for_parent(backup.clone()).unwrap();
        let mirror_closure = resolver.resolve_for_parent(mirror.clone()).unwrap();
        assert_eq!(backup_closure.primary().pin().name(), "backup");
        assert_eq!(mirror_closure.primary().pin().name(), "mirror");
        assert_eq!(
            backup_closure.primary().pin().commit().as_str(),
            backup_commit
        );
        assert_eq!(
            mirror_closure.primary().pin().commit().as_str(),
            mirror_commit
        );
        let backup_leaf = backup_closure.database("leaf").unwrap();
        let mirror_leaf = mirror_closure.database("leaf").unwrap();
        assert_eq!(backup_leaf.pin(), mirror_leaf.pin());
        assert_eq!(backup_leaf.pin().commit().as_str(), leaf_commit);
        assert!(backup_closure.units_structurally_equivalent(
            "backup",
            "meter",
            "leaf",
            "meter"
        ));
        assert!(!mirror_closure.units_structurally_equivalent(
            "mirror",
            "meter",
            "leaf",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("backup")
                .unwrap()
                .pin()
                .commit(),
            backup.pin().commit()
        );
        assert_eq!(
            root_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit(),
            mirror.pin().commit()
        );
    }

    #[test]
    fn nested_pin_closure_resolves_revision_specific_leaf_pins_for_shared_aliases() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let compatible_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let incompatible_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (package_dir, package_repository, _) = repository(compatible_source);
        let (leaf_dir, leaf_repository, leaf_current_commit) = repository(compatible_source);

        let leaf_historical_commit =
            write_commit(leaf_dir.path(), "main.orna", incompatible_source);
        write_commit(package_dir.path(), "main.orna", incompatible_source);
        let package_historical_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_historical_commit}\n"),
        );
        write_commit(package_dir.path(), "main.orna", compatible_source);
        let package_current_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {package_current_commit}\nmirror {package_historical_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), package_repository.clone()),
                ("mirror".to_owned(), package_repository),
                ("leaf".to_owned(), leaf_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        assert_eq!(root_session.attached().count(), 2);
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            package_current_commit
        );
        assert_eq!(
            root_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            package_historical_commit
        );
        assert!(!root_session.units_structurally_equivalent(
            "archive",
            "meter",
            "mirror",
            "meter"
        ));

        // The reference fixes exact parent pins but does not define aliases
        // sharing one repository. V1 resolves each nested OID from its
        // selected revision, even when another alias points to repository HEAD.
        let archive_closure = resolver
            .resolve_for_parent(root_session.database("archive").unwrap().clone())
            .unwrap();
        let mirror_closure = resolver
            .resolve_for_parent(root_session.database("mirror").unwrap().clone())
            .unwrap();
        let archive_leaf = archive_closure.database("leaf").unwrap();
        let mirror_leaf = mirror_closure.database("leaf").unwrap();
        assert_eq!(archive_leaf.pin().commit().as_str(), leaf_current_commit);
        assert_eq!(
            mirror_leaf.pin().commit().as_str(),
            leaf_historical_commit
        );
        assert_ne!(archive_leaf.pin().commit(), mirror_leaf.pin().commit());
        assert_eq!(
            git(leaf_dir.path(), &["rev-parse", "HEAD"]),
            leaf_historical_commit
        );
        assert!(archive_closure.units_structurally_equivalent(
            "archive",
            "meter",
            "leaf",
            "meter"
        ));
        assert!(mirror_closure.units_structurally_equivalent(
            "mirror",
            "meter",
            "leaf",
            "meter"
        ));
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            package_current_commit
        );
        assert_eq!(
            root_session
                .database("mirror")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            package_historical_commit
        );
    }

    #[test]
    fn nested_pin_closure_keeps_revision_specific_pins_through_terminal_edges() {
        let compatible_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let incompatible_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(incompatible_source);
        let (package_dir, package_repository, _) = repository(compatible_source);
        let (leaf_dir, leaf_repository, _) = repository(compatible_source);
        let (terminal_dir, terminal_repository, terminal_current_commit) =
            repository(compatible_source);

        let terminal_historical_commit =
            write_commit(terminal_dir.path(), "main.orna", incompatible_source);
        let leaf_current_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("terminal {terminal_current_commit}\n"),
        );
        write_commit(leaf_dir.path(), "main.orna", incompatible_source);
        let leaf_historical_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("terminal {terminal_historical_commit}\n"),
        );
        let package_current_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_current_commit}\n"),
        );
        write_commit(package_dir.path(), "main.orna", incompatible_source);
        let package_historical_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_historical_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {package_current_commit}\nmirror {package_historical_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), package_repository.clone()),
                ("mirror".to_owned(), package_repository),
                ("leaf".to_owned(), leaf_repository),
                ("terminal".to_owned(), terminal_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive_closure = resolver
            .resolve_for_parent(root_session.database("archive").unwrap().clone())
            .unwrap();
        let mirror_closure = resolver
            .resolve_for_parent(root_session.database("mirror").unwrap().clone())
            .unwrap();
        let archive_leaf = archive_closure.database("leaf").unwrap().clone();
        let mirror_leaf = mirror_closure.database("leaf").unwrap().clone();

        // Exact revision pinning is specified; recursive alias closure across
        // shared repositories is not. V1 follows each selected snapshot's
        // manifest independently at every depth, including the terminal edge.
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            package_current_commit
        );
        assert_eq!(
            root_session.database("mirror").unwrap().pin().commit().as_str(),
            package_historical_commit
        );
        assert_eq!(archive_leaf.pin().commit().as_str(), leaf_current_commit);
        assert_eq!(mirror_leaf.pin().commit().as_str(), leaf_historical_commit);
        assert_ne!(archive_leaf.pin().commit(), mirror_leaf.pin().commit());
        assert_eq!(
            git(package_dir.path(), &["rev-parse", "HEAD"]),
            package_historical_commit
        );
        assert_eq!(
            git(leaf_dir.path(), &["rev-parse", "HEAD"]),
            leaf_historical_commit
        );

        let archive_terminal_closure = resolver.resolve_for_parent(archive_leaf).unwrap();
        let mirror_terminal_closure = resolver.resolve_for_parent(mirror_leaf).unwrap();
        let archive_terminal = archive_terminal_closure.database("terminal").unwrap();
        let mirror_terminal = mirror_terminal_closure.database("terminal").unwrap();
        assert_eq!(
            archive_terminal_closure.primary().pin().commit().as_str(),
            leaf_current_commit
        );
        assert_eq!(
            mirror_terminal_closure.primary().pin().commit().as_str(),
            leaf_historical_commit
        );
        assert_eq!(
            archive_terminal.pin().commit().as_str(),
            terminal_current_commit
        );
        assert_eq!(
            mirror_terminal.pin().commit().as_str(),
            terminal_historical_commit
        );
        assert_ne!(archive_terminal.pin().commit(), mirror_terminal.pin().commit());
        assert_eq!(
            git(terminal_dir.path(), &["rev-parse", "HEAD"]),
            terminal_historical_commit
        );
        assert!(archive_closure.units_structurally_equivalent(
            "archive", "meter", "leaf", "meter"
        ));
        assert!(mirror_closure.units_structurally_equivalent(
            "mirror", "meter", "leaf", "meter"
        ));
        assert!(archive_terminal_closure.units_structurally_equivalent(
            "leaf", "meter", "terminal", "meter"
        ));
        assert!(mirror_terminal_closure.units_structurally_equivalent(
            "leaf", "meter", "terminal", "meter"
        ));
        assert_eq!(
            archive_closure.database("leaf").unwrap().pin().commit().as_str(),
            leaf_current_commit
        );
        assert_eq!(
            mirror_closure.database("leaf").unwrap().pin().commit().as_str(),
            leaf_historical_commit
        );
    }

    #[test]
    fn nested_pin_terminal_closure_preserves_swapped_equal_alias_edges() {
        let compatible_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let incompatible_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(incompatible_source);
        let (package_dir, package_repository, _) = repository(compatible_source);
        let (leaf_dir, leaf_repository, _) = repository(compatible_source);
        let (end_dir, end_repository, end_current_commit) = repository(compatible_source);

        let end_historical_commit = write_commit(end_dir.path(), "main.orna", incompatible_source);
        let leaf_current_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("end {end_current_commit}\n"),
        );
        write_commit(leaf_dir.path(), "main.orna", incompatible_source);
        let leaf_historical_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("end {end_historical_commit}\n"),
        );
        let package_current_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_current_commit}\nterminal {leaf_historical_commit}\n"),
        );
        write_commit(package_dir.path(), "main.orna", incompatible_source);
        let package_historical_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_historical_commit}\nterminal {leaf_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {package_current_commit}\nmirror {package_historical_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), package_repository.clone()),
                ("mirror".to_owned(), package_repository),
                ("leaf".to_owned(), leaf_repository.clone()),
                ("terminal".to_owned(), leaf_repository),
                ("end".to_owned(), end_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive_closure = resolver
            .resolve_for_parent(root_session.database("archive").unwrap().clone())
            .unwrap();
        let mirror_closure = resolver
            .resolve_for_parent(root_session.database("mirror").unwrap().clone())
            .unwrap();
        let archive_leaf = archive_closure.database("leaf").unwrap();
        let archive_terminal = archive_closure.database("terminal").unwrap();
        let mirror_leaf = mirror_closure.database("leaf").unwrap();
        let mirror_terminal = mirror_closure.database("terminal").unwrap();

        // The reference fixes each exact pin but leaves recursive closure
        // behavior for shared-repository aliases unspecified. V1 retains every
        // alias edge and reads its next pins from that edge's selected revision.
        assert_eq!(
            archive_closure.primary().pin().commit().as_str(),
            package_current_commit
        );
        assert_eq!(
            mirror_closure.primary().pin().commit().as_str(),
            package_historical_commit
        );
        assert_eq!(archive_leaf.pin().commit().as_str(), leaf_current_commit);
        assert_eq!(archive_terminal.pin().commit().as_str(), leaf_historical_commit);
        assert_eq!(mirror_leaf.pin().commit().as_str(), leaf_historical_commit);
        assert_eq!(mirror_terminal.pin().commit().as_str(), leaf_current_commit);
        assert_eq!(archive_leaf.pin().commit(), mirror_terminal.pin().commit());
        assert_ne!(archive_leaf.pin(), mirror_terminal.pin());
        assert_eq!(archive_terminal.pin().commit(), mirror_leaf.pin().commit());
        assert_ne!(archive_terminal.pin(), mirror_leaf.pin());

        for (closure, alias, expected_leaf, expected_end) in [
            (
                &archive_closure,
                "leaf",
                leaf_current_commit.as_str(),
                end_current_commit.as_str(),
            ),
            (
                &archive_closure,
                "terminal",
                leaf_historical_commit.as_str(),
                end_historical_commit.as_str(),
            ),
            (
                &mirror_closure,
                "leaf",
                leaf_historical_commit.as_str(),
                end_historical_commit.as_str(),
            ),
            (
                &mirror_closure,
                "terminal",
                leaf_current_commit.as_str(),
                end_current_commit.as_str(),
            ),
        ] {
            let tail = resolver
                .resolve_for_parent(closure.database(alias).unwrap().clone())
                .unwrap();
            assert_eq!(tail.primary().pin().name(), alias);
            assert_eq!(tail.primary().pin().commit().as_str(), expected_leaf);
            assert_eq!(tail.database("end").unwrap().pin().commit().as_str(), expected_end);
            assert!(tail.units_structurally_equivalent(alias, "meter", "end", "meter"));
        }

        assert_eq!(git(package_dir.path(), &["rev-parse", "HEAD"]), package_historical_commit);
        assert_eq!(git(leaf_dir.path(), &["rev-parse", "HEAD"]), leaf_historical_commit);
        assert_eq!(git(end_dir.path(), &["rev-parse", "HEAD"]), end_historical_commit);
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            package_current_commit
        );
        assert_eq!(
            root_session.database("mirror").unwrap().pin().commit().as_str(),
            package_historical_commit
        );
    }

    #[test]
    fn nested_pin_swapped_alias_closure_composes_terminal_pin_swaps() {
        let compatible_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let incompatible_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(incompatible_source);
        let (package_dir, package_repository, _) = repository(compatible_source);
        let (leaf_dir, leaf_repository, _) = repository(compatible_source);
        let (end_dir, end_repository, end_current_commit) = repository(compatible_source);

        let end_historical_commit = write_commit(end_dir.path(), "main.orna", incompatible_source);
        let leaf_current_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("end {end_current_commit}\ntail {end_historical_commit}\n"),
        );
        write_commit(leaf_dir.path(), "main.orna", incompatible_source);
        let leaf_historical_commit = write_commit(
            leaf_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("end {end_historical_commit}\ntail {end_current_commit}\n"),
        );
        let package_current_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_current_commit}\nterminal {leaf_historical_commit}\n"),
        );
        write_commit(package_dir.path(), "main.orna", incompatible_source);
        let package_historical_commit = write_commit(
            package_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("leaf {leaf_historical_commit}\nterminal {leaf_current_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {package_current_commit}\nmirror {package_historical_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), package_repository.clone()),
                ("mirror".to_owned(), package_repository),
                ("leaf".to_owned(), leaf_repository.clone()),
                ("terminal".to_owned(), leaf_repository),
                ("end".to_owned(), end_repository.clone()),
                ("tail".to_owned(), end_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive_closure = resolver
            .resolve_for_parent(root_session.database("archive").unwrap().clone())
            .unwrap();
        let mirror_closure = resolver
            .resolve_for_parent(root_session.database("mirror").unwrap().clone())
            .unwrap();

        // The reference fixes exact pins but leaves recursive shared-alias
        // traversal unspecified. V1 applies each selected snapshot's manifest
        // edge by edge while retaining alias identity at both closure levels.
        assert_eq!(
            archive_closure.primary().pin().commit().as_str(),
            package_current_commit
        );
        assert_eq!(
            mirror_closure.primary().pin().commit().as_str(),
            package_historical_commit
        );
        assert_eq!(
            archive_closure.database("leaf").unwrap().pin().commit().as_str(),
            leaf_current_commit
        );
        assert_eq!(
            archive_closure
                .database("terminal")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            leaf_historical_commit
        );
        assert_eq!(
            mirror_closure.database("leaf").unwrap().pin().commit().as_str(),
            leaf_historical_commit
        );
        assert_eq!(
            mirror_closure
                .database("terminal")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            leaf_current_commit
        );
        assert_eq!(
            archive_closure.database("leaf").unwrap().pin().commit(),
            mirror_closure
                .database("terminal")
                .unwrap()
                .pin()
                .commit()
        );
        assert_ne!(
            archive_closure.database("leaf").unwrap().pin(),
            mirror_closure.database("terminal").unwrap().pin()
        );

        let routes = [
            (
                &archive_closure,
                "leaf",
                leaf_current_commit.as_str(),
                end_current_commit.as_str(),
                end_historical_commit.as_str(),
            ),
            (
                &archive_closure,
                "terminal",
                leaf_historical_commit.as_str(),
                end_historical_commit.as_str(),
                end_current_commit.as_str(),
            ),
            (
                &mirror_closure,
                "leaf",
                leaf_historical_commit.as_str(),
                end_historical_commit.as_str(),
                end_current_commit.as_str(),
            ),
            (
                &mirror_closure,
                "terminal",
                leaf_current_commit.as_str(),
                end_current_commit.as_str(),
                end_historical_commit.as_str(),
            ),
        ];
        let mut leaf_closures = Vec::new();
        for (closure, alias, expected_leaf, expected_end, expected_tail) in routes {
            let leaf_closure = resolver
                .resolve_for_parent(closure.database(alias).unwrap().clone())
                .unwrap();
            assert_eq!(leaf_closure.primary().pin().name(), alias);
            assert_eq!(
                leaf_closure.primary().pin().commit().as_str(),
                expected_leaf
            );
            let end = leaf_closure.database("end").unwrap();
            let tail = leaf_closure.database("tail").unwrap();
            assert_eq!(end.pin().commit().as_str(), expected_end);
            assert_eq!(tail.pin().commit().as_str(), expected_tail);
            assert_ne!(end.pin(), tail.pin());
            leaf_closures.push(leaf_closure);
        }

        for (closure, alias, expected_commit) in [
            (&leaf_closures[0], "end", end_current_commit.as_str()),
            (&leaf_closures[0], "tail", end_historical_commit.as_str()),
            (&leaf_closures[1], "end", end_historical_commit.as_str()),
            (&leaf_closures[1], "tail", end_current_commit.as_str()),
        ] {
            let terminal = resolver
                .resolve_for_parent(closure.database(alias).unwrap().clone())
                .unwrap();
            assert_eq!(terminal.primary().pin().name(), alias);
            assert_eq!(terminal.primary().pin().commit().as_str(), expected_commit);
            assert_eq!(terminal.attached().count(), 0);
        }

        assert_eq!(git(package_dir.path(), &["rev-parse", "HEAD"]), package_historical_commit);
        assert_eq!(git(leaf_dir.path(), &["rev-parse", "HEAD"]), leaf_historical_commit);
        assert_eq!(git(end_dir.path(), &["rev-parse", "HEAD"]), end_historical_commit);
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            package_current_commit
        );
        assert_eq!(
            root_session.database("mirror").unwrap().pin().commit().as_str(),
            package_historical_commit
        );
    }

    #[test]
    fn nested_pin_closure_rejects_alias_edge_that_reuses_primary_name() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, archive_base_commit) = repository(archive_source);

        let archive_terminal_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_base_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_terminal_commit}\n"),
        );
        assert!(archive_repository
            .resolve_snapshot(&archive_base_commit)
            .is_ok());

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        assert_eq!(archive.pin().commit().as_str(), archive_terminal_commit);

        // The reference fixes the nested OID but does not define a child edge
        // that reuses its primary alias. V1 rejects it to preserve one binding
        // for that alias in each session and leaves the parent's pin unchanged.
        assert!(matches!(
            resolver.resolve_for_parent(archive),
            Err(AttachmentError::DuplicateAttachment)
        ));
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            archive_terminal_commit
        );
    }

    #[test]
    fn nested_pin_primary_alias_collision_does_not_block_sibling_closure() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let archive_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let mirror_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (archive_dir, archive_repository, archive_base_commit) = repository(archive_source);
        let (_mirror_dir, mirror_repository, mirror_commit) = repository(mirror_source);

        let archive_closure_commit = write_commit(
            archive_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_base_commit}\nmirror {mirror_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_closure_commit}\nmirror {mirror_commit}\n"),
        );
        assert!(archive_repository
            .resolve_snapshot(&archive_base_commit)
            .is_ok());
        assert!(mirror_repository.resolve_snapshot(&mirror_commit).is_ok());

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), archive_repository),
                ("mirror".to_owned(), mirror_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        assert_eq!(root_session.attached().count(), 2);
        let archive = root_session.database("archive").unwrap().clone();
        let mirror = root_session.database("mirror").unwrap().clone();
        assert_eq!(archive.pin().commit().as_str(), archive_closure_commit);
        assert_eq!(mirror.pin().commit().as_str(), mirror_commit);

        // The reference fixes exact pins but leaves recursive name-collision
        // behavior unspecified. V1 rejects the self-alias closure atomically;
        // the valid sibling alias still resolves in its own parent session.
        assert!(matches!(
            resolver.resolve_for_parent(archive),
            Err(AttachmentError::DuplicateAttachment)
        ));
        let mirror_closure = resolver.resolve_for_parent(mirror).unwrap();
        assert_eq!(mirror_closure.primary().pin().name(), "mirror");
        assert_eq!(mirror_closure.primary().pin().commit().as_str(), mirror_commit);
        assert_eq!(mirror_closure.attached().count(), 0);
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            archive_closure_commit
        );
        assert_eq!(
            root_session.database("mirror").unwrap().pin().commit().as_str(),
            mirror_commit
        );
    }

    #[test]
    fn nested_pin_shared_repository_sibling_closure_survives_primary_alias_collision() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let shared_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_base_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_closure_commit}\nmirror {shared_closure_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("mirror".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let mirror = root_session.database("mirror").unwrap().clone();
        assert_eq!(archive.pin().commit(), mirror.pin().commit());
        assert_eq!(archive.pin().commit().as_str(), shared_closure_commit);
        assert_ne!(archive.pin(), mirror.pin());

        // The reference fixes the shared exact snapshot but does not define
        // nested alias collisions. V1 rejects the alias matching the primary,
        // while that same manifest edge is valid under the sibling's name.
        assert!(matches!(
            resolver.resolve_for_parent(archive),
            Err(AttachmentError::DuplicateAttachment)
        ));
        let mirror_closure = resolver.resolve_for_parent(mirror).unwrap();
        assert_eq!(mirror_closure.primary().pin().name(), "mirror");
        assert_eq!(
            mirror_closure.primary().pin().commit().as_str(),
            shared_closure_commit
        );
        assert_eq!(mirror_closure.attached().count(), 1);
        let archive_child = mirror_closure.database("archive").unwrap();
        assert_eq!(archive_child.pin().commit().as_str(), shared_base_commit);
        assert_ne!(archive_child.pin(), mirror_closure.primary().pin());
        assert!(mirror_closure.units_structurally_equivalent(
            "mirror", "meter", "archive", "meter"
        ));
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            shared_closure_commit
        );
        assert_eq!(
            root_session.database("mirror").unwrap().pin().commit().as_str(),
            shared_closure_commit
        );
    }

    #[test]
    fn nested_pin_prefix_sibling_closure_survives_exact_primary_alias_collision() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let shared_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_base_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_closure_commit}\narchive_copy {shared_closure_commit}\n"),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_copy = root_session.database("archive_copy").unwrap().clone();
        assert_eq!(archive.pin().commit(), archive_copy.pin().commit());
        assert_eq!(archive.pin().commit().as_str(), shared_closure_commit);
        assert_ne!(archive.pin(), archive_copy.pin());

        // The reference fixes the shared exact snapshot but leaves name
        // collision behavior open. V1 compares complete aliases: `archive`
        // collides with that primary while `archive_copy` remains independent.
        assert!(matches!(
            resolver.resolve_for_parent(archive),
            Err(AttachmentError::DuplicateAttachment)
        ));
        let copy_closure = resolver.resolve_for_parent(archive_copy).unwrap();
        assert_eq!(copy_closure.primary().pin().name(), "archive_copy");
        assert_eq!(
            copy_closure.primary().pin().commit().as_str(),
            shared_closure_commit
        );
        assert_eq!(copy_closure.attached().count(), 1);
        let archive_child = copy_closure.database("archive").unwrap();
        assert_eq!(archive_child.pin().commit().as_str(), shared_base_commit);
        assert_ne!(archive_child.pin(), copy_closure.primary().pin());
        assert!(copy_closure.units_structurally_equivalent(
            "archive_copy", "meter", "archive", "meter"
        ));
        assert_eq!(
            root_session.database("archive").unwrap().pin().commit().as_str(),
            shared_closure_commit
        );
        assert_eq!(
            root_session
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_closure_commit
        );
    }

    #[test]
    fn nested_pin_prefix_chain_keeps_sibling_closures_independent() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let shared_closure_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_base_commit}\narchive_copy {shared_base_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {shared_closure_commit}\narchive_copy {shared_closure_commit}\narchive_copy_archive {shared_closure_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive = root_session.database("archive").unwrap().clone();
        let archive_copy = root_session.database("archive_copy").unwrap().clone();
        let longest_sibling = root_session
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        for sibling in [&archive, &archive_copy, &longest_sibling] {
            assert_eq!(sibling.pin().commit().as_str(), shared_closure_commit);
        }
        assert_ne!(archive.pin(), archive_copy.pin());
        assert_ne!(archive_copy.pin(), longest_sibling.pin());

        // The reference fixes the OIDs but does not prescribe prefix behavior
        // for nested aliases. V1 collides only on the exact primary name; two
        // failed sibling closures do not prevent the longer alias from owning
        // independent archive and archive_copy child edges.
        assert!(matches!(
            resolver.resolve_for_parent(archive),
            Err(AttachmentError::DuplicateAttachment)
        ));
        assert!(matches!(
            resolver.resolve_for_parent(archive_copy),
            Err(AttachmentError::DuplicateAttachment)
        ));
        let mut longest_closure = resolver.resolve_for_parent(longest_sibling).unwrap();
        assert_eq!(
            longest_closure.primary().pin().name(),
            "archive_copy_archive"
        );
        assert_eq!(longest_closure.attached().count(), 2);
        let archive_child = longest_closure.database("archive").unwrap();
        let archive_copy_child = longest_closure.database("archive_copy").unwrap();
        assert_eq!(archive_child.pin().commit().as_str(), shared_base_commit);
        assert_eq!(archive_copy_child.pin().commit().as_str(), shared_base_commit);
        assert_ne!(archive_child.pin(), archive_copy_child.pin());
        assert!(longest_closure.units_structurally_equivalent(
            "archive_copy_archive",
            "meter",
            "archive",
            "meter"
        ));

        longest_closure.detach_database("archive").unwrap();
        assert!(longest_closure.database("archive").is_none());
        assert_eq!(
            longest_closure
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_base_commit
        );
        for sibling_name in ["archive", "archive_copy", "archive_copy_archive"] {
            assert_eq!(
                root_session
                    .database(sibling_name)
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                shared_closure_commit
            );
        }
    }

    #[test]
    fn nested_pin_prefix_sibling_revisions_keep_closures_isolated() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive_copy {shared_base_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );
        let archive_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {shared_base_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );
        let longest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_base_commit}\narchive_copy {shared_base_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_commit}\narchive_copy {archive_copy_commit}\narchive_copy_archive {longest_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let closure_specs = [
            (
                "archive",
                archive_commit.as_str(),
                &["archive_copy", "archive_copy_archive"][..],
            ),
            (
                "archive_copy",
                archive_copy_commit.as_str(),
                &["archive", "archive_copy_archive"][..],
            ),
            (
                "archive_copy_archive",
                longest_commit.as_str(),
                &["archive", "archive_copy"][..],
            ),
        ];
        let mut closures = closure_specs.map(|(parent_name, parent_commit, children)| {
            let parent = root_session.database(parent_name).unwrap().clone();
            assert_eq!(parent.pin().commit().as_str(), parent_commit);
            let closure = resolver.resolve_for_parent(parent).unwrap();
            assert_eq!(closure.attached().count(), children.len());
            for child_name in children {
                assert_eq!(
                    closure.database(child_name).unwrap().pin().commit().as_str(),
                    shared_base_commit
                );
            }
            closure
        });

        // The reference fixes each exact pin but is silent on closure alias
        // overlap across revisions. V1 uses full alias names independently:
        // detaching a shorter prefix preserves its longer neighbor and does
        // not mutate sibling closures or the root snapshot.
        closures[0].detach_database("archive_copy").unwrap();
        assert!(closures[0].database("archive_copy").is_none());
        assert!(closures[0].database("archive_copy_archive").is_some());
        assert!(closures[1].database("archive_copy_archive").is_some());
        assert!(closures[2].database("archive_copy").is_some());
        assert_eq!(
            root_session
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_copy_commit
        );
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            longest_commit
        );
    }

    #[test]
    fn nested_pin_prefix_reverse_detach_keeps_shorter_sibling_routes() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let archive_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {shared_base_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {shared_base_commit}\narchive_copy {archive_copy_commit}\narchive_copy_archive {shared_base_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let archive_copy = root_session.database("archive_copy").unwrap().clone();
        let mut copy_closure = resolver.resolve_for_parent(archive_copy.clone()).unwrap();
        let sibling_closure = resolver.resolve_for_parent(archive_copy).unwrap();
        for closure in [&copy_closure, &sibling_closure] {
            assert_eq!(closure.primary().pin().name(), "archive_copy");
            assert_eq!(closure.attached().count(), 2);
            assert_eq!(
                closure
                    .database("archive")
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                shared_base_commit
            );
            assert_eq!(
                closure
                    .database("archive_copy_archive")
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                shared_base_commit
            );
        }

        // The reference fixes exact pins but is silent on reverse-prefix
        // detach effects. V1 removes only the full longer alias in one closure;
        // its shorter neighbor, a sibling closure, and the root stay pinned.
        copy_closure
            .detach_database("archive_copy_archive")
            .unwrap();
        assert!(copy_closure.database("archive_copy_archive").is_none());
        assert!(copy_closure.database("archive").is_some());
        assert!(sibling_closure.database("archive_copy_archive").is_some());
        assert_eq!(
            root_session
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_copy_commit
        );
        assert_eq!(
            root_session
                .database("archive_copy_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_base_commit
        );
    }

    #[test]
    fn nested_pin_reverse_prefix_edges_keep_distinct_closures_isolated() {
        let app_source = include_str!("../tests/fixtures/attached-incompatible-main.orna");
        let shared_source = include_str!("../tests/fixtures/attached-equivalent-main.orna");
        let (app_dir, app_repository, _) = repository(app_source);
        let (shared_dir, shared_repository, shared_base_commit) = repository(shared_source);

        let archive_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive_copy {shared_base_commit}\n"),
        );
        let archive_copy_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {shared_base_commit}\n"),
        );
        let longest_commit = write_commit(
            shared_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("archive {archive_commit}\narchive_copy {archive_copy_commit}\n"),
        );
        let app_commit = write_commit(
            app_dir.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!(
                "archive {archive_commit}\narchive_copy {archive_copy_commit}\narchive_copy_archive {longest_commit}\n"
            ),
        );

        let loader = ProjectLoader::default();
        let app = PinnedDatabase::resolve(
            "app",
            app_repository.clone(),
            &app_commit,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [
                ("app".to_owned(), app_repository),
                ("archive".to_owned(), shared_repository.clone()),
                ("archive_copy".to_owned(), shared_repository.clone()),
                ("archive_copy_archive".to_owned(), shared_repository),
            ],
            loader,
        )
        .unwrap();

        let root_session = resolver.resolve_for_parent(app).unwrap();
        let longest = root_session
            .database("archive_copy_archive")
            .unwrap()
            .clone();
        let mut longest_closure = resolver.resolve_for_parent(longest).unwrap();
        let archive_child = longest_closure.database("archive").unwrap().clone();
        let archive_copy_child = longest_closure.database("archive_copy").unwrap().clone();
        assert_eq!(archive_child.pin().commit().as_str(), archive_commit);
        assert_eq!(
            archive_copy_child.pin().commit().as_str(),
            archive_copy_commit
        );
        assert_ne!(archive_child.pin(), archive_copy_child.pin());

        let archive_closure = resolver.resolve_for_parent(archive_child).unwrap();
        let archive_copy_closure = resolver.resolve_for_parent(archive_copy_child).unwrap();
        assert_eq!(archive_closure.primary().pin().name(), "archive");
        assert_eq!(archive_copy_closure.primary().pin().name(), "archive_copy");
        assert_eq!(
            archive_closure
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_base_commit
        );
        assert_eq!(
            archive_copy_closure
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            shared_base_commit
        );

        // The reference fixes the revisions but leaves reverse prefix closure
        // routing unspecified. V1 follows each exact edge independently, so
        // detaching one sibling does not retarget its reverse-edge closure.
        longest_closure.detach_database("archive_copy").unwrap();
        assert!(longest_closure.database("archive_copy").is_none());
        assert!(longest_closure.database("archive").is_some());
        assert!(archive_closure.database("archive_copy").is_some());
        assert!(archive_copy_closure.database("archive").is_some());
        assert_eq!(
            root_session
                .database("archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_commit
        );
        assert_eq!(
            root_session
                .database("archive_copy")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_copy_commit
        );
    }

    #[test]
    fn equivalent_units_interoperate_across_attachments_but_name_match_is_not_enough() {
        let primary_source = include_str!("fixtures/attached-primary-main.orna");
        let (_primary_dir, primary_repository, primary_commit) = repository(&primary_source);
        let (_same_dir, same_repository, same_commit) = repository(
            &include_str!("fixtures/attached-package-main.orna")
                .replace("unit meter:", "pub unit meter :")
                .replace("base;", "base ;"),
        );
        let (_different_dir, different_repository, different_commit) = repository(
            &include_str!("fixtures/attached-package-main.orna")
                .replace("meter: Length", "meter: Time"),
        );
        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &primary_commit,
            loader,
        )
        .unwrap();
        let same = PinnedDatabase::resolve(
            "measurements",
            same_repository,
            &same_commit,
            loader,
        )
        .unwrap();
        let different = PinnedDatabase::resolve(
            "archive",
            different_repository,
            &different_commit,
            loader,
        )
        .unwrap();
        let mut session = AttachedDatabaseSession::new(primary).unwrap();
        session.attach_database(same).unwrap();
        session.attach_database(different).unwrap();

        assert!(session.units_structurally_equivalent(
            "app",
            "meter",
            "measurements",
            "meter"
        ));
        assert!(!session.units_structurally_equivalent(
            "app",
            "meter",
            "archive",
            "meter"
        ));
    }

    #[test]
    fn package_manifest_rejects_moving_selectors_and_duplicate_names() {
        let oid = "a".repeat(40);
        assert!(PackagePinManifest::parse(&format!("math {oid}\n")).is_ok());
        assert!(PackagePinManifest::parse("math HEAD\n").is_err());
        assert!(PackagePinManifest::parse(&format!("math {oid}\nmath {oid}\n")).is_err());
        assert!(PackagePinManifest::parse(&format!("sys {oid}\n")).is_err());
    }

    #[test]
    fn std_package_pin_must_match_the_gitlink_when_both_are_committed() {
        let (std_directory, std_repository, first_std_commit) =
            repository(include_str!("fixtures/attached-package-main.orna"));
        let second_std_source = include_str!("fixtures/attached-package-main.orna").replace("42", "43");
        let second_std_commit = write_commit(
            std_directory.path(),
            "main.orna",
            &second_std_source,
        );

        let primary_directory = tempfile::tempdir().unwrap();
        git(primary_directory.path(), &["init", "--quiet"]);
        git(primary_directory.path(), &["config", "user.name", "kierandrewett"]);
        git(
            primary_directory.path(),
            &["config", "user.email", "kieran@drewett.dev"],
        );
        git(primary_directory.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(
            primary_directory.path().join("main.orna"),
            include_str!("fixtures/attached-primary-main.orna"),
        )
        .unwrap();
        fs::create_dir_all(primary_directory.path().join(".orna")).unwrap();
        fs::write(
            primary_directory.path().join(PACKAGE_PIN_MANIFEST_PATH),
            format!("std {first_std_commit}\n"),
        )
        .unwrap();
        git(primary_directory.path(), &["add", "main.orna", ".orna/packages"]);
        let link = format!("160000,{second_std_commit},stdlib/std");
        git(
            primary_directory.path(),
            &["update-index", "--add", "--cacheinfo", &link],
        );
        git(
            primary_directory.path(),
            &["commit", "--quiet", "-m", "conflicting std pins"],
        );
        let primary_repository = Repository::discover(primary_directory.path()).unwrap();
        let parent = git(primary_directory.path(), &["rev-parse", "HEAD"]);
        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository.clone(),
            &parent,
            loader,
        )
        .unwrap();
        let resolver = PackageResolver::new(
            [("std".to_owned(), std_repository.clone())],
            loader,
        )
        .unwrap();
        assert!(matches!(
            resolver.resolve_for_parent(primary),
            Err(AttachmentError::PinUnavailable)
        ));

        write_commit(
            primary_directory.path(),
            PACKAGE_PIN_MANIFEST_PATH,
            &format!("std {second_std_commit}\n"),
        );
        let matching_parent = git(primary_directory.path(), &["rev-parse", "HEAD"]);
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &matching_parent,
            loader,
        )
        .unwrap();
        let session = resolver.resolve_for_parent(primary).unwrap();
        assert_eq!(
            session.standard_snapshot().unwrap().as_str(),
            second_std_commit
        );
    }

    #[test]
    fn relation_sources_compose_rows_from_each_pinned_snapshot() {
        fn committed_database(main: &str, row: &str) -> (TempDir, Repository, String) {
            let directory = tempfile::tempdir().unwrap();
            git(directory.path(), &["init", "--quiet"]);
            git(directory.path(), &["config", "user.name", "kierandrewett"]);
            git(
                directory.path(),
                &["config", "user.email", "kieran@drewett.dev"],
            );
            git(directory.path(), &["config", "commit.gpgsign", "false"]);
            write_commit(directory.path(), "main.orna", main);
            write_commit(
                directory.path(),
                "contacts.orna",
                include_str!("fixtures/attached-read-table.orna"),
            );
            let commit = write_commit(directory.path(), "contacts/Contact/1.orna", row);
            let repository = Repository::discover(directory.path()).unwrap();
            (directory, repository, commit)
        }

        let (_primary_dir, primary_repository, primary_commit) = committed_database(
            include_str!("fixtures/attached-read-primary-main.orna"),
            include_str!("fixtures/attached-read-primary-row.orna"),
        );
        let (_archive_dir, archive_repository, archive_commit) = committed_database(
            include_str!("fixtures/attached-read-package-main.orna"),
            include_str!("fixtures/attached-read-package-row.orna"),
        );
        let loader = ProjectLoader::default();
        let primary = PinnedDatabase::resolve(
            "app",
            primary_repository,
            &primary_commit,
            loader,
        )
        .unwrap();
        let archive = PinnedDatabase::resolve(
            "archive",
            archive_repository,
            &archive_commit,
            loader,
        )
        .unwrap();
        let mut session = AttachedDatabaseSession::new(primary).unwrap();
        session.attach_database(archive).unwrap();

        let sources = session.relation_sources("contacts/Contact");
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].database(), "app");
        assert_eq!(sources[0].commit().as_str(), primary_commit);
        assert!(sources[0].row().source().contains("value: 7"));
        assert_eq!(sources[1].database(), "archive");
        assert_eq!(sources[1].commit().as_str(), archive_commit);
        assert!(sources[1].row().source().contains("value: 42"));
    }
}
