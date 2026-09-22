//! Bounded source-project loading for a Git-backed Orna 1.0 worktree.
//!
//! This crate resolves only ordinary source-module imports.  `sys` and `std`
//! remain catalogue dependencies; row loading, execution, and runtime state
//! are deliberately outside this boundary.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt, fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use orna_repository_v1::{
    CommittedTreeEntry, CommittedTreeEntryKind, GitCommitRef, Repository, RepositoryError,
};
use orna_semantic_v1::{Catalogue, ModuleInput, StandardCatalogueError, StandardDependencyProfile};
use orna_syntax_v1::{Declaration, parse_module};
use unicode_normalization::UnicodeNormalization;

mod unicode16;

/// Bounded resource limits applied before source contents are read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectLimits {
    pub max_modules: usize,
    pub max_source_bytes: usize,
    /// Maximum non-administrative directory entries inspected while enforcing
    /// portable repository ownership rules.
    pub max_repository_entries: usize,
}

impl Default for ProjectLimits {
    fn default() -> Self {
        Self {
            max_modules: 256,
            max_source_bytes: 4 * 1024 * 1024,
            max_repository_entries: 4_096,
        }
    }
}

/// One loaded, repository-relative module identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleIdentity {
    logical_path: String,
    namespace: Vec<String>,
}

impl ModuleIdentity {
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    pub fn namespace(&self) -> &[String] {
        &self.namespace
    }
}

/// One reachable editable loose-row source unit.
///
/// The loader owns only repository identity and bounded source discovery.  The
/// table path and key path remain opaque strings until semantic admission binds
/// them to a declared schema and decodes their types.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LooseRowCandidate {
    logical_path: String,
    table_path: String,
    key_path: Vec<String>,
    source: String,
}

impl LooseRowCandidate {
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    /// Canonical repository-relative directory owning this row's table.
    pub fn table_path(&self) -> &str {
        &self.table_path
    }

    /// Canonical, encoded path components after the table directory.  The
    /// final component retains its `.orna` suffix; semantic admission owns
    /// decoding and key-arity/type checks.
    pub fn key_path(&self) -> &[String] {
        &self.key_path
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn source_bytes(&self) -> &[u8] {
        self.source.as_bytes()
    }

    /// The downstream semantic/conformance boundary's stable unit tag.
    pub const fn parse_as(&self) -> &'static str {
        "row_unit"
    }
}

/// The deterministic source inputs suitable for `analyze_with_catalogue`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedProject {
    modules: Vec<ModuleInput>,
    identities: Vec<ModuleIdentity>,
    loose_rows: Vec<LooseRowCandidate>,
    standard_profile: Option<StandardDependencyProfile>,
    standard_imports: bool,
    standard_modules: BTreeSet<String>,
}

impl LoadedProject {
    pub fn modules(&self) -> &[ModuleInput] {
        &self.modules
    }

    pub fn identities(&self) -> &[ModuleIdentity] {
        &self.identities
    }
    /// Reachable editable row units, in canonical logical-path order.
    pub fn loose_rows(&self) -> &[LooseRowCandidate] {
        &self.loose_rows
    }

    pub fn into_loose_rows(self) -> Vec<LooseRowCandidate> {
        self.loose_rows
    }


    pub fn into_modules(self) -> Vec<ModuleInput> {
        self.modules
    }

    /// Returns the explicitly supplied immutable standard dependency profile,
    /// if this project was loaded with one.
    pub fn standard_profile(&self) -> Option<&StandardDependencyProfile> {
        self.standard_profile.as_ref()
    }

    /// Reports whether reachable project source requested the reserved
    /// standard namespace. The loader does not treat that request as proof
    /// that a verified standard profile was supplied.
    pub const fn has_standard_imports(&self) -> bool {
        self.standard_imports
    }

    /// Returns the logical standard modules requested by reachable source.
    pub fn standard_modules(&self) -> &BTreeSet<String> {
        &self.standard_modules
    }

