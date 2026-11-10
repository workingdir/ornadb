//! One analysis per document version, and the HIR queries every editor
//! feature reads.
//!
//! Resolution is not repeated here. The workspace lowers every document and
//! every standard module it sees into one `orna_hir::Program`, and answers
//! definition, references, rename, hover and semantic-token questions from the
//! program's `NameUse` index, its definitions and its body slots. A local slot
//! is identified by the body it belongs to, so same-numbered slots of different
//! bodies never collapse into one target.
//!
//! Documents outside a project share one scope, as they always have: a name
//! the language would leave unresolved is looked up among the top-level
//! declarations of every detached document. That editor-only rule is applied
//! after lowering, to the HIR's answer, not as a second resolver.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use lsp_types::Uri;
use orna_hir::{
    BodyId, DefId, DefKind, LocalId, ModuleId as HirModuleId, Name, NameTarget, NameUse, Program,
    SourceId, SourceUnit, TyKind, UseContext, lower,
};
use orna_syntax::{Declaration, Keyword, SyntaxSpan, Token, TokenKind, UseTail, lex, parse_module};

use crate::analysis::{self, EditorParse, EditorSymbol};
use crate::documents::Document;
use crate::project::ProjectHome;
use crate::standard::StandardLibrary;

#[cfg(test)]
thread_local! {
    /// Analyses built on this thread, for tests that prove caching.
    pub(crate) static ANALYSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// An open document at one version. Its analysis is built on first use and
/// shared by every request for that version.
pub(crate) struct OpenDocument {
    pub document: Document,
    pub home: Option<ProjectHome>,
    analysis: OnceLock<Arc<FileAnalysis>>,
}

impl OpenDocument {
    pub(crate) fn new(document: Document, home: Option<ProjectHome>) -> Self {
        Self {
            document,
            home,
            analysis: OnceLock::new(),
        }
    }

    pub(crate) fn analysis(&self) -> &Arc<FileAnalysis> {
        self.analysis
            .get_or_init(|| Arc::new(FileAnalysis::new(self.document.clone())))
    }
}

/// Everything editor features derive from one source text, computed once.
pub(crate) struct FileAnalysis {
    pub document: Document,
    pub parse: EditorParse,
    pub tokens: Vec<Token>,
    pub symbols: Vec<EditorSymbol>,
}

impl FileAnalysis {
    pub(crate) fn new(document: Document) -> Self {
        #[cfg(test)]
        ANALYSES.with(|count| count.set(count.get() + 1));
        let parse = parse_module(&document.text);
        let tokens = lex(&document.text).unwrap_or_default();
        let symbols = analysis::declaration_symbols_from_tokens(&parse, &document.text, &tokens);
        Self {
            document,
            parse,
            tokens,
            symbols,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.document.text
    }

    pub(crate) fn uri(&self) -> &Uri {
        &self.document.uri
    }

    /// Tokens that lie inside `span`.
    pub(crate) fn tokens_in(&self, span: &SyntaxSpan) -> &[Token] {
        let start = self
            .tokens
            .partition_point(|token| token.span.start < span.start);
        let end = start + self.tokens[start..].partition_point(|token| token.span.end <= span.end);
        &self.tokens[start..end]
    }

    /// The keyword strictly containing `byte`.
    pub(crate) fn keyword_at(&self, byte: usize) -> Option<Keyword> {
        let index = self.tokens.partition_point(|token| token.span.end <= byte);
        let token = self.tokens.get(index)?;
        match token.kind {
            TokenKind::Keyword(keyword) if token.span.start <= byte => Some(keyword),
            _ => None,
        }
    }
}

/// One module visible to a request.
pub(crate) struct Module {
    pub file: Arc<FileAnalysis>,
    /// The module namespace; empty for the project root and for detached
    /// documents.
    pub namespace: Vec<String>,
    /// The repository-relative path of a project module.
    pub logical_path: Option<String>,
    /// The canonical module path lowering registers.
    pub path: orna_hir::ModulePath,
}

impl Module {
    pub(crate) fn project(file: Arc<FileAnalysis>, logical_path: &str) -> Self {
        Self {
            file,
            namespace: module_namespace(logical_path),
            logical_path: Some(logical_path.to_owned()),
            path: module_path_of(logical_path),
        }
    }

    pub(crate) fn detached(file: Arc<FileAnalysis>, path: orna_hir::ModulePath) -> Self {
        Self {
            file,
            namespace: Vec::new(),
            logical_path: None,
            path,
        }
    }
}

/// What a name refers to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Target {
    Local { module: usize, binding: usize },
    Declared { module: usize, index: usize },
    Module(Vec<String>),
    Standard { module: String, name: String },
}

/// The outcome of resolving one occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    Target(Target),
    /// Several declarations are equally visible.
    Ambiguous,
    Unknown,
}

