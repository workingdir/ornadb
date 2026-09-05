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
    AssignmentOperator, AssignmentTarget, ControlKind, Declaration, Expr, FieldInitializer, Item,
    LiteralKind, Pattern, Statement, StringSegment, SyntaxTree, TypeExpr, TypeMember,
    TypeRepresentation, UseTail, Visibility, parse_module_with_file,
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
    /// A bounded numeric range.  This semantic-only value retains its bound
    /// type without constructing an iterator or admitting generic ranges.
    Range(Box<Type>),
    /// A composable table/query value. This is a semantic summary only; it
    /// neither enumerates rows nor constructs a runtime query plan.
    Relation(Box<Type>),
    /// A checkpointable source summary. This crate never starts or resumes it.
    Stream(Box<Type>),
    Record(BTreeMap<String, Type>),
    Tuple(Vec<Type>),
    Optional(Box<Type>),
    /// A named type constructor with its statically resolved arguments.  This
    /// retains enough identity for closed collection rules without treating a
    /// parameterized type as an untyped nominal name.
    Applied {
        base: String,
        arguments: Vec<Type>,
    },
    /// The one rate shape needed by the exact money rule. This is not a
    /// general dimensional-algebra representation: unsupported products still
    /// remain `Error` and are never treated as dynamic values.
    MoneyPerUnit {
        currency: Box<Type>,
        unit: Box<Type>,
    },
    Function {
        parameters: Vec<Type>,
        /// Source parameter names when every declaration pattern is a name.
        /// Function type expressions and catalogue summaries do not invent
        /// names, so named invocation of those summaries fails closed.
        parameter_names: Option<Vec<String>>,
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
        parameter_names: None,
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
    stabilize_function_summaries(&parsed, &mut result.modules);
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
        let table_rows = tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Table { name, .. } => Some((name.clone(), table_row_type(item))),
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let mut plans = Vec::new();
        for item in &tree.items {
            check_item(
                item,
                &mut symbols,
                &scope,
                &table_rows,
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

/// Function bodies may depend on inferred summaries declared later in their
/// module or exported by a later project input. Iterate the finite declaration
/// graph before emitting diagnostics so public result and effect summaries are
/// independent of manifest order.
fn stabilize_function_summaries(
    parsed: &[(Namespace, SyntaxTree)],
    modules: &mut BTreeMap<Namespace, ModuleHeader>,
) {
    let pass_limit = parsed
        .iter()
        .map(|(_, tree)| {
            tree.items
                .iter()
                .filter(|item| matches!(item.declaration, Declaration::Function { .. }))
                .count()
        })
        .sum::<usize>()
        .saturating_add(1);
    for _ in 0..pass_limit {
        let before = modules.clone();
        for (namespace, tree) in parsed {
            let Some(header) = modules.get(namespace).cloned() else {
                continue;
            };
            let mut discarded = Vec::new();
            let scope = resolve_imports(namespace, tree, &header, modules, &mut discarded);
            let mut symbols = header.symbols;
            for item in &tree.items {
                if matches!(item.declaration, Declaration::Function { .. }) {
                    check_function(item, &mut symbols, &scope, &mut discarded);
                }
            }
            if let Some(module) = modules.get_mut(namespace) {
                module.symbols = symbols;
                module.exports = module
                    .symbols
                    .iter()
                    .filter(|(_, symbol)| symbol.public)
                    .map(|(name, symbol)| (name.clone(), symbol.clone()))
                    .collect();
            }
        }
        if *modules == before {
            break;
        }
    }
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
            let parameter_names = signature
                .parameters
                .iter()
                .map(|parameter| match &parameter.pattern {
                    Pattern::Name(name, _) => Some(name.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>();
            Some((
                signature.name.clone(),
                SymbolKind::Function,
                Type::Function {
                    parameters,
                    parameter_names,
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
        TypeExpr::Name {
            path, arguments, ..
        } if arguments.is_empty() => {
            primitive(&path.join(".")).unwrap_or_else(|| Type::Named(path.join(".")))
        }
        TypeExpr::Name {
            path, arguments, ..
        } => Type::Applied {
            base: path.join("."),
            arguments: arguments.iter().map(type_of).collect(),
        },
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
            parameter_names: None,
            result: Box::new(type_of(result)),
        },
        TypeExpr::Optional { inner, .. } => Type::Optional(Box::new(type_of(inner))),
        TypeExpr::Product { lhs, op, rhs, .. } => {
            let lhs = type_of(lhs);
            let rhs = type_of(rhs);
            match (lhs, op.as_str(), rhs) {
                (Type::Applied { base, arguments }, "/", unit)
                    if base == "Money" && arguments.len() == 1 =>
                {
                    Type::MoneyPerUnit {
                        currency: Box::new(arguments.into_iter().next().unwrap_or(Type::Error)),
                        unit: Box::new(unit),
                    }
                }
                _ => Type::Error,
            }
        }
    }
}
fn primitive(name: &str) -> Option<Type> {
    Some(match name {
        "Int" => Type::Int,
        "Decimal" => Type::Decimal,
        "Float" => Type::Float,
        "Date" => Type::Date,
        "Instant" => Type::Instant,
        "Str" | "Text" | "String" => Type::Text,
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
    /// Direct `use module [as alias]` bindings. These are deliberately kept
    /// apart from ordinary values so only explicitly imported module roots can
    /// begin qualified module-member lookup.
    modules: BTreeMap<String, Namespace>,
    /// The closed source/catalogue module set used by qualified lookup. This
    /// is data assembled by the caller, never a filesystem loader.
    available_modules: BTreeMap<Namespace, ModuleHeader>,
    /// Local table rows are separate from public table identities: a table
    /// expression produces its relation row shape, while table operations
    /// retain the declared nominal table result.
    table_rows: BTreeMap<String, Type>,
    /// Local nominal record constructors elaborate to their declared row shape
    /// so field access remains structural inside inferred stream pipelines.
    nominal_rows: BTreeMap<String, Type>,
    /// Local nominal types with an explicit nested `Currency` implementation.
    /// This is intentionally not inferred from names or static members.
    currency_types: BTreeSet<String>,
    /// Local enum payloads retained for closed constructor-pattern checking.
    enum_variants: BTreeMap<String, BTreeMap<String, BTreeMap<String, Type>>>,
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
        modules: BTreeMap::new(),
        available_modules: modules.clone(),
        table_rows: tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Table { name, .. } => Some((name.clone(), table_row_type(item))),
                _ => None,
            })
            .collect(),
        nominal_rows: tree.items.iter().filter_map(nominal_row_type).collect(),
        currency_types: currency_types(tree),
        enum_variants: enum_variant_types(tree, diagnostics),
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
                scope
                    .modules
                    .entry(name.clone())
                    .or_insert_with(|| target.clone());
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
            UseTail::Alias { name, .. } => {
                scope
                    .modules
                    .entry(name.clone())
                    .or_insert_with(|| target.clone());
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
    table_rows: &BTreeMap<String, Type>,
    plans: &mut Vec<AssertionPlan>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match &item.declaration {
        Declaration::Let {
            pattern,
            annotation,
            value,
        } => {
            if let Some(expected) = annotation.as_ref().map(type_of) {
                let inferred =
                    infer_contextual(value, &expected, scope, &BTreeMap::new(), diagnostics);
                require_same(&expected, &inferred.ty, diagnostics);
                bind_pattern(pattern, inferred.ty, symbols, diagnostics);
            } else {
                let inferred = infer(value, scope, &BTreeMap::new(), diagnostics);
                bind_pattern(pattern, inferred.ty, symbols, diagnostics);
            }
        }
        Declaration::Function { .. } => check_function(item, symbols, scope, diagnostics),
        Declaration::Assertion { value } => {
            let inferred = infer_module_assertion(value, table_rows, scope, diagnostics);
            assertion(AssertionOwner::Module, value, inferred, plans, diagnostics);
        }
        Declaration::Table { name, members, .. } => {
            let row = table_row_type(item);
            for member in members {
                match member {
                    orna_syntax_v1::TableMember::Assertion { value, .. } => {
                        let inferred = infer_table_assertion(value, &row, scope, diagnostics);
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
                    } => {
                        let expected = type_of(ty);
                        let inferred = infer_contextual(
                            value,
                            &expected,
                            scope,
                            &BTreeMap::new(),
                            diagnostics,
                        );
                        require_same(&expected, &inferred.ty, diagnostics);
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn check_function(
    item: &Item,
    symbols: &mut BTreeMap<String, Symbol>,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Declaration::Function { signature, body } = &item.declaration else {
        unreachable!("function checker called for a non-function declaration");
    };
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
    if let Some(expected) = signature.result.as_ref().map(type_of) {
        let inferred = infer_contextual(body, &expected, scope, &local, diagnostics);
        require_same(&expected, &inferred.ty, diagnostics);
        if let Some(symbol) = symbols.get_mut(&signature.name) {
            symbol.effects = inferred.effects;
        }
    } else {
        let inferred = infer(body, scope, &local, diagnostics);
        if let Some(symbol) = symbols.get_mut(&signature.name) {
            symbol.effects = inferred.effects;
            if let Type::Function { result, .. } = &mut symbol.ty {
                **result = inferred.ty;
            }
        }
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

/// Applies the sole contextual numeric conversion admitted by the language:
/// an exact fractional literal may be checked directly as `Float`. All other
/// expressions remain inferred through the ordinary closed-world path.
fn infer_contextual(
    expr: &Expr,
    expected: &Type,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if matches!(expected, Type::Float)
        && matches!(
            expr,
            Expr::Literal {
                kind: LiteralKind::Decimal,
                ..
            }
        )
    {
        return Inferred {
            ty: Type::Float,
            effects: EffectSummary::default(),
        };
    }
    infer(expr, scope, local, diagnostics)
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
        Expr::InterpolatedString { segments, .. } => {
            let mut effects = EffectSummary::default();
            for segment in segments {
                if let StringSegment::Expression { value, .. } = segment {
                    let value = infer(value, scope, local, diagnostics);
                    effects.join(&value.effects);
                    require_same(&Type::Text, &value.ty, diagnostics);
                }
            }
            Inferred {
                ty: Type::Text,
                effects,
            }
        }
        Expr::Name { text, .. } => {
            if let Some(ty) = intrinsic_value_type(text) {
                return Inferred {
                    ty,
                    effects: EffectSummary::default(),
                };
            }
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
                Some(s) if s.kind == SymbolKind::Table => Inferred {
                    ty: Type::Relation(Box::new(
                        scope
                            .table_rows
                            .get(text)
                            .cloned()
                            .unwrap_or_else(|| s.ty.clone()),
                    )),
                    effects: EffectSummary {
                        effects: BTreeSet::from(["database read".into()]),
                        may_fail: true,
                    },
                },
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
        Expr::Nominal { path, fields, .. } => {
            infer_nominal(path, fields, scope, local, diagnostics)
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
            if let Some(path) = qualified_path(expr)
                && scope.modules.contains_key(path[0])
            {
                if let Some(table) = table_symbol(expr, scope, local) {
                    return Inferred {
                        ty: Type::Relation(Box::new(table.ty.clone())),
                        effects: EffectSummary {
                            effects: BTreeSet::from(["database read".into()]),
                            may_fail: true,
                        },
                    };
                }
                return resolve_qualified_module_member(&path, scope, diagnostics);
            }
            let base = infer(base, scope, local, diagnostics);
            if let Some(ty) = infer_numeric_member(&base.ty, name) {
                return Inferred {
                    ty,
                    effects: base.effects,
                };
            }
            if let Some(ty) = infer_numeric_postfix(&base.ty, name, scope) {
                return Inferred {
                    ty,
                    effects: base.effects,
                };
            }
            let ty = match &base.ty {
                Type::Record(fields) => fields.get(name).cloned().unwrap_or_else(|| {
                    diagnostics.push(diag(DIAG_UNRESOLVED, "record field cannot be resolved"));
                    Type::Error
                }),
                Type::Named(table) => scope
                    .table_rows
                    .get(table)
                    .and_then(|row| match row {
                        Type::Record(fields) => fields.get(name).cloned(),
                        _ => None,
                    })
                    .unwrap_or_else(|| {
                        diagnostics.push(diag(DIAG_TYPE, "field access requires a record"));
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
                    parameter_names: parameters
                        .iter()
                        .map(|parameter| match &parameter.pattern {
                            Pattern::Name(name, _) => Some(name.clone()),
                            _ => None,
                        })
                        .collect::<Option<Vec<_>>>(),
                    result: Box::new(value.ty),
                },
                effects: value.effects,
            }
        }
        Expr::Call {
            callee, arguments, ..
        } => {
            if let Some(inferred) =
                infer_stream_from_list(callee, arguments, scope, local, diagnostics)
            {
                return inferred;
            }
            if let Some(inferred) =
                infer_relation_call(callee, arguments, scope, local, diagnostics)
            {
                return inferred;
            }
            if let Some(inferred) =
                infer_table_operation(callee, arguments, scope, local, diagnostics)
            {
                return inferred;
            }
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
                Type::Function {
                    parameters,
                    parameter_names,
                    result,
                } => {
                    check_call_arguments(
                        &parameters,
                        parameter_names.as_deref(),
                        arguments,
                        &values,
                        diagnostics,
                    );
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
            if op == "|" {
                return infer_success_pipeline(lhs, rhs, scope, local, diagnostics);
            }
            let left = infer(lhs, scope, local, diagnostics);
            let right = infer(rhs, scope, local, diagnostics);
            let mut effects = left.effects;
            effects.join(&right.effects);
            if matches!(op.as_str(), ".." | "..=") {
                let ty = if left.ty == right.ty && is_numeric_range_bound(&left.ty) {
                    Type::Range(Box::new(left.ty))
                } else {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "range bounds must have the same numeric type",
                    ));
                    Type::Error
                };
                return Inferred { ty, effects };
            }
            if op == "in" {
                let ty = match &right.ty {
                    Type::Range(element) => {
                        require_same(element, &left.ty, diagnostics);
                        Type::Bool
                    }
                    _ => {
                        diagnostics.push(diag(DIAG_TYPE, "membership requires a numeric range"));
                        Type::Error
                    }
                };
                return Inferred { ty, effects };
            }
            if op == "??" {
                let ty = match (&left.ty, &right.ty) {
                    (Type::Optional(_), Type::Error) | (Type::Error, _) => Type::Error,
                    (Type::Optional(inner), right) if inner.as_ref() == right => (**inner).clone(),
                    _ => {
                        diagnostics.push(diag(
                            DIAG_TYPE,
                            "coalesce fallback must match the optional value type",
                        ));
                        Type::Error
                    }
                };
                return Inferred { ty, effects };
            }
            if op == "*" {
                if let Some(ty) = reduce_exact_money_rate(&left.ty, &right.ty) {
                    return Inferred { ty, effects };
                }
                if matches!(left.ty, Type::MoneyPerUnit { .. })
                    || matches!(right.ty, Type::MoneyPerUnit { .. })
                {
                    diagnostics.push(diag(
                        DIAG_UNSUPPORTED,
                        "dimensional algebra is outside this semantic slice",
                    ));
                    return Inferred {
                        ty: Type::Error,
                        effects,
                    };
                }
            }
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
            let mut final_control = None;
            for (index, statement) in statements.iter().enumerate() {
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
                    Statement::Expression { value, .. } => {
                        let x = infer(value, scope, &locals, diagnostics);
                        effects.join(&x.effects);
                    }
                    Statement::Control { value, .. } => {
                        let x = infer(value, scope, &locals, diagnostics);
                        effects.join(&x.effects);
                        if index + 1 == statements.len() && tail.is_none() {
                            final_control = Some(x);
                        }
                    }
                    Statement::Assignment {
                        target,
                        operator,
                        value,
                        ..
                    } => effects.join(&infer_assignment(
                        target,
                        operator,
                        value,
                        scope,
                        &mut locals,
                        diagnostics,
                    )),
                    _ => diagnostics.push(diag(
                        DIAG_UNSUPPORTED,
                        "control statement is outside this semantic slice",
                    )),
                }
            }
            let tail = tail
                .as_ref()
                .map(|x| infer(x, scope, &locals, diagnostics))
                .or(final_control)
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
        Expr::Control {
            kind: ControlKind::If,
            binding,
            condition,
            body,
            arms,
            alternate,
            ..
        } => infer_if(
            binding.as_ref(),
            condition.as_deref(),
            body.as_deref(),
            arms,
            alternate.as_deref(),
            scope,
            local,
            diagnostics,
        ),
        Expr::Control {
            kind: ControlKind::Case,
            binding,
            condition,
            body,
            arms,
            alternate,
            ..
        } => infer_case(
            binding.as_ref(),
            condition.as_deref(),
            body.as_deref(),
            arms,
            alternate.as_deref(),
            scope,
            local,
            diagnostics,
        ),
        Expr::Control {
            kind: ControlKind::For,
            binding,
            condition,
            body,
            arms,
            alternate,
            ..
        } => infer_for(
            binding.as_ref(),
            condition.as_deref(),
            body.as_deref(),
            arms,
            alternate.as_deref(),
            scope,
            local,
            diagnostics,
        ),
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

fn infer_assignment(
    target: &AssignmentTarget,
    operator: &AssignmentOperator,
    value: &Expr,
    scope: &Scope,
    local: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> EffectSummary {
    let value = infer(value, scope, local, diagnostics);
    match (target, operator) {
        (AssignmentTarget::Name { name, .. }, AssignmentOperator::Set) => match local.get(name) {
            Some(symbol) => require_same(&symbol.ty, &value.ty, diagnostics),
            None => diagnostics.push(diag(
                DIAG_UNRESOLVED,
                "assignment target cannot be resolved",
            )),
        },
        (AssignmentTarget::Name { name, .. }, _) => match local.get(name) {
            Some(symbol) => require_same(&symbol.ty, &value.ty, diagnostics),
            None => diagnostics.push(diag(
                DIAG_UNRESOLVED,
                "assignment target cannot be resolved",
            )),
        },
        _ => diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "only local-name assignment targets are supported in this semantic slice",
        )),
    }
    value.effects
}

#[allow(clippy::too_many_arguments)]
fn infer_for(
    binding: Option<&Pattern>,
    iterable: Option<&Expr>,
    body: Option<&Expr>,
    arms: &[orna_syntax_v1::CaseArm],
    alternate: Option<&Expr>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let Some(binding) = binding else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "for control requires a binding"));
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let Some(iterable) = iterable else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "for control requires an iterable"));
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let Some(body) = body else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "for control requires a body"));
        return Inferred {
            ty: Type::Error,
            effects: infer(iterable, scope, local, diagnostics).effects,
        };
    };
    if alternate.is_some()
        || arms.len() != 1
        || arms[0].guard.is_some()
        || arms[0].pattern != *binding
        || arms[0].body != *body
    {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "malformed for control shape is outside this semantic slice",
        ));
        return Inferred {
            ty: Type::Error,
            effects: infer(iterable, scope, local, diagnostics).effects,
        };
    }

    let iterable = infer(iterable, scope, local, diagnostics);
    let Type::List(element) = iterable.ty else {
        if !matches!(iterable.ty, Type::Error) {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "for control supports only list iteration in this semantic slice",
            ));
        }
        return Inferred {
            ty: Type::Error,
            effects: iterable.effects,
        };
    };
    let mut body_locals = local.clone();
    bind_pattern(binding, *element, &mut body_locals, diagnostics);
    let body = infer(body, scope, &body_locals, diagnostics);
    let mut effects = iterable.effects;
    effects.join(&body.effects);
    Inferred {
        ty: Type::Null,
        effects,
    }
}

#[allow(clippy::too_many_arguments)]
fn infer_if(
    binding: Option<&Pattern>,
    condition: Option<&Expr>,
    body: Option<&Expr>,
    arms: &[orna_syntax_v1::CaseArm],
    alternate: Option<&Expr>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if binding.is_some() || !arms.is_empty() {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "malformed if control shape is outside this semantic slice",
        ));
    }
    let Some(condition) = condition else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "if expression requires a condition"));
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let condition = infer(condition, scope, local, diagnostics);
    require_same(&Type::Bool, &condition.ty, diagnostics);

    let Some(body) = body else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "if expression requires a body"));
        return Inferred {
            ty: Type::Error,
            effects: condition.effects,
        };
    };
    let body = infer(body, scope, local, diagnostics);

    let Some(alternate) = alternate else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "if expression requires an else branch in this semantic slice",
        ));
        let mut effects = condition.effects;
        effects.join(&body.effects);
        return Inferred {
            ty: Type::Error,
            effects,
        };
    };
    let alternate = infer(alternate, scope, local, diagnostics);
    let mut effects = condition.effects;
    effects.join(&body.effects);
    effects.join(&alternate.effects);
    require_same(&body.ty, &alternate.ty, diagnostics);
    Inferred {
        ty: if body.ty == alternate.ty {
            body.ty
        } else {
            Type::Error
        },
        effects,
    }
}

