//! One analysis per document version, and name resolution across the
//! modules a request can see.
//!
//! In a project, modules see each other through `use` imports, as in
//! `orna check`: locals first, then the module's own declarations, then its
//! imports. Documents outside a project share one scope, as if they were one
//! module. Member names resolve only when the receiver's declared type is
//! known; otherwise they stay unresolved and rename refuses rather than
//! guess.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use lsp_types::Uri;
use orna_syntax_v1::{Keyword, SyntaxSpan, Token, TokenKind, TypeExpr, lex, parse_module};

use crate::analysis::{self, EditorParse, EditorSymbol};
use crate::documents::Document;
use crate::names::{
    Declared, DeclaredKind, Hint, Key, NameIndex, NameOccurrence, Role, TypePath, key_of,
};
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
    pub names: NameIndex,
}

impl FileAnalysis {
    pub(crate) fn new(document: Document) -> Self {
        #[cfg(test)]
        ANALYSES.with(|count| count.set(count.get() + 1));
        let parse = parse_module(&document.text);
        let tokens = lex(&document.text).unwrap_or_default();
        let symbols = analysis::declaration_symbols_from_tokens(&parse, &document.text, &tokens);
        let names = NameIndex::new(&parse.value, &tokens);
        Self {
            document,
            parse,
            tokens,
            symbols,
            names,
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
        let end = start
            + self.tokens[start..].partition_point(|token| token.span.end <= span.end);
        &self.tokens[start..end]
    }

    /// The token containing `byte`, or ending at it.
    pub(crate) fn token_at(&self, byte: usize) -> Option<&Token> {
        let index = self.tokens.partition_point(|token| token.span.end < byte);
        self.tokens[index..]
            .iter()
            .take_while(|token| token.span.start <= byte)
            .find(|token| {
                !matches!(token.kind, TokenKind::Eof)
                    && token.span.start <= byte
                    && byte <= token.span.end
            })
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
    pub namespace: Vec<Key>,
    /// The repository-relative path of a project module.
    pub logical_path: Option<String>,
}

impl Module {
    pub(crate) fn project(file: Arc<FileAnalysis>, logical_path: &str) -> Self {
        Self {
            file,
            namespace: module_namespace(logical_path),
            logical_path: Some(logical_path.to_owned()),
        }
    }
}

/// What a name refers to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Target {
    Local { module: usize, binding: usize },
    Declared { module: usize, index: usize },
    Module(Vec<Key>),
    Standard { module: String, name: Key },
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

/// What a value is, after resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Unknown,
    Module(Vec<Key>),
    /// The declaration itself: a table, type, enum or protocol name.
    Static(usize, usize),
    /// A value of the declared type.
    Instance(usize, usize),
    Function(usize, usize),
    /// A list or relation of the inner values.
    Collection(Box<Value>),
}

#[derive(Default)]
struct Imports {
    /// `use path;` and `use path as alias;`.
    aliases: HashMap<Key, Vec<Key>>,
    /// `use path.{name}`.
    names: HashMap<Key, Vec<Key>>,
    /// `use path.*`.
    globs: Vec<Vec<Key>>,
}

/// The modules one request can see.
pub(crate) struct Workspace {
    pub modules: Vec<Module>,
    /// The standard library these modules see, as `orna check` sees it.
    pub standard: Arc<StandardLibrary>,
    /// Detached documents share one scope instead of importing each other.
    shared_scope: bool,
    /// Why some project modules are missing, when loading failed.
    pub incomplete: Option<String>,
    imports: Vec<Imports>,
    /// Top-level declarations of each module by key.
    top_level: Vec<HashMap<Key, Vec<usize>>>,
    /// Member declarations of each module by owner and key.
    members: Vec<HashMap<(usize, Key), Vec<usize>>>,
    by_namespace: HashMap<Vec<Key>, usize>,
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

const MAX_HINT_DEPTH: usize = 32;

impl Workspace {
    /// Modules that see each other through imports.
    pub(crate) fn project(
        modules: Vec<Module>,
        standard: Arc<StandardLibrary>,
        incomplete: Option<String>,
    ) -> Self {
        Self::build(modules, standard, false, incomplete)
    }

