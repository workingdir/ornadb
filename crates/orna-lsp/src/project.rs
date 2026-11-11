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
use orna_check::{Catalogue, analyze_with_catalogue_located};
use orna_hir::{DefKind, FnBody, NativeBinding, NativeKey, SourceId, SourceUnit, lower};
use orna_link::{EngineRegistry, LinkError, LinkProfile, ProjectManifest, host_target, plan};
use orna_project::{LoadedProject, ProjectLoadError, ProjectLoader};
use orna_repository::Repository;
use orna_syntax::{Declaration, SyntaxSpan, SyntaxTree, parse_module};

use crate::analysis;
use crate::documents::PositionMapper;
use crate::sync::lock;
use crate::workspace::module_path_of;

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
            let mut cache = lock(&self.by_directory);
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
    link_diagnostics(project.root(), &sources, &mut diagnostics);
    let Some(catalogue) = check_catalogue(&loaded) else {
        for (logical_path, source) in &sources {
            let tree = parse_module(source).value;
            let mapper = PositionMapper::new(source);
            for span in standard_import_spans(&tree) {
                diagnostics
                    .entry((*logical_path).to_owned())
                    .or_default()
                    .push(analysis::source_diagnostic(
                        mapper.range(&span),
                        orna_check::DIAG_IMPORT,
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
fn check_catalogue(loaded: &LoadedProject) -> Option<Catalogue> {
    if loaded.has_uncaptured_standard_imports() {
        return None;
    }
    loaded.production_catalogue().ok()
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

/// Plans the project's native-library links (docs/design/native-libraries.md, section 10,
/// steps 1 to 3) and publishes each failure. The plan reads the manifest and the bytes of
/// each library file. It never loads a library: opening a project in the editor must not
/// run project code.
///
/// The editor holds no engine registry, so every engine native the project declares counts
/// as bound. The run path checks the host's registry.
fn link_diagnostics(
    root: &Path,
    sources: &BTreeMap<&str, &str>,
    diagnostics: &mut BTreeMap<String, Vec<Diagnostic>>,
) {
    let manifest = match ProjectManifest::load(root) {
        Ok(manifest) => manifest,
        Err(errors) => return publish_link_errors(&errors, sources, diagnostics),
    };
    let trees: Vec<(&str, SyntaxTree)> = sources
        .iter()
        .map(|(logical, source)| {
            (
                *logical,
                orna_syntax::parse_module_with_file(source, *logical).value,
            )
        })
        .collect();
    let units: Vec<SourceUnit<'_>> = trees
        .iter()
        .zip(0u32..)
        .map(|((logical, tree), source)| SourceUnit {
            source: SourceId::new(source),
            tree,
            path: module_path_of(logical),
        })
        .collect();
    let program = lower(&units);
    let registry = EngineRegistry::new(engine_keys(&program));
    if let Err(errors) = plan(
        &program,
        &manifest,
        root,
        host_target(),
        LinkProfile::Native,
        &registry,
    ) {
        publish_link_errors(&errors, sources, diagnostics);
    }
}

/// The keys of the engine natives a program declares.
fn engine_keys(program: &orna_hir::Program) -> Vec<NativeKey> {
    program
        .defs
        .iter()
        .filter_map(|def| match &def.kind {
            DefKind::Fn(function) => match &function.body {
                FnBody::Native(decl) if matches!(decl.binding, NativeBinding::Engine { .. }) => {
                    Some(decl.key.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Publishes link failures on the module each names. A failure that names no module
/// (a manifest error, or a library failure with no declaration) lands on the root module.
fn publish_link_errors(
    errors: &[LinkError],
    sources: &BTreeMap<&str, &str>,
    diagnostics: &mut BTreeMap<String, Vec<Diagnostic>>,
) {
    for error in errors {
        let location = error
            .declarations
            .first()
            .and_then(|site| site.location.rsplit_once(':'));
        let module = location
            .map(|(file, _)| file)
            .filter(|file| sources.contains_key(file))
            .unwrap_or(ROOT_MODULE)
            .to_owned();
        let line = location
            .and_then(|(_, line)| line.parse::<u32>().ok())
            .map_or(0, |line| line.saturating_sub(1));
        let position = lsp_types::Position::new(line, 0);
        let message = match &error.library {
            Some(library) => format!("{} (library `{library}`)", error.detail),
            None => error.detail.clone(),
        };
        diagnostics
            .entry(module)
            .or_default()
            .push(analysis::source_diagnostic(
                lsp_types::Range::new(position, position),
                error.code.code(),
                &message,
                &["fix the native-library manifest or the library file, then run check again"],
            ));
    }
}

#[cfg(test)]
mod link_tests {
    use super::*;
    use lsp_types::NumberOrString;
    use tempfile::TempDir;

    const SLUG: &str = "pub native fn clock(): Int effect none;\n";
    const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn code_of(diagnostic: &Diagnostic) -> Option<&str> {
        match &diagnostic.code {
            Some(NumberOrString::String(code)) => Some(code.as_str()),
            _ => None,
        }
    }

    #[test]
    fn library_digest_mismatch_is_published_on_the_declaring_module() {
        let root = TempDir::new().unwrap();
        let triple = host_target();
        let file = format!("native/{triple}/libslug.so");
        fs::create_dir_all(root.path().join(format!("native/{triple}"))).unwrap();
        fs::write(root.path().join(&file), b"slug library bytes").unwrap();
        fs::write(
            root.path().join("orna.toml"),
            format!(
                "[[native_library]]\nname = \"slug\"\nmodules = [\"slug\"]\nabi = 1\n\n\
                 [native_library.target.{triple}]\npath = \"{file}\"\nsha256 = \"{ZERO_DIGEST}\"\n"
            ),
        )
        .unwrap();
        let sources = BTreeMap::from([("slug.orna", SLUG)]);
        let mut diagnostics = BTreeMap::new();
        link_diagnostics(root.path(), &sources, &mut diagnostics);
        let published = diagnostics
            .get("slug.orna")
            .expect("the digest failure names slug.orna");
        assert_eq!(published.len(), 1);
        assert_eq!(code_of(&published[0]), Some("ORNA-STD-NATIVE-DIGEST"));
    }

    #[test]
    fn project_without_native_declarations_has_no_link_diagnostics() {
        let root = TempDir::new().unwrap();
        let sources = BTreeMap::from([("main.orna", "")]);
        let mut diagnostics = BTreeMap::new();
        link_diagnostics(root.path(), &sources, &mut diagnostics);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn malformed_manifest_is_published_on_the_root_module() {
        let root = TempDir::new().unwrap();
        fs::write(
            root.path().join("orna.toml"),
            "[[native_library]]\nname = \n",
        )
        .unwrap();
        let sources = BTreeMap::from([("main.orna", "")]);
        let mut diagnostics = BTreeMap::new();
        link_diagnostics(root.path(), &sources, &mut diagnostics);
        let published = diagnostics
            .get(ROOT_MODULE)
            .expect("the manifest failure lands on main.orna");
        assert!(!published.is_empty());
        assert!(
            published
                .iter()
                .all(|diagnostic| code_of(diagnostic) == Some("ORNA-STD-NATIVE-MANIFEST"))
        );
    }
}