fn check_call_arguments(
    parameters: &[Type],
    parameter_names: Option<&[String]>,
    arguments: &[orna_syntax_v1::Argument],
    values: &[Type],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(parameter_names) = parameter_names.filter(|names| names.len() == parameters.len())
    else {
        if arguments.iter().any(|argument| argument.name.is_some()) {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "named arguments require declared parameter names",
            ));
        } else {
            if parameters.len() != values.len() {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "function argument count does not match its static signature",
                ));
            }
            for (expected, actual) in parameters.iter().zip(values) {
                require_same(expected, actual, diagnostics);
            }
        }
        return;
    };
    let mut seen = BTreeSet::new();
    let mut malformed = parameters.len() != values.len();
    let mut positional = 0usize;
    let mut named_started = false;
    for (argument, actual) in arguments.iter().zip(values) {
        let index = if let Some(name) = argument.name.as_deref() {
            named_started = true;
            parameter_names
                .iter()
                .position(|parameter| parameter == name)
        } else if named_started {
            malformed = true;
            None
        } else {
            let index = (positional < parameters.len()).then_some(positional);
            positional += 1;
            index
        };
        let Some(index) = index else {
            malformed = true;
            continue;
        };
        if !seen.insert(index) {
            malformed = true;
            continue;
        }
        require_same(&parameters[index], actual, diagnostics);
    }
    if seen.len() != parameters.len() {
        malformed = true;
    }
    if malformed {
        diagnostics.push(diag(
            DIAG_TYPE,
            "named function arguments do not match its static signature",
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn infer_case(
    binding: Option<&Pattern>,
    condition: Option<&Expr>,
    body: Option<&Expr>,
    arms: &[orna_syntax_v1::CaseArm],
    alternate: Option<&Expr>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut effects = EffectSummary::default();
    if binding.is_some() || body.is_some() || alternate.is_some() {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "malformed case control shape is outside this semantic slice",
        ));
    }
    if let Some(body) = body {
        effects.join(&infer(body, scope, local, diagnostics).effects);
    }
    if let Some(alternate) = alternate {
        effects.join(&infer(alternate, scope, local, diagnostics).effects);
    }
    let Some(condition) = condition else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "case expression requires a value"));
        let mut discarded_result = None;
        for arm in arms {
            infer_case_arm_body(
                arm,
                local,
                scope,
                diagnostics,
                &mut effects,
                &mut discarded_result,
            );
        }
        return Inferred {
            ty: Type::Error,
            effects,
        };
    };
    let scrutinee = infer(condition, scope, local, diagnostics);
    effects.join(&scrutinee.effects);
    if arms.is_empty() {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "case expression requires at least one arm",
        ));
        return Inferred {
            ty: Type::Error,
            effects,
        };
    }

    let mut result = None;
    match &scrutinee.ty {
        Type::Named(name) if scope.enum_variants.contains_key(name) => {
            let variants = &scope.enum_variants[name];
            let mut covered = BTreeSet::new();
            for arm in arms {
                let mut arm_locals = local.clone();
                if let Some(variant) = bind_enum_case_pattern(
                    &arm.pattern,
                    name,
                    variants,
                    &mut arm_locals,
                    diagnostics,
                ) && !covered.insert(variant)
                {
                    diagnostics.push(diag(DIAG_TYPE, "case enum variant is duplicated"));
                }
                infer_case_arm_body(
                    arm,
                    &arm_locals,
                    scope,
                    diagnostics,
                    &mut effects,
                    &mut result,
                );
            }
            if covered.len() != variants.len() {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "case expression does not cover every enum variant",
                ));
            }
        }
        Type::Optional(inner) => {
            let mut covered = BTreeSet::new();
            for arm in arms {
                let mut arm_locals = local.clone();
                if let Some(part) =
                    bind_optional_case_pattern(&arm.pattern, inner, &mut arm_locals, diagnostics)
                    && !covered.insert(part)
                {
                    diagnostics.push(diag(DIAG_TYPE, "case optional arm is duplicated"));
                }
                infer_case_arm_body(
                    arm,
                    &arm_locals,
                    scope,
                    diagnostics,
                    &mut effects,
                    &mut result,
                );
            }
            if covered != BTreeSet::from(["Some", "null"]) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "case expression must cover Some and null exactly once",
                ));
            }
        }
        Type::Error => {
            for arm in arms {
                infer_case_arm_body(arm, local, scope, diagnostics, &mut effects, &mut result);
            }
        }
        _ => {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "case inference supports only local enums and optional values",
            ));
            for arm in arms {
                infer_case_arm_body(arm, local, scope, diagnostics, &mut effects, &mut result);
            }
        }
    }
    Inferred {
        ty: result.unwrap_or(Type::Error),
        effects,
    }
}