    /// Documents outside a project, sharing one scope and the reference
    /// standard library.
    pub(crate) fn detached(files: Vec<Arc<FileAnalysis>>) -> Self {
        let modules = files
            .into_iter()
            .map(|file| Module {
                file,
                namespace: Vec::new(),
                logical_path: None,
            })
            .collect();
        Self::build(modules, StandardLibrary::reference(), true, None)
    }

    fn build(
        modules: Vec<Module>,
        standard: Arc<StandardLibrary>,
        shared_scope: bool,
        incomplete: Option<String>,
    ) -> Self {
        let mut imports = Vec::with_capacity(modules.len());
        let mut top_level = Vec::with_capacity(modules.len());
        let mut members = Vec::with_capacity(modules.len());
        let mut by_namespace = HashMap::new();
        for (index, module) in modules.iter().enumerate() {
            if !shared_scope {
                by_namespace.entry(module.namespace.clone()).or_insert(index);
            }
            imports.push(module_imports(&module.file));
            let mut tops = HashMap::<Key, Vec<usize>>::new();
            let mut owned = HashMap::<(usize, Key), Vec<usize>>::new();
            for (declaration, declared) in module.file.names.declarations().iter().enumerate() {
                match declared.owner {
                    None => tops.entry(declared.key.clone()).or_default().push(declaration),
                    Some(owner) => owned
                        .entry((owner, declared.key.clone()))
                        .or_default()
                        .push(declaration),
                }
            }
            top_level.push(tops);
            members.push(owned);
        }
        Self {
            modules,
            standard,
            shared_scope,
            incomplete,
            imports,
            top_level,
            members,
            by_namespace,
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
            })
            .collect();
        Self::build(
            modules,
            Arc::clone(&self.standard),
            self.shared_scope,
            self.incomplete.clone(),
        )
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

    /// Top-level declarations a name in `module` may refer to without a
    /// qualifier: its own, every detached peer's in a shared scope, and those
    /// it imports by name or glob.
    pub(crate) fn completion_symbols(&self, module: usize) -> Vec<&EditorSymbol> {
        let mut symbols = self.modules[module].file.symbols.iter().collect::<Vec<_>>();
        if self.shared_scope {
            for (index, other) in self.modules.iter().enumerate() {
                if index != module {
                    symbols.extend(other.file.symbols.iter());
                }
            }
            return symbols;
        }
        let imports = &self.imports[module];
        let mut declared = Vec::new();
        for (key, path) in &imports.names {
            if let Resolved::Target(Target::Declared { module, index }) = self.module_member(path, key) {
                declared.push((module, index));
            }
        }
        for path in &imports.globs {
            if let Some(&glob_module) = self.by_namespace.get(path) {
                for declarations in self.top_level[glob_module].values() {
                    declared.extend(
                        declarations
                            .iter()
                            .filter(|index| self.declaration(glob_module, **index).public)
                            .map(|index| (glob_module, *index)),
                    );
                }
            }
        }
        declared.sort_unstable();
        declared.dedup();
        symbols.extend(declared.into_iter().filter_map(|(declared_module, index)| {
            analysis::declared_symbol(
                &self.modules[declared_module].file.symbols,
                &self.declaration(declared_module, index).selection,
            )
        }));
        symbols
    }

    /// The name at `byte` in `module` (a use, declaration or local binding)
    /// with what it refers to.
    pub(crate) fn resolve_at(&self, module: usize, byte: usize) -> Option<(SyntaxSpan, Resolved)> {
        let names = &self.modules[module].file.names;
        if let Some(occurrence) = names.occurrence_at(byte) {
            // A shorthand pattern field is also a binding: the cursor names
            // the local it binds.
            if let Some(binding) = names.binding_at(&occurrence.span)
                && matches!(occurrence.role, Role::PatternField { .. })
            {
                return Some((
                    occurrence.span.clone(),
                    Resolved::Target(Target::Local { module, binding }),
                ));
            }
            return Some((occurrence.span.clone(), self.resolve(module, occurrence)));
        }
        let binding = names.binding_containing(byte)?;
        Some((
            names.binding(binding).selection.clone(),
            Resolved::Target(Target::Local { module, binding }),
        ))
    }

