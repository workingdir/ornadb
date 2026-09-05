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

use orna_repository_v1::{Repository, RepositoryError};
use orna_semantic_v1::ModuleInput;
use orna_syntax_v1::{Declaration, parse_module};
use unicode_normalization::UnicodeNormalization;

mod unicode16;

/// Bounded resource limits applied before source contents are read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectLimits {
    pub max_modules: usize,
    pub max_source_bytes: usize,
}

impl Default for ProjectLimits {
    fn default() -> Self {
        Self {
            max_modules: 256,
            max_source_bytes: 4 * 1024 * 1024,
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

/// The deterministic source inputs suitable for `analyze_with_catalogue`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedProject {
    modules: Vec<ModuleInput>,
    identities: Vec<ModuleIdentity>,
}

impl LoadedProject {
    pub fn modules(&self) -> &[ModuleInput] {
        &self.modules
    }

    pub fn identities(&self) -> &[ModuleIdentity] {
        &self.identities
    }

    pub fn into_modules(self) -> Vec<ModuleInput> {
        self.modules
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
        let root = canonical_worktree(repository)?;
        validate_repository_paths(&root)?;
        let mut pending = VecDeque::from([String::from("main.orna")]);
        let mut loaded = BTreeMap::<String, LoadedModule>::new();
        let mut namespaces = BTreeMap::<Vec<String>, String>::new();
        let mut total_bytes = 0usize;

        while let Some(logical_path) = pending.pop_front() {
            if loaded.contains_key(&logical_path) {
                continue;
            }
            if loaded.len() == self.limits.max_modules {
                return Err(ProjectLoadError::ModuleLimit);
            }
            let source = read_module(&root, &logical_path, &mut total_bytes, self.limits)?;
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
                    continue;
                }
                imports.insert(resolve_import(&root, &segments)?);
            }
            pending.extend(imports);
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
        Ok(LoadedProject {
            modules,
            identities,
        })
    }
}

#[derive(Debug)]
pub enum ProjectLoadError {
    Repository(RepositoryError),
    RootUnavailable,
    UnsafePath,
    Symlink,
    SourceUnavailable,
    SourceTooLarge,
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
fn validate_repository_paths(root: &Path) -> Result<(), ProjectLoadError> {
    let mut pending = VecDeque::from([root.to_path_buf()]);
    let mut module_owners = BTreeMap::<Vec<String>, String>::new();
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
            } else if name.ends_with(".orna") && !is_committed_metadata_path(root, &entry.path()) {
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

fn is_committed_metadata_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(|component| component.as_os_str() == ".orna")
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