fn bind_enum_case_pattern(
    pattern: &Pattern,
    enum_name: &str,
    variants: &BTreeMap<String, BTreeMap<String, Type>>,
    local: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<String> {
    let Pattern::Constructor {
        path,
        arguments,
        fields,
        ..
    } = pattern
    else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "enum case arm requires a qualified constructor pattern",
        ));
        return None;
    };
    let [owner, variant] = path.as_slice() else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "enum case constructor must be exactly Enum.variant",
        ));
        return None;
    };
    if owner.text != enum_name {
        diagnostics.push(diag(
            DIAG_TYPE,
            "enum case constructor does not match the scrutinee type",
        ));
        return None;
    }
    let Some(expected) = variants.get(&variant.text) else {
        diagnostics.push(diag(
            DIAG_UNRESOLVED,
            "enum case variant cannot be resolved",
        ));
        return None;
    };
    if !arguments.is_empty() {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "enum case payload requires named record fields",
        ));
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut valid = true;
    for field in fields {
        let Some(ty) = expected.get(&field.name) else {
            diagnostics.push(diag(
                DIAG_UNRESOLVED,
                "enum case payload field cannot be resolved",
            ));
            valid = false;
            continue;
        };
        if !seen.insert(field.name.as_str()) {
            diagnostics.push(diag(DIAG_TYPE, "enum case payload field is duplicated"));
            valid = false;
            continue;
        }
        match &field.pattern {
            None => insert_case_binding(&field.name, ty.clone(), local),
            Some(Pattern::Name(name, _)) => insert_case_binding(name, ty.clone(), local),
            Some(Pattern::Wildcard(_)) => {}
            Some(_) => {
                diagnostics.push(diag(
                    DIAG_UNSUPPORTED,
                    "nested enum payload patterns are outside this semantic slice",
                ));
                valid = false;
            }
        }
    }
    if seen.len() != expected.len() {
        diagnostics.push(diag(
            DIAG_TYPE,
            "enum case payload fields do not match the declared variant",
        ));
        valid = false;
    }
    valid.then(|| variant.text.clone())
}

