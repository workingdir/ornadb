//! The standard library a workspace sees, and where its declarations open.
//!
//! A project sees the standard dependency captured in its repository: the
//! same verified sources `orna check` analyses. A project without one sees no
//! standard module, as `orna check` rejects those imports. Documents outside
//! a project see the reference standard sources this server ships.
//!
//! Editors open a standard declaration from a read-only copy of its source,
//! written once below the user cache directory, in a directory named by the
//! digest of the source set: `orna/std/<digest>/std/<module>.orna`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};

use lsp_types::{Location, Uri};
use orna_semantic_v1::StandardDependencyProfile;
use orna_syntax_v1::{Declaration, SyntaxTree, UseTail, Visibility, lex, parse_module};

use crate::analysis::{EditorSymbol, EditorSymbolKind, symbol_for_item};
use crate::documents::PositionMapper;

/// One public declaration of a standard module.
#[derive(Debug)]
pub(crate) struct StandardSymbol {
    /// The dotted module name, such as `std.math`.
    pub module: String,
    pub symbol: EditorSymbol,
    /// The logical source path, such as `std/math.orna`.
    path: String,
    /// The declaring source text, shared by every symbol of the file.
    source: Arc<str>,
}

/// The standard modules and public declarations of one source set.
#[derive(Debug, Default)]
pub(crate) struct StandardLibrary {
    /// Hex digest of the source set; names its cache directory.
    digest: String,
    modules: BTreeSet<String>,
    /// Sorted by module, then name.
    symbols: Vec<StandardSymbol>,
}

static REFERENCE: LazyLock<Arc<StandardLibrary>> = LazyLock::new(|| {
    let sources = orna_standard_sources::reference_standard_sources_v1();
    let digest = StandardDependencyProfile::from_sources("reference", sources.iter().cloned())
        .map(|profile| hex(&profile.revision_digest()))
        .unwrap_or_default();
    Arc::new(StandardLibrary::new(digest, sources))
});

static NONE: LazyLock<Arc<StandardLibrary>> = LazyLock::new(Arc::default);

impl StandardLibrary {
    /// The reference sources, for documents outside a project.
    pub(crate) fn reference() -> Arc<Self> {
        Arc::clone(&REFERENCE)
    }

    /// No standard library, for a project that captures none.
    pub(crate) fn none() -> Arc<Self> {
        Arc::clone(&NONE)
    }

    /// The standard dependency a project captures, verified by `profile`.
    pub(crate) fn captured(profile: &StandardDependencyProfile, sources: &[(String, String)]) -> Self {
        Self::new(hex(&profile.revision_digest()), sources.iter().cloned())
    }

    fn new(digest: String, sources: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut library = Self {
            digest,
            ..Self::default()
        };
        for (path, source) in sources {
            let Some(module) = module_name(&path) else {
                continue;
            };
            let Ok(tokens) = lex(&source) else {
                continue;
            };
            let parsed = parse_module(&source);
            let mut prefix = String::new();
            for segment in module.split('.') {
                if !prefix.is_empty() {
                    prefix.push('.');
                }
                prefix.push_str(segment);
                library.modules.insert(prefix.clone());
            }
            let shared: Arc<str> = Arc::from(source.as_str());
            library.symbols.extend(
                parsed
                    .value
                    .items
                    .iter()
                    .filter(|item| matches!(&item.visibility, Visibility::Public { .. }))
                    .filter_map(|item| {
                        symbol_for_item(item, &source, &tokens).map(|symbol| StandardSymbol {
                            module: module.clone(),
                            symbol,
                            path: path.clone(),
                            source: Arc::clone(&shared),
                        })
                    }),
            );
        }
        library.symbols.sort_by(|left, right| {
            left.module
                .cmp(&right.module)
                .then_with(|| left.symbol.name.cmp(&right.symbol.name))
        });
        library
    }

    /// Every standard module, dotted from `std`.
    pub(crate) fn modules(&self) -> &BTreeSet<String> {
        &self.modules
    }