    /// Derives the semantic catalogue for this project's explicitly pinned
    /// standard dependency. Source bytes remain caller-supplied and are
    /// verified against the profile; ordinary project loading never discovers
    /// a host standard library.
    pub fn standard_catalogue(
        &self,
        sources: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Option<Catalogue>, StandardCatalogueError> {
        self.standard_profile
            .as_ref()
            .map(|profile| Catalogue::from_standard_sources(profile, sources))
            .transpose()
    }
}

/// Loads the root module and its reachable ordinary source imports.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProjectLoader {
    limits: ProjectLimits,
}

impl ProjectLoader {
    pub fn new(limits: ProjectLimits) -> Self {
        Self { limits }
    }

    pub fn load(&self, repository: &Repository) -> Result<LoadedProject, ProjectLoadError> {
        self.load_with_standard_profile(repository, None)
    }

    /// Loads a project while carrying an explicitly selected standard
    /// dependency profile. The profile is metadata only at this boundary:
    /// standard modules are never discovered from the worktree and are not
    /// silently replaced with the current host library.
    pub fn load_with_standard_profile(
        &self,
        repository: &Repository,
        standard_profile: Option<StandardDependencyProfile>,
    ) -> Result<LoadedProject, ProjectLoadError> {
        let root = canonical_worktree(repository)?;
        validate_repository_paths(&root, self.limits)?;
        load_reachable_project(
            self.limits,
            standard_profile,
            |logical_path, total_bytes| read_module(&root, logical_path, total_bytes, self.limits),
            |segments| resolve_import(&root, segments),
            |tables, total_bytes| {
                discover_worktree_rows(&root, tables, total_bytes, self.limits)
            },
        )
    }

    /// Loads the root module and reachable ordinary imports from an already
    /// verified, reachable Git commit. The committed tree is read directly:
    /// this does not consult or modify the worktree, index, or refs.
    pub fn load_committed_snapshot(
        &self,
        repository: &Repository,
        commit: &GitCommitRef,
    ) -> Result<LoadedProject, ProjectLoadError> {
        self.load_committed_snapshot_with_standard_profile(repository, commit, None)
    }

    /// Snapshot variant of [`Self::load_with_standard_profile`]. Standard
    /// dependencies remain explicitly caller supplied; committed project
    /// loading never discovers host standard-library files.
    pub fn load_committed_snapshot_with_standard_profile(
        &self,
        repository: &Repository,
        commit: &GitCommitRef,
        standard_profile: Option<StandardDependencyProfile>,
    ) -> Result<LoadedProject, ProjectLoadError> {
        let source_paths = validate_committed_tree(repository, commit, self.limits)?;
        load_reachable_project(
            self.limits,
            standard_profile,
            |logical_path, total_bytes| {
                read_committed_module(
                    repository,
                    commit,
                    &source_paths,
                    logical_path,
                    total_bytes,
                    self.limits,
                )
            },
            |segments| resolve_committed_import(&source_paths, segments),
            |tables, total_bytes| {
                discover_committed_rows(
                    repository,
                    commit,
                    &source_paths,
                    tables,
                    total_bytes,
                    self.limits,
                )
            },
        )
    }
}

