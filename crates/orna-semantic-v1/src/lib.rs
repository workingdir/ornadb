//! Read-only semantic loading for the frozen Orna 1.0 syntax tree.
//!
//! This crate accepts source bytes supplied by its caller.  It deliberately
//! does not read the host filesystem, execute expressions, or install a
//! standard library.  Its result is a deterministic module graph, headers,
//! imports, conservative type summaries, effect/failure summaries, and
//! declaration-assertion plans.

use std::collections::{BTreeMap, BTreeSet};

use orna_foundation_v1::{Diagnostic, DiagnosticSeverity, SafeText};
use orna_syntax_v1::{
    Declaration, Expr, FieldInitializer, Item, LiteralKind, Pattern, Statement, SyntaxTree,
    TypeExpr, UseTail, Visibility, parse_module_with_file,
};
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::{UnicodeNormalization, is_nfc};

pub const DIAG_BAD_PATH: &str = "ORNA-S001-PATH";
pub const DIAG_NAMESPACE: &str = "ORNA-S002-NAMESPACE";
pub const DIAG_RESERVED: &str = "ORNA-S003-RESERVED";
pub const DIAG_IMPORT: &str = "ORNA-S010-IMPORT";
pub const DIAG_AMBIGUOUS: &str = "ORNA-S011-AMBIGUOUS";
pub const DIAG_UNRESOLVED: &str = "ORNA-S012-UNRESOLVED";
pub const DIAG_DUPLICATE: &str = "ORNA-S013-DUPLICATE";
pub const DIAG_ANNOTATION: &str = "ORNA-S020-ANNOTATION";
pub const DIAG_TYPE: &str = "ORNA-S021-TYPE";
pub const DIAG_UNSUPPORTED: &str = "ORNA-S022-UNSUPPORTED";
pub const DIAG_ASSERTION: &str = "ORNA-A091-004";
pub const DIAG_ASSERTION_SCOPE: &str = "ORNA-A091-012";
pub const DIAG_ASSERTION_EFFECT: &str = "ORNA-A091-007";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleInput {
    /// Repository-relative, slash-separated `.orna` logical path.
    pub logical_path: String,
    /// Source supplied by the caller; never retained in diagnostics.
    pub source: String,
    /// Explicit prelude exports for this module.  The AST has no prelude
    /// declaration node, so a repository/catalogue adapter supplies this set.
    pub prelude_exports: BTreeSet<String>,
}
impl ModuleInput {
    pub fn new(logical_path: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            logical_path: logical_path.into(),
            source: source.into(),
            prelude_exports: BTreeSet::new(),
        }
    }
    pub fn with_prelude_exports(
        mut self,
        names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.prelude_exports = names.into_iter().map(Into::into).collect();
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Namespace(pub Vec<String>);
impl Namespace {
    pub fn display(&self) -> String {
        self.0.join(".")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Type {
    Int,
    Decimal,
    Float,
    Date,
    Instant,
    Text,
    Bool,
    Null,
    List(Box<Type>),
    Record(BTreeMap<String, Type>),
    Tuple(Vec<Type>),
    Function {
        parameters: Vec<Type>,
        result: Box<Type>,
    },
    Named(String),
    /// A diagnostic has already been emitted; this is never a dynamic `Any`.
    Error,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffectSummary {
    pub effects: BTreeSet<String>,
    pub may_fail: bool,
}
impl EffectSummary {
    fn join(&mut self, other: &Self) {
        self.effects.extend(other.effects.iter().cloned());
        self.may_fail |= other.may_fail;
    }
    fn forbidden_for_assertion(&self) -> bool {
        !self.effects.is_empty() || self.may_fail
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SymbolKind {
    Function,
    Table,
    Type,
    Enum,
    Protocol,
    Dimension,
    Unit,
    Let,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Symbol {
    pub kind: SymbolKind,
    pub ty: Type,
    pub public: bool,
    pub effects: EffectSummary,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleHeader {
    pub namespace: Namespace,
    pub exports: BTreeMap<String, Symbol>,
    pub symbols: BTreeMap<String, Symbol>,
    pub prelude_exports: BTreeSet<String>,
}

/// Explicit, caller-provided declarations available in addition to source
/// modules.  A catalogue is data, not a loader: this crate never reads a
/// standard-library directory or treats an absent name as a standard name.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Catalogue {
    modules: BTreeMap<Namespace, ModuleHeader>,
}
impl Catalogue {
    pub fn empty() -> Self {
        Self::default()
    }

    /// The stable core surface represented by the authoritative `stdlib/std`
    /// corpus.  It is intentionally a small declaration catalogue rather than
    /// executable standard-library source.
    pub fn authoritative_core() -> Self {
        let mut modules = BTreeMap::new();
        let primitive_types = [
            ("BOOLEAN", Type::Bool),
            ("BOOL", Type::Bool),
            ("INTEGER", Type::Int),
            ("INT", Type::Int),
            ("BIGINT", Type::Int),
            ("FLOAT", Type::Float),
            ("DECIMAL", Type::Decimal),
            ("TEXT", Type::Text),
            ("DATE", Type::Date),
            ("TIMESTAMP", Type::Instant),
            ("VOID", Type::Null),
        ];
        let root_types = [
            ("UUID", Type::Named("std.UUID".into())),
            ("TIME", Type::Named("std.TIME".into())),
            ("DURATION", Type::Named("std.DURATION".into())),
            ("BYTES", Type::Named("std.BINARY_LARGE_OBJECT".into())),
            ("Action", Type::Named("std.Action".into())),
            ("Rows", Type::Named("std.Rows".into())),
            ("JsonValue", Type::Named("std.JsonValue".into())),
            ("Document", Type::Named("std.Document".into())),
            ("ByteStream", Type::Named("std.ByteStream".into())),
            ("UI", Type::Named("std.UI".into())),
        ];
        let mut root = Vec::new();
        root.extend(primitive_types.iter().cloned());
        root.extend(root_types.iter().cloned());
        modules.insert(
            Namespace(vec!["std".into()]),
            catalogue_module(
                Namespace(vec!["std".into()]),
                root,
                primitive_types
                    .iter()
                    .map(|(name, _)| *name)
                    .chain(root_types.iter().map(|(name, _)| *name)),
            ),
        );
        modules.insert(
            Namespace(vec!["std".into(), "types".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "types".into()]),
                primitive_types
                    .iter()
                    .map(|(name, ty)| (*name, ty.clone()))
                    .chain(root_types.iter().map(|(name, ty)| (*name, ty.clone()))),
                std::iter::empty::<&str>(),
            ),
        );
        modules.insert(
            Namespace(vec!["std".into(), "math".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "math".into()]),
                [
                    ("increment", function(vec![Type::Int], Type::Int)),
                    ("decrement", function(vec![Type::Int], Type::Int)),
                    ("is_zero", function(vec![Type::Int], Type::Bool)),
                    ("min", function(vec![Type::Int, Type::Int], Type::Int)),
                    ("max", function(vec![Type::Int, Type::Int], Type::Int)),
                    (
                        "clamp",
                        function(vec![Type::Int, Type::Int, Type::Int], Type::Int),
                    ),
                ],
                std::iter::empty::<&str>(),
            ),
        );
        modules.insert(
            Namespace(vec!["std".into(), "invoke".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "invoke".into()]),
                [("echo", function(vec![Type::Int], Type::Int))],
                std::iter::empty::<&str>(),
            ),
        );
        let action = Type::Named("std.Action".into());
        let rows = Type::Named("std.Rows".into());
        let json = Type::Named("std.JsonValue".into());
        let document = Type::Named("std.Document".into());
        let byte_stream = Type::Named("std.ByteStream".into());
        modules.insert(
            Namespace(vec!["std".into(), "action".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "action".into()]),
                [("Action", action)],
                std::iter::empty::<&str>(),
            ),
        );
        modules.insert(
            Namespace(vec!["std".into(), "data".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "data".into()]),
                [("Rows", rows.clone())],
                std::iter::empty::<&str>(),
            ),
        );
        modules.insert(
            Namespace(vec!["std".into(), "json".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "json".into()]),
                [
                    ("Value", json.clone()),
                    ("encode", function(vec![json], byte_stream.clone())),
                ],
                std::iter::empty::<&str>(),
            ),
        );
        modules.insert(
            Namespace(vec!["std".into(), "terminal".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "terminal".into()]),
                [
                    ("Document", document.clone()),
                    ("present_table", function(vec![rows], document)),
                ],
                std::iter::empty::<&str>(),
            ),
        );
        modules.insert(
            Namespace(vec!["std".into(), "io".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "io".into()]),
                [("ByteStream", byte_stream)],
                std::iter::empty::<&str>(),
            ),
        );
        let ui = Type::Named("std.UI".into());
        modules.insert(
            Namespace(vec!["std".into(), "ui".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "ui".into()]),
                [
                    ("UI", ui.clone()),
                    ("text", function(vec![Type::Text], ui.clone())),
                    ("button", function(vec![Type::Text, Type::Bool], ui.clone())),
                    ("panel", function(vec![ui.clone()], ui.clone())),
                    ("row", function(vec![ui.clone()], ui.clone())),
                    ("column", function(vec![ui.clone()], ui.clone())),
                    (
                        "text_input",
                        function(vec![Type::Text, Type::Text, Type::Bool], ui.clone()),
                    ),
                    ("tabs", function(vec![ui.clone()], ui.clone())),
                    ("window", function(vec![Type::Text, ui.clone()], ui)),
                ],
                std::iter::empty::<&str>(),
            ),
        );
        modules.insert(
            Namespace(vec!["std".into(), "cli".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "cli".into()]),
                [("repl", function(vec![], Type::Named("std.UI".into())))],
                std::iter::empty::<&str>(),
            ),
        );
        Self { modules }
    }
}