    pub(crate) fn declaration(&self, module: usize, index: usize) -> &Declared {
        self.modules[module].file.names.declaration(index)
    }

    /// What one occurrence refers to.
    pub(crate) fn resolve(&self, module: usize, occurrence: &NameOccurrence) -> Resolved {
        let names = &self.modules[module].file.names;
        let key = occurrence.key.as_str();
        match &occurrence.role {
            Role::Declaration => match names.declared_at(&occurrence.span) {
                Some(index) => Resolved::Target(Target::Declared { module, index }),
                None => Resolved::Unknown,
            },
            Role::Value { .. } => match occurrence.binding {
                Some(binding) => Resolved::Target(Target::Local { module, binding }),
                None => self.lookup(module, key),
            },
            Role::Path { path, index } => self.lookup_path(module, &path[..=*index]),
            Role::Member { receiver, .. } => match self.value_of(module, receiver, 0) {
                Value::Module(path) => self.module_member(&path, key),
                Value::Static(owner_module, owner) => {
                    self.member_of(owner_module, owner, key, MemberUse::Static)
                }
                Value::Instance(owner_module, owner) => {
                    self.member_of(owner_module, owner, key, MemberUse::Instance)
                }
                _ => Resolved::Unknown,
            },
            Role::RecordLabel { owner } | Role::PatternField { owner } => {
                match self.value_of(module, owner, 0) {
                    Value::Instance(owner_module, owner) => {
                        self.member_of(owner_module, owner, key, MemberUse::Field)
                    }
                    _ => Resolved::Unknown,
                }
            }
            Role::ArgumentLabel { callee } => match self.value_of(module, callee, 0) {
                Value::Function(function_module, function) => {
                    let declared = self.declaration(function_module, function);
                    let Some((_, span)) = declared
                        .parameters
                        .iter()
                        .find(|(parameter, _)| parameter == key)
                    else {
                        return Resolved::Unknown;
                    };
                    match self.modules[function_module].file.names.binding_at(span) {
                        Some(binding) => Resolved::Target(Target::Local {
                            module: function_module,
                            binding,
                        }),
                        None => Resolved::Unknown,
                    }
                }
                _ => Resolved::Unknown,
            },
            Role::UsePath { path, index } => {
                let prefix = &path[..=*index];
                if self.module_exists(prefix) {
                    Resolved::Target(Target::Module(prefix.to_vec()))
                } else {
                    Resolved::Unknown
                }
            }
            Role::UseName { module: path } => self.module_member(path, key),
            Role::UseAlias { module: path } => Resolved::Target(Target::Module(path.clone())),
            Role::UnitSuffix => self.unit(module, key),
        }
    }

    /// A unit suffix: a unit of the module itself, else the one unit with
    /// that name in the workspace, as `orna check` resolves suffixes.
    fn unit(&self, module: usize, key: &str) -> Resolved {
        let units = |candidate: usize| {
            self.top_level[candidate]
                .get(key)
                .into_iter()
                .flatten()
                .copied()
                .filter(move |index| self.declaration(candidate, *index).kind == DeclaredKind::Unit)
                .map(move |index| Target::Declared {
                    module: candidate,
                    index,
                })
        };
        if let Some(own) = units(module).next() {
            return Resolved::Target(own);
        }
        let mut found = (0..self.modules.len()).flat_map(units);
        match (found.next(), found.next()) {
            (Some(target), None) => Resolved::Target(target),
            (Some(_), Some(_)) => Resolved::Ambiguous,
            (None, _) => Resolved::Unknown,
        }
    }

