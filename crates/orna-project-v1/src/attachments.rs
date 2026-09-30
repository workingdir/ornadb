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