impl Resolved {
    pub(crate) fn target(self) -> Option<Target> {
        match self {
            Resolved::Target(target) => Some(target),
            Resolved::Ambiguous | Resolved::Unknown => None,
        }
    }
}

/// How the modules of a workspace see each other.
#[derive(Clone)]
enum Scope {
    /// Detached documents share one scope instead of importing each other.
    Detached,
    /// Modules see each other through `use` imports, as in `orna check`.
    Project {
        /// Why some project modules are missing, when loading failed.
        incomplete: Option<String>,
    },
}

/// What a declaration is, in the terms editor features use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeclaredKind {
    Function,
    Table,
    Type,
    Enum,
    Protocol,
    Dimension,
    Unit,
    Let,
    Field,
    KeyField,
    Variant,
    PayloadField,
    Method,
    Static,
}

/// One declaration of a module.
#[derive(Clone, Debug)]
pub(crate) struct Declared {
    pub name: String,
    pub key: String,
    pub selection: SyntaxSpan,
    /// The whole item or member.
    pub full: SyntaxSpan,
    pub kind: DeclaredKind,
    /// The declaration this one is a member of, as an index in the same
    /// module.
    pub owner: Option<usize>,
    pub public: bool,
    /// The lowered definition.
    pub def: DefId,
    /// A function's parameter names and selection spans.
    pub parameters: Vec<(String, SyntaxSpan)>,
}

/// How a local binding was introduced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalBindingKind {
    Parameter,
    Local,
}

/// One local binding of a module.
#[derive(Clone, Debug)]
pub(crate) struct LocalBinding {
    pub name: String,
    pub key: String,
    pub selection: SyntaxSpan,
    /// The parameter or statement that declares it, for hover text.
    pub context: SyntaxSpan,
    pub kind: LocalBindingKind,
    pub body: BodyId,
    pub local: LocalId,
    /// The lexical extent the binding is visible in.
    pub scope: SyntaxSpan,
}

/// One name occurrence and what it resolved to.
#[derive(Clone, Debug)]
pub(crate) struct NameOccurrence {
    pub span: SyntaxSpan,
    pub key: String,
    pub resolved: Resolved,
    pub context: UseContext,
    pub written: bool,
    pub callee: bool,
    pub shorthand: bool,
}

/// References to one target.
#[derive(Default)]
pub(crate) struct Search {
    /// Occurrences that resolve to the target, including its declaration.
    pub found: Vec<(usize, SyntaxSpan)>,
    /// Occurrences with the same name that may refer to the target but whose
    /// receiver or scope could not be resolved.
    pub uncertain: Vec<(usize, SyntaxSpan)>,
}

/// The modules one request can see.
pub(crate) struct Workspace {
    pub modules: Vec<Module>,
    /// The standard library these modules see, as `orna check` sees it.
    pub standard: Arc<StandardLibrary>,
    /// The parsed standard sources, kept so an edit can be re-lowered.
    std_sources: Vec<(String, String)>,
    scope: Scope,
    /// The one lowered program.
    program: Program,
    /// The HIR module of each workspace module.
    hir_modules: Vec<Option<HirModuleId>>,
    /// Declarations of each module, by module index.
    declarations: Vec<Vec<Declared>>,
    /// Declaration indexes by lowered definition.
    declarations_by_def: HashMap<DefId, (usize, usize)>,
    /// Local bindings of each module.
    bindings: Vec<Vec<LocalBinding>>,
    /// Binding indexes by body and slot.
    bindings_by_local: HashMap<(BodyId, LocalId), (usize, usize)>,
    /// Name occurrences of each module, sorted by span.
    occurrences: Vec<Vec<NameOccurrence>>,
    /// Top-level declarations of every detached module, by key.
    detached_defs: HashMap<String, Vec<DefId>>,
}

impl Workspace {
    /// Modules that see each other through imports.
    #[cfg(test)]
    pub(crate) fn project(
        modules: Vec<Module>,
        standard: Arc<StandardLibrary>,
        incomplete: Option<String>,
    ) -> Self {
        Self::project_with_sources(modules, standard, Vec::new(), incomplete)
    }

    /// Documents outside a project, sharing one scope and the reference
    /// standard library.
    #[cfg(test)]
    pub(crate) fn detached(files: Vec<Arc<FileAnalysis>>) -> Self {
        Self::detached_with_sources(
            files,
            StandardLibrary::reference(),
            orna_standard_sources::reference_standard_sources_v1().to_vec(),
        )
    }

    /// Modules that see each other through imports, with the standard sources
    /// `orna check` analyses.
    pub(crate) fn project_with_sources(
        modules: Vec<Module>,
        standard: Arc<StandardLibrary>,
        std_sources: Vec<(String, String)>,
        incomplete: Option<String>,
    ) -> Self {
        Self::build(
            modules,
            standard,
            std_sources,
            Scope::Project { incomplete },
        )
    }