fn load_reachable_project(
    limits: ProjectLimits,
    standard_profile: Option<StandardDependencyProfile>,
    mut read_module: impl FnMut(&str, &mut usize) -> Result<String, ProjectLoadError>,
    mut resolve_import: impl FnMut(&[&str]) -> Result<String, ProjectLoadError>,
    mut discover_rows: impl FnMut(
        &BTreeSet<String>,
        &mut usize,
    ) -> Result<Vec<LooseRowCandidate>, ProjectLoadError>,
) -> Result<LoadedProject, ProjectLoadError> {
    let mut pending = VecDeque::from([String::from("main.orna")]);
    let mut loaded = BTreeMap::<String, LoadedModule>::new();
    let mut namespaces = BTreeMap::<Vec<String>, String>::new();
    let mut total_bytes = 0usize;
    let mut standard_imports = false;
    let mut standard_modules = BTreeSet::new();
    let mut table_paths = BTreeSet::new();

    while let Some(logical_path) = pending.pop_front() {
        if loaded.contains_key(&logical_path) {
            continue;
        }
        if loaded.len() == limits.max_modules {
            return Err(ProjectLoadError::ModuleLimit);
        }
        let source = read_module(&logical_path, &mut total_bytes)?;
        let parsed = parse_module(&source);
        if !parsed.is_ok() {
            return Err(ProjectLoadError::InvalidModule);
        }
        let namespace = namespace_for_path(&logical_path)?;
        if let Some(previous) = namespaces.insert(namespace.clone(), logical_path.clone())
            && previous != logical_path
        {
            return Err(ProjectLoadError::DuplicateNamespace);
        }

        let mut imports = BTreeSet::new();
        for item in &parsed.value.items {
            let Declaration::Use { path, .. } = &item.declaration else {
                continue;
            };
            let segments = path
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>();
            if segments.is_empty() {
                return Err(ProjectLoadError::UnsupportedImport);
            }
            if matches!(segments[0], "sys" | "std") {
                standard_imports |= segments[0] == "std";
                if segments[0] == "std" {
                    let mut logical_path = segments.join("/");
                    if segments.len() == 1 {
                        logical_path.push_str("/main");
                    }
                    if !logical_path.ends_with(".orna") {
                        logical_path.push_str(".orna");
                    }
                    standard_modules.insert(logical_path);
                }
                continue;
            }
            imports.insert(resolve_import(&segments)?);
        }
        pending.extend(imports);
        for item in &parsed.value.items {
            if let Declaration::Table { name, .. } = &item.declaration {
                let mut table_path = namespace.clone();
                table_path.push(name.clone());
                table_paths.insert(table_path.join("/"));
            }
        }
        loaded.insert(logical_path, LoadedModule { source, namespace });
    }

    let mut modules = Vec::with_capacity(loaded.len());
    let mut identities = Vec::with_capacity(loaded.len());
    for (logical_path, module) in loaded {
        identities.push(ModuleIdentity {
            logical_path: logical_path.clone(),
            namespace: module.namespace,
        });
        modules.push(ModuleInput::new(logical_path, module.source));
    }
    let loose_rows = discover_rows(&table_paths, &mut total_bytes)?;
    Ok(LoadedProject {
        modules,
        identities,
        loose_rows,
        standard_profile,
        standard_imports,
        standard_modules,
    })
}

#[derive(Debug)]
pub enum ProjectLoadError {
    Repository(RepositoryError),
    RootUnavailable,
    UnsafePath,
    Symlink,
    SourceUnavailable,
    SourceTooLarge,
    RepositoryLimit,
    ModuleLimit,
    InvalidModule,
    UnsupportedImport,
    ImportUnavailable,
    AmbiguousImport,
    DuplicateNamespace,
    SiblingCollision,
    NonPortablePath,
    DuplicateModuleNamespace,
    ReservedNamespace,
}

impl fmt::Display for ProjectLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Repository(_) => "could not discover the Git worktree",
            Self::RootUnavailable => "could not resolve the Git worktree",
            Self::UnsafePath => "project contains an unsafe module path",
            Self::Symlink => "project module path contains a symbolic link",
            Self::SourceUnavailable => "reachable source module is unavailable",
            Self::SourceTooLarge => "project source exceeds the configured limit",
            Self::RepositoryLimit => "project exceeds the configured repository-entry limit",
            Self::ModuleLimit => "project exceeds the configured module limit",
            Self::InvalidModule => "reachable source module is not a valid module unit",
            Self::UnsupportedImport => "source import cannot be resolved by this loader",
            Self::ImportUnavailable => "imported source module is unavailable",
            Self::AmbiguousImport => "imported source module has ambiguous ownership",
            Self::DuplicateNamespace => "reachable source modules define the same namespace",
            Self::SiblingCollision => "repository contains colliding sibling path components",
            Self::NonPortablePath => "repository contains a non-portable path component",
            Self::DuplicateModuleNamespace => {
                "repository contains multiple source files for one module namespace"
            }
            Self::ReservedNamespace => "repository source module shadows a reserved namespace",
        })
    }
}