fn catalogue_module<I, N, P, Q>(
    namespace: Namespace,
    symbols: I,
    prelude_exports: P,
) -> ModuleHeader
where
    I: IntoIterator<Item = (N, Type)>,
    N: Into<String>,
    P: IntoIterator<Item = Q>,
    Q: Into<String>,
{
    let symbols = symbols
        .into_iter()
        .map(|(name, ty)| {
            (
                name.into(),
                Symbol {
                    kind: if matches!(ty, Type::Function { .. }) {
                        SymbolKind::Function
                    } else {
                        SymbolKind::Type
                    },
                    public: true,
                    effects: EffectSummary::default(),
                    ty,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    ModuleHeader {
        namespace,
        exports: symbols.clone(),
        symbols,
        prelude_exports: prelude_exports.into_iter().map(Into::into).collect(),
    }
}

fn function(parameters: Vec<Type>, result: Type) -> Type {
    Type::Function {
        parameters,
        result: Box::new(result),
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssertionOwner {
    Module,
    Table(String),
    RefinedType(String),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssertionPlan {
    pub owner: AssertionOwner,
    pub dependencies: BTreeSet<String>,
    pub effects: EffectSummary,
}
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub modules: BTreeMap<Namespace, ModuleHeader>,
    pub assertions: BTreeMap<Namespace, Vec<AssertionPlan>>,
    pub diagnostics: Vec<Diagnostic>,
}
impl Analysis {
    pub fn is_ok(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

/// Load and check caller-provided modules.  `sys` is a built-in namespace,
/// while `std` remains absent unless an adapter supplies it separately; source
/// modules may not define either reserved root.
pub fn analyze(inputs: &[ModuleInput]) -> Analysis {
    analyze_with_catalogue(inputs, &Catalogue::empty())
}

/// Load source modules with an explicit declaration catalogue.  Names not in
/// source or this catalogue remain unresolved; no profile is installed by
/// default.
pub fn analyze_with_catalogue(inputs: &[ModuleInput], catalogue: &Catalogue) -> Analysis {
    let mut result = Analysis {
        modules: catalogue.modules.clone(),
        ..Analysis::default()
    };
    let mut parsed = Vec::new();
    let mut siblings: BTreeMap<Vec<String>, BTreeMap<String, String>> = BTreeMap::new();
    for input in inputs {
        let Some(namespace) = namespace_for_path(&input.logical_path, &mut result.diagnostics)
        else {
            continue;
        };
        if matches!(namespace.0.first().map(String::as_str), Some("sys" | "std")) {
            result.diagnostics.push(diag(
                DIAG_RESERVED,
                "reserved namespace cannot be defined by source",
            ));
            continue;
        }
        for (parent, child) in namespace
            .0
            .iter()
            .enumerate()
            .map(|(i, n)| (namespace.0[..i].to_vec(), n))
        {
            let folded = unicode_sibling_key(child);
            let entry = siblings.entry(parent).or_default();
            if let Some(prior) = entry.insert(folded, child.clone())
                && prior != *child
            {
                result.diagnostics.push(diag(
                    DIAG_NAMESPACE,
                    "Unicode-folded sibling namespace collision",
                ));
            }
        }
        if result.modules.contains_key(&namespace) {
            result
                .diagnostics
                .push(diag(DIAG_NAMESPACE, "duplicate module namespace"));
            continue;
        }
        let parse = parse_module_with_file(&input.source, &input.logical_path);
        for _ in parse.diagnostics {
            result.diagnostics.push(diag(
                "ORNA-S000-PARSE",
                "source was not admitted by the frozen syntax parser",
            ));
        }
        let header = collect_header(
            &namespace,
            &parse.value,
            &input.prelude_exports,
            &mut result.diagnostics,
        );
        result.modules.insert(namespace.clone(), header);
        parsed.push((namespace, parse.value));
    }
    for (namespace, tree) in &parsed {
        let Some(header) = result.modules.get(namespace).cloned() else {
            continue;
        };
        let scope = resolve_imports(
            namespace,
            tree,
            &header,
            &result.modules,
            &mut result.diagnostics,
        );
        let mut symbols = header.symbols.clone();
        let mut plans = Vec::new();
        for item in &tree.items {
            check_item(
                item,
                &mut symbols,
                &scope,
                &mut plans,
                &mut result.diagnostics,
            );
        }
        if let Some(module) = result.modules.get_mut(namespace) {
            module.symbols = symbols;
            module.exports = module
                .symbols
                .iter()
                .filter(|(_, s)| s.public)
                .map(|(n, s)| (n.clone(), s.clone()))
                .collect();
        }
        result.assertions.insert(namespace.clone(), plans);
    }
    result
        .diagnostics
        .sort_by(|a, b| a.code().cmp(b.code()).then(a.message().cmp(b.message())));
    result
}

/// Host-independent NFKC case fold key used for sibling collision checking.
fn unicode_sibling_key(value: &str) -> String {
    value.nfkc().case_fold().collect()
}

fn namespace_for_path(path: &str, diagnostics: &mut Vec<Diagnostic>) -> Option<Namespace> {
    if path.is_empty() || path.starts_with('/') || path.contains("\\") || !path.ends_with(".orna") {
        diagnostics.push(diag(
            DIAG_BAD_PATH,
            "module path must be a relative .orna path",
        ));
        return None;
    }
    let mut parts = path.split('/').collect::<Vec<_>>();
    let file = parts.pop()?;
    if parts
        .iter()
        .any(|part| part.is_empty() || matches!(*part, "." | "..") || !is_nfc(part))
        || !is_nfc(file)
    {
        diagnostics.push(diag(
            DIAG_BAD_PATH,
            "module path components must be NFC logical components",
        ));
        return None;
    }
    let stem = file.strip_suffix(".orna")?;
    if stem.is_empty() || stem.contains('.') {
        diagnostics.push(diag(
            DIAG_BAD_PATH,
            "module filename has no valid logical stem",
        ));
        return None;
    }
    let mut names = parts.into_iter().map(str::to_owned).collect::<Vec<_>>();
    if stem != "main" {
        names.push(stem.to_owned());
    }
    Some(Namespace(names))
}

fn collect_header(
    namespace: &Namespace,
    tree: &SyntaxTree,
    prelude: &BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ModuleHeader {
    let mut symbols = BTreeMap::new();
    for item in &tree.items {
        let Some((name, kind, ty)) = declared_symbol(item, diagnostics) else {
            continue;
        };
        let public = matches!(item.visibility, Visibility::Public { .. });
        if symbols
            .insert(
                name,
                Symbol {
                    kind,
                    ty,
                    public,
                    effects: EffectSummary::default(),
                },
            )
            .is_some()
        {
            diagnostics.push(diag(DIAG_DUPLICATE, "duplicate declaration name"));
        }
    }
    let exports = symbols
        .iter()
        .filter(|(_, s)| s.public)
        .map(|(n, s)| (n.clone(), s.clone()))
        .collect();
    ModuleHeader {
        namespace: namespace.clone(),
        exports,
        symbols,
        prelude_exports: prelude.clone(),
    }
}
fn declared_symbol(
    item: &Item,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(String, SymbolKind, Type)> {
    match &item.declaration {
        Declaration::Function { signature, .. } => {
            let parameters = signature
                .parameters
                .iter()
                .map(|p| {
                    p.annotation.as_ref().map(type_of).unwrap_or_else(|| {
                        diagnostics.push(diag(
                            DIAG_ANNOTATION,
                            "function parameter needs a static annotation",
                        ));
                        Type::Error
                    })
                })
                .collect();
            Some((
                signature.name.clone(),
                SymbolKind::Function,
                Type::Function {
                    parameters,
                    result: Box::new(
                        signature
                            .result
                            .as_ref()
                            .map(type_of)
                            .unwrap_or(Type::Error),
                    ),
                },
            ))
        }
        Declaration::Table { name, .. } => {
            Some((name.clone(), SymbolKind::Table, Type::Named(name.clone())))
        }
        Declaration::Type { name, .. } => {
            Some((name.clone(), SymbolKind::Type, Type::Named(name.clone())))
        }
        Declaration::Enum { name, .. } => {
            Some((name.clone(), SymbolKind::Enum, Type::Named(name.clone())))
        }
        Declaration::Protocol { name, .. } => Some((
            name.clone(),
            SymbolKind::Protocol,
            Type::Named(name.clone()),
        )),
        Declaration::Dimension { name, .. } => Some((
            name.clone(),
            SymbolKind::Dimension,
            Type::Named(name.clone()),
        )),
        Declaration::Unit { name, .. } => {
            Some((name.clone(), SymbolKind::Unit, Type::Named(name.clone())))
        }
        Declaration::Let {
            pattern: Pattern::Name(name, _),
            annotation,
            ..
        } => Some((
            name.clone(),
            SymbolKind::Let,
            annotation.as_ref().map(type_of).unwrap_or(Type::Error),
        )),
        _ => None,
    }
}
fn type_of(ty: &TypeExpr) -> Type {
    match ty {
        TypeExpr::Name { path, .. } => {
            primitive(&path.join(".")).unwrap_or_else(|| Type::Named(path.join(".")))
        }
        TypeExpr::List { inner, .. } => Type::List(Box::new(type_of(inner))),
        TypeExpr::Record { fields, .. } => Type::Record(
            fields
                .iter()
                .map(|(n, t, _)| (n.clone(), type_of(t)))
                .collect(),
        ),
        TypeExpr::Tuple { elements, .. } => Type::Tuple(elements.iter().map(type_of).collect()),
        TypeExpr::Function {
            parameters, result, ..
        } => Type::Function {
            parameters: parameters.iter().map(type_of).collect(),
            result: Box::new(type_of(result)),
        },
        TypeExpr::Optional { inner, .. } => Type::Named(format!("Optional<{:?}>", type_of(inner))),
        TypeExpr::Product { .. } => Type::Error,
    }
}
fn primitive(name: &str) -> Option<Type> {
    Some(match name {
        "Int" => Type::Int,
        "Decimal" => Type::Decimal,
        "Float" => Type::Float,
        "Date" => Type::Date,
        "Instant" => Type::Instant,
        "Text" | "String" => Type::Text,
        "Bool" => Type::Bool,
        "Null" => Type::Null,
        "BOOLEAN" | "BOOL" => Type::Bool,
        "INTEGER" | "INT" | "BIGINT" => Type::Int,
        "FLOAT" => Type::Float,
        "DECIMAL" => Type::Decimal,
        "TEXT" => Type::Text,
        "DATE" => Type::Date,
        "TIMESTAMP" => Type::Instant,
        "VOID" => Type::Null,
        "UUID" => Type::Named("std.UUID".into()),
        "TIME" => Type::Named("std.TIME".into()),
        "DURATION" => Type::Named("std.DURATION".into()),
        "BYTES" => Type::Named("std.BINARY_LARGE_OBJECT".into()),
        "Action" => Type::Named("std.Action".into()),
        "Rows" => Type::Named("std.Rows".into()),
        "JsonValue" => Type::Named("std.JsonValue".into()),
        "Document" => Type::Named("std.Document".into()),
        "ByteStream" => Type::Named("std.ByteStream".into()),
        "UI" => Type::Named("std.UI".into()),
        _ => return None,
    })
}

#[derive(Default)]
struct Scope {
    names: BTreeMap<String, Symbol>,
    ambiguous: BTreeSet<String>,
}
fn resolve_imports(
    namespace: &Namespace,
    tree: &SyntaxTree,
    header: &ModuleHeader,
    modules: &BTreeMap<Namespace, ModuleHeader>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Scope {
    let mut scope = Scope {
        names: header.symbols.clone(),
        ambiguous: BTreeSet::new(),
    };
    let mut explicit = BTreeMap::<String, Symbol>::new();
    let mut glob = BTreeMap::<String, Vec<Symbol>>::new();
    for item in &tree.items {
        let Declaration::Use { path, tail } = &item.declaration else {
            continue;
        };
        let target = Namespace(path.iter().map(|x| x.name.clone()).collect());
        if target.0.first().is_some_and(|r| r == "sys") {
            let binding = Symbol {
                kind: SymbolKind::Let,
                ty: Type::Named(target.display()),
                public: true,
                effects: EffectSummary::default(),
            };
            match tail {
                UseTail::None => {
                    if let Some(name) = target.0.last() {
                        insert_explicit(&mut explicit, name.clone(), binding, diagnostics);
                    }
                }
                UseTail::Alias { name, .. } if name != "_" => {
                    insert_explicit(&mut explicit, name.clone(), binding, diagnostics);
                }
                UseTail::Alias { .. } | UseTail::Glob { .. } | UseTail::Names(_) => {
                    diagnostics.push(diag(
                        DIAG_IMPORT,
                        "built-in namespace has no source export table",
                    ));
                }
            }
            continue;
        }
        let Some(module) = modules.get(&target) else {
            diagnostics.push(diag(DIAG_IMPORT, "imported module is unavailable"));
            continue;
        };
        match tail {
            UseTail::None => {
                let Some(name) = target.0.last() else {
                    diagnostics.push(diag(DIAG_IMPORT, "root namespace requires an alias"));
                    continue;
                };
                insert_explicit(
                    &mut explicit,
                    name.clone(),
                    Symbol {
                        kind: SymbolKind::Let,
                        ty: Type::Named(target.display()),
                        public: true,
                        effects: EffectSummary::default(),
                    },
                    diagnostics,
                );
            }
            UseTail::Alias { name, .. } if name == "_" => {
                for name in &module.prelude_exports {
                    if let Some(symbol) = module.exports.get(name) {
                        insert_explicit(&mut explicit, name.clone(), symbol.clone(), diagnostics);
                    } else {
                        diagnostics.push(diag(DIAG_IMPORT, "prelude export is not public"));
                    }
                }
            }
            UseTail::Alias { name, .. } => insert_explicit(
                &mut explicit,
                name.clone(),
                Symbol {
                    kind: SymbolKind::Let,
                    ty: Type::Named(target.display()),
                    public: true,
                    effects: EffectSummary::default(),
                },
                diagnostics,
            ),
            UseTail::Names(names) => {
                for import in names {
                    if let Some(symbol) = module.exports.get(&import.name) {
                        insert_explicit(
                            &mut explicit,
                            import.name.clone(),
                            symbol.clone(),
                            diagnostics,
                        )
                    } else {
                        diagnostics
                            .push(diag(DIAG_IMPORT, "named import is unavailable or private"));
                    }
                }
            }
            UseTail::Glob { .. } => {
                for (name, symbol) in &module.exports {
                    glob.entry(name.clone()).or_default().push(symbol.clone());
                }
            }
        }
    }
    for (name, symbol) in explicit {
        scope.names.entry(name).or_insert(symbol);
    }
    for (name, candidates) in glob {
        if scope.names.contains_key(&name) {
            continue;
        }
        if candidates.len() == 1 {
            scope.names.insert(name, candidates[0].clone());
        } else {
            scope.ambiguous.insert(name);
        }
    }
    let _ = namespace;
    scope
}
fn insert_explicit(
    map: &mut BTreeMap<String, Symbol>,
    name: String,
    symbol: Symbol,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if map.insert(name, symbol).is_some() {
        diagnostics.push(diag(DIAG_AMBIGUOUS, "conflicting explicit imports"));
    }
}

fn check_item(
    item: &Item,
    symbols: &mut BTreeMap<String, Symbol>,
    scope: &Scope,
    plans: &mut Vec<AssertionPlan>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match &item.declaration {
        Declaration::Let {
            pattern,
            annotation,
            value,
        } => {
            let inferred = infer(value, scope, &BTreeMap::new(), diagnostics);
            if let Some(expected) = annotation.as_ref().map(type_of) {
                require_same(&expected, &inferred.ty, diagnostics);
            }
            bind_pattern(pattern, inferred.ty, symbols, diagnostics);
        }
        Declaration::Function { signature, body } => {
            let mut local = BTreeMap::new();
            for parameter in &signature.parameters {
                bind_pattern(
                    &parameter.pattern,
                    parameter
                        .annotation
                        .as_ref()
                        .map(type_of)
                        .unwrap_or(Type::Error),
                    &mut local,
                    diagnostics,
                );
            }
            let inferred = infer(body, scope, &local, diagnostics);
            if let Some(expected) = signature.result.as_ref().map(type_of) {
                require_same(&expected, &inferred.ty, diagnostics);
            }
            if let Some(symbol) = symbols.get_mut(&signature.name) {
                symbol.effects = inferred.effects;
                if signature.result.is_none()
                    && let Type::Function { result, .. } = &mut symbol.ty
                {
                    **result = inferred.ty;
                }
            }
        }
        Declaration::Assertion { value } => {
            let inferred = infer(value, scope, &BTreeMap::new(), diagnostics);
            assertion(AssertionOwner::Module, value, inferred, plans, diagnostics);
        }
        Declaration::Table { name, members, .. } => {
            for member in members {
                match member {
                    orna_syntax_v1::TableMember::Assertion { value, .. } => {
                        let inferred = infer(value, scope, &BTreeMap::new(), diagnostics);
                        assertion(
                            AssertionOwner::Table(name.clone()),
                            value,
                            inferred,
                            plans,
                            diagnostics,
                        );
                    }
                    orna_syntax_v1::TableMember::Field {
                        initializer:
                            Some(
                                FieldInitializer::Default(value)
                                | FieldInitializer::Computed(value),
                            ),
                        ty,
                        ..
                    } => require_same(
                        &type_of(ty),
                        &infer(value, scope, &BTreeMap::new(), diagnostics).ty,
                        diagnostics,
                    ),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}
fn bind_pattern(
    pattern: &Pattern,
    ty: Type,
    into: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match pattern {
        Pattern::Name(name, _) => {
            into.insert(
                name.clone(),
                Symbol {
                    kind: SymbolKind::Let,
                    ty,
                    public: false,
                    effects: EffectSummary::default(),
                },
            );
        }
        Pattern::Wildcard(_) => {}
        _ => diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "destructuring pattern inference is not supported in this slice",
        )),
    }
}
struct Inferred {
    ty: Type,
    effects: EffectSummary,
}
fn infer(
    expr: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    match expr {
        Expr::Literal { kind, .. } => Inferred {
            ty: match kind {
                LiteralKind::Integer => Type::Int,
                LiteralKind::Decimal => Type::Decimal,
                LiteralKind::Float => Type::Float,
                LiteralKind::Date => Type::Date,
                LiteralKind::Instant => Type::Instant,
                LiteralKind::String => Type::Text,
                LiteralKind::Boolean => Type::Bool,
                LiteralKind::Null => Type::Null,
            },
            effects: EffectSummary::default(),
        },
        Expr::Name { text, .. } => {
            if scope.ambiguous.contains(text) {
                diagnostics.push(diag(
                    DIAG_AMBIGUOUS,
                    "name has multiple wildcard-import candidates",
                ));
                return Inferred {
                    ty: Type::Error,
                    effects: EffectSummary::default(),
                };
            }
            let symbol = local.get(text).or_else(|| scope.names.get(text));
            match symbol {
                Some(s) => Inferred {
                    ty: s.ty.clone(),
                    effects: s.effects.clone(),
                },
                None => {
                    diagnostics.push(diag(DIAG_UNRESOLVED, "name cannot be resolved"));
                    Inferred {
                        ty: Type::Error,
                        effects: EffectSummary::default(),
                    }
                }
            }
        }
        Expr::Group { inner, .. } => infer(inner, scope, local, diagnostics),
        Expr::Record { fields, .. } => {
            let mut values = BTreeMap::new();
            let mut effects = EffectSummary::default();
            for field in fields {
                let value = infer(&field.value, scope, local, diagnostics);
                effects.join(&value.effects);
                values.insert(field.name.clone(), value.ty);
            }
            Inferred {
                ty: Type::Record(values),
                effects,
            }
        }
        Expr::List { elements, .. } => {
            let mut ty = None;
            let mut effects = EffectSummary::default();
            for element in elements {
                let value = infer(element, scope, local, diagnostics);
                effects.join(&value.effects);
                if let Some(prior) = &ty {
                    require_same(prior, &value.ty, diagnostics);
                } else {
                    ty = Some(value.ty);
                }
            }
            if ty.is_none() {
                diagnostics.push(diag(
                    DIAG_ANNOTATION,
                    "empty list needs an expected element type",
                ));
            }
            Inferred {
                ty: Type::List(Box::new(ty.unwrap_or(Type::Error))),
                effects,
            }
        }
        Expr::Tuple { elements, .. } => {
            let mut effects = EffectSummary::default();
            let values = elements
                .iter()
                .map(|e| {
                    let x = infer(e, scope, local, diagnostics);
                    effects.join(&x.effects);
                    x.ty
                })
                .collect();
            Inferred {
                ty: Type::Tuple(values),
                effects,
            }
        }
        Expr::Field { base, name, .. } => {
            let base = infer(base, scope, local, diagnostics);
            let ty = match &base.ty {
                Type::Record(fields) => fields.get(name).cloned().unwrap_or_else(|| {
                    diagnostics.push(diag(DIAG_UNRESOLVED, "record field cannot be resolved"));
                    Type::Error
                }),
                _ => {
                    diagnostics.push(diag(DIAG_TYPE, "field access requires a record"));
                    Type::Error
                }
            };
            Inferred {
                ty,
                effects: base.effects,
            }
        }
        Expr::Lambda {
            parameters, body, ..
        } => {
            let mut locals = local.clone();
            let mut types = Vec::new();
            for parameter in parameters {
                let ty = parameter
                    .annotation
                    .as_ref()
                    .map(type_of)
                    .unwrap_or_else(|| {
                        diagnostics.push(diag(
                            DIAG_ANNOTATION,
                            "lambda parameter needs a static annotation",
                        ));
                        Type::Error
                    });
                bind_pattern(&parameter.pattern, ty.clone(), &mut locals, diagnostics);
                types.push(ty);
            }
            let value = infer(body, scope, &locals, diagnostics);
            Inferred {
                ty: Type::Function {
                    parameters: types,
                    result: Box::new(value.ty),
                },
                effects: value.effects,
            }
        }
        Expr::Call {
            callee, arguments, ..
        } => {
            let intrinsic = intrinsic_call_effects(callee);
            let callee = infer(callee, scope, local, diagnostics);
            let mut effects = callee.effects.clone();
            effects.join(&intrinsic);
            let values = arguments
                .iter()
                .map(|a| {
                    let x = infer(&a.value, scope, local, diagnostics);
                    effects.join(&x.effects);
                    x.ty
                })
                .collect::<Vec<_>>();
            match callee.ty {
                Type::Function { parameters, result } => {
                    if parameters.len() != values.len() {
                        diagnostics.push(diag(
                            DIAG_TYPE,
                            "function argument count does not match its static signature",
                        ));
                    }
                    for (a, b) in parameters.iter().zip(&values) {
                        require_same(a, b, diagnostics);
                    }
                    Inferred {
                        ty: *result,
                        effects,
                    }
                }
                _ => {
                    diagnostics.push(diag(DIAG_TYPE, "call requires a statically known function"));
                    Inferred {
                        ty: Type::Error,
                        effects,
                    }
                }
            }
        }
        Expr::Unary { rhs, .. } => infer(rhs, scope, local, diagnostics),
        Expr::Binary { lhs, op, rhs, .. } => {
            let left = infer(lhs, scope, local, diagnostics);
            let right = infer(rhs, scope, local, diagnostics);
            let mut effects = left.effects;
            effects.join(&right.effects);
            let ty = if matches!(
                op.as_str(),
                "==" | "!=" | "<" | "<=" | ">" | ">=" | "&&" | "||"
            ) {
                Type::Bool
            } else {
                require_same(&left.ty, &right.ty, diagnostics);
                left.ty
            };
            Inferred { ty, effects }
        }
        Expr::Block {
            statements, tail, ..
        } => {
            let mut locals = local.clone();
            let mut effects = EffectSummary::default();
            for statement in statements {
                match statement {
                    Statement::Let {
                        pattern,
                        annotation,
                        value,
                        ..
                    } => {
                        let x = infer(value, scope, &locals, diagnostics);
                        effects.join(&x.effects);
                        let ty = annotation.as_ref().map(type_of).unwrap_or(x.ty);
                        bind_pattern(pattern, ty, &mut locals, diagnostics);
                    }
                    Statement::Assert { value, .. } => {
                        let x = infer(value, scope, &locals, diagnostics);
                        effects.join(&x.effects);
                        require_same(&Type::Bool, &x.ty, diagnostics);
                    }
                    _ => diagnostics.push(diag(
                        DIAG_UNSUPPORTED,
                        "control or assignment statement is outside this semantic slice",
                    )),
                }
            }
            let tail = tail
                .as_ref()
                .map(|x| infer(x, scope, &locals, diagnostics))
                .unwrap_or(Inferred {
                    ty: Type::Null,
                    effects: EffectSummary::default(),
                });
            effects.join(&tail.effects);
            Inferred {
                ty: tail.ty,
                effects,
            }
        }
        _ => {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "expression form is outside this semantic slice",
            ));
            Inferred {
                ty: Type::Error,
                effects: EffectSummary::default(),
            }
        }
    }
}
fn require_same(expected: &Type, actual: &Type, diagnostics: &mut Vec<Diagnostic>) {
    if expected != actual && !matches!(expected, Type::Error) && !matches!(actual, Type::Error) {
        diagnostics.push(diag(DIAG_TYPE, "static types are incompatible"));
    }
}

fn intrinsic_call_effects(callee: &Expr) -> EffectSummary {
    let Some(path) = qualified_path(callee) else {
        return EffectSummary::default();
    };
    let effect = match path.as_slice() {
        ["sys", "io", ..] => Some("filesystem"),
        ["sys", "net", ..] => Some("network"),
        ["sys", "process", ..] => Some("process"),
        ["sys", "ui", ..] => Some("ui"),
        ["sys", "clock", ..] => Some("clock"),
        ["sys", "random", ..] => Some("random"),
        _ => None,
    };
    match effect {
        Some(effect) => EffectSummary {
            effects: BTreeSet::from([effect.into()]),
            may_fail: true,
        },
        None => EffectSummary::default(),
    }
}
fn qualified_path(expr: &Expr) -> Option<Vec<&str>> {
    match expr {
        Expr::Name { text, .. } => Some(vec![text]),
        Expr::Field { base, name, .. } => {
            let mut path = qualified_path(base)?;
            path.push(name);
            Some(path)
        }
        _ => None,
    }
}
fn assertion(
    owner: AssertionOwner,
    value: &Expr,
    inferred: Inferred,
    plans: &mut Vec<AssertionPlan>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let dependencies = tables_referenced(value);
    if inferred.ty != Type::Bool {
        diagnostics.push(diag(
            DIAG_ASSERTION,
            "assertion does not have the required Boolean form",
        ));
    }
    if inferred.effects.forbidden_for_assertion() {
        diagnostics.push(diag(
            DIAG_ASSERTION_EFFECT,
            "assertion has forbidden effects or failure",
        ));
    }
    if matches!(owner, AssertionOwner::Module) && dependencies.len() < 2 {
        diagnostics.push(diag(
            DIAG_ASSERTION_SCOPE,
            "module assertion needs at least two table dependencies",
        ));
    }
    plans.push(AssertionPlan {
        owner,
        dependencies,
        effects: inferred.effects,
    });
}
fn tables_referenced(expr: &Expr) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    fn visit(e: &Expr, names: &mut BTreeSet<String>) {
        match e {
            Expr::Name { text, .. } if text.chars().next().is_some_and(char::is_uppercase) => {
                names.insert(text.clone());
            }
            Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => visit(rhs, names),
            Expr::Binary { lhs, rhs, .. } => {
                visit(lhs, names);
                visit(rhs, names);
            }
            Expr::Call {
                callee, arguments, ..
            } => {
                visit(callee, names);
                for a in arguments {
                    visit(&a.value, names);
                }
            }
            Expr::Field { base, .. } => visit(base, names),
            Expr::Index { base, index, .. } => {
                visit(base, names);
                visit(index, names);
            }
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                for x in elements {
                    visit(x, names);
                }
            }
            Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
                for x in fields {
                    visit(&x.value, names);
                }
            }
            Expr::Lambda { body, .. } => visit(body, names),
            Expr::Block { tail: Some(x), .. } => visit(x, names),
            Expr::Control {
                condition,
                body,
                alternate,
                ..
            } => {
                if let Some(x) = condition {
                    visit(x, names)
                }
                if let Some(x) = body {
                    visit(x, names)
                }
                if let Some(x) = alternate {
                    visit(x, names)
                }
            }
            _ => {}
        }
    }
    visit(expr, &mut names);
    names
}
fn diag(code: &'static str, message: &'static str) -> Diagnostic {
    Diagnostic::new(
        SafeText::new(code).expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new(message).expect("static message"),
    )
    .expect("valid diagnostic")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn checked(inputs: &[ModuleInput]) -> Analysis {
        analyze(inputs)
    }
    fn has(analysis: &Analysis, code: &str) -> bool {
        analysis.diagnostics.iter().any(|d| d.code() == code)
    }
    #[test]
    fn maps_main_and_leaf_paths() {
        let a = checked(&[
            ModuleInput::new("sensors/greenhouse/main.orna", ""),
            ModuleInput::new("sensors/greenhouse/input.orna", ""),
        ]);
        assert!(
            a.modules
                .contains_key(&Namespace(vec!["sensors".into(), "greenhouse".into()]))
        );
        assert!(a.modules.contains_key(&Namespace(vec![
            "sensors".into(),
            "greenhouse".into(),
            "input".into()
        ])));
    }
    #[test]
    fn rejects_duplicate_namespace_and_unicode_siblings() {
        let a = checked(&[
            ModuleInput::new("x.orna", ""),
            ModuleInput::new("x/main.orna", ""),
            ModuleInput::new("Café/a.orna", ""),
        ]);
        assert!(has(&a, DIAG_NAMESPACE));
        assert!(has(&a, DIAG_BAD_PATH));
    }
    #[test]
    fn reserved_roots_cannot_be_source_modules() {
        let a = checked(&[
            ModuleInput::new("sys/main.orna", ""),
            ModuleInput::new("std/main.orna", ""),
        ]);
        assert!(has(&a, DIAG_RESERVED));
    }
    #[test]
    fn named_alias_glob_and_prelude_imports_resolve() {
        let source = "pub fn f(x: Int): Int = x; pub fn p(): Bool = true;";
        let a = checked(&[
            ModuleInput::new("lib.orna", source).with_prelude_exports(["p"]),
            ModuleInput::new(
                "client.orna",
                "use lib.{f}; use lib as l; use lib as _; fn g(x: Int) = f(x);",
            ),
        ]);
        assert!(!has(&a, DIAG_IMPORT), "{:?}", a.diagnostics);
        assert!(!has(&a, DIAG_UNRESOLVED));
    }
    #[test]
    fn wildcard_ambiguity_is_stable() {
        let a = checked(&[
            ModuleInput::new("a.orna", "pub fn f(): Int = 1;"),
            ModuleInput::new("b.orna", "pub fn f(): Int = 2;"),
            ModuleInput::new("c.orna", "use a.*; use b.*; fn g() = f();"),
        ]);
        assert!(has(&a, DIAG_AMBIGUOUS));
        let codes = a.diagnostics.iter().map(|d| d.code()).collect::<Vec<_>>();
        assert_eq!(codes, {
            let mut x = codes.clone();
            x.sort();
            x
        });
    }
    #[test]
    fn unresolved_name_and_omitted_parameter_annotation_are_diagnostics() {
        let a = checked(&[ModuleInput::new("m.orna", "fn f(x) = missing;")]);
        assert!(has(&a, DIAG_ANNOTATION));
        assert!(has(&a, DIAG_UNRESOLVED));
    }
    #[test]
    fn infers_primitive_record_list_and_function_without_any() {
        let a = checked(&[ModuleInput::new(
            "m.orna",
            "fn record() = { a: 1, }; fn list() = [1, 2]; fn f(x: Int) = x;",
        )]);
        let m = a.modules.values().next().unwrap();
        assert_eq!(
            m.symbols.get("record").expect("record is collected").ty,
            Type::Function {
                parameters: vec![],
                result: Box::new(Type::Record(BTreeMap::from([("a".into(), Type::Int)])))
            }
        );
        assert_eq!(
            m.symbols.get("list").expect("list is collected").ty,
            Type::Function {
                parameters: vec![],
                result: Box::new(Type::List(Box::new(Type::Int)))
            }
        );
        assert!(!has(&a, DIAG_TYPE));
    }
    #[test]
    fn assertion_plan_rejects_compile_time_boolean() {
        let a = checked(&[ModuleInput::new("m.orna", "assert true;")]);
        assert!(has(&a, DIAG_ASSERTION_SCOPE));
        assert_eq!(
            a.assertions.values().next().unwrap()[0].owner,
            AssertionOwner::Module
        );
    }
    #[test]
    fn diagnostics_are_redacted_and_stable() {
        let a = checked(&[ModuleInput::new("secret.orna", "fn f() = missing;")]);
        let json = serde_json::to_string(&a.diagnostics[0]).unwrap();
        assert!(!json.contains("secret.orna"));
        assert!(!json.contains("missing"));
    }
}