    /// Documents outside a project with an explicit standard source set.
    pub(crate) fn detached_with_sources(
        files: Vec<Arc<FileAnalysis>>,
        standard: Arc<StandardLibrary>,
        std_sources: Vec<(String, String)>,
    ) -> Self {
        let modules = files
            .into_iter()
            .enumerate()
            .map(|(index, file)| {
                Module::detached(
                    file,
                    orna_hir::ModulePath::new([Name::new(&format!("_detached_{index}"))]),
                )
            })
            .collect();
        Self::build(modules, standard, std_sources, Scope::Detached)
    }

    fn build(
        modules: Vec<Module>,
        standard: Arc<StandardLibrary>,
        std_sources: Vec<(String, String)>,
        scope: Scope,
    ) -> Self {
        let std_trees = std_sources
            .iter()
            .map(|(logical, source)| {
                (
                    module_path_of(logical),
                    orna_syntax::parse_module_with_file(source, logical.clone()).value,
                )
            })
            .collect::<Vec<_>>();
        let mut units = Vec::with_capacity(modules.len() + std_trees.len());
        let mut source = 0u32;
        for module in &modules {
            units.push(SourceUnit {
                source: SourceId::new(source),
                tree: &module.file.parse.value,
                path: module.path.clone(),
            });
            source += 1;
        }
        for (path, tree) in &std_trees {
            units.push(SourceUnit {
                source: SourceId::new(source),
                tree,
                path: path.clone(),
            });
            source += 1;
        }
        let program = lower(&units);
        let mut workspace = Self {
            modules,
            standard,
            std_sources,
            scope,
            program,
            hir_modules: Vec::new(),
            declarations: Vec::new(),
            declarations_by_def: HashMap::new(),
            bindings: Vec::new(),
            bindings_by_local: HashMap::new(),
            occurrences: Vec::new(),
            detached_defs: HashMap::new(),
        };
        workspace.index();
        workspace
    }

    /// Builds every query table from the lowered program.
    fn index(&mut self) {
        self.hir_modules = self
            .modules
            .iter()
            .map(|module| {
                self.program
                    .modules
                    .iter_enumerated()
                    .find(|(_, candidate)| candidate.path == module.path)
                    .map(|(id, _)| id)
            })
            .collect();
        self.declarations = vec![Vec::new(); self.modules.len()];
        self.bindings = vec![Vec::new(); self.modules.len()];
        self.occurrences = vec![Vec::new(); self.modules.len()];

        let owner_of = owner_map(&self.program);
        for (def_id, def) in self.program.defs.iter_enumerated() {
            let Some((module, _)) =
                self.hir_modules
                    .iter()
                    .enumerate()
                    .find_map(|(module, hir)| {
                        hir.filter(|hir| *hir == def.module)
                            .map(|hir| (module, hir))
                    })
            else {
                continue;
            };
            let owner_def = owner_of.get(&def_id).copied();
            let Some(declared) = self.declared_from_def(module, def_id, &def.module, owner_def)
            else {
                continue;
            };
            let owner = owner_def
                .and_then(|owner| self.declarations_by_def.get(&owner))
                .filter(|(owner_module, _)| *owner_module == module)
                .map(|(_, index)| *index);
            let declared = Declared { owner, ..declared };
            let index = self.declarations[module].len();
            self.declarations_by_def.insert(def_id, (module, index));
            self.declarations[module].push(declared);
        }

        self.index_bindings();

        self.index_parameters();

        self.index_detached_defs();

        self.index_occurrences();
    }

    /// Lexical bindings of every body, with the module they belong to.
    fn index_bindings(&mut self) {
        for (body_id, body) in self.program.bodies.iter_enumerated() {
            let module = self.program.defs[body.owner].module;
            let Some(module_index) = self.hir_modules.iter().position(|hir| *hir == Some(module))
            else {
                continue;
            };
            for (local_id, local) in body.locals.iter_enumerated() {
                let kind = if body.params.contains(&local_id) {
                    LocalBindingKind::Parameter
                } else {
                    LocalBindingKind::Local
                };
                let index = self.bindings[module_index].len();
                self.bindings_by_local
                    .insert((body_id, local_id), (module_index, index));
                self.bindings[module_index].push(LocalBinding {
                    name: local.name.as_str().to_owned(),
                    key: local.name.as_str().to_owned(),
                    selection: local.span.clone(),
                    context: local.span.clone(),
                    kind,
                    body: body_id,
                    local: local_id,
                    scope: local.scope.clone(),
                });
            }
        }
    }