impl Error for ProjectLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            _ => None,
        }
    }
}

struct LoadedModule {
    source: String,
    namespace: Vec<String>,
}

fn canonical_worktree(repository: &Repository) -> Result<PathBuf, ProjectLoadError> {
    fs::canonicalize(repository.worktree()).map_err(|_| ProjectLoadError::RootUnavailable)
}

/// Validates path portability without reading or parsing repository file bodies.
/// Git administrative directories are outside repository content and are skipped.
fn validate_repository_paths(root: &Path, limits: ProjectLimits) -> Result<(), ProjectLoadError> {
    let mut pending = VecDeque::from([root.to_path_buf()]);
    let mut module_owners = BTreeMap::<Vec<String>, String>::new();
    let mut entries_seen = 0usize;
    while let Some(directory) = pending.pop_front() {
        let mut entries = fs::read_dir(&directory)
            .map_err(|_| ProjectLoadError::SourceUnavailable)?
            .map(|entry| entry.map_err(|_| ProjectLoadError::SourceUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());

        let mut siblings = BTreeMap::<String, String>::new();
        for entry in entries {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ProjectLoadError::UnsafePath)?;
            if name == ".git" {
                continue;
            }
            if entries_seen == limits.max_repository_entries {
                return Err(ProjectLoadError::RepositoryLimit);
            }
            entries_seen += 1;
            if !portable_component(&name) {
                return Err(ProjectLoadError::NonPortablePath);
            }
            let key = unicode_sibling_key(&name);
            if let Some(existing) = siblings.insert(key, name.clone())
                && existing != name
            {
                return Err(ProjectLoadError::SiblingCollision);
            }

            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| ProjectLoadError::SourceUnavailable)?;
            if metadata.file_type().is_symlink() {
                return Err(ProjectLoadError::Symlink);
            }
            if metadata.is_dir() {
                pending.push_back(entry.path());
            } else if !metadata.is_file() {
                return Err(ProjectLoadError::UnsafePath);
            } else if name.ends_with(".orna")
                && !is_committed_metadata_path(root, &entry.path())
                && is_module_source_path(root, &entry.path())
            {
                let logical_path = logical_path(root, &entry.path())?;
                let namespace = namespace_for_path(&logical_path)?;
                if namespace
                    .first()
                    .is_some_and(|component| matches!(component.as_str(), "sys" | "std"))
                {
                    return Err(ProjectLoadError::ReservedNamespace);
                }
                if let Some(existing) = module_owners.insert(namespace, logical_path.clone())
                    && existing != logical_path
                {
                    return Err(ProjectLoadError::DuplicateModuleNamespace);
                }
            }
        }
    }
    Ok(())
}

/// Validates the complete immutable tree before any reachable source body is
/// read. Git emits only leaf entries, so sibling ownership is reconstructed
/// from every component rather than inferred from the mutable worktree.
fn validate_committed_tree(
    repository: &Repository,
    commit: &GitCommitRef,
    limits: ProjectLimits,
) -> Result<BTreeSet<String>, ProjectLoadError> {
    let entries = repository
        .list_committed_tree(commit, limits.max_repository_entries.saturating_add(1))
        .map_err(ProjectLoadError::Repository)?;
    if entries.len() > limits.max_repository_entries {
        return Err(ProjectLoadError::RepositoryLimit);
    }

    let mut source_paths = BTreeSet::new();
    let mut module_owners = BTreeMap::<Vec<String>, String>::new();
    let mut siblings = BTreeMap::<PathBuf, BTreeMap<String, String>>::new();
    for entry in entries {
        validate_committed_tree_entry(entry, &mut siblings, &mut module_owners, &mut source_paths)?;
    }
    Ok(source_paths)
}

