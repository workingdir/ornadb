//! The Orna project behind a document, and the check `orna check` runs on it.
//!
//! A document belongs to a project when it is a `.orna` file inside a Git
//! worktree whose root holds `main.orna`. Such a project is checked exactly
//! as `orna check` checks it: the module graph is loaded from `main.orna`
//! (with open editor buffers in place of the saved files), uncaptured
//! standard imports are rejected, and semantic analysis runs with the core
//! catalogue plus the captured standard library. Other documents are
//! detached; `orna check` has no project to check for them, so they receive
//! syntax diagnostics only.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use lsp_types::{Diagnostic, Uri};
use orna_project_v1::{LoadedProject, ProjectLoadError, ProjectLoader};
use orna_repository_v1::Repository;
use orna_semantic_v1::{Catalogue, analyze_with_catalogue_located};
use orna_syntax_v1::{Declaration, SyntaxSpan, SyntaxTree, parse_module};

use crate::analysis;
use crate::documents::PositionMapper;

/// The root module every project check starts from.
pub(crate) const ROOT_MODULE: &str = "main.orna";

/// A Git worktree whose root holds `main.orna`.
#[derive(Debug)]
pub(crate) struct Project {
    repository: Repository,
    /// Canonical worktree path; logical paths are relative to it.
    root: PathBuf,
}

impl Project {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn repository(&self) -> &Repository {
        &self.repository
    }

    /// The file URI of one module of this project.
    pub(crate) fn module_uri(&self, logical_path: &str) -> Option<Uri> {
        path_uri(&self.root.join(logical_path))
    }
}

/// A document's place in a project.
#[derive(Clone, Debug)]
pub(crate) struct ProjectHome {
    pub project: Arc<Project>,
    /// The module's repository-relative path with `/` separators.
    pub logical_path: String,
}

/// Finds the project of each document, caching Git discovery per directory.
#[derive(Default)]
pub(crate) struct ProjectLocator {
    by_directory: Mutex<HashMap<PathBuf, Option<Arc<Project>>>>,
}

impl ProjectLocator {
    pub(crate) fn home(&self, uri: &Uri) -> Option<ProjectHome> {
        let path = uri_path(uri)?;
        if path.extension().and_then(|extension| extension.to_str()) != Some("orna") {
            return None;
        }
        let directory = fs::canonicalize(path.parent()?).ok()?;
        let project = {
            let mut cache = self
                .by_directory
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache
                .entry(directory.clone())
                .or_insert_with(|| discover(&directory))
                .clone()
        }?;
        let relative = directory
            .join(path.file_name()?)
            .strip_prefix(&project.root)
            .ok()?
            .to_path_buf();
        let mut components = Vec::new();
        for component in relative.components() {
            let Component::Normal(component) = component else {
                return None;
            };
            components.push(component.to_str()?.to_owned());
        }
        Some(ProjectHome {
            project,
            logical_path: components.join("/"),
        })
    }
}

fn discover(directory: &Path) -> Option<Arc<Project>> {
    let repository = Repository::discover(directory).ok()?;
    let root = fs::canonicalize(repository.worktree()).ok()?;
    root.join(ROOT_MODULE)
        .is_file()
        .then(|| Arc::new(Project { repository, root }))
}

pub(crate) fn uri_path(uri: &Uri) -> Option<PathBuf> {
    url::Url::parse(uri.as_str()).ok()?.to_file_path().ok()
}

pub(crate) fn path_uri(path: &Path) -> Option<Uri> {
    url::Url::from_file_path(path).ok()?.as_str().parse().ok()
}

/// One open module of a project, as the editor currently holds it.
pub(crate) struct OpenModule<'a> {
    pub logical_path: &'a str,
    pub text: &'a str,
    pub has_syntax_errors: bool,
}

/// The result of checking one project.
pub(crate) struct ProjectCheck {
    /// Diagnostics by module logical path. Syntax errors in open modules are
    /// not repeated here; the caller publishes them from the open buffer.
    pub diagnostics: BTreeMap<String, Vec<Diagnostic>>,
}