    /// Function parameter names, from the lowered body's first slots.
    fn index_parameters(&mut self) {
        for declarations in &mut self.declarations {
            for declared in declarations.iter_mut() {
                let Some(body) = self.program.source_body(declared.def) else {
                    continue;
                };
                declared.parameters = self.program.bodies[body]
                    .params
                    .iter()
                    .map(|slot| {
                        let local = &self.program.bodies[body].locals[*slot];
                        (local.name.as_str().to_owned(), local.span.clone())
                    })
                    .collect();
            }
        }
    }

    /// Same-name top-level declarations of detached documents share one scope.
    fn index_detached_defs(&mut self) {
        if !matches!(self.scope, Scope::Detached) {
            return;
        }
        for declarations in &self.declarations {
            // Same-name declarations of one document are one overload
            // group, as the old shared scope treated them.
            let mut first_in_module = HashMap::<&str, DefId>::new();
            for declared in declarations.iter() {
                if declared.owner.is_none() {
                    first_in_module
                        .entry(declared.key.as_str())
                        .or_insert(declared.def);
                }
            }
            for (key, def) in first_in_module {
                self.detached_defs
                    .entry(key.to_owned())
                    .or_default()
                    .push(def);
            }
        }
    }

    /// Name occurrences of every module, sorted and deduplicated by span.
    fn index_occurrences(&mut self) {
        for (module, hir_module) in self.hir_modules.iter().enumerate() {
            let Some(hir_module) = hir_module else {
                continue;
            };
            for use_ in &self.program.modules[*hir_module].name_uses {
                let resolved = self.resolved_for(module, use_);
                self.occurrences[module].push(NameOccurrence {
                    span: use_.span.clone(),
                    key: use_.name.as_str().to_owned(),
                    resolved,
                    context: use_.context,
                    written: use_.written,
                    callee: use_.callee,
                    shorthand: use_.shorthand,
                });
            }
            self.occurrences[module].sort_by(|left, right| {
                (left.span.start, left.span.end, left.context as u8).cmp(&(
                    right.span.start,
                    right.span.end,
                    right.context as u8,
                ))
            });
            self.occurrences[module].dedup_by(|right, left| {
                if left.span.start == right.span.start && left.span.end == right.span.end {
                    left.callee |= right.callee;
                    left.written |= right.written;
                    true
                } else {
                    false
                }
            });
        }
    }

    fn declared_from_def(
        &self,
        module: usize,
        def_id: DefId,
        hir_module: &HirModuleId,
        owner: Option<DefId>,
    ) -> Option<Declared> {
        let def = self.program.defs.get(def_id)?;
        let owner_kind = owner
            .and_then(|owner| self.program.defs.get(owner))
            .map(|owner| &owner.kind);
        let kind = match &def.kind {
            DefKind::Fn(_) => match owner_kind {
                Some(DefKind::Impl(_) | DefKind::Protocol(_)) => DeclaredKind::Method,
                _ => DeclaredKind::Function,
            },
            DefKind::Table(_) => DeclaredKind::Table,
            DefKind::Type(_) => DeclaredKind::Type,
            DefKind::Enum(_) => DeclaredKind::Enum,
            DefKind::Variant(_) => DeclaredKind::Variant,
            DefKind::Protocol(_) => DeclaredKind::Protocol,
            DefKind::Dimension(_) => DeclaredKind::Dimension,
            DefKind::Unit(_) => DeclaredKind::Unit,
            DefKind::Let(_) => match owner_kind {
                Some(DefKind::Impl(_)) => DeclaredKind::Static,
                _ => DeclaredKind::Let,
            },
            DefKind::Field(_) => match owner_kind {
                Some(DefKind::Variant(_)) => DeclaredKind::PayloadField,
                Some(DefKind::Table(table)) if table.key.contains(&def_id) => {
                    DeclaredKind::KeyField
                }
                _ => DeclaredKind::Field,
            },
            DefKind::Impl(_) | DefKind::Assertion(_) => return None,
        };
        let selection = selection_span(&self.modules[module].file, &def.span, def.name.as_str())?;
        debug_assert_eq!(def.module, *hir_module);
        Some(Declared {
            name: def.name.as_str().to_owned(),
            key: def.name.as_str().to_owned(),
            selection,
            full: def.span.clone(),
            kind,
            owner: None,
            public: def.vis == orna_hir::Visibility::Public,
            def: def_id,
            parameters: Vec::new(),
        })
    }