fn validate_committed_tree_entry(
    entry: CommittedTreeEntry,
    siblings: &mut BTreeMap<PathBuf, BTreeMap<String, String>>,
    module_owners: &mut BTreeMap<Vec<String>, String>,
    source_paths: &mut BTreeSet<String>,
) -> Result<(), ProjectLoadError> {
    match entry.kind() {
        CommittedTreeEntryKind::File { .. } => {}
        CommittedTreeEntryKind::Symlink => return Err(ProjectLoadError::Symlink),
        CommittedTreeEntryKind::Submodule => return Err(ProjectLoadError::UnsafePath),
    }

    let path = entry.path().as_path();
    let mut parent = PathBuf::new();
    let mut components = Vec::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(ProjectLoadError::UnsafePath);
        };
        let component = component.to_str().ok_or(ProjectLoadError::UnsafePath)?;
        if !portable_component(component) {
            return Err(ProjectLoadError::NonPortablePath);
        }
        let owned = component.to_owned();
        let key = unicode_sibling_key(component);
        if let Some(existing) = siblings
            .entry(parent.clone())
            .or_default()
            .insert(key, owned.clone())
            && existing != owned
        {
            return Err(ProjectLoadError::SiblingCollision);
        }
        parent.push(component);
        components.push(component);
    }
    if components.is_empty() {
        return Err(ProjectLoadError::UnsafePath);
    }
    if components[0] == ".orna" {
        return Ok(());
    }

    let logical_path = path.to_str().ok_or(ProjectLoadError::UnsafePath)?;
    if !logical_path.ends_with(".orna") {
        return Ok(());
    }
    source_paths.insert(logical_path.to_owned());
    if !is_module_source_logical(logical_path) {
        return Ok(());
    }
    let namespace = namespace_for_path(logical_path)?;
    if namespace
        .first()
        .is_some_and(|component| matches!(component.as_str(), "sys" | "std"))
    {
        return Err(ProjectLoadError::ReservedNamespace);
    }
    if let Some(existing) = module_owners.insert(namespace, logical_path.to_owned())
        && existing != logical_path
    {
        return Err(ProjectLoadError::DuplicateModuleNamespace);
    }
    Ok(())
}

fn is_committed_metadata_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(|component| component.as_os_str() == ".orna")
}

fn is_module_source_path(root: &Path, path: &Path) -> bool {
    logical_path(root, path)
        .map(|logical_path| is_module_source_logical(&logical_path))
        .unwrap_or(false)
}

fn is_module_source_logical(logical_path: &str) -> bool {
    let mut components = logical_path.split('/');
    let Some(first) = components.next() else {
        return false;
    };
    components.next().is_none() || logical_path.ends_with("/main.orna") || first == "main.orna"
}

fn resolve_import(root: &Path, segments: &[&str]) -> Result<String, ProjectLoadError> {
    if segments.iter().any(|segment| !valid_component(segment)) {
        return Err(ProjectLoadError::UnsupportedImport);
    }
    let mut base = root.to_path_buf();
    for segment in segments {
        base.push(segment);
    }
    let file = base.with_extension("orna");
    let directory = base.join("main.orna");
    let file_exists = checked_candidate(root, &file)?;
    let directory_exists = checked_candidate(root, &directory)?;
    match (file_exists, directory_exists) {
        (false, false) => Err(ProjectLoadError::ImportUnavailable),
        (true, true) => Err(ProjectLoadError::AmbiguousImport),
        (true, false) => logical_path(root, &file),
        (false, true) => logical_path(root, &directory),
    }
}