fn bind_optional_case_pattern(
    pattern: &Pattern,
    inner: &Type,
    local: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<&'static str> {
    match pattern {
        Pattern::Constructor {
            path,
            arguments,
            fields,
            ..
        } if path.len() == 1 && path[0].text == "Some" && fields.is_empty() => {
            let [binding] = arguments.as_slice() else {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "Some case pattern requires exactly one payload binding",
                ));
                return None;
            };
            match binding {
                Pattern::Name(name, _) => insert_case_binding(name, inner.clone(), local),
                Pattern::Wildcard(_) => {}
                _ => {
                    diagnostics.push(diag(
                        DIAG_UNSUPPORTED,
                        "nested optional payload patterns are outside this semantic slice",
                    ));
                    return None;
                }
            }
            Some("Some")
        }
        Pattern::Literal {
            kind: LiteralKind::Null,
            ..
        } => Some("null"),
        _ => {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "optional case arm must be Some(binding) or null",
            ));
            None
        }
    }
}

fn insert_case_binding(name: &str, ty: Type, local: &mut BTreeMap<String, Symbol>) {
    local.insert(
        name.to_owned(),
        Symbol {
            kind: SymbolKind::Let,
            ty,
            public: false,
            effects: EffectSummary::default(),
        },
    );
}