    /// The editor answer for one HIR name use.
    fn resolved_for(&self, module: usize, use_: &NameUse) -> Resolved {
        match use_.target {
            NameTarget::Local(local) => {
                let Some(body) = use_.body else {
                    return Resolved::Unknown;
                };
                match self.bindings_by_local.get(&(body, local)) {
                    Some((binding_module, binding)) if *binding_module == module => {
                        Resolved::Target(Target::Local {
                            module,
                            binding: *binding,
                        })
                    }
                    _ => Resolved::Unknown,
                }
            }
            NameTarget::Def(def) => {
                if matches!(self.scope, Scope::Detached)
                    && matches!(
                        use_.context,
                        UseContext::Value | UseContext::Type | UseContext::Pattern
                    )
                {
                    // Outside a project every document shares one scope; a
                    // name declared by two documents is ambiguous even when
                    // one of them is the document that writes the use.
                    match self
                        .detached_defs
                        .get(use_.name.as_str())
                        .map(Vec::as_slice)
                    {
                        Some([only]) => self.target_of_def(*only),
                        Some(_) => Resolved::Ambiguous,
                        None => self.target_of_def(def),
                    }
                } else {
                    self.target_of_def(def)
                }
            }
            NameTarget::Module(hir) => Resolved::Target(Target::Module(
                self.program.modules[hir]
                    .path
                    .segments()
                    .iter()
                    .map(|name| name.as_str().to_owned())
                    .collect(),
            )),
            NameTarget::Intrinsic => {
                if matches!(self.scope, Scope::Detached) && use_.context == UseContext::Value {
                    // A declaration in the shared detached scope outranks a
                    // language-defined form of the same spelling.
                    match self
                        .detached_defs
                        .get(use_.name.as_str())
                        .map(Vec::as_slice)
                    {
                        Some([only]) => self.target_of_def(*only),
                        Some(_) => Resolved::Ambiguous,
                        None => Resolved::Unknown,
                    }
                } else {
                    Resolved::Unknown
                }
            }
            NameTarget::Ambiguous => Resolved::Ambiguous,
            NameTarget::Unresolved => self.detached_lookup(use_),
        }
    }

    /// The target of a definition, whether it belongs to a document or to the
    /// standard library.
    fn target_of_def(&self, def: DefId) -> Resolved {
        let Some(definition) = self.program.defs.get(def) else {
            return Resolved::Unknown;
        };
        if self.standard_module(def) {
            let path = self.program.modules[definition.module].path.to_string();
            return Resolved::Target(Target::Standard {
                module: path,
                name: definition.name.as_str().to_owned(),
            });
        }
        match self.declarations_by_def.get(&def) {
            Some((module, index)) => Resolved::Target(Target::Declared {
                module: *module,
                index: *index,
            }),
            None => Resolved::Unknown,
        }
    }

    fn standard_module(&self, def: DefId) -> bool {
        self.program.defs.get(def).is_some_and(|definition| {
            self.program.modules[definition.module]
                .path
                .segments()
                .first()
                .is_some_and(|name| name.as_str() == "std")
        })
    }

    /// The editor-only flat scope detached documents share.
    fn detached_lookup(&self, use_: &NameUse) -> Resolved {
        if !matches!(self.scope, Scope::Detached)
            || !matches!(
                use_.context,
                UseContext::Value | UseContext::Type | UseContext::Pattern
            )
        {
            return Resolved::Unknown;
        }
        let Some(candidates) = self.detached_defs.get(use_.name.as_str()) else {
            return Resolved::Unknown;
        };
        match candidates.as_slice() {
            [def] => self.target_of_def(*def),
            [] => Resolved::Unknown,
            _ => Resolved::Ambiguous,
        }
    }

    pub(crate) fn module_of(&self, uri: &Uri) -> Option<usize> {
        self.modules
            .iter()
            .position(|module| module.file.uri() == uri)
    }

    pub(crate) fn file(&self, module: usize) -> &FileAnalysis {
        &self.modules[module].file
    }

    /// The same modules with some files replaced, for checking an edit.
    pub(crate) fn with_files(&self, mut replaced: HashMap<usize, Arc<FileAnalysis>>) -> Self {
        let modules = self
            .modules
            .iter()
            .enumerate()
            .map(|(index, module)| Module {
                file: replaced
                    .remove(&index)
                    .unwrap_or_else(|| Arc::clone(&module.file)),
                namespace: module.namespace.clone(),
                logical_path: module.logical_path.clone(),
                path: module.path.clone(),
            })
            .collect();
        Self::build(
            modules,
            Arc::clone(&self.standard),
            self.std_sources.clone(),
            self.scope.clone(),
        )
    }

    /// Whether the modules are documents outside a project, sharing one scope.
    pub(crate) fn is_detached(&self) -> bool {
        matches!(self.scope, Scope::Detached)
    }

    /// Why some project modules are missing, when loading the project failed.
    pub(crate) fn incomplete(&self) -> Option<&str> {
        match &self.scope {
            Scope::Project { incomplete } => incomplete.as_deref(),
            Scope::Detached => None,
        }
    }