    /// A bare name in `module`.
    fn lookup(&self, module: usize, key: &str) -> Resolved {
        if self.shared_scope {
            // Same-name declarations of one module are one overload group,
            // resolved to its first declaration as in a project; only the
            // same name declared by two modules is ambiguous.
            let mut found = self.top_level.iter().enumerate().filter_map(|(index, tops)| {
                tops.get(key).and_then(|declarations| declarations.first()).map(|declaration| {
                    Target::Declared {
                        module: index,
                        index: *declaration,
                    }
                })
            });
            return match (found.next(), found.next()) {
                (Some(target), None) => Resolved::Target(target),
                (Some(_), Some(_)) => Resolved::Ambiguous,
                (None, _) => self.lookup_imports(module, key),
            };
        }
        if let Some(declarations) = self.top_level[module].get(key) {
            return Resolved::Target(Target::Declared {
                module,
                index: declarations[0],
            });
        }
        self.lookup_imports(module, key)
    }

    fn lookup_imports(&self, module: usize, key: &str) -> Resolved {
        let imports = &self.imports[module];
        if let Some(path) = imports.names.get(key) {
            return self.module_member(path, key);
        }
        if let Some(path) = imports.aliases.get(key) {
            return Resolved::Target(Target::Module(path.clone()));
        }
        let mut found = None;
        for path in &imports.globs {
            if let Resolved::Target(target) = self.module_member(path, key) {
                if found.as_ref().is_some_and(|existing| existing != &target) {
                    return Resolved::Ambiguous;
                }
                found = Some(target);
            }
        }
        if let Some(target) = found {
            return Resolved::Target(target);
        }
        if matches!(key, "std" | "sys") {
            return Resolved::Target(Target::Module(vec![key.to_owned()]));
        }
        Resolved::Unknown
    }

    /// A dotted path: a module path, a module member, or a declaration's
    /// member such as an enum variant.
    fn lookup_path(&self, module: usize, path: &[Key]) -> Resolved {
        let Some((first, rest)) = path.split_first() else {
            return Resolved::Unknown;
        };
        let mut current = self.lookup(module, first);
        for key in rest {
            current = match current {
                Resolved::Target(Target::Module(path)) => self.module_member(&path, key),
                Resolved::Target(Target::Declared {
                    module: owner_module,
                    index,
                }) => self.member_of(owner_module, index, key, MemberUse::Static),
                _ => return Resolved::Unknown,
            };
        }
        current
    }

    fn module_exists(&self, path: &[Key]) -> bool {
        if path.first().is_some_and(|root| root == "std") {
            return self.standard.module_exists(&path.join("."));
        }
        self.modules
            .iter()
            .any(|module| module.namespace.starts_with(path))
    }

    /// Member `key` of the module at `path`.
    fn module_member(&self, path: &[Key], key: &str) -> Resolved {
        if path.first().is_some_and(|root| root == "std") {
            let module = path.join(".");
            if self.standard.symbol(&module, key).is_some() {
                return Resolved::Target(Target::Standard {
                    module,
                    name: key.to_owned(),
                });
            }
        } else if let Some(&index) = self.by_namespace.get(path)
            && let Some(declarations) = self.top_level[index].get(key)
        {
            let declaration = declarations[0];
            return if self.declaration(index, declaration).public {
                Resolved::Target(Target::Declared {
                    module: index,
                    index: declaration,
                })
            } else {
                Resolved::Unknown
            };
        }
        let mut child = path.to_vec();
        child.push(key.to_owned());
        if self.module_exists(&child) {
            Resolved::Target(Target::Module(child))
        } else {
            Resolved::Unknown
        }
    }

    fn member_of(&self, module: usize, owner: usize, key: &str, member_use: MemberUse) -> Resolved {
        let Some(declarations) = self.members[module].get(&(owner, key.to_owned())) else {
            return Resolved::Unknown;
        };
        declarations
            .iter()
            .copied()
            .find(|declaration| member_use.admits(self.declaration(module, *declaration).kind))
            .map_or(Resolved::Unknown, |index| {
                Resolved::Target(Target::Declared { module, index })
            })
    }