    pub(crate) fn module_exists(&self, module: &str) -> bool {
        self.modules.contains(module)
    }

    /// The public declarations of `module`.
    pub(crate) fn symbols_in(&self, module: &str) -> &[StandardSymbol] {
        let start = self
            .symbols
            .partition_point(|entry| entry.module.as_str() < module);
        let end = start
            + self.symbols[start..].partition_point(|entry| entry.module.as_str() == module);
        &self.symbols[start..end]
    }

    /// The first public declaration of `module` named `name`.
    pub(crate) fn symbol(&self, module: &str, name: &str) -> Option<&StandardSymbol> {
        self.symbols_in(module)
            .iter()
            .find(|entry| entry.symbol.name == name)
    }

    /// The parameter names of the function `name` in `module`.
    pub(crate) fn function_parameters(&self, module: &str, name: &str) -> Option<Vec<Option<String>>> {
        let standard = self.symbol(module, name)?;
        (standard.symbol.kind == EditorSymbolKind::Function)
            .then(|| standard.symbol.parameter_names.clone())
    }

    /// The declared result type of the function `name` in `module`.
    pub(crate) fn function_result(&self, module: &str, name: &str) -> Option<String> {
        let standard = self.symbol(module, name)?;
        if standard.symbol.kind == EditorSymbolKind::Function {
            standard.symbol.result_type.clone()
        } else {
            None
        }
    }

    /// The standard declarations a module imports by name or glob.
    pub(crate) fn imported_symbols(&self, tree: &SyntaxTree) -> Vec<&StandardSymbol> {
        let mut imported = BTreeMap::new();
        for item in &tree.items {
            let Declaration::Use { path, tail } = &item.declaration else {
                continue;
            };
            let module = path
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            match tail {
                UseTail::Names(names) => {
                    for name in names {
                        if let Some(symbol) = self.symbol(&module, &name.name) {
                            imported.insert((module.clone(), name.name.clone()), symbol);
                        }
                    }
                }
                UseTail::Glob { .. } => {
                    for symbol in self.symbols_in(&module) {
                        imported.insert((module.clone(), symbol.symbol.name.clone()), symbol);
                    }
                }
                UseTail::None | UseTail::Alias { .. } => {}
            }
        }
        imported.into_values().collect()
    }

    /// The standard module a qualifier names, directly or through a module
    /// alias the tree imports.
    pub(crate) fn resolve_module(&self, tree: &SyntaxTree, qualifier: &str) -> Option<String> {
        if self.module_exists(qualifier) {
            return Some(qualifier.to_owned());
        }
        let (alias, child_path) = qualifier
            .split_once('.')
            .map_or((qualifier, None), |(alias, child)| (alias, Some(child)));
        let base = self.module_aliases(tree).remove(alias)?;
        let module = child_path.map_or(base.clone(), |child| format!("{base}.{child}"));
        self.module_exists(&module).then_some(module)
    }

    fn module_aliases(&self, tree: &SyntaxTree) -> BTreeMap<String, String> {
        let mut aliases = BTreeMap::new();
        for item in &tree.items {
            let Declaration::Use { path, tail } = &item.declaration else {
                continue;
            };
            let module = path
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if !self.module_exists(&module) {
                continue;
            }
            let alias = match tail {
                UseTail::Alias { name, .. } => Some(name.as_str()),
                UseTail::None => path.last().map(|segment| segment.name.as_str()),
                UseTail::Glob { .. } | UseTail::Names(_) => None,
            };
            if let Some(alias) = alias {
                aliases.insert(alias.to_owned(), module);
            }
        }
        aliases
    }

    /// Where `standard` is declared: its name in a read-only copy of its
    /// source below the user cache directory.
    pub(crate) fn location(&self, standard: &StandardSymbol) -> Option<Location> {
        let path = cache_root()?.join(&self.digest).join(relative_path(&standard.path)?);
        write_read_only(&path, &standard.source)?;
        let uri = url::Url::from_file_path(&path).ok()?.as_str().parse::<Uri>().ok()?;
        Some(Location {
            uri,
            range: PositionMapper::new(&standard.source).range(&standard.symbol.selection),
        })
    }
}