    /// `file:line:column` of `byte` in `module`, for messages.
    pub(crate) fn location_label(&self, module: usize, byte: usize) -> String {
        let file = self.file(module);
        let name = file
            .uri()
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        let position = crate::documents::PositionMapper::new(file.text()).position(byte);
        format!("{name}:{}:{}", position.line + 1, position.character + 1)
    }

    pub(crate) fn declarations(&self, module: usize) -> &[Declared] {
        &self.declarations[module]
    }

    pub(crate) fn declaration(&self, module: usize, index: usize) -> &Declared {
        &self.declarations[module][index]
    }

    pub(crate) fn bindings(&self, module: usize) -> &[LocalBinding] {
        &self.bindings[module]
    }

    pub(crate) fn binding(&self, module: usize, index: usize) -> &LocalBinding {
        &self.bindings[module][index]
    }

    /// Whether a unit with this name is declared in the module or visible to
    /// it. Unit suffixes need no type inference; they name a unit declaration.
    pub(crate) fn unit_named(&self, module: usize, name: &str) -> bool {
        self.declarations(module)
            .iter()
            .any(|declared| declared.key == name && declared.kind == DeclaredKind::Unit)
    }

    /// The declared type of a local binding, when the source states one.
    pub(crate) fn binding_type_text(&self, module: usize, index: usize) -> Option<String> {
        let binding = self.binding(module, index);
        let body = self.program.bodies.get(binding.body)?;
        let local = body.locals.get(binding.local)?;
        declared_type_name(&self.program, local.ty)
    }

    pub(crate) fn occurrences(&self, module: usize) -> &[NameOccurrence] {
        &self.occurrences[module]
    }

    /// Top-level declarations a name in `module` may refer to without a
    /// qualifier: its own, every detached peer's in a shared scope, and those
    /// it imports by name or glob.
    pub(crate) fn completion_symbols(&self, module: usize) -> Vec<&EditorSymbol> {
        let mut symbols = self.modules[module].file.symbols.iter().collect::<Vec<_>>();
        if self.is_detached() {
            for (index, other) in self.modules.iter().enumerate() {
                if index != module {
                    symbols.extend(other.file.symbols.iter());
                }
            }
            return symbols;
        }
        for def in self.imported_defs(module) {
            if let Some((imported_module, index)) = self.declarations_by_def.get(&def)
                && self.declarations[*imported_module][*index].public
            {
                symbols.extend(analysis::declared_symbol(
                    &self.modules[*imported_module].file.symbols,
                    &self.declarations[*imported_module][*index].selection,
                ));
            }
        }
        symbols
    }

    /// Definitions a module imports by name or wildcard.
    fn imported_defs(&self, module: usize) -> Vec<DefId> {
        let Some(hir) = self.hir_modules[module] else {
            return Vec::new();
        };
        let tree = &self.modules[module].file.parse.value;
        let mut defs = Vec::new();
        for item in &tree.items {
            let Declaration::Use { path, tail } = &item.declaration else {
                continue;
            };
            let names = path
                .iter()
                .map(|segment| Name::new(&segment.name))
                .collect::<Vec<_>>();
            let target = self
                .program
                .modules
                .iter_enumerated()
                .find(|(_, candidate)| candidate.path.segments() == names.as_slice())
                .map(|(id, _)| id);
            let Some(target) = target else {
                continue;
            };
            match tail {
                UseTail::Names(names) => {
                    for name in names {
                        if let Some(def) = self.program.modules[target]
                            .exports
                            .get(&Name::new(&name.name))
                        {
                            defs.push(*def);
                        }
                    }
                }
                UseTail::Glob { .. } => {
                    defs.extend(self.program.modules[target].exports.values().copied());
                }
                UseTail::None | UseTail::Alias { .. } => {}
            }
        }
        let _ = hir;
        defs.sort_unstable_by_key(|def| def.as_u32());
        defs.dedup();
        defs
    }

    /// The name at `byte` in `module` (a use, declaration or local binding)
    /// with what it refers to.
    pub(crate) fn resolve_at(&self, module: usize, byte: usize) -> Option<(SyntaxSpan, Resolved)> {
        let occurrences = self.occurrences(module);
        let index = occurrences.partition_point(|occurrence| occurrence.span.end < byte);
        if let Some(occurrence) = occurrences[index..]
            .iter()
            .take_while(|occurrence| occurrence.span.start <= byte)
            .find(|occurrence| brings(occurrence, byte))
        {
            return Some((occurrence.span.clone(), occurrence.resolved.clone()));
        }
        if let Some(binding) = self.binding_containing(module, byte) {
            return Some((
                self.bindings(module)[binding].selection.clone(),
                Resolved::Target(Target::Local { module, binding }),
            ));
        }
        let declared = self
            .declarations(module)
            .iter()
            .position(|declared| brings_span(&declared.selection, byte))?;
        Some((
            self.declarations(module)[declared].selection.clone(),
            Resolved::Target(Target::Declared {
                module,
                index: declared,
            }),
        ))
    }