    fn value_of(&self, module: usize, hint: &Hint, depth: usize) -> Value {
        if depth > MAX_HINT_DEPTH {
            return Value::Unknown;
        }
        match hint {
            Hint::Unknown => Value::Unknown,
            Hint::Name { key, span } => {
                let names = &self.modules[module].file.names;
                if let Some(binding) = names.occurrence(span).and_then(|occurrence| occurrence.binding)
                {
                    return self.value_of(module, &names.binding(binding).hint, depth + 1);
                }
                match self.lookup(module, key) {
                    Resolved::Target(target) => self.value_of_target(&target, depth),
                    _ => Value::Unknown,
                }
            }
            Hint::Type(path) => match self.lookup_path(module, &path_keys(path)) {
                Resolved::Target(Target::Declared {
                    module: declared_module,
                    index,
                }) if self.declaration(declared_module, index).kind.is_type() => {
                    Value::Instance(declared_module, index)
                }
                _ => Value::Unknown,
            },
            Hint::Call(callee) => match self.value_of(module, callee, depth + 1) {
                Value::Function(function_module, function) => {
                    match &self.declaration(function_module, function).ty {
                        Some(result) => self.type_value(function_module, result, depth + 1),
                        None => Value::Unknown,
                    }
                }
                Value::Static(declared_module, index) => Value::Instance(declared_module, index),
                _ => Value::Unknown,
            },
            Hint::Member(receiver, key) => match self.value_of(module, receiver, depth + 1) {
                Value::Instance(owner_module, owner) => {
                    match self.member_of(owner_module, owner, key, MemberUse::Instance) {
                        Resolved::Target(target) => self.value_of_target(&target, depth),
                        _ => Value::Unknown,
                    }
                }
                Value::Static(owner_module, owner) => {
                    match self.member_of(owner_module, owner, key, MemberUse::Static) {
                        Resolved::Target(Target::Declared { module: m, index })
                            if self.declaration(m, index).kind == DeclaredKind::Variant =>
                        {
                            Value::Instance(owner_module, owner)
                        }
                        Resolved::Target(target) => self.value_of_target(&target, depth),
                        _ => Value::Unknown,
                    }
                }
                Value::Module(path) => match self.module_member(&path, key) {
                    Resolved::Target(target) => self.value_of_target(&target, depth),
                    _ => Value::Unknown,
                },
                _ => Value::Unknown,
            },
            Hint::Element(collection) => match self.value_of(module, collection, depth + 1) {
                Value::Static(declared_module, index)
                    if self.declaration(declared_module, index).kind == DeclaredKind::Table =>
                {
                    Value::Instance(declared_module, index)
                }
                Value::Collection(element) => *element,
                _ => Value::Unknown,
            },
            Hint::Collection(element) => {
                Value::Collection(Box::new(self.value_of(module, element, depth + 1)))
            }
            Hint::Owner(item) => self.modules[module]
                .file
                .names
                .declarations()
                .iter()
                .position(|declared| {
                    declared.owner.is_none()
                        && declared.full.start == item.start
                        && declared.full.end == item.end
                })
                .map_or(Value::Unknown, |index| Value::Instance(module, index)),
        }
    }

    /// What naming a resolved target evaluates to.
    fn value_of_target(&self, target: &Target, depth: usize) -> Value {
        match target {
            Target::Module(path) => Value::Module(path.clone()),
            Target::Declared { module, index } => {
                let declared = self.declaration(*module, *index);
                match declared.kind {
                    kind if kind.is_type() => Value::Static(*module, *index),
                    DeclaredKind::Function | DeclaredKind::Method => {
                        Value::Function(*module, *index)
                    }
                    DeclaredKind::Field
                    | DeclaredKind::KeyField
                    | DeclaredKind::PayloadField
                    | DeclaredKind::Static
                    | DeclaredKind::Let => match &declared.ty {
                        Some(ty) => self.type_value(*module, ty, depth + 1),
                        None => Value::Unknown,
                    },
                    _ => Value::Unknown,
                }
            }
            Target::Local { .. } | Target::Standard { .. } => Value::Unknown,
        }
    }