fn infer_case_arm_body(
    arm: &orna_syntax_v1::CaseArm,
    local: &BTreeMap<String, Symbol>,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
    effects: &mut EffectSummary,
    result: &mut Option<Type>,
) {
    if let Some(guard) = &arm.guard {
        let guard = infer(guard, scope, local, diagnostics);
        effects.join(&guard.effects);
        require_same(&Type::Bool, &guard.ty, diagnostics);
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "guarded case arms are outside this semantic slice",
        ));
    }
    let body = infer(&arm.body, scope, local, diagnostics);
    effects.join(&body.effects);
    if let Some(expected) = result.as_ref() {
        require_same(expected, &body.ty, diagnostics);
    } else {
        *result = Some(body.ty);
    }
}

fn infer_nominal(
    path: &[orna_syntax_v1::NameSegment],
    fields: &[orna_syntax_v1::RecordField],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut actual = BTreeMap::new();
    let mut effects = EffectSummary::default();
    for field in fields {
        let value = infer(&field.value, scope, local, diagnostics);
        effects.join(&value.effects);
        actual.insert(field.name.clone(), value.ty);
    }
    let [name] = path else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "qualified nominal constructors are outside this semantic slice",
        ));
        return Inferred {
            ty: Type::Error,
            effects,
        };
    };
    let declared = scope
        .names
        .get(&name.text)
        .filter(|symbol| symbol.kind == SymbolKind::Type)
        .and_then(|_| scope.nominal_rows.get(&name.text));
    let Some(Type::Record(expected)) = declared else {
        diagnostics.push(diag(
            DIAG_UNRESOLVED,
            "nominal record type cannot be resolved",
        ));
        return Inferred {
            ty: Type::Error,
            effects,
        };
    };
    if expected.keys().ne(actual.keys()) {
        diagnostics.push(diag(
            DIAG_TYPE,
            "nominal constructor fields do not match the declared row",
        ));
    }
    for (name, expected) in expected {
        if let Some(actual) = actual.get(name) {
            require_same(expected, actual, diagnostics);
        }
    }
    Inferred {
        ty: Type::Record(expected.clone()),
        effects,
    }
}

/// Resolves the finite, declarative core stream constructor. The result is a
/// type/effect summary; list hashing, replay and checkpoint behavior belong to
/// the runtime contract.
fn infer_stream_from_list(
    callee: &Expr,
    arguments: &[orna_syntax_v1::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    if qualified_path(callee)?.as_slice() != ["Stream", "from_list"] {
        return None;
    }
    let values = arguments.first().map(|argument| &argument.value);
    let source_identity = arguments.get(1);
    let Some(values) = values else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "Stream.from_list requires values and source_identity",
        ));
        return Some(Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        });
    };
    let values = infer(values, scope, local, diagnostics);
    let valid_identity = source_identity.is_some_and(|argument| {
        argument.name.as_deref() == Some("source_identity")
            && matches!(
                infer(&argument.value, scope, local, diagnostics).ty,
                Type::Text
            )
    });
    if arguments.len() != 2 || !valid_identity {
        diagnostics.push(diag(
            DIAG_TYPE,
            "Stream.from_list requires a named Text source_identity",
        ));
    }
    let Type::List(element) = values.ty else {
        diagnostics.push(diag(DIAG_TYPE, "Stream.from_list values must be a list"));
        return Some(Inferred {
            ty: Type::Error,
            effects: values.effects,
        });
    };
    Some(Inferred {
        ty: Type::Stream(element),
        effects: values.effects,
    })
}

fn infer_relation_call(
    callee: &Expr,
    arguments: &[orna_syntax_v1::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let Some("exists") = core_relation_call_name(callee) else {
        return None;
    };
    if arguments.len() != 2 {
        return None;
    }
    let relation = infer(&arguments[0].value, scope, local, diagnostics);
    let Type::Relation(element) = relation.ty else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "exists requires a relation as its first argument",
        ));
        return Some(Inferred {
            ty: Type::Error,
            effects: relation.effects,
        });
    };
    let callback = infer_callback(
        &arguments[1].value,
        *element,
        Type::Bool,
        scope,
        local,
        diagnostics,
    );
    let mut effects = relation.effects;
    effects.join(&callback.effects);
    Some(Inferred {
        ty: Type::Bool,
        effects,
    })
}

/// The frozen parser represents `!exists(rows, predicate)` as a call whose
/// callee is unary `!exists`; it is still the root `exists` relation form.
fn core_relation_call_name(callee: &Expr) -> Option<&str> {
    match callee {
        Expr::Name { text, .. } => Some(text),
        Expr::Unary { op, rhs, .. } if op == "!" => match rhs.as_ref() {
            Expr::Name { text, .. } => Some(text),
            _ => None,
        },
        _ => None,
    }
}