    /// The local binding whose declared name contains `byte`.
    pub(crate) fn binding_containing(&self, module: usize, byte: usize) -> Option<usize> {
        self.bindings(module)
            .iter()
            .enumerate()
            .filter(|(_, binding)| brings_span(&binding.selection, byte))
            .min_by_key(|(_, binding)| binding.selection.end - binding.selection.start)
            .map(|(index, _)| index)
    }

    /// The local binding declared at `span`, or used at `span`.
    pub(crate) fn binding_at(&self, module: usize, span: &SyntaxSpan) -> Option<usize> {
        if let Some(index) = self
            .bindings(module)
            .iter()
            .position(|binding| same_span(&binding.selection, span))
        {
            return Some(index);
        }
        match self.occurrence(module, span)?.resolved {
            Resolved::Target(Target::Local { binding, .. }) => Some(binding),
            _ => None,
        }
    }

    /// The occurrence with exactly this span.
    pub(crate) fn occurrence(&self, module: usize, span: &SyntaxSpan) -> Option<&NameOccurrence> {
        self.occurrences(module)
            .iter()
            .find(|occurrence| same_span(&occurrence.span, span))
    }

    /// The declaration whose name is at `span`.
    pub(crate) fn declared_at(&self, module: usize, span: &SyntaxSpan) -> Option<usize> {
        self.declarations(module)
            .iter()
            .position(|declared| same_span(&declared.selection, span))
    }

    /// Every local binding visible at `byte`, innermost last.
    pub(crate) fn visible_bindings(&self, module: usize, byte: usize) -> Vec<&LocalBinding> {
        let mut visible = self
            .bindings(module)
            .iter()
            .filter(|binding| binding.selection.start <= byte)
            .filter(|binding| brings_span(&binding.scope, byte))
            .collect::<Vec<_>>();
        visible.sort_by_key(|binding| binding.selection.start);
        let mut by_name = HashMap::<&str, usize>::new();
        let mut result = Vec::new();
        for binding in visible {
            match by_name.get(binding.key.as_str()) {
                Some(index) => result[*index] = binding,
                None => {
                    by_name.insert(binding.key.as_str(), result.len());
                    result.push(binding);
                }
            }
        }
        result
    }

    /// Every use of one local binding, without its declaration.
    pub(crate) fn binding_references(&self, module: usize, binding: usize) -> Vec<SyntaxSpan> {
        let target = Target::Local { module, binding };
        self.occurrences(module)
            .iter()
            .filter(|occurrence| {
                occurrence.resolved == Resolved::Target(target.clone())
                    && occurrence.context != UseContext::Binding
            })
            .map(|occurrence| occurrence.span.clone())
            .collect()
    }

    /// The name a target is declared with.
    pub(crate) fn target_key(&self, target: &Target) -> String {
        match target {
            Target::Local { module, binding } => self.binding(*module, *binding).key.clone(),
            Target::Declared { module, index } => self.declaration(*module, *index).key.clone(),
            Target::Module(path) => path.last().cloned().unwrap_or_default(),
            Target::Standard { name, .. } => name.clone(),
        }
    }

    /// Every occurrence that refers to `target`, and every same-named
    /// occurrence that might.
    pub(crate) fn search(&self, target: &Target) -> Search {
        let key = self.target_key(target);
        let member = match target {
            Target::Declared { module, index } => self.declaration(*module, *index).owner.is_some(),
            _ => false,
        };
        let parameter = match target {
            Target::Local { module, binding } => {
                self.binding(*module, *binding).kind == LocalBindingKind::Parameter
            }
            _ => false,
        };
        let mut search = Search::default();
        if let Target::Local { module, binding } = target {
            search
                .found
                .push((*module, self.binding(*module, *binding).selection.clone()));
        }
        if let Target::Declared { module, index } = target {
            search
                .found
                .push((*module, self.declaration(*module, *index).selection.clone()));
        }
        for (module, occurrences) in self.occurrences.iter().enumerate() {
            if self.modules[module]
                .path
                .segments()
                .first()
                .is_some_and(|name| name.as_str() == "std")
            {
                continue;
            }
            for occurrence in occurrences {
                if occurrence.key != key {
                    continue;
                }
                match &occurrence.resolved {
                    Resolved::Target(found) if found == target => {
                        search.found.push((module, occurrence.span.clone()));
                    }
                    Resolved::Target(_) => {}
                    Resolved::Ambiguous => search.uncertain.push((module, occurrence.span.clone())),
                    Resolved::Unknown => {
                        let possible = match occurrence.context {
                            UseContext::Field | UseContext::Label => member,
                            UseContext::Value => false,
                            _ => false,
                        } || (occurrence.context == UseContext::Label && parameter);
                        if possible {
                            search.uncertain.push((module, occurrence.span.clone()));
                        }
                    }
                }
            }
        }
        search
            .found
            .sort_by_key(|(module, span)| (*module, span.start, span.end));
        search
            .found
            .dedup_by(|right, left| left.0 == right.0 && left.1.start == right.1.start);
        search
    }
}