    /// The values of a type written in `module`.
    fn type_value(&self, module: usize, ty: &TypeExpr, depth: usize) -> Value {
        if depth > MAX_HINT_DEPTH {
            return Value::Unknown;
        }
        match ty {
            TypeExpr::Name { path, .. } => {
                let keys = path.iter().map(|segment| key_of(segment)).collect::<Vec<_>>();
                match self.lookup_path(module, &keys) {
                    Resolved::Target(Target::Declared {
                        module: declared_module,
                        index,
                    }) if self.declaration(declared_module, index).kind.is_type() => {
                        Value::Instance(declared_module, index)
                    }
                    _ => Value::Unknown,
                }
            }
            TypeExpr::Optional { inner, .. } => self.type_value(module, inner, depth + 1),
            TypeExpr::List { inner, .. } => {
                Value::Collection(Box::new(self.type_value(module, inner, depth + 1)))
            }
            _ => Value::Unknown,
        }
    }

    /// The name a target is declared with.
    pub(crate) fn target_key(&self, target: &Target) -> Key {
        match target {
            Target::Local { module, binding } => {
                self.modules[*module].file.names.binding(*binding).key.clone()
            }
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
                self.modules[*module].file.names.binding(*binding).kind
                    == crate::names::LocalBindingKind::Parameter
            }
            _ => false,
        };
        let mut search = Search::default();
        if let Target::Local { module, binding } = target {
            search.found.push((
                *module,
                self.modules[*module]
                    .file
                    .names
                    .binding(*binding)
                    .selection
                    .clone(),
            ));
        }
        for (module, entry) in self.modules.iter().enumerate() {
            for occurrence in entry.file.names.occurrences() {
                if occurrence.key != key {
                    continue;
                }
                match self.resolve(module, occurrence) {
                    Resolved::Target(found) if &found == target => {
                        search.found.push((module, occurrence.span.clone()));
                    }
                    Resolved::Target(_) => {}
                    Resolved::Ambiguous => search.uncertain.push((module, occurrence.span.clone())),
                    Resolved::Unknown => {
                        let possible = match &occurrence.role {
                            Role::Member { .. } | Role::RecordLabel { .. } | Role::PatternField { .. } => {
                                member
                            }
                            Role::ArgumentLabel { .. } => parameter,
                            _ => false,
                        };
                        if possible {
                            search.uncertain.push((module, occurrence.span.clone()));
                        }
                    }
                }
            }
        }
        search.found.sort_by_key(|(module, span)| (*module, span.start, span.end));
        search
            .found
            .dedup_by(|right, left| left.0 == right.0 && left.1.start == right.1.start);
        search
    }
}

#[derive(Clone, Copy)]
enum MemberUse {
    /// `Type.member`: variants, statics and functions of the declaration.
    Static,
    /// `value.member`: fields and functions of a value of the declaration.
    Instance,
    /// A record label or pattern field: fields only.
    Field,
}

impl MemberUse {
    fn admits(self, kind: DeclaredKind) -> bool {
        match self {
            MemberUse::Static => matches!(
                kind,
                DeclaredKind::Variant | DeclaredKind::Static | DeclaredKind::Method
            ),
            MemberUse::Instance => matches!(
                kind,
                DeclaredKind::Field
                    | DeclaredKind::KeyField
                    | DeclaredKind::PayloadField
                    | DeclaredKind::Method
            ),
            MemberUse::Field => matches!(
                kind,
                DeclaredKind::Field | DeclaredKind::KeyField | DeclaredKind::PayloadField
            ),
        }
    }
}

impl DeclaredKind {
    /// Declarations whose name denotes a type of values.
    pub(crate) fn is_type(self) -> bool {
        matches!(
            self,
            DeclaredKind::Table
                | DeclaredKind::Type
                | DeclaredKind::Enum
                | DeclaredKind::Protocol
                | DeclaredKind::Variant
        )
    }
}

fn path_keys(path: &TypePath) -> Vec<Key> {
    path.segments.iter().map(|(key, _)| key.clone()).collect()
}

fn module_imports(file: &FileAnalysis) -> Imports {
    let mut imports = Imports::default();
    for item in &file.parse.value.items {
        let orna_syntax_v1::Declaration::Use { path, tail } = &item.declaration else {
            continue;
        };
        let keys = path
            .iter()
            .map(|segment| key_of(&segment.name))
            .collect::<Vec<_>>();
        match tail {
            orna_syntax_v1::UseTail::None => {
                if let Some(last) = keys.last() {
                    imports.aliases.insert(last.clone(), keys.clone());
                }
            }
            orna_syntax_v1::UseTail::Alias { name, .. } => {
                imports.aliases.insert(key_of(name), keys);
            }
            orna_syntax_v1::UseTail::Names(names) => {
                for name in names {
                    imports.names.insert(key_of(&name.name), keys.clone());
                }
            }
            orna_syntax_v1::UseTail::Glob { .. } => imports.globs.push(keys),
        }
    }
    imports
}

/// The namespace of a project module at `logical_path`: its directories,
/// then its file stem unless the file is a directory's `main.orna`.
pub(crate) fn module_namespace(logical_path: &str) -> Vec<Key> {
    let mut parts = logical_path
        .strip_suffix(".orna")
        .unwrap_or(logical_path)
        .split('/')
        .map(key_of)
        .collect::<Vec<_>>();
    if parts.last().is_some_and(|last| last == "main") {
        parts.pop();
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(uri: &str, text: &str) -> Arc<FileAnalysis> {
        Arc::new(FileAnalysis::new(Document::new(
            uri.parse().unwrap(),
            text.to_owned(),
            1,
        )))
    }

    fn at(text: &str, needle: &str, offset: usize) -> usize {
        text.find(needle).unwrap() + offset
    }

    /// The audit fixture for U03-09: a function and a field share a name, and
    /// a call sits inside an implementation body.
    const RENAME_FIXTURE: &str = "pub table Contact(id: Str) {\n    title: Str,\n}\n\
        pub fn title(contact: Contact): Str = contact.title;\n\
        pub fn show(contact: Contact): Str = title(contact);\n\
        pub fn shout(value: Str): Str = value;\n\
        pub type Badge {\n    pub text: Str,\n    impl Display {\n        \
        fn display(self, context: DisplayContext): Str = shout(self.text);\n    }\n}\n";

    fn spans<'a>(workspace: &'a Workspace, search: &Search) -> Vec<&'a str> {
        search
            .found
            .iter()
            .map(|(module, span)| &workspace.file(*module).text()[span.start..span.end])
            .collect()
    }