fn resolve_committed_import(
    source_paths: &BTreeSet<String>,
    segments: &[&str],
) -> Result<String, ProjectLoadError> {
    if segments.iter().any(|segment| !valid_component(segment)) {
        return Err(ProjectLoadError::UnsupportedImport);
    }
    let base = segments.join("/");
    let file = format!("{base}.orna");
    let directory = format!("{base}/main.orna");
    match (
        source_paths.contains(&file),
        source_paths.contains(&directory),
    ) {
        (false, false) => Err(ProjectLoadError::ImportUnavailable),
        (true, true) => Err(ProjectLoadError::AmbiguousImport),
        (true, false) => Ok(file),

        (false, true) => Ok(directory),
    }
}
fn discover_worktree_rows(
    root: &Path,
    table_paths: &BTreeSet<String>,
    total_bytes: &mut usize,
    limits: ProjectLimits,
) -> Result<Vec<LooseRowCandidate>, ProjectLoadError> {
    let mut paths = BTreeSet::new();
    let mut pending = VecDeque::from([root.to_path_buf()]);
    while let Some(directory) = pending.pop_front() {
        let mut entries = fs::read_dir(&directory)
            .map_err(|_| ProjectLoadError::SourceUnavailable)?
            .map(|entry| entry.map_err(|_| ProjectLoadError::SourceUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|_| ProjectLoadError::SourceUnavailable)?;
            if metadata.is_dir() {
                pending.push_back(path);
                continue;
            }
            if !metadata.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("orna")
            {
                continue;
            }
            if is_committed_metadata_path(root, &path) {
                continue;
            }
            let logical_path = logical_path(root, &path)?;
            if table_owner_and_key(&logical_path, table_paths).is_some() {
                paths.insert(logical_path);
            }
        }
    }

    paths
        .into_iter()
        .map(|logical_path| {
            let (table_path, key_path) =
                table_owner_and_key(&logical_path, table_paths).ok_or(ProjectLoadError::UnsafePath)?;
            let source = read_module(root, &logical_path, total_bytes, limits)?;
            Ok(LooseRowCandidate {
                logical_path,
                table_path,
                key_path,
                source,
            })
        })
        .collect()
}

fn discover_committed_rows(
    repository: &Repository,
    commit: &GitCommitRef,
    source_paths: &BTreeSet<String>,
    table_paths: &BTreeSet<String>,
    total_bytes: &mut usize,
    limits: ProjectLimits,
) -> Result<Vec<LooseRowCandidate>, ProjectLoadError> {
    source_paths
        .iter()
        .filter_map(|logical_path| {
            table_owner_and_key(logical_path, table_paths)
                .map(|(table_path, key_path)| (logical_path, table_path, key_path))
        })
        .map(|(logical_path, table_path, key_path)| {
            let maximum = limits.max_source_bytes.saturating_sub(*total_bytes);
            let bytes = repository
                .read_committed_file(commit, logical_path, maximum)
                .map_err(ProjectLoadError::Repository)?;
            let source =
                String::from_utf8(bytes).map_err(|_| ProjectLoadError::SourceUnavailable)?;
            if source.len() > maximum {
                return Err(ProjectLoadError::SourceTooLarge);
            }
            *total_bytes += source.len();
            Ok(LooseRowCandidate {
                logical_path: logical_path.clone(),
                table_path,
                key_path,
                source,
            })
        })
        .collect()
}

fn table_owner_and_key(
    logical_path: &str,
    table_paths: &BTreeSet<String>,
) -> Option<(String, Vec<String>)> {
    let mut selected: Option<(String, Vec<String>)> = None;
    for table_path in table_paths {
        let prefix = format!("{table_path}/");
        let Some(suffix) = logical_path.strip_prefix(&prefix) else {
            continue;
        };
        if suffix.is_empty() || !suffix.ends_with(".orna") {
            continue;
        }
        let key_path = suffix.split('/').map(str::to_owned).collect::<Vec<_>>();
        if key_path.iter().any(|component| component.is_empty()) {
            continue;
        }
        let replace = match &selected {
            None => true,
            Some((previous, _)) => table_path.len() > previous.len(),
        };
        if replace {
            selected = Some((table_path.clone(), key_path));
        }
    }
    selected
}

fn checked_candidate(root: &Path, candidate: &Path) -> Result<bool, ProjectLoadError> {
    ensure_no_symlink_ancestors(root, candidate)?;
    match fs::symlink_metadata(candidate) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(ProjectLoadError::Symlink);
            }
            Ok(metadata.is_file())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(ProjectLoadError::SourceUnavailable),
    }
}