/// Applies one of the parsed success-pipeline forms used by the reference
/// project. It models argument-one insertion only; it does not invoke stages.
fn infer_success_pipeline(
    lhs: &Expr,
    rhs: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let input = infer(lhs, scope, local, diagnostics);
    if let Expr::Call {
        callee, arguments, ..
    } = rhs
        && matches!(callee.as_ref(), Expr::Name { text, .. } if text == "take")
        && let Type::List(element) = &input.ty
    {
        let mut effects = input.effects;
        let ty = match arguments.as_slice() {
            [argument] if argument.name.is_none() => {
                let range = infer(&argument.value, scope, local, diagnostics);
                effects.join(&range.effects);
                if range.ty == Type::Range(Box::new(Type::Int)) {
                    Type::List(element.clone())
                } else {
                    diagnostics.push(diag(DIAG_TYPE, "take requires an integer range"));
                    Type::Error
                }
            }
            _ => {
                diagnostics.push(diag(DIAG_TYPE, "take requires one integer range"));
                Type::Error
            }
        };
        return Inferred { ty, effects };
    }
    if let Expr::Name { text, .. } = rhs
        && let Type::List(element) = &input.ty
        && let Some(ty) = infer_affine_collection_aggregate(text, element, diagnostics)
    {
        return Inferred {
            ty,
            effects: input.effects,
        };
    }
    if let Expr::Name { text, .. } = rhs
        && text == "count"
        && matches!(input.ty, Type::List(_))
    {
        return Inferred {
            ty: Type::Int,
            effects: input.effects,
        };
    }
    if let Expr::Name { text, .. } = rhs
        && text == "count"
        && matches!(input.ty, Type::Relation(_))
    {
        return Inferred {
            ty: Type::Int,
            effects: input.effects,
        };
    }
    if !matches!(input.ty, Type::Relation(_) | Type::Stream(_)) {
        return match rhs {
            Expr::Call {
                callee, arguments, ..
            } => infer_named_pipeline_stage(input, callee, arguments, scope, local, diagnostics),
            Expr::Name { .. } => {
                infer_named_pipeline_stage(input, rhs, &[], scope, local, diagnostics)
            }
            _ => {
                diagnostics.push(diag(
                    DIAG_UNSUPPORTED,
                    "pipeline stage is outside this semantic slice",
                ));
                Inferred {
                    ty: Type::Error,
                    effects: input.effects,
                }
            }
        };
    }
    let Expr::Call {
        callee, arguments, ..
    } = rhs
    else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "pipeline stage is outside this semantic slice",
        ));
        return input;
    };
    let Expr::Name { text, .. } = callee.as_ref() else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "pipeline stage is outside this semantic slice",
        ));
        return input;
    };
    let (element, is_stream) = match &input.ty {
        Type::Relation(element) => (element.as_ref().clone(), false),
        Type::Stream(element) => (element.as_ref().clone(), true),
        _ => {
            diagnostics.push(diag(
                DIAG_TYPE,
                "pipeline input is not a supported relation or stream",
            ));
            return Inferred {
                ty: Type::Error,
                effects: input.effects,
            };
        }
    };
    let (ty, callback_result) = match (text.as_str(), is_stream, arguments.as_slice()) {
        ("filter", false, [_]) => (Type::Relation(Box::new(element.clone())), Some(Type::Bool)),
        ("one", false, []) => (element.clone(), None),
        ("count", false, []) => (Type::Int, None),
        ("for_each", true, [_]) => (Type::Null, Some(Type::Null)),
        _ => {
            let ignored = infer(rhs, scope, local, diagnostics);
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "pipeline operation is outside this semantic slice",
            ));
            return Inferred {
                ty: Type::Error,
                effects: {
                    let mut effects = input.effects;
                    effects.join(&ignored.effects);
                    effects
                },
            };
        }
    };
    let mut effects = input.effects;
    if let Some(result) = callback_result {
        let callback = infer_callback(
            &arguments[0].value,
            element,
            result,
            scope,
            local,
            diagnostics,
        );
        effects.join(&callback.effects);
    }
    if text == "one" {
        effects.may_fail = true;
    }
    Inferred { ty, effects }
}

/// Applies a named callable pipeline stage by checking the implicit input as
/// its first positional argument. Relation and stream stages are intentionally
/// handled by `infer_success_pipeline`'s closed intrinsic path above.
fn infer_named_pipeline_stage(
    input: Inferred,
    callee: &Expr,
    arguments: &[orna_syntax_v1::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if !matches!(callee, Expr::Name { .. }) {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "pipeline stage must be a named callable",
        ));
        return Inferred {
            ty: Type::Error,
            effects: input.effects,
        };
    }
    let callee = infer(callee, scope, local, diagnostics);
    let mut effects = input.effects;
    effects.join(&callee.effects);
    let mut values = Vec::with_capacity(arguments.len() + 1);
    values.push(input.ty);
    for argument in arguments {
        let value = infer(&argument.value, scope, local, diagnostics);
        effects.join(&value.effects);
        values.push(value.ty);
    }
    if arguments.iter().any(|argument| argument.name.is_some()) {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "pipeline callable arguments must be positional in this semantic slice",
        ));
        return Inferred {
            ty: Type::Error,
            effects,
        };
    }
    let Type::Function {
        parameters, result, ..
    } = callee.ty
    else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "pipeline stage is not a supported named callable",
        ));
        return Inferred {
            ty: Type::Error,
            effects,
        };
    };
    if parameters.len() != values.len() {
        diagnostics.push(diag(
            DIAG_TYPE,
            "function argument count does not match its static signature",
        ));
    }
    for (expected, actual) in parameters.iter().zip(&values) {
        require_same(expected, actual, diagnostics);
    }
    Inferred {
        ty: *result,
        effects,
    }
}

fn is_numeric_range_bound(ty: &Type) -> bool {
    matches!(ty, Type::Int | Type::Decimal | Type::Float)
}

/// Applies the closed affine-absolute aggregation matrix from the core
/// intrinsic environment.  This is deliberately limited to the core Celsius
/// identity: user-defined unit declaration resolution remains outside this
/// read-only slice, and unrecognised collection pipelines retain their prior
/// conservative diagnostics.
fn infer_affine_collection_aggregate(
    operation: &str,
    element: &Type,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Type> {
    let is_celsius = matches!(
        element,
        Type::Applied { base, arguments }
            if base == "Float"
                && matches!(arguments.as_slice(), [Type::Named(unit)] if unit == "C")
    );
    if !is_celsius {
        return None;
    }
    match operation {
        "max" | "mean" => Some(Type::Optional(Box::new(element.clone()))),
        "sum" => {
            diagnostics.push(diag(
                DIAG_TYPE,
                "sum is invalid for absolute affine quantities",
            ));
            Some(Type::Error)
        }
        _ => None,
    }
}

fn infer_callback(
    expression: &Expr,
    parameter: Type,
    result: Type,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let Expr::Lambda {
        parameters, body, ..
    } = expression
    else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "core operation callback must be a one-parameter lambda",
        ));
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let [lambda_parameter] = parameters.as_slice() else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "core operation callback must take one parameter",
        ));
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let Pattern::Name(name, _) = &lambda_parameter.pattern else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "core operation callback pattern is outside this semantic slice",
        ));
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let mut callback_locals = local.clone();
    callback_locals.insert(
        name.clone(),
        Symbol {
            kind: SymbolKind::Let,
            ty: parameter,
            public: false,
            effects: EffectSummary::default(),
        },
    );
    let inferred = infer(body, scope, &callback_locals, diagnostics);
    require_same(&result, &inferred.ty, diagnostics);
    inferred
}