    #[test]
    fn functions_and_fields_with_one_name_resolve_apart() {
        let workspace = Workspace::detached(vec![file("file:///w/main.orna", RENAME_FIXTURE)]);
        let function = at(RENAME_FIXTURE, "fn title", 3);
        let (_, resolved) = workspace.resolve_at(0, function).unwrap();
        let target = resolved.target().unwrap();
        let search = workspace.search(&target);
        assert_eq!(
            search
                .found
                .iter()
                .map(|(_, span)| span.start)
                .collect::<Vec<_>>(),
            [function, at(RENAME_FIXTURE, "title(contact)", 0)]
        );
        assert!(search.uncertain.is_empty());

        let field = at(RENAME_FIXTURE, "title: Str", 0);
        let (_, resolved) = workspace.resolve_at(0, field).unwrap();
        let search = workspace.search(&resolved.target().unwrap());
        assert_eq!(
            search
                .found
                .iter()
                .map(|(_, span)| span.start)
                .collect::<Vec<_>>(),
            [field, at(RENAME_FIXTURE, "contact.title", 8)]
        );
        assert_eq!(spans(&workspace, &search), ["title", "title"]);
    }

    #[test]
    fn calls_and_self_members_inside_implementations_resolve() {
        let workspace = Workspace::detached(vec![file("file:///w/main.orna", RENAME_FIXTURE)]);
        let shout = at(RENAME_FIXTURE, "fn shout", 3);
        let (_, resolved) = workspace.resolve_at(0, shout).unwrap();
        let search = workspace.search(&resolved.target().unwrap());
        assert_eq!(search.found.len(), 2);
        assert_eq!(search.found[1].1.start, at(RENAME_FIXTURE, "shout(self", 0));

        let text = at(RENAME_FIXTURE, "self.text", 5);
        let (_, resolved) = workspace.resolve_at(0, text).unwrap();
        let Some(Target::Declared { index, .. }) = resolved.target() else {
            panic!("self.text resolves to the field");
        };
        assert_eq!(workspace.declaration(0, index).kind, DeclaredKind::Field);
    }