/// The dotted module of a standard source path: `std/math.orna` is
/// `std.math`, and a directory's `main.orna` names the directory.
fn module_name(path: &str) -> Option<String> {
    let relative = path.strip_prefix("std/")?.strip_suffix(".orna")?;
    let module_path = if relative == "main" {
        ""
    } else {
        relative.strip_suffix("/main").unwrap_or(relative)
    };
    if module_path.is_empty() {
        Some("std".to_owned())
    } else {
        Some(format!("std.{}", module_path.replace('/', ".")))
    }
}

/// `$XDG_CACHE_HOME/orna/std`, or `~/.cache/orna/std`.
fn cache_root() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
    };
    Some(base.join("orna").join("std"))
}

/// A logical source path as a relative path of plain components.
fn relative_path(logical: &str) -> Option<PathBuf> {
    let path = Path::new(logical);
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
        .then(|| path.to_path_buf())
}

/// Writes `source` to `path` unless it already holds exactly that text, and
/// leaves the file read-only.
fn write_read_only(path: &Path, source: &str) -> Option<()> {
    if fs::read(path).is_ok_and(|existing| existing == source.as_bytes()) {
        return Some(());
    }
    fs::create_dir_all(path.parent()?).ok()?;
    let staging = path.with_extension(format!("orna.{}.partial", std::process::id()));
    fs::write(&staging, source).ok()?;
    let mut permissions = fs::metadata(&staging).ok()?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&staging, permissions).ok()?;
    if fs::rename(&staging, path).is_err() {
        let _ = fs::remove_file(&staging);
        return None;
    }
    Some(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_names_follow_the_standard_layout() {
        assert_eq!(module_name("std/main.orna").as_deref(), Some("std"));
        assert_eq!(module_name("std/math.orna").as_deref(), Some("std.math"));
        assert_eq!(module_name("std/io/main.orna").as_deref(), Some("std.io"));
        assert_eq!(module_name("std/io/fs.orna").as_deref(), Some("std.io.fs"));
        assert_eq!(module_name("lib/math.orna"), None);
    }

    #[test]
    fn captured_sources_form_their_own_library() {
        let sources = vec![
            (
                "std/collection.orna".to_owned(),
                "/// The answer.\npub fn answer(): Int = 42;\nfn hidden(): Int = 1;\n".to_owned(),
            ),
            ("std/io/main.orna".to_owned(), "pub fn read(): Int = 1;\n".to_owned()),
        ];
        let profile = StandardDependencyProfile::from_sources("captured", sources.clone()).unwrap();
        let library = StandardLibrary::captured(&profile, &sources);
        assert!(library.module_exists("std"));
        assert!(library.module_exists("std.collection"));
        assert!(library.module_exists("std.io"));
        assert!(library.symbol("std.collection", "answer").is_some());
        assert!(library.symbol("std.collection", "hidden").is_none());
        assert_eq!(library.digest, hex(&profile.revision_digest()));
        assert!(!StandardLibrary::none().module_exists("std"));
    }

    #[test]
    fn definitions_open_a_read_only_copy_named_by_the_source_digest() {
        let cache = tempfile::tempdir().unwrap();
        let sources = vec![(
            "std/collection.orna".to_owned(),
            "pub fn answer(): Int = 42;\n".to_owned(),
        )];
        let profile = StandardDependencyProfile::from_sources("captured", sources.clone()).unwrap();
        let library = StandardLibrary::captured(&profile, &sources);
        let standard = library.symbol("std.collection", "answer").unwrap();
        let path = cache
            .path()
            .join("orna/std")
            .join(&library.digest)
            .join("std/collection.orna");
        write_read_only(&path, &standard.source).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "pub fn answer(): Int = 42;\n");
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        // A second write keeps the existing copy.
        write_read_only(&path, &standard.source).unwrap();
        assert!(relative_path("../escape.orna").is_none());
        assert!(relative_path("/etc/passwd").is_none());
    }
}