/// Resolves the small, intrinsic associated-operation surface of a table.
///
/// This deliberately runs only for call expressions: a table is still a
/// relation value in every other expression, and ordinary record field access
/// keeps its existing diagnostics.  The row-shape and mutation execution
/// contracts remain runtime concerns outside this read-only semantic slice.
fn infer_table_operation(
    callee: &Expr,
    arguments: &[orna_syntax_v1::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let Expr::Field { base, name, .. } = callee else {
        return None;
    };
    let table = table_symbol(base, scope, local)?;
    let (parameters, result, effect) = match name.as_str() {
        "insert" | "upsert" => (1, table.ty.clone(), Some("database write")),
        "update" | "rekey" => (2, table.ty.clone(), Some("database write")),
        "delete" => (1, Type::Null, Some("database write")),
        "count" => (0, Type::Int, Some("database read")),
        "first" => (
            0,
            Type::Optional(Box::new(table.ty.clone())),
            Some("database read"),
        ),
        "one" => (0, table.ty.clone(), Some("database read")),
        "as_of" => (1, table.ty.clone(), Some("database read")),
        _ => return None,
    };
    if arguments.len() != parameters {
        diagnostics.push(diag(
            DIAG_TYPE,
            "table operation argument count does not match its static signature",
        ));
    }
    let mut effects = EffectSummary {
        effects: BTreeSet::from([effect.expect("all table operations have an effect").into()]),
        may_fail: true,
    };
    for argument in arguments {
        effects.join(&infer(&argument.value, scope, local, diagnostics).effects);
    }
    Some(Inferred {
        ty: result,
        effects,
    })
}

fn table_symbol<'a>(
    expr: &Expr,
    scope: &'a Scope,
    local: &'a BTreeMap<String, Symbol>,
) -> Option<&'a Symbol> {
    match expr {
        Expr::Name { text, .. } => local
            .get(text)
            .or_else(|| scope.names.get(text))
            .filter(|symbol| symbol.kind == SymbolKind::Table),
        Expr::Field { .. } => {
            let path = qualified_path(expr)?;
            let root = scope.modules.get(path[0])?;
            let namespace = Namespace(
                root.0
                    .iter()
                    .cloned()
                    .chain(
                        path[1..path.len() - 1]
                            .iter()
                            .map(|part| (*part).to_owned()),
                    )
                    .collect(),
            );
            scope
                .available_modules
                .get(&namespace)
                .and_then(|module| module.exports.get(path[path.len() - 1]))
                .filter(|symbol| symbol.kind == SymbolKind::Table)
        }
        _ => None,
    }
}