    #[test]
    fn pipeline_lambda_and_let_rows_know_their_table() {
        let source = "pub table Contact(id: Str) {\n    name: Str,\n}\n\
            pub fn lookup(key: Str): Str {\n    \
            let row = Contact | one(item => item.id == key);\n    row.name\n}\n";
        let workspace = Workspace::detached(vec![file("file:///w/main.orna", source)]);
        for needle in ["item.id", "row.name"] {
            let byte = at(source, needle, needle.find('.').unwrap() + 1);
            let (occurrence, resolved) = workspace.resolve_at(0, byte).unwrap();
            assert!(
                matches!(resolved, Resolved::Target(Target::Declared { .. })),
                "{needle}: {occurrence:?} {resolved:?}"
            );
        }
    }

    #[test]
    fn project_imports_resolve_qualified_named_and_private_members() {
        let main = "use util;\nuse util.{helper};\n\
            pub fn total(): Int = util.helper() + helper() + hidden();\n";
        let util = "pub fn helper(): Int = 1;\nfn hidden(): Int = 2;\n";
        let workspace = Workspace::project(
            vec![
                Module::project(file("file:///p/main.orna", main), "main.orna"),
                Module::project(file("file:///p/util.orna", util), "util.orna"),
            ],
            StandardLibrary::none(),
            None,
        );
        let declaration = at(util, "helper", 0);
        let (_, resolved) = workspace.resolve_at(1, declaration).unwrap();
        let search = workspace.search(&resolved.target().unwrap());
        let found = search
            .found
            .iter()
            .map(|(module, span)| (*module, span.start))
            .collect::<Vec<_>>();
        assert_eq!(
            found,
            [
                (0, at(main, "{helper}", 1)),
                (0, at(main, "util.helper", 5)),
                (0, at(main, "+ helper", 2)),
                (1, declaration),
            ]
        );
        let (_, hidden) = workspace.resolve_at(0, at(main, "hidden", 0)).unwrap();
        assert_eq!(hidden, Resolved::Unknown, "private functions are not imported");
    }

    #[test]
    fn detached_documents_share_one_scope_and_duplicates_are_ambiguous() {
        let first = "pub fn shared(value: Int): Int = value;\n\
            pub fn caller(value: Int): Int = shared(value);\n";
        let second = "pub fn shared(value: Int): Int = value + 1;\n";
        let workspace = Workspace::detached(vec![
            file("file:///w/a.orna", first),
            file("file:///w/b.orna", second),
        ]);
        let (_, resolved) = workspace
            .resolve_at(0, at(first, "shared(value)", 0))
            .unwrap();
        assert_eq!(resolved, Resolved::Ambiguous);
    }

    #[test]
    fn module_namespaces_follow_logical_paths() {
        assert!(module_namespace("main.orna").is_empty());
        assert_eq!(module_namespace("util.orna"), ["util"]);
        assert_eq!(module_namespace("sensors/greenhouse/main.orna"), ["sensors", "greenhouse"]);
    }
}