fn brings(occurrence: &NameOccurrence, byte: usize) -> bool {
    occurrence.span.start <= byte && byte <= occurrence.span.end
}

fn brings_span(span: &SyntaxSpan, byte: usize) -> bool {
    span.start <= byte && byte <= span.end
}

fn same_span(left: &SyntaxSpan, right: &SyntaxSpan) -> bool {
    left.start == right.start && left.end == right.end
}

/// The definition each definition is a member of.
fn owner_map(program: &Program) -> HashMap<DefId, DefId> {
    let mut owners = HashMap::new();
    for (def, definition) in program.defs.iter_enumerated() {
        let children = match &definition.kind {
            DefKind::Table(table) => table
                .fields
                .iter()
                .chain(&table.assertions)
                .copied()
                .collect::<Vec<_>>(),
            DefKind::Type(ty) => ty.members.clone(),
            DefKind::Enum(enumeration) => enumeration.variants.clone(),
            DefKind::Variant(variant) => variant.fields.clone(),
            DefKind::Protocol(protocol) => protocol.members.clone(),
            DefKind::Impl(implementation) => implementation.members.clone(),
            _ => Vec::new(),
        };
        for child in children {
            owners.insert(child, def);
        }
    }
    owners
}

/// The identifier span of a declaration within its written extent.
fn selection_span(file: &FileAnalysis, full: &SyntaxSpan, name: &str) -> Option<SyntaxSpan> {
    file.tokens_in(full)
        .iter()
        .find(|token| {
            token.text == name
                && matches!(
                    token.kind,
                    TokenKind::Identifier { .. } | TokenKind::Keyword(_)
                )
        })
        .map(|token| token.span.clone())
}

/// The namespace of a project module at `logical_path`: its directories,
/// then its file stem unless the file is a directory's `main.orna`.
pub(crate) fn module_namespace(logical_path: &str) -> Vec<String> {
    let mut parts = logical_path
        .strip_suffix(".orna")
        .unwrap_or(logical_path)
        .split('/')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if parts.last().is_some_and(|last| last == "main") {
        parts.pop();
    }
    parts
}

/// The HIR module path a logical source path declares.
pub(crate) fn module_path_of(logical: &str) -> orna_hir::ModulePath {
    orna_hir::ModulePath::new(
        module_namespace(logical)
            .into_iter()
            .map(|part| Name::new(&part)),
    )
}

/// The type a field's declared type names, for hover and member checks.
pub(crate) fn declared_type_name(program: &Program, ty: orna_hir::Ty) -> Option<String> {
    match program.tys.kind(ty)? {
        TyKind::Bool => Some("Bool".to_owned()),
        TyKind::Int => Some("Int".to_owned()),
        TyKind::Decimal => Some("Decimal".to_owned()),
        TyKind::Float => Some("Float".to_owned()),
        TyKind::Str => Some("Str".to_owned()),
        TyKind::Blob => Some("Blob".to_owned()),
        TyKind::Uuid => Some("Uuid".to_owned()),
        TyKind::Date => Some("Date".to_owned()),
        TyKind::Instant => Some("Instant".to_owned()),
        TyKind::Duration => Some("Duration".to_owned()),
        TyKind::Period => Some("Period".to_owned()),
        TyKind::Digest => Some("Digest".to_owned()),
        TyKind::Unit => Some("Unit".to_owned()),
        TyKind::Never => Some("Never".to_owned()),
        TyKind::Option(inner) => Some(format!("{}?", declared_type_name(program, *inner)?)),
        TyKind::List(inner) => Some(format!("[{}]", declared_type_name(program, *inner)?)),
        TyKind::Set(inner) => Some(format!("Set<{}>", declared_type_name(program, *inner)?)),
        TyKind::Range(inner) => Some(format!("Range<{}>", declared_type_name(program, *inner)?)),
        TyKind::Relation(inner) => Some(format!(
            "Relation<{}>",
            declared_type_name(program, *inner)?
        )),
        TyKind::Stream(inner) => Some(format!("Stream<{}>", declared_type_name(program, *inner)?)),
        TyKind::Adt(def, _) => Some(program.defs.get(*def)?.name.as_str().to_owned()),
        _ => None,
    }
}