/// Runs the checks of `orna check` over the project, reading open modules
/// from their editor buffers.
pub(crate) fn check(project: &Project, open: &[OpenModule<'_>]) -> ProjectCheck {
    let overlays = open
        .iter()
        .map(|module| (module.logical_path.to_owned(), module.text.to_owned()))
        .collect::<BTreeMap<_, _>>();
    let mut diagnostics = BTreeMap::<String, Vec<Diagnostic>>::new();
    let loaded = match ProjectLoader::default().load_with_overlays(&project.repository, &overlays) {
        Ok(loaded) => loaded,
        // The syntax diagnostics of the open buffer explain why loading failed.
        Err(ProjectLoadError::InvalidModule { .. })
            if open.iter().any(|module| module.has_syntax_errors) =>
        {
            return ProjectCheck { diagnostics };
        }
        Err(error) => {
            diagnostics.entry(ROOT_MODULE.to_owned()).or_default().push(
                analysis::source_diagnostic(
                    lsp_types::Range::default(),
                    "E2100",
                    &format!("project source could not be loaded: {error}"),
                    &["fix the project module graph and source boundaries, then run check again"],
                ),
            );
            return ProjectCheck { diagnostics };
        }
    };

    let sources = loaded
        .modules()
        .iter()
        .map(|module| (module.logical_path.as_str(), module.source.as_str()))
        .collect::<BTreeMap<_, _>>();
    let Some(catalogue) = standard_catalogue(&loaded) else {
        for (logical_path, source) in &sources {
            let tree = parse_module(source).value;
            let mapper = PositionMapper::new(source);
            for span in standard_import_spans(&tree) {
                diagnostics
                    .entry((*logical_path).to_owned())
                    .or_default()
                    .push(analysis::source_diagnostic(
                        mapper.range(&span),
                        orna_semantic_v1::DIAG_IMPORT,
                        "imported module is unavailable",
                        &["use a captured standard dependency or remove the import"],
                    ));
            }
        }
        return ProjectCheck { diagnostics };
    };

    let located = analyze_with_catalogue_located(loaded.modules(), &catalogue);
    let mut selections = HashMap::<&str, ModuleSelections>::new();
    for (diagnostic, origin) in located.analysis.diagnostics.iter().zip(&located.origins) {
        let logical_path = origin
            .logical_path
            .as_deref()
            .filter(|path| sources.contains_key(path))
            .unwrap_or(ROOT_MODULE);
        let source = sources.get(logical_path).copied().unwrap_or_default();
        let module = selections
            .entry(logical_path)
            .or_insert_with(|| ModuleSelections::new(source));
        let range = origin
            .primary
            .as_ref()
            .map_or_else(lsp_types::Range::default, |item| module.range(item));
        diagnostics
            .entry(logical_path.to_owned())
            .or_default()
            .push(analysis::source_diagnostic(
                range,
                diagnostic.code(),
                diagnostic.message(),
                &[],
            ));
    }
    ProjectCheck { diagnostics }
}

/// The catalogue `orna check` uses, or `None` when the project imports a
/// standard module that no captured standard dependency provides.
fn standard_catalogue(loaded: &LoadedProject) -> Option<Catalogue> {
    let catalogue = Catalogue::authoritative_core();
    if loaded.standard_modules().is_empty() {
        return Some(catalogue);
    }
    let profile = loaded.standard_profile()?;
    catalogue
        .with_standard_sources(profile, loaded.standard_sources().iter().cloned())
        .ok()
}

/// Spans of the module paths of `use std...` declarations.
fn standard_import_spans(tree: &SyntaxTree) -> Vec<SyntaxSpan> {
    tree.items
        .iter()
        .filter_map(|item| {
            let Declaration::Use { path, .. } = &item.declaration else {
                return None;
            };
            let (first, last) = (path.first()?, path.last()?);
            (first.name == "std").then(|| SyntaxSpan::new(first.span.start, last.span.end))
        })
        .collect()
}

/// Maps the item span a checker diagnostic came from to the name the user
/// reads: the declared name for named items, otherwise the item's first line.
struct ModuleSelections<'a> {
    source: &'a str,
    mapper: PositionMapper<'a>,
    names: HashMap<(usize, usize), SyntaxSpan>,
}

impl<'a> ModuleSelections<'a> {
    fn new(source: &'a str) -> Self {
        let parse = parse_module(source);
        let names = analysis::declaration_symbols(&parse, source)
            .into_iter()
            .map(|symbol| ((symbol.full.start, symbol.full.end), symbol.selection))
            .collect();
        Self {
            source,
            mapper: PositionMapper::new(source),
            names,
        }
    }

    fn range(&self, item: &Range<usize>) -> lsp_types::Range {
        if let Some(name) = self.names.get(&(item.start, item.end)) {
            return self.mapper.range(name);
        }
        let start = item.start.min(self.source.len());
        let end = item.end.min(self.source.len()).max(start);
        let first_line = self.source[start..end]
            .find('\n')
            .map_or(end, |offset| start + offset);
        let trimmed = start + self.source[start..first_line].trim_end().len();
        self.mapper.range(&SyntaxSpan::new(start, trimmed))
    }
}