/// Resolves a member through one explicitly imported module root. A root may
/// name a nested catalogue namespace (`core.json.encode`); intermediate path
/// segments are namespaces, not values, so arbitrary nominal/record values
/// remain governed by ordinary field inference.
fn resolve_qualified_module_member(
    path: &[&str],
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let root = scope.modules.get(path[0]).expect("checked imported root");
    let namespace = Namespace(
        root.0
            .iter()
            .cloned()
            .chain(
                path[1..path.len() - 1]
                    .iter()
                    .map(|part| (*part).to_owned()),
            )
            .collect(),
    );
    let symbol = scope
        .available_modules
        .get(&namespace)
        .and_then(|module| module.exports.get(path[path.len() - 1]));
    match symbol {
        Some(symbol) => Inferred {
            ty: symbol.ty.clone(),
            effects: symbol.effects.clone(),
        },
        None => {
            diagnostics.push(diag(DIAG_UNRESOLVED, "module member cannot be resolved"));
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

fn intrinsic_value_type(name: &str) -> Option<Type> {
    match name {
        "half_even" => Some(Type::Named("std.Rounding".into())),
        _ => None,
    }
}

fn infer_numeric_member(base: &Type, name: &str) -> Option<Type> {
    match (base, name) {
        (Type::Decimal, "divide") => Some(Type::Function {
            parameters: vec![Type::Decimal, Type::Int, Type::Named("std.Rounding".into())],
            parameter_names: Some(vec!["value".into(), "scale".into(), "rounding".into()]),
            result: Box::new(Type::Decimal),
        }),
        _ => None,
    }
}

fn infer_numeric_postfix(base: &Type, name: &str, scope: &Scope) -> Option<Type> {
    match base {
        Type::Decimal if scope.currency_types.contains(name) => Some(Type::Applied {
            base: "Money".into(),
            arguments: vec![Type::Named(name.into())],
        }),
        Type::Int | Type::Decimal | Type::Float if name == "decimal" => Some(Type::Decimal),
        Type::Int | Type::Decimal | Type::Float => Some(Type::Applied {
            base: numeric_base(base)?.into(),
            arguments: vec![Type::Named(name.into())],
        }),
        Type::Applied { base, arguments }
            if arguments.len() == 1
                && arguments
                    .first()
                    .is_some_and(|unit| unit == &Type::Named(name.into())) =>
        {
            Some(Type::Applied {
                base: base.clone(),
                arguments: arguments.clone(),
            })
        }
        _ => None,
    }
}

fn numeric_base(ty: &Type) -> Option<&'static str> {
    match ty {
        Type::Int => Some("Int"),
        Type::Decimal => Some("Decimal"),
        Type::Float => Some("Float"),
        _ => None,
    }
}

fn reduce_exact_money_rate(left: &Type, right: &Type) -> Option<Type> {
    match (left, right) {
        (Type::Applied { base, arguments }, Type::MoneyPerUnit { currency, unit })
        | (Type::MoneyPerUnit { currency, unit }, Type::Applied { base, arguments })
            if base == "Decimal" && arguments.as_slice() == [unit.as_ref().clone()] =>
        {
            Some(Type::Applied {
                base: "Money".into(),
                arguments: vec![currency.as_ref().clone()],
            })
        }
        _ => None,
    }
}

fn intrinsic_call_effects(callee: &Expr) -> EffectSummary {
    let Some(path) = qualified_path(callee) else {
        return EffectSummary::default();
    };
    let effect = match path.as_slice() {
        ["sys", "io", ..] => Some("filesystem"),
        ["sys", "net", ..] => Some("network"),
        ["std", "net", ..] => Some("network"),
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
        let message = match &owner {
            AssertionOwner::Table(name) => {
                format!("table assertion must be a predicate over Relation<{name}>")
            }
            _ => "assertion does not have the required Boolean form".into(),
        };
        diagnostics.push(diag(DIAG_ASSERTION, message));
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

/// Elaborates the two owner-local relational predicate constructors used by
/// the frozen reference corpus. They are not evaluator functions: the table
/// supplies the unpublished `Relation<Row>` subject, and the lambda receives
/// one statically shaped row. All other assertion forms retain ordinary
/// inference and therefore remain unsupported unless the general checker can
/// prove them.
fn infer_table_assertion(
    value: &Expr,
    row: &Type,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let Expr::Call {
        callee, arguments, ..
    } = value
    else {
        return infer(value, scope, &BTreeMap::new(), diagnostics);
    };
    let Expr::Name { text, .. } = callee.as_ref() else {
        return infer(value, scope, &BTreeMap::new(), diagnostics);
    };
    if !matches!(text.as_str(), "every" | "all_unique") || arguments.len() != 1 {
        return infer(value, scope, &BTreeMap::new(), diagnostics);
    }
    let Expr::Lambda {
        parameters, body, ..
    } = &arguments[0].value
    else {
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let [parameter] = parameters.as_slice() else {
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let Pattern::Name(name, _) = &parameter.pattern else {
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let local = BTreeMap::from([(
        name.clone(),
        Symbol {
            kind: SymbolKind::Let,
            ty: row.clone(),
            public: false,
            effects: EffectSummary::default(),
        },
    )]);
    let inferred = infer(body, scope, &local, diagnostics);
    let valid = text == "all_unique" || inferred.ty == Type::Bool;
    Inferred {
        ty: if valid { Type::Bool } else { Type::Error },
        effects: inferred.effects,
    }
}

/// Elaborates the bounded relational query form used by module assertions in
/// the frozen reference project. A module assertion names each relation
/// explicitly, so this has no implicit owner subject and no runtime behavior.
fn infer_module_assertion(
    value: &Expr,
    table_rows: &BTreeMap<String, Type>,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    infer_module_relation(value, table_rows, scope, &BTreeMap::new(), diagnostics)
}

fn infer_module_relation(
    value: &Expr,
    table_rows: &BTreeMap<String, Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let Expr::Call {
        callee, arguments, ..
    } = value
    else {
        return infer(value, scope, local, diagnostics);
    };
    let Expr::Name { text, .. } = callee.as_ref() else {
        return infer(value, scope, local, diagnostics);
    };
    if !matches!(text.as_str(), "every" | "exists") || arguments.len() != 2 {
        return infer(value, scope, local, diagnostics);
    }
    let Expr::Name { text: table, .. } = &arguments[0].value else {
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let Some(row) = table_rows.get(table) else {
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let Expr::Lambda {
        parameters, body, ..
    } = &arguments[1].value
    else {
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let [parameter] = parameters.as_slice() else {
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let Pattern::Name(name, _) = &parameter.pattern else {
        return Inferred {
            ty: Type::Error,
            effects: EffectSummary::default(),
        };
    };
    let mut locals = local.clone();
    locals.insert(
        name.clone(),
        Symbol {
            kind: SymbolKind::Let,
            ty: row.clone(),
            public: false,
            effects: EffectSummary::default(),
        },
    );
    let inferred = infer_module_relation(body, table_rows, scope, &locals, diagnostics);
    Inferred {
        ty: if inferred.ty == Type::Bool {
            Type::Bool
        } else {
            Type::Error
        },
        effects: inferred.effects,
    }
}

fn table_row_type(item: &Item) -> Type {
    let Declaration::Table { keys, members, .. } = &item.declaration else {
        unreachable!("table row type requested for a non-table declaration");
    };
    let mut fields = BTreeMap::new();
    for key in keys {
        if let Pattern::Name(name, _) = &key.pattern {
            fields.insert(
                name.clone(),
                key.annotation.as_ref().map(type_of).unwrap_or(Type::Error),
            );
        }
    }
    for member in members {
        if let orna_syntax_v1::TableMember::Field { name, ty, .. } = member {
            fields.insert(name.clone(), type_of(ty));
        }
    }
    Type::Record(fields)
}

fn nominal_row_type(item: &Item) -> Option<(String, Type)> {
    let Declaration::Type {
        name,
        representation: TypeRepresentation::Nominal { members },
        ..
    } = &item.declaration
    else {
        return None;
    };
    let fields = members
        .iter()
        .filter_map(|member| match member {
            TypeMember::Field { name, ty, .. } => Some((name.clone(), type_of(ty))),
            TypeMember::Assertion { .. } | TypeMember::Implementation { .. } => None,
        })
        .collect();
    Some((name.clone(), Type::Record(fields)))
}

fn currency_types(tree: &SyntaxTree) -> BTreeSet<String> {
    tree.items
        .iter()
        .filter_map(|item| {
            let Declaration::Type {
                name,
                representation: TypeRepresentation::Nominal { members },
                ..
            } = &item.declaration
            else {
                return None;
            };
            members
                .iter()
                .any(|member| {
                    matches!(
                        member,
                        TypeMember::Implementation { implementation, .. }
                            if matches!(
                                &implementation.protocol,
                                TypeExpr::Name { path, arguments, .. }
                                    if path.as_slice() == ["Currency"] && arguments.is_empty()
                            )
                    )
                })
                .then_some(name.clone())
        })
        .collect()
}

fn enum_variant_types(
    tree: &SyntaxTree,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeMap<String, BTreeMap<String, BTreeMap<String, Type>>> {
    let mut enums = BTreeMap::new();
    for item in &tree.items {
        let Declaration::Enum { name, variants, .. } = &item.declaration else {
            continue;
        };
        let mut typed_variants = BTreeMap::new();
        for variant in variants {
            let mut fields = BTreeMap::new();
            for field in &variant.fields {
                if fields
                    .insert(field.name.clone(), type_of(&field.ty))
                    .is_some()
                {
                    diagnostics.push(diag(DIAG_DUPLICATE, "duplicate enum payload field"));
                }
            }
            if typed_variants
                .insert(variant.name.clone(), fields)
                .is_some()
            {
                diagnostics.push(diag(DIAG_DUPLICATE, "duplicate enum variant"));
            }
        }
        enums.insert(name.clone(), typed_variants);
    }
    enums
}

fn tables_referenced(expr: &Expr) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    fn visit(e: &Expr, names: &mut BTreeSet<String>) {
        match e {
            Expr::Name { text, .. } if text.chars().next().is_some_and(char::is_uppercase) => {
                names.insert(text.clone());
            }
            Expr::InterpolatedString { segments, .. } => {
                for segment in segments {
                    if let StringSegment::Expression { value, .. } = segment {
                        visit(value, names);
                    }
                }
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
                arms,
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
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        visit(guard, names);
                    }
                    visit(&arm.body, names);
                }
            }
            _ => {}
        }
    }
    visit(expr, &mut names);
    names
}
fn diag(code: &'static str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(
        SafeText::new(code).expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new(message.into()).expect("valid diagnostic message"),
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
                parameter_names: Some(vec![]),
                result: Box::new(Type::Record(BTreeMap::from([("a".into(), Type::Int)])))
            }
        );
        assert_eq!(
            m.symbols.get("list").expect("list is collected").ty,
            Type::Function {
                parameters: vec![],
                parameter_names: Some(vec![]),
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