fn read_module(
    root: &Path,
    logical_path: &str,
    total_bytes: &mut usize,
    limits: ProjectLimits,
) -> Result<String, ProjectLoadError> {
    let path = root.join(logical_path);
    ensure_no_symlink_ancestors(root, &path)?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| ProjectLoadError::SourceUnavailable)?;
    if metadata.file_type().is_symlink() {
        return Err(ProjectLoadError::Symlink);
    }
    if !metadata.is_file() {
        return Err(ProjectLoadError::SourceUnavailable);
    }
    let length = usize::try_from(metadata.len()).map_err(|_| ProjectLoadError::SourceTooLarge)?;
    if length > limits.max_source_bytes.saturating_sub(*total_bytes) {
        return Err(ProjectLoadError::SourceTooLarge);
    }
    let maximum = limits.max_source_bytes.saturating_sub(*total_bytes);
    let mut source = String::new();
    fs::File::open(path)
        .map_err(|_| ProjectLoadError::SourceUnavailable)?
        .take(maximum.saturating_add(1) as u64)
        .read_to_string(&mut source)
        .map_err(|_| ProjectLoadError::SourceUnavailable)?;
    if source.len() > maximum {
        return Err(ProjectLoadError::SourceTooLarge);
    }
    *total_bytes += source.len();
    Ok(source)
}

fn read_committed_module(
    repository: &Repository,
    commit: &GitCommitRef,
    source_paths: &BTreeSet<String>,
    logical_path: &str,
    total_bytes: &mut usize,
    limits: ProjectLimits,
) -> Result<String, ProjectLoadError> {
    if !source_paths.contains(logical_path) {
        return Err(ProjectLoadError::SourceUnavailable);
    }
    let maximum = limits.max_source_bytes.saturating_sub(*total_bytes);
    let bytes = repository
        .read_committed_file(commit, logical_path, maximum)
        .map_err(ProjectLoadError::Repository)?;
    let source = String::from_utf8(bytes).map_err(|_| ProjectLoadError::SourceUnavailable)?;
    if source.len() > maximum {
        return Err(ProjectLoadError::SourceTooLarge);
    }
    *total_bytes += source.len();
    Ok(source)
}

fn logical_path(root: &Path, path: &Path) -> Result<String, ProjectLoadError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| ProjectLoadError::UnsafePath)?;
    let mut components = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(ProjectLoadError::UnsafePath);
        };
        let component = component.to_str().ok_or(ProjectLoadError::UnsafePath)?;
        components.push(component);
    }
    let logical_path = components.join("/");
    if !logical_path.ends_with(".orna") || namespace_for_path(&logical_path).is_err() {
        return Err(ProjectLoadError::UnsafePath);
    }
    Ok(logical_path)
}

fn ensure_no_symlink_ancestors(root: &Path, path: &Path) -> Result<(), ProjectLoadError> {
    let mut current = path.parent();
    while let Some(ancestor) = current {
        if !ancestor.starts_with(root) {
            return Err(ProjectLoadError::UnsafePath);
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ProjectLoadError::Symlink);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ProjectLoadError::SourceUnavailable),
        }
        if ancestor == root {
            return Ok(());
        }
        current = ancestor.parent();
    }
    Err(ProjectLoadError::UnsafePath)
}

fn namespace_for_path(path: &str) -> Result<Vec<String>, ProjectLoadError> {
    let mut parts = path.split('/').collect::<Vec<_>>();
    let Some(file) = parts.pop() else {
        return Err(ProjectLoadError::UnsafePath);
    };
    let Some(stem) = file.strip_suffix(".orna") else {
        return Err(ProjectLoadError::UnsafePath);
    };
    if !valid_component(stem) {
        return Err(ProjectLoadError::UnsafePath);
    }
    if parts.iter().any(|part| !valid_component(part)) {
        return Err(ProjectLoadError::UnsafePath);
    }
    let mut namespace = parts.into_iter().map(str::to_owned).collect::<Vec<_>>();
    if stem != "main" {
        namespace.push(stem.to_owned());
    }
    Ok(namespace)
}

fn valid_component(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && !component.contains('.')
        && portable_component(component)
}

fn portable_component(component: &str) -> bool {
    !component.is_empty() && component.nfc().eq(component.chars())
}

/// Host-independent NFKC case-fold key for portable sibling ownership.
fn unicode_sibling_key(component: &str) -> String {
    unicode16::to_nfkc_casefold(component)
}
