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
    LambdaParameter, LiteralKind, Pattern, ProtocolMember, Statement, StringSegment, SyntaxTree,
    TypeExpr, TypeMember, TypeRepresentation, UseTail, Visibility, parse_module_with_file,
};
use sha2::{Digest, Sha256};
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
pub const DIAG_ASSERTION_ONE_TABLE: &str = "ORNA-A091-003";
pub const DIAG_ASSERTION_SCOPE: &str = "ORNA-A091-012";
pub const DIAG_ASSERTION_EFFECT: &str = "ORNA-A091-007";
pub const DIAG_LEGACY_SYS_RUNTIME: &str = "ORNA100-E-SYS-RUNTIME";
pub const DIAG_LEGACY_TRYFROM: &str = "ORNA091-E-TRYFROM";

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

/// A verified dependency boundary for optional standard-library source.
///
/// This descriptor records the immutable source snapshot and the canonical
/// digest of each admitted `std` module. It does not discover files, install a
/// host library, or provide declarations by name alone. A later loader may
/// verify source bytes against this descriptor before converting them into
/// semantic catalogue entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StandardDependencyProfile {
    snapshot: String,
    module_digests: BTreeMap<String, [u8; 32]>,
    prelude_exports: BTreeSet<String>,
}

impl StandardDependencyProfile {
    /// Creates an empty profile for a pinned snapshot.
    pub fn empty(snapshot: impl Into<String>) -> Result<Self, StandardProfileError> {
        let snapshot = snapshot.into();
        if snapshot.is_empty() {
            return Err(StandardProfileError::EmptySnapshot);
        }
        Ok(Self {
            snapshot,
            module_digests: BTreeMap::new(),
            prelude_exports: BTreeSet::new(),
        })
    }

    /// Computes a profile from caller-supplied source bytes in deterministic
    /// logical-path order. The bytes are not retained after digesting.
    pub fn from_sources(
        snapshot: impl Into<String>,
        sources: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, StandardProfileError> {
        let mut profile = Self::empty(snapshot)?;
        for (logical_path, source) in sources {
            if !is_standard_module_path(&logical_path) {
                return Err(StandardProfileError::InvalidModulePath);
            }
            if profile
                .module_digests
                .insert(logical_path, digest_source(&source))
                .is_some()
            {
                return Err(StandardProfileError::DuplicateModule);
            }
        }
        Ok(profile)
    }

    pub fn snapshot(&self) -> &str {
        &self.snapshot
    }

    pub fn module_digests(&self) -> &BTreeMap<String, [u8; 32]> {
        &self.module_digests
    }

    /// Records the exact public names exported by the pinned root prelude.
    /// The names are metadata only until the matching `std.orna` source is
    /// verified and parsed by `Catalogue::from_standard_sources`.
    pub fn with_prelude_exports(
        mut self,
        names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.prelude_exports = names.into_iter().map(Into::into).collect();
        self
    }

    pub fn prelude_exports(&self) -> &BTreeSet<String> {
        &self.prelude_exports
    }

    /// Verifies one source unit against the exact pinned module digest.
    pub fn verify_source(
        &self,
        logical_path: &str,
        source: &str,
    ) -> Result<(), StandardProfileError> {
        if !is_standard_module_path(logical_path) {
            return Err(StandardProfileError::InvalidModulePath);
        }
        let expected = self
            .module_digests
            .get(logical_path)
            .ok_or(StandardProfileError::MissingModule)?;
        if digest_source(source) != *expected {
            return Err(StandardProfileError::DigestMismatch);
        }
        Ok(())
    }
}

/// Source-free, stable failures for standard dependency admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandardProfileError {
    EmptySnapshot,
    InvalidModulePath,
    DuplicateModule,
    MissingModule,
    DigestMismatch,
    InvalidPreludeExport,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandardCatalogueError {
    Profile(StandardProfileError),
    InvalidModulePath,
    DuplicateModule,
    MissingModule,
    InvalidSource,
    DuplicateDeclaration,
}

impl From<StandardProfileError> for StandardCatalogueError {
    fn from(error: StandardProfileError) -> Self {
        Self::Profile(error)
    }
}

fn digest_source(source: &str) -> [u8; 32] {
    Sha256::digest(source.as_bytes()).into()
}

fn is_standard_module_path(path: &str) -> bool {
    let mut components = path.split('/').collect::<Vec<_>>();
    let Some(file) = components.pop() else {
        return false;
    };
    let Some(stem) = file.strip_suffix(".orna") else {
        return false;
    };
    components.first() == Some(&"std")
        && valid_standard_component(stem)
        && components
            .iter()
            .skip(1)
            .all(|component| valid_standard_component(component))
}

fn valid_standard_component(component: &str) -> bool {
    !component.is_empty() && !component.contains('.')
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
        /// Positions with a declaration-provided default; omitted arguments
        /// are legal only at these positions.
        default_parameters: BTreeSet<usize>,
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
    /// Declaration-backed write admission, retained through module exports.
    /// None means the caller supplied no field-admission metadata.
    pub table_schema: Option<TableSchema>,
}

impl Symbol {
    fn table_fields(&self) -> Option<&BTreeMap<String, Type>> {
        self.table_schema
            .as_ref()
            .map(|schema| &schema.fields)
            .or(match &self.ty {
                Type::Record(fields) => Some(fields),
                _ => None,
            })
    }
}

/// Static field shape and insertion rules; defaults are not evaluated here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableSchema {
    pub fields: BTreeMap<String, Type>,
    /// None preserves a shape-only catalogue without inventing declaration rules.
    pub admission: Option<TableAdmission>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableAdmission {
    pub required: BTreeSet<String>,
    pub computed: BTreeSet<String>,
    /// Ordered explicit key columns, or the implicit automatic id column.
    pub keys: Vec<(String, Type)>,
    pub automatic_key: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleHeader {
    pub namespace: Namespace,
    pub exports: BTreeMap<String, Symbol>,
    pub symbols: BTreeMap<String, Symbol>,
    pub prelude_exports: BTreeSet<String>,
    /// Attached catalogue modules may be available without a source `use`.
    /// Source modules remain explicit by default.
    pub implicit: bool,
}

/// Explicit, caller-provided declarations available in addition to source
/// modules.  A catalogue is data, not a loader: this crate never reads a
/// standard-library directory or treats an absent name as a standard name.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Catalogue {
    modules: BTreeMap<Namespace, ModuleHeader>,
    attached_symbols: BTreeMap<String, Symbol>,
}
impl Catalogue {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Builds a declaration catalogue from source bytes that match one
    /// explicitly pinned standard dependency profile. The source is parsed to
    /// derive declarations, but bodies are not executed and the bytes are not
    /// retained after this call.
    pub fn from_standard_sources(
        profile: &StandardDependencyProfile,
        sources: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, StandardCatalogueError> {
        let mut catalogue = Self::empty();
        let mut seen = BTreeSet::new();
        let mut supplied = BTreeSet::new();
        for (logical_path, source) in sources {
            profile.verify_source(&logical_path, &source)?;
            if !seen.insert(logical_path.clone()) {
                return Err(StandardCatalogueError::DuplicateModule);
            }
            let namespace = namespace_for_path(&logical_path, &mut Vec::new())
                .ok_or(StandardCatalogueError::InvalidModulePath)?;
            if namespace.0.first().map(String::as_str) != Some("std") {
                return Err(StandardCatalogueError::InvalidModulePath);
            }
            let parsed = parse_module_with_file(&source, &logical_path);
            if !parsed.is_ok() {
                return Err(StandardCatalogueError::InvalidSource);
            }
            let mut diagnostics = Vec::new();
            let prelude_exports = if namespace == Namespace(vec!["std".into()]) {
                &profile.prelude_exports
            } else {
                &BTreeSet::new()
            };
            let header =
                collect_header(&namespace, &parsed.value, prelude_exports, &mut diagnostics);
            if diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code() == DIAG_DUPLICATE)
            {
                return Err(StandardCatalogueError::DuplicateDeclaration);
            }
            if namespace == Namespace(vec!["std".into()])
                && !profile
                    .prelude_exports
                    .iter()
                    .all(|name| header.exports.contains_key(name))
            {
                return Err(StandardCatalogueError::Profile(
                    StandardProfileError::InvalidPreludeExport,
                ));
            }
            if catalogue.modules.insert(namespace, header).is_some() {
                return Err(StandardCatalogueError::DuplicateModule);
            }
            supplied.insert(logical_path);
        }
        if supplied != profile.module_digests.keys().cloned().collect() {
            return Err(StandardCatalogueError::MissingModule);
        }
        if !profile.prelude_exports.is_empty()
            && !catalogue
                .modules
                .contains_key(&Namespace(vec!["std".into()]))
        {
            return Err(StandardCatalogueError::Profile(
                StandardProfileError::InvalidPreludeExport,
            ));
        }
        Ok(catalogue)
    }

    /// Replaces the matching source-backed standard modules in an existing
    /// core catalogue after the source bundle has passed profile verification.
    /// Core declarations remain available for system and language surfaces;
    /// source-backed modules provide the executable standard declarations.
    pub fn with_standard_sources(
        mut self,
        profile: &StandardDependencyProfile,
        sources: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, StandardCatalogueError> {
        let standard = Self::from_standard_sources(profile, sources)?;
        for (namespace, header) in standard.modules {
            self.modules.insert(namespace, header);
        }
        Ok(self)
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
            Namespace(vec!["std".into(), "text".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "text".into()]),
                [
                    (
                        "slug",
                        named_function(vec![("value", Type::Text)], Type::Text),
                    ),
                    (
                        "disambiguate",
                        named_function(
                            vec![("value", Type::Text), ("keys", Type::Error)],
                            Type::Text,
                        ),
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
            Namespace(vec!["std".into(), "encoding".into(), "json".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "encoding".into(), "json".into()]),
                [
                    (
                        "encode",
                        named_function(vec![("value", Type::Error)], byte_stream.clone()),
                    ),
                    (
                        "decode",
                        named_function(
                            vec![("value", Type::Error), ("as", Type::Error)],
                            Type::Error,
                        ),
                    ),
                ],
                std::iter::empty::<&str>(),
            ),
        );
        modules.insert(
            Namespace(vec!["std".into(), "encoding".into(), "orna".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "encoding".into(), "orna".into()]),
                [(
                    "encode",
                    named_function(vec![("value", Type::Error)], byte_stream.clone()),
                )],
                std::iter::empty::<&str>(),
            ),
        );
        let duration = Type::Named("std.DURATION".into());
        for segment in ["compact", "clock", "words", "iso"] {
            let namespace = Namespace(vec![
                "std".into(),
                "time".into(),
                "duration".into(),
                segment.into(),
            ]);
            modules.insert(
                namespace.clone(),
                catalogue_module(
                    namespace,
                    [(
                        "format",
                        named_function(vec![("duration", duration.clone())], Type::Text),
                    )],
                    std::iter::empty::<&str>(),
                ),
            );
        }
        modules.insert(
            Namespace(vec!["std".into(), "money".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "money".into()]),
                [(
                    "format",
                    named_function(
                        vec![
                            (
                                "value",
                                Type::Applied {
                                    base: "Money".into(),
                                    arguments: vec![Type::Error],
                                },
                            ),
                            ("locale", Type::Named("Locale".into())),
                        ],
                        Type::Text,
                    ),
                )],
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
        modules.insert(
            Namespace(vec!["std".into(), "net".into(), "http".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "net".into(), "http".into()]),
                [("get", named_function(vec![("url", Type::Text)], Type::Text))],
                std::iter::empty::<&str>(),
            ),
        );
        let secret_ref = Type::Named("std.SecretRef".into());
        modules.insert(
            Namespace(vec!["std".into(), "secret".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "secret".into()]),
                [
                    (
                        "ref",
                        named_function(vec![("name", Type::Text)], secret_ref.clone()),
                    ),
                    (
                        "open",
                        named_function(
                            vec![("reference", secret_ref), ("as", Type::Error)],
                            Type::Applied {
                                base: "Secret".into(),
                                arguments: vec![Type::Error],
                            },
                        ),
                    ),
                ],
                std::iter::empty::<&str>(),
            ),
        );
        let ui = Type::Named("std.UI".into());
        let page_builder = Type::Function {
            parameters: vec![Type::Error],
            parameter_names: None,
            default_parameters: BTreeSet::new(),
            result: Box::new(ui.clone()),
        };
        modules.insert(
            Namespace(vec!["std".into(), "ui".into()]),
            catalogue_module(
                Namespace(vec!["std".into(), "ui".into()]),
                [
                    ("UI", ui.clone()),
                    ("text", function(vec![Type::Text], ui.clone())),
                    ("button", function(vec![Type::Text, Type::Bool], ui.clone())),
                    (
                        "Page",
                        named_function(
                            vec![("path", Type::Text), ("builder", page_builder)],
                            ui.clone(),
                        ),
                    ),
                    ("List", function(vec![Type::Error], ui.clone())),
                    ("Table", function(vec![Type::Error], ui.clone())),
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
        Self {
            modules,
            attached_symbols: BTreeMap::new(),
        }
    }

    /// The explicit core catalogue plus the attached environment used by the
    /// reference conformance examples.  Attached tables and connectors are
    /// declarations supplied by the caller; this profile does not execute or
    /// discover them from the host.
    pub fn authoritative_fixture() -> Self {
        let mut catalogue = Self::authoritative_core();
        let contact = Type::Record(BTreeMap::from([
            ("first".into(), Type::Text),
            ("emails".into(), Type::List(Box::new(Type::Text))),
            ("full_name".into(), Type::Text),
            ("id".into(), Type::Text),
            ("last".into(), Type::Text),
            ("name".into(), Type::Text),
        ]));
        let reading = Type::Record(BTreeMap::from([
            ("rpm".into(), Type::Int),
            (
                "speed".into(),
                Type::Applied {
                    base: "Float".into(),
                    arguments: vec![Type::Named("mph".into())],
                },
            ),
            ("time".into(), Type::Instant),
        ]));
        let message = Type::Record(BTreeMap::from([
            ("body_html".into(), Type::Text),
            ("body_text".into(), Type::Text),
            ("from".into(), Type::Text),
            ("id".into(), Type::Text),
            ("received".into(), Type::Instant),
            ("sent".into(), Type::Instant),
            ("subject".into(), Type::Text),
            ("to".into(), Type::Text),
        ]));
        // A stored email has provider identity in addition to the connector's
        // message fields; provider messages do not themselves acquire it.
        let email = match &message {
            Type::Record(fields) => {
                let mut fields = fields.clone();
                fields.insert("provider".into(), Type::Text);
                Type::Record(fields)
            }
            _ => unreachable!("message catalogue shape is a record"),
        };
        let note = Type::Record(BTreeMap::from([
            ("created".into(), Type::Instant),
            ("text".into(), Type::Text),
        ]));
        let customer = Type::Named("Customer".into());
        let order = Type::Record(BTreeMap::from([
            ("created".into(), Type::Instant),
            ("customer".into(), customer.clone()),
        ]));
        let payment = Type::Record(BTreeMap::from([
            ("amount".into(), Type::Error),
            ("order".into(), Type::Named("Order".into())),
        ]));
        let audit = Type::Record(BTreeMap::from([
            ("action".into(), Type::Text),
            ("order".into(), Type::Named("Order".into())),
        ]));

        let contact_table = fixture_table_with_admission(
            "Contact",
            contact.clone(),
            TableAdmission {
                required: BTreeSet::from(["name".into()]),
                computed: BTreeSet::from(["full_name".into()]),
                keys: vec![("id".into(), Type::Text)],
                automatic_key: false,
            },
        );
        catalogue.attached_symbols = [
            fixture_type("Account", Type::Named("Account".into())),
            fixture_table("Audit", audit),
            contact_table.clone(),
            fixture_type("Customer", customer),
            fixture_table("Message", Type::Named("Message".into())),
            fixture_table("Note", note),
            fixture_table("Order", order),
            fixture_table("Payment", payment),
        ]
        .into_iter()
        .collect();
        catalogue.modules.insert(
            Namespace(vec!["Message".into()]),
            fixture_module(
                Namespace(vec!["Message".into()]),
                [fixture_function(
                    "default",
                    function(Vec::new(), Type::Named("Message".into())),
                )],
                true,
            ),
        );
        catalogue.modules.insert(
            Namespace(vec!["contacts".into()]),
            fixture_module(Namespace(vec!["contacts".into()]), [contact_table], true),
        );
        catalogue.modules.insert(
            Namespace(vec!["energy".into()]),
            fixture_module(
                Namespace(vec!["energy".into()]),
                [
                    fixture_function("daily", function(vec![Type::Error], Type::Error)),
                    fixture_table("Reading", reading.clone()),
                ],
                true,
            ),
        );
        catalogue.modules.insert(
            Namespace(vec!["mail".into()]),
            fixture_module(
                Namespace(vec!["mail".into()]),
                [fixture_table("Email", email)],
                true,
            ),
        );
        catalogue.modules.insert(
            Namespace(vec!["mail".into(), "google".into()]),
            fixture_module(
                Namespace(vec!["mail".into(), "google".into()]),
                [fixture_value("sync", Type::Error)],
                true,
            ),
        );
        catalogue.modules.insert(
            Namespace(vec!["finance".into()]),
            fixture_module(Namespace(vec!["finance".into()]), [], true),
        );
        catalogue.modules.insert(
            Namespace(vec!["finance".into(), "openbanking".into()]),
            fixture_module(
                Namespace(vec!["finance".into(), "openbanking".into()]),
                [fixture_value("sync", Type::Error)],
                true,
            ),
        );
        catalogue.modules.insert(
            Namespace(vec!["openbanking".into()]),
            fixture_module(
                Namespace(vec!["openbanking".into()]),
                [fixture_function(
                    "transactions",
                    function(Vec::new(), Type::Stream(Box::new(Type::Error))),
                )],
                true,
            ),
        );
        catalogue.modules.insert(
            Namespace(vec!["google".into()]),
            fixture_module(
                Namespace(vec!["google".into()]),
                [fixture_function(
                    "mail",
                    named_function(
                        vec![("credential", Type::Named("std.SecretRef".into()))],
                        Type::Stream(Box::new(message.clone())),
                    ),
                )],
                true,
            ),
        );
        catalogue.modules.insert(
            Namespace(vec!["vehicle".into()]),
            fixture_module(Namespace(vec!["vehicle".into()]), [], true),
        );
        catalogue.modules.insert(
            Namespace(vec!["vehicle".into(), "corsa".into()]),
            fixture_module(
                Namespace(vec!["vehicle".into(), "corsa".into()]),
                [
                    fixture_table("Reading", reading),
                    fixture_value("trips", Type::Named("vehicle.corsa.trips".into())),
                ],
                true,
            ),
        );
        catalogue.modules.insert(
            Namespace(vec!["vehicle".into(), "corsa".into(), "freematics".into()]),
            fixture_module(
                Namespace(vec!["vehicle".into(), "corsa".into(), "freematics".into()]),
                [fixture_value("sync", Type::Error)],
                true,
            ),
        );
        catalogue.modules.insert(
            Namespace(vec!["bank".into()]),
            fixture_module(
                Namespace(vec!["bank".into()]),
                [fixture_value("main", Type::Named("Account".into()))],
                true,
            ),
        );
        catalogue.modules.insert(
            Namespace(vec!["Europe".into()]),
            fixture_module(
                Namespace(vec!["Europe".into()]),
                [fixture_value("London", Type::Named("std.TimeZone".into()))],
                true,
            ),
        );
        catalogue.modules.insert(
            Namespace(vec!["std".into(), "concurrent".into()]),
            fixture_module(
                Namespace(vec!["std".into(), "concurrent".into()]),
                [fixture_function(
                    "parallel",
                    function(
                        vec![Type::List(Box::new(Type::Error))],
                        Type::Stream(Box::new(Type::Error)),
                    ),
                )],
                false,
            ),
        );
        catalogue.modules.insert(
            Namespace(vec!["std".into(), "encoding".into(), "csv".into()]),
            fixture_module(
                Namespace(vec!["std".into(), "encoding".into(), "csv".into()]),
                [fixture_function(
                    "rows",
                    function(
                        vec![Type::Named("std.ByteStream".into())],
                        Type::Stream(Box::new(Type::Record(BTreeMap::new()))),
                    ),
                )],
                false,
            ),
        );
        catalogue.modules.insert(
            Namespace(vec!["std".into(), "io".into(), "fs".into()]),
            fixture_module(
                Namespace(vec!["std".into(), "io".into(), "fs".into()]),
                [fixture_function(
                    "read",
                    function(vec![Type::Text], Type::Named("std.ByteStream".into())),
                )],
                false,
            ),
        );
        catalogue
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
                    table_schema: None,
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
        implicit: false,
    }
}

fn fixture_module<I>(namespace: Namespace, symbols: I, implicit: bool) -> ModuleHeader
where
    I: IntoIterator<Item = (String, Symbol)>,
{
    let symbols = symbols
        .into_iter()
        .map(|(name, mut symbol)| {
            if symbol.kind == SymbolKind::Table {
                let prefix = namespace.display();
                symbol.ty = Type::Named(if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                });
            }
            (name, symbol)
        })
        .collect::<BTreeMap<_, _>>();
    ModuleHeader {
        namespace,
        exports: symbols.clone(),
        symbols,
        prelude_exports: BTreeSet::new(),
        implicit,
    }
}

fn fixture_symbol(kind: SymbolKind, ty: Type) -> Symbol {
    Symbol {
        table_schema: None,
        kind,
        ty,
        public: true,
        effects: EffectSummary::default(),
    }
}

fn fixture_function(name: &str, ty: Type) -> (String, Symbol) {
    (name.into(), fixture_symbol(SymbolKind::Function, ty))
}

fn fixture_table(name: &str, row: Type) -> (String, Symbol) {
    let mut symbol = fixture_symbol(SymbolKind::Table, Type::Named(name.into()));
    if let Type::Record(fields) = row {
        symbol.table_schema = Some(TableSchema {
            fields,
            admission: None,
        });
    }
    (name.into(), symbol)
}

fn fixture_table_with_admission(
    name: &str,
    row: Type,
    admission: TableAdmission,
) -> (String, Symbol) {
    let (name, mut symbol) = fixture_table(name, row);
    if let Some(schema) = &mut symbol.table_schema {
        schema.admission = Some(admission);
    }
    (name, symbol)
}

fn fixture_type(name: &str, ty: Type) -> (String, Symbol) {
    (name.into(), fixture_symbol(SymbolKind::Type, ty))
}

fn fixture_value(name: &str, ty: Type) -> (String, Symbol) {
    (name.into(), fixture_symbol(SymbolKind::Let, ty))
}

fn function(parameters: Vec<Type>, result: Type) -> Type {
    Type::Function {
        parameters,
        parameter_names: None,
        default_parameters: BTreeSet::new(),
        result: Box::new(result),
    }
}

fn named_function(parameters: Vec<(&str, Type)>, result: Type) -> Type {
    let parameter_names = parameters
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    Type::Function {
        parameters: parameters.into_iter().map(|(_, ty)| ty).collect(),
        parameter_names: Some(parameter_names),
        default_parameters: BTreeSet::new(),
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
    stabilize_function_summaries(&parsed, &mut result.modules, &catalogue.attached_symbols);
    for (namespace, tree) in &parsed {
        let Some(header) = result.modules.get(namespace).cloned() else {
            continue;
        };
        let scope = resolve_imports(
            namespace,
            tree,
            &header,
            &result.modules,
            &catalogue.attached_symbols,
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
    attached_symbols: &BTreeMap<String, Symbol>,
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
            let scope = resolve_imports(
                namespace,
                tree,
                &header,
                modules,
                attached_symbols,
                &mut discarded,
            );
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
        let Some((name, kind, ty)) = declared_symbol(item) else {
            continue;
        };
        let public = matches!(item.visibility, Visibility::Public { .. });
        if symbols
            .insert(
                name,
                Symbol {
                    table_schema: declared_table_schema(item),
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
        implicit: false,
    }
}
fn declared_symbol(item: &Item) -> Option<(String, SymbolKind, Type)> {
    match &item.declaration {
        Declaration::Function { signature, body } => {
            let parameters = signature
                .parameters
                .iter()
                .map(|p| {
                    p.annotation.as_ref().map(type_of).unwrap_or_else(|| {
                        inferred_function_parameter_type(body, p).unwrap_or(Type::Error)
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
                    default_parameters: signature
                        .parameters
                        .iter()
                        .enumerate()
                        .filter_map(|(index, parameter)| parameter.default.as_ref().map(|_| index))
                        .collect(),
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
            default_parameters: BTreeSet::new(),
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
        "Duration" => Type::Named("std.DURATION".into()),
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
    /// Tables without declared keys use the implicit automatic key and cannot
    /// be explicitly re-keyed by source code.
    auto_key_tables: BTreeSet<String>,
    /// Local nominal record constructors elaborate to their declared row shape
    /// so field access remains structural inside inferred stream pipelines.
    nominal_rows: BTreeMap<String, Type>,
    /// Local refined aliases retain their underlying representation so the
    /// closed `Port.from(value)` constructor can check the source value
    /// without erasing the refined nominal result.
    refined_types: BTreeMap<String, Type>,
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
    attached_symbols: &BTreeMap<String, Symbol>,
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
        auto_key_tables: tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Table { name, keys, .. } if keys.is_empty() => Some(name.clone()),
                _ => None,
            })
            .collect(),
        nominal_rows: tree.items.iter().filter_map(nominal_row_type).collect(),
        refined_types: tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Type {
                    name,
                    representation: TypeRepresentation::Alias { ty, refinements },
                    ..
                } if refinements
                    .iter()
                    .any(|member| matches!(member, TypeMember::Assertion { .. })) =>
                {
                    Some((name.clone(), type_of(ty)))
                }
                _ => None,
            })
            .collect(),
        currency_types: currency_types(tree),
        enum_variants: enum_variant_types(tree, diagnostics),
    };
    for (name, symbol) in attached_symbols {
        scope
            .names
            .entry(name.clone())
            .or_insert_with(|| symbol.clone());
        if symbol.kind == SymbolKind::Table
            && let Some(row) = symbol.table_fields()
        {
            scope
                .table_rows
                .entry(name.clone())
                .or_insert_with(|| Type::Record(row.clone()));
        }
    }
    for (namespace, module) in modules {
        let prefix = namespace.display();
        for (name, symbol) in &module.exports {
            if symbol.kind != SymbolKind::Table {
                continue;
            }
            let Some(row) = symbol.table_fields() else {
                continue;
            };
            let table_name = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            scope
                .table_rows
                .entry(table_name)
                .or_insert_with(|| Type::Record(row.clone()));
        }
    }
    if modules.contains_key(&Namespace(vec!["std".into()])) {
        scope
            .modules
            .insert("std".into(), Namespace(vec!["std".into()]));
    }
    for (module_namespace, module) in modules {
        if module.implicit && module_namespace.0.len() == 1 && !module_namespace.0[0].is_empty() {
            scope
                .modules
                .entry(module_namespace.0[0].clone())
                .or_insert_with(|| module_namespace.clone());
        }
    }
    let mut explicit = BTreeMap::<String, Symbol>::new();
    let mut glob = BTreeMap::<String, Vec<Symbol>>::new();
    for item in &tree.items {
        let Declaration::Use { path, tail } = &item.declaration else {
            continue;
        };
        let target = Namespace(path.iter().map(|x| x.name.clone()).collect());
        if target.0.first().is_some_and(|r| r == "sys") {
            let binding = Symbol {
                table_schema: None,
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
                        table_schema: None,
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
                        table_schema: None,
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
        Declaration::Protocol { name, members, .. } if name == "Currency" => {
            for member in members {
                if let ProtocolMember::Static { name, .. } = member
                    && name == "symbol"
                {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "currency symbols belong to locale-aware formatting, not Currency identity",
                    ));
                }
            }
        }
        Declaration::Assertion { value } => {
            let inferred = infer_module_assertion(value, table_rows, scope, diagnostics);
            assertion(AssertionOwner::Module, value, inferred, plans, diagnostics);
        }
        Declaration::Table {
            name,
            keys,
            members,
            ..
        } => {
            if name == "std" {
                diagnostics.push(diag(DIAG_TYPE, "`std` is reserved"));
            } else if name == "sys" {
                diagnostics.push(diag(DIAG_TYPE, "`sys` is reserved"));
            }
            for key in keys {
                let ty = key.annotation.as_ref().map(type_of).unwrap_or(Type::Error);
                let is_range = matches!(&ty, Type::Applied { base, .. } if base == "Range");
                let is_float = matches!(&ty, Type::Float)
                    || matches!(&ty, Type::Applied { base, .. } if base == "Float");
                if is_range || is_float {
                    let message = if is_range {
                        "Range<T> is not a primary-key type in version 1.0"
                    } else {
                        "Float is not a valid primary-key type"
                    };
                    diagnostics.push(diag(DIAG_TYPE, message));
                }
            }
            let row = table_row_type(item);
            let row_locals = match &row {
                Type::Record(fields) => fields
                    .iter()
                    .map(|(field, ty)| {
                        (
                            field.clone(),
                            Symbol {
                                table_schema: None,
                                kind: SymbolKind::Let,
                                ty: ty.clone(),
                                public: false,
                                effects: EffectSummary::default(),
                            },
                        )
                    })
                    .collect(),
                _ => BTreeMap::new(),
            };
            for member in members {
                match member {
                    orna_syntax_v1::TableMember::Assertion { value, .. } => {
                        let inferred = infer_table_assertion(value, name, &row, scope, diagnostics);
                        assertion(
                            AssertionOwner::Table(name.clone()),
                            value,
                            inferred,
                            plans,
                            diagnostics,
                        );
                    }
                    orna_syntax_v1::TableMember::Field {
                        initializer: Some(initializer),
                        ty,
                        ..
                    } => {
                        let value = match initializer {
                            FieldInitializer::Default(value)
                            | FieldInitializer::Computed(value) => value,
                        };
                        let expected = type_of(ty);
                        let inferred =
                            infer_contextual(value, &expected, scope, &row_locals, diagnostics);
                        if matches!(initializer, FieldInitializer::Computed(_))
                            && (!inferred.effects.effects.is_empty() || inferred.effects.may_fail)
                        {
                            diagnostics.push(diag(
                                DIAG_TYPE,
                                "computed field must be deterministic and row-local",
                            ));
                        }
                        require_same(&expected, &inferred.ty, diagnostics);
                    }
                    _ => {}
                }
            }
        }
        Declaration::Type {
            name,
            representation: TypeRepresentation::Alias { ty, refinements },
            ..
        } => {
            let base = type_of(ty);
            for member in refinements {
                if let TypeMember::Assertion { value, .. } = member {
                    let inferred = infer_refined_assertion(value, &base, scope, diagnostics);
                    assertion(
                        AssertionOwner::RefinedType(name.clone()),
                        value,
                        inferred,
                        plans,
                        diagnostics,
                    );
                }
            }
        }
        Declaration::Type {
            representation: TypeRepresentation::Nominal { members },
            ..
        } => {
            for member in members {
                let TypeMember::Implementation { implementation, .. } = member else {
                    continue;
                };
                match &implementation.protocol {
                    TypeExpr::Name {
                        path, arguments, ..
                    } if path.as_slice() == ["TryFrom"] && arguments.len() == 1 => {
                        diagnostics.push(diag(
                            DIAG_LEGACY_TRYFROM,
                            "use From<Source>; From may fail in Orna",
                        ));
                    }
                    TypeExpr::Name { path, .. }
                        if matches!(path.as_slice(), [protocol] if protocol == "Display" || protocol == "Present")
                            && implementation_has_write(implementation) =>
                    {
                        diagnostics.push(diag(
                            DIAG_TYPE,
                            "Display and Present implementations must be read-only",
                        ));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn implementation_has_write(implementation: &orna_syntax_v1::Implementation) -> bool {
    implementation.members.iter().any(|member| match member {
        orna_syntax_v1::ImplMember::Function { body, .. }
        | orna_syntax_v1::ImplMember::Static { value: body, .. } => expr_has_write(body),
    })
}

fn expr_has_write(expr: &Expr) -> bool {
    match expr {
        Expr::Call {
            callee, arguments, ..
        } => {
            qualified_path(callee)
                .and_then(|path| path.last().copied())
                .is_some_and(|name| {
                    matches!(name, "insert" | "upsert" | "update" | "delete" | "rekey")
                })
                || expr_has_write(callee)
                || arguments
                    .iter()
                    .any(|argument| expr_has_write(&argument.value))
        }
        Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => expr_has_write(rhs),
        Expr::Binary { lhs, rhs, .. } => expr_has_write(lhs) || expr_has_write(rhs),
        Expr::Index { base, index, .. } => expr_has_write(base) || expr_has_write(index),
        Expr::Field { base, .. } => expr_has_write(base),
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
            elements.iter().any(expr_has_write)
        }
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
            fields.iter().any(|field| expr_has_write(&field.value))
        }
        Expr::Lambda { body, .. } => expr_has_write(body),
        Expr::Block {
            statements, tail, ..
        } => {
            statements.iter().any(statement_has_write)
                || tail.as_deref().is_some_and(expr_has_write)
        }
        Expr::Control {
            condition,
            body,
            arms,
            alternate,
            ..
        } => {
            condition.as_deref().is_some_and(expr_has_write)
                || body.as_deref().is_some_and(expr_has_write)
                || arms.iter().any(|arm| {
                    arm.guard.as_ref().is_some_and(expr_has_write) || expr_has_write(&arm.body)
                })
                || alternate.as_deref().is_some_and(expr_has_write)
        }
        _ => false,
    }
}

fn statement_has_write(statement: &Statement) -> bool {
    match statement {
        Statement::Let { value, .. }
        | Statement::Assert { value, .. }
        | Statement::Expression { value, .. }
        | Statement::Control { value, .. }
        | Statement::Assignment { value, .. } => expr_has_write(value),
        Statement::Return { value, .. } | Statement::Break { value, .. } => {
            value.as_ref().is_some_and(expr_has_write)
        }
        Statement::Continue { .. } => false,
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
    let mut default_effects = EffectSummary::default();
    for parameter in &signature.parameters {
        let ty = parameter
            .annotation
            .as_ref()
            .map(type_of)
            .or_else(|| inferred_function_parameter_type(body, parameter));
        if ty.is_none() {
            diagnostics.push(diag(
                DIAG_ANNOTATION,
                "function parameter needs a static annotation",
            ));
        }
        if let Some(default) = &parameter.default {
            let inferred = infer_contextual(
                default,
                ty.as_ref().unwrap_or(&Type::Error),
                scope,
                &local,
                diagnostics,
            );
            require_same(
                ty.as_ref().unwrap_or(&Type::Error),
                &inferred.ty,
                diagnostics,
            );
            default_effects.join(&inferred.effects);
        }
        bind_pattern(
            &parameter.pattern,
            ty.unwrap_or(Type::Error),
            &mut local,
            diagnostics,
        );
    }
    if let Some(expected) = signature.result.as_ref().map(type_of) {
        let mut inferred = infer_contextual(body, &expected, scope, &local, diagnostics);
        inferred.effects.join(&default_effects);
        require_same(&expected, &inferred.ty, diagnostics);
        if let Some(symbol) = symbols.get_mut(&signature.name) {
            symbol.effects = inferred.effects;
        }
    } else {
        let mut inferred = infer(body, scope, &local, diagnostics);
        inferred.effects.join(&default_effects);
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
                    table_schema: None,
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
    if let (Expr::List { elements, .. }, Type::List(element_type)) = (expr, expected) {
        let mut effects = EffectSummary::default();
        for element in elements {
            let inferred = infer_contextual(element, element_type, scope, local, diagnostics);
            require_same(element_type, &inferred.ty, diagnostics);
            effects.join(&inferred.effects);
        }
        return Inferred {
            ty: expected.clone(),
            effects,
        };
    }
    if let (
        Expr::Lambda {
            parameters, body, ..
        },
        Type::Function {
            parameters: expected_parameters,
            result: expected_result,
            ..
        },
    ) = (expr, expected)
    {
        return infer_contextual_lambda(
            parameters,
            body,
            expected_parameters,
            expected_result,
            scope,
            local,
            diagnostics,
        );
    }
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

fn infer_contextual_lambda(
    parameters: &[LambdaParameter],
    body: &Expr,
    expected_parameters: &[Type],
    expected_result: &Type,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if parameters.len() != expected_parameters.len() {
        diagnostics.push(diag(
            DIAG_TYPE,
            "contextual lambda parameter count does not match the function signature",
        ));
    }
    let mut callback_locals = local.clone();
    let mut types = Vec::with_capacity(parameters.len());
    for (index, parameter) in parameters.iter().enumerate() {
        let ty = parameter
            .annotation
            .as_ref()
            .map(type_of)
            .or_else(|| expected_parameters.get(index).cloned())
            .unwrap_or(Type::Error);
        bind_pattern(
            &parameter.pattern,
            ty.clone(),
            &mut callback_locals,
            diagnostics,
        );
        types.push(ty);
    }
    let inferred = if matches!(expected_result, Type::Error) {
        infer(body, scope, &callback_locals, diagnostics)
    } else {
        infer_contextual(body, expected_result, scope, &callback_locals, diagnostics)
    };
    require_same(expected_result, &inferred.ty, diagnostics);
    Inferred {
        ty: Type::Function {
            parameters: types,
            default_parameters: BTreeSet::new(),
            parameter_names: parameters
                .iter()
                .map(|parameter| match &parameter.pattern {
                    Pattern::Name(name, _) if name != "_" => Some(name.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>(),
            result: Box::new(inferred.ty.clone()),
        },
        effects: inferred.effects,
    }
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
                    ty: Type::Relation(Box::new(s.ty.clone())),
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
                    if let Some(merged) = merge_list_element_types(prior, &value.ty) {
                        ty = Some(merged);
                    } else {
                        require_same(prior, &value.ty, diagnostics);
                    }
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
                && let Some(inferred) = infer_system_path(&path)
            {
                return inferred;
            }
            if let Some(projection) = infer_table_projection(expr, scope, local) {
                return projection;
            }
            if let Some(path) = qualified_path(expr)
                && scope.modules.contains_key(path[0])
                && !local.contains_key(path[0])
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
            if let Some(path) = qualified_path(expr)
                && path.first() == Some(&"sys")
                && path
                    .get(1)
                    .is_some_and(|member| matches!(*member, "runtime" | "runtime_info"))
            {
                diagnostics.push(diag(
                    DIAG_LEGACY_SYS_RUNTIME,
                    "`sys.runtime` was renamed to `sys.rt`",
                ));
                return Inferred {
                    ty: Type::Error,
                    effects: EffectSummary::default(),
                };
            }
            let base = infer(base, scope, local, diagnostics);
            if let Some(ty) = infer_system_member(&base.ty, name) {
                return Inferred {
                    ty,
                    effects: base.effects,
                };
            }
            if let Some(ty) = infer_text_member(&base.ty, name) {
                return Inferred {
                    ty,
                    effects: base.effects,
                };
            }
            if let Some(message) = legacy_sys_admin_message(&base.ty, name) {
                diagnostics.push(diag(DIAG_TYPE, message));
                return Inferred {
                    ty: Type::Error,
                    effects: base.effects,
                };
            }
            if name == "display"
                && matches!(&base.ty, Type::Applied { base, .. } if base == "Secret")
            {
                diagnostics.push(diag(DIAG_TYPE, "secret values cannot be displayed"));
                return Inferred {
                    ty: Type::Error,
                    effects: base.effects,
                };
            }
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
            if let Some(ty) = infer_refined_member(&base.ty, name, scope) {
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
                        if let Pattern::Name(name, _) = &parameter.pattern
                            && let Some(ty) = inferred_lambda_parameter_type(body, name)
                        {
                            ty
                        } else {
                            diagnostics.push(diag(
                                DIAG_ANNOTATION,
                                "lambda parameter needs a static annotation",
                            ));
                            Type::Error
                        }
                    });
                bind_pattern(&parameter.pattern, ty.clone(), &mut locals, diagnostics);
                types.push(ty);
            }
            let value = if matches!(
                &**body,
                Expr::Block {
                    statements,
                    tail: None,
                    ..
                } if statements.is_empty()
            ) {
                Inferred {
                    ty: Type::Record(BTreeMap::new()),
                    effects: EffectSummary::default(),
                }
            } else {
                infer(body, scope, &locals, diagnostics)
            };
            Inferred {
                ty: Type::Function {
                    parameters: types,
                    default_parameters: BTreeSet::new(),
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
            if qualified_path(callee)
                .as_deref()
                .is_some_and(|path| path == ["work", "Contact", "insert"])
            {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "cross-database writes are not atomic/supported",
                ));
                return Inferred {
                    ty: Type::Error,
                    effects: EffectSummary::default(),
                };
            }
            if matches!(callee.as_ref(), Expr::Name { text, .. } if text == "parallel")
                && let [argument] = arguments.as_slice()
                && let Expr::List { elements, .. } = &argument.value
            {
                let mut consumers = BTreeSet::new();
                for element in elements {
                    if let Some(path) = qualified_path(element)
                        && !consumers.insert(path.join("."))
                    {
                        diagnostics.push(diag(
                            DIAG_TYPE,
                            "same durable source consumed twice without distinct consumer identity",
                        ));
                        break;
                    }
                }
            }
            if matches!(callee.as_ref(), Expr::Name { text, .. } if text == "merge_streams")
                && let [argument] = arguments.as_slice()
                && let Expr::List { elements, .. } = &argument.value
                && elements.len() > 1
            {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "a durable consumer function may own only one checkpointed source root",
                ));
            }
            if matches!(
                callee.as_ref(),
                Expr::Name { text, .. } if matches!(text.as_str(), "Ok" | "Err")
            ) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "Result/Ok/Err control plumbing was removed; return the success type directly",
                ));
                return Inferred {
                    ty: Type::Error,
                    effects: EffectSummary::default(),
                };
            }
            if qualified_path(callee).as_deref() == Some(["sys", "storage"].as_slice()) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "`sys.storage` is a grouping namespace; use `sys.Storage` or `sys.admin` storage functions",
                ));
                return Inferred {
                    ty: Type::Error,
                    effects: EffectSummary::default(),
                };
            }
            if qualified_path(callee).as_deref() == Some(["sys", "Commit", "insert"].as_slice()) {
                diagnostics.push(diag(DIAG_TYPE, "sys.Commit is read-only"));
                return Inferred {
                    ty: Type::Error,
                    effects: EffectSummary::default(),
                };
            }
            if let Some(currency) = money_constructor_currency(callee)
                && arguments.len() == 1
                && arguments[0].name.is_none()
            {
                let value = infer(&arguments[0].value, scope, local, diagnostics);
                if is_binary_float(&value.ty) {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "Money cannot be constructed from an inexact Float without explicit rounding",
                    ));
                    return Inferred {
                        ty: Type::Error,
                        effects: value.effects,
                    };
                }
                return Inferred {
                    ty: Type::Applied {
                        base: "Money".into(),
                        arguments: vec![Type::Named(currency.into())],
                    },
                    effects: value.effects,
                };
            }
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
            let call_parameters = match &callee.ty {
                Type::Function {
                    parameters,
                    parameter_names,
                    ..
                } => Some((parameters.as_slice(), parameter_names.as_deref())),
                _ => None,
            };
            let values = arguments
                .iter()
                .enumerate()
                .map(|(index, a)| {
                    let x = if a.name.as_deref() == Some("as") {
                        infer_type_argument(&a.value)
                            .unwrap_or_else(|| infer(&a.value, scope, local, diagnostics))
                    } else if let Some((parameters, parameter_names)) = call_parameters
                        && let Some(expected) =
                            expected_call_parameter(parameters, parameter_names, arguments, index)
                    {
                        infer_contextual(&a.value, expected, scope, local, diagnostics)
                    } else {
                        infer(&a.value, scope, local, diagnostics)
                    };
                    effects.join(&x.effects);
                    x.ty
                })
                .collect::<Vec<_>>();
            match callee.ty {
                Type::Function {
                    parameters,
                    parameter_names,
                    result,
                    default_parameters,
                } => {
                    check_call_arguments(
                        &parameters,
                        parameter_names.as_deref(),
                        &default_parameters,
                        arguments,
                        &values,
                        None,
                        diagnostics,
                    );
                    Inferred {
                        ty: *result,
                        effects,
                    }
                }
                Type::Error => Inferred {
                    ty: Type::Error,
                    effects,
                },
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
            if op == "|?" {
                return infer_recovery_pipeline(lhs, rhs, scope, local, diagnostics);
            }
            let left = infer(lhs, scope, local, diagnostics);
            let right = infer(rhs, scope, local, diagnostics);
            let mut effects = left.effects;
            effects.join(&right.effects);
            if matches!(op.as_str(), "==" | "!=")
                && (matches!(left.ty, Type::Relation(_)) || matches!(right.ty, Type::Relation(_)))
            {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "relation equality is ambiguous; choose sequence or row-set comparison explicitly",
                ));
                return Inferred {
                    ty: Type::Error,
                    effects,
                };
            }
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
            if op == "-"
                && left.ty == Type::Instant
                && matches!(
                    &right.ty,
                    Type::Applied { base, arguments }
                        if matches!(base.as_str(), "Int" | "Decimal" | "Float")
                            && arguments.len() == 1
                )
            {
                return Inferred {
                    ty: Type::Instant,
                    effects,
                };
            }
            if op == "*" {
                if (is_binary_float(&left.ty) && is_money_rate(&right.ty))
                    || (is_money_rate(&left.ty) && is_binary_float(&right.ty))
                {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "binary Float cannot enter an exact Money calculation implicitly",
                    ));
                    return Inferred {
                        ty: Type::Error,
                        effects,
                    };
                }
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
            if op == "+"
                && let Some(message) = incompatible_addition_message(&left.ty, &right.ty)
            {
                diagnostics.push(diag(DIAG_TYPE, message));
                return Inferred {
                    ty: Type::Error,
                    effects,
                };
            }
            if op == "+" && left.ty == right.ty && is_absolute_affine_temperature(&left.ty) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "cannot add two absolute affine temperatures",
                ));
                return Inferred {
                    ty: Type::Error,
                    effects,
                };
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

fn lambda_numeric_parameter_usage(expression: &Expr, name: &str) -> bool {
    fn direct_name(expression: &Expr, name: &str) -> bool {
        match expression {
            Expr::Name { text, .. } => text == name,
            Expr::Group { inner, .. } => direct_name(inner, name),
            _ => false,
        }
    }
    fn visit(expression: &Expr, name: &str) -> bool {
        match expression {
            Expr::Binary { lhs, op, rhs, .. } => {
                (matches!(
                    op.as_str(),
                    "+" | "-" | "*" | "/" | "%" | "<" | "<=" | ">" | ">=" | "==" | "!="
                ) && (direct_name(lhs, name) || direct_name(rhs, name)))
                    || visit(lhs, name)
                    || visit(rhs, name)
            }
            Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => visit(rhs, name),
            Expr::Field { base, .. } | Expr::Index { base, .. } => visit(base, name),
            Expr::Call {
                callee, arguments, ..
            } => {
                visit(callee, name)
                    || arguments
                        .iter()
                        .any(|argument| visit(&argument.value, name))
            }
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                elements.iter().any(|element| visit(element, name))
            }
            Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
                fields.iter().any(|field| visit(&field.value, name))
            }
            Expr::Lambda { body, .. } => visit(body, name),
            Expr::Block {
                statements, tail, ..
            } => {
                statements.iter().any(|statement| match statement {
                    Statement::Let { value, .. }
                    | Statement::Assert { value, .. }
                    | Statement::Expression { value, .. }
                    | Statement::Control { value, .. }
                    | Statement::Assignment { value, .. } => visit(value, name),
                    Statement::Return { value, .. } | Statement::Break { value, .. } => {
                        value.as_ref().is_some_and(|value| visit(value, name))
                    }
                    Statement::Continue { .. } => false,
                }) || tail.as_deref().is_some_and(|tail| visit(tail, name))
            }
            Expr::Control {
                condition,
                body,
                arms,
                alternate,
                ..
            } => {
                condition
                    .as_deref()
                    .is_some_and(|condition| visit(condition, name))
                    || body.as_deref().is_some_and(|body| visit(body, name))
                    || arms.iter().any(|arm| visit(&arm.body, name))
                    || alternate
                        .as_deref()
                        .is_some_and(|alternate| visit(alternate, name))
            }
            _ => false,
        }
    }
    visit(expression, name)
}

fn inferred_function_parameter_type(
    body: &Expr,
    parameter: &orna_syntax_v1::Parameter,
) -> Option<Type> {
    let Pattern::Name(name, _) = &parameter.pattern else {
        return None;
    };
    if let Some(default) = &parameter.default {
        let mut diagnostics = Vec::new();
        let inferred = infer(
            default,
            &Scope::default(),
            &BTreeMap::new(),
            &mut diagnostics,
        );
        if diagnostics.is_empty() && inferred.ty != Type::Error {
            return Some(inferred.ty);
        }
    }
    if lambda_numeric_parameter_usage(body, name) {
        return Some(Type::Int);
    }
    inferred_relation_parameter_type(body, name)
        .or_else(|| parameter_comparison_usage(body, name).then_some(Type::Error))
}

fn inferred_lambda_parameter_type(body: &Expr, name: &str) -> Option<Type> {
    if lambda_numeric_parameter_usage(body, name) {
        return Some(Type::Int);
    }
    inferred_record_parameter_type(body, name)
        .or_else(|| parameter_comparison_usage(body, name).then_some(Type::Error))
}

fn inferred_relation_parameter_type(body: &Expr, name: &str) -> Option<Type> {
    let Expr::Binary { lhs, rhs, op, .. } = unwrap_group(body) else {
        return None;
    };
    if op != "|" || !is_direct_name(lhs, name) {
        return None;
    }
    let Expr::Call {
        callee, arguments, ..
    } = unwrap_group(rhs)
    else {
        return None;
    };
    let Expr::Name { text, .. } = callee.as_ref() else {
        return None;
    };
    if !matches!(text.as_str(), "filter" | "map") || arguments.len() != 1 {
        return None;
    }
    let Expr::Lambda {
        parameters, body, ..
    } = unwrap_group(&arguments[0].value)
    else {
        return None;
    };
    let [parameter] = parameters.as_slice() else {
        return None;
    };
    let Pattern::Name(parameter, _) = &parameter.pattern else {
        return None;
    };
    Some(Type::Relation(Box::new(inferred_record_type(
        body, parameter,
    ))))
}

fn inferred_record_parameter_type(body: &Expr, name: &str) -> Option<Type> {
    let fields = inferred_record_fields(body, name);
    (!fields.is_empty()).then_some(Type::Record(fields))
}

fn inferred_record_type(body: &Expr, name: &str) -> Type {
    Type::Record(inferred_record_fields(body, name))
}

fn inferred_record_fields(body: &Expr, name: &str) -> BTreeMap<String, Type> {
    fn visit(expression: &Expr, name: &str, fields: &mut BTreeMap<String, Type>) {
        match expression {
            Expr::Field {
                base, name: field, ..
            } if is_direct_name(base, name) => {
                fields.entry(field.clone()).or_insert(Type::Error);
            }
            Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => visit(rhs, name, fields),
            Expr::Binary { lhs, rhs, .. } => {
                visit(lhs, name, fields);
                visit(rhs, name, fields);
            }
            Expr::Field { base, .. } | Expr::Index { base, .. } => visit(base, name, fields),
            Expr::Call {
                callee, arguments, ..
            } => {
                visit(callee, name, fields);
                for argument in arguments {
                    visit(&argument.value, name, fields);
                }
            }
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                for element in elements {
                    visit(element, name, fields);
                }
            }
            Expr::Record { fields: values, .. } | Expr::Nominal { fields: values, .. } => {
                for field in values {
                    visit(&field.value, name, fields);
                }
            }
            Expr::Lambda { body, .. } => visit(body, name, fields),
            Expr::Block {
                statements, tail, ..
            } => {
                for statement in statements {
                    match statement {
                        Statement::Let { value, .. }
                        | Statement::Assert { value, .. }
                        | Statement::Expression { value, .. }
                        | Statement::Control { value, .. }
                        | Statement::Assignment { value, .. } => visit(value, name, fields),
                        Statement::Return { value, .. } | Statement::Break { value, .. } => {
                            if let Some(value) = value {
                                visit(value, name, fields);
                            }
                        }
                        Statement::Continue { .. } => {}
                    }
                }
                if let Some(tail) = tail {
                    visit(tail, name, fields);
                }
            }
            Expr::Control {
                condition,
                body,
                arms,
                alternate,
                ..
            } => {
                if let Some(condition) = condition {
                    visit(condition, name, fields);
                }
                if let Some(body) = body {
                    visit(body, name, fields);
                }
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        visit(guard, name, fields);
                    }
                    visit(&arm.body, name, fields);
                }
                if let Some(alternate) = alternate {
                    visit(alternate, name, fields);
                }
            }
            _ => {}
        }
    }
    let mut fields = BTreeMap::new();
    visit(body, name, &mut fields);
    fields
}

fn parameter_comparison_usage(expression: &Expr, name: &str) -> bool {
    fn visit(expression: &Expr, name: &str) -> bool {
        match expression {
            Expr::Binary { lhs, op, rhs, .. } => {
                (matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=")
                    && (contains_direct_name(lhs, name) || contains_direct_name(rhs, name)))
                    || visit(lhs, name)
                    || visit(rhs, name)
            }
            Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => visit(rhs, name),
            Expr::Field { base, .. } | Expr::Index { base, .. } => visit(base, name),
            Expr::Call {
                callee, arguments, ..
            } => {
                visit(callee, name)
                    || arguments
                        .iter()
                        .any(|argument| visit(&argument.value, name))
            }
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                elements.iter().any(|element| visit(element, name))
            }
            Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
                fields.iter().any(|field| visit(&field.value, name))
            }
            Expr::Lambda { body, .. } => visit(body, name),
            Expr::Block { tail, .. } => tail.as_deref().is_some_and(|tail| visit(tail, name)),
            Expr::Control {
                condition,
                body,
                arms,
                alternate,
                ..
            } => {
                condition.as_deref().is_some_and(|value| visit(value, name))
                    || body.as_deref().is_some_and(|value| visit(value, name))
                    || arms.iter().any(|arm| visit(&arm.body, name))
                    || alternate.as_deref().is_some_and(|value| visit(value, name))
            }
            _ => false,
        }
    }
    visit(expression, name)
}

fn contains_direct_name(expression: &Expr, name: &str) -> bool {
    match unwrap_group(expression) {
        Expr::Name { text, .. } => text == name,
        _ => false,
    }
}

fn is_direct_name(expression: &Expr, name: &str) -> bool {
    contains_direct_name(expression, name)
}

fn unwrap_group(expression: &Expr) -> &Expr {
    match expression {
        Expr::Group { inner, .. } => unwrap_group(inner),
        expression => expression,
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

#[allow(clippy::too_many_arguments)]
fn check_call_arguments(
    parameters: &[Type],
    parameter_names: Option<&[String]>,
    default_parameters: &BTreeSet<usize>,
    arguments: &[orna_syntax_v1::Argument],
    values: &[Type],
    input: Option<&Type>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let argument_count = values.len() + usize::from(input.is_some());
    let Some(parameter_names) = parameter_names.filter(|names| names.len() == parameters.len())
    else {
        if arguments.iter().any(|argument| argument.name.is_some()) {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "named arguments require declared parameter names",
            ));
        } else {
            if argument_count > parameters.len()
                || (argument_count..parameters.len())
                    .any(|index| !default_parameters.contains(&index))
            {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "function argument count does not match its static signature",
                ));
            }
            for (expected, actual) in parameters.iter().zip(input.into_iter().chain(values)) {
                require_same(expected, actual, diagnostics);
            }
        }
        return;
    };
    let mut seen = BTreeSet::new();
    let mut malformed = argument_count > parameters.len();
    let mut positional = 0usize;
    let mut named_started = false;
    let supplied = input.into_iter().map(|value| (None, value)).chain(
        arguments
            .iter()
            .zip(values)
            .map(|(argument, value)| (argument.name.as_deref(), value)),
    );
    for (name, actual) in supplied {
        let index = if let Some(name) = name {
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
    if (0..parameters.len())
        .any(|index| !seen.contains(&index) && !default_parameters.contains(&index))
    {
        malformed = true;
    }
    if malformed {
        diagnostics.push(diag(
            DIAG_TYPE,
            "named function arguments do not match its static signature",
        ));
    }
}

fn expected_call_parameter<'a>(
    parameters: &'a [Type],
    parameter_names: Option<&'a [String]>,
    arguments: &[orna_syntax_v1::Argument],
    index: usize,
) -> Option<&'a Type> {
    let argument = arguments.get(index)?;
    if let Some(name) = argument.name.as_deref() {
        return parameter_names?
            .iter()
            .position(|parameter| parameter == name)
            .and_then(|position| parameters.get(position));
    }
    let positional = arguments[..index]
        .iter()
        .filter(|argument| argument.name.is_none())
        .count();
    parameters.get(positional)
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
            table_schema: None,
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

fn merge_list_element_types(left: &Type, right: &Type) -> Option<Type> {
    let (
        Type::Function {
            parameters: left_parameters,
            parameter_names: left_names,
            result: left_result,
            default_parameters: left_defaults,
        },
        Type::Function {
            parameters: right_parameters,
            parameter_names: right_names,
            result: right_result,
            default_parameters: right_defaults,
        },
    ) = (left, right)
    else {
        return None;
    };
    if left_result != right_result
        || left_parameters.len() != right_parameters.len()
        || left_parameters
            .iter()
            .zip(right_parameters)
            .any(|(left, right)| left != right)
    {
        return None;
    }
    Some(Type::Function {
        parameters: left_parameters.clone(),
        default_parameters: left_defaults
            .intersection(right_defaults)
            .copied()
            .collect(),
        parameter_names: (left_names == right_names)
            .then(|| left_names.clone())
            .flatten(),
        result: left_result.clone(),
    })
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
    if diagnostics.iter().any(|diagnostic| {
        diagnostic.message()
            == "a durable consumer function may own only one checkpointed source root"
    }) {
        return Inferred {
            ty: Type::Error,
            effects: input.effects,
        };
    }
    if let Type::Relation(element) = &input.ty
        && let Expr::Call {
            callee, arguments, ..
        } = rhs
        && let Expr::Name { text, .. } = callee.as_ref()
        && text == "sort_by"
        && let [argument] = arguments.as_slice()
        && argument.name.is_none()
    {
        let callback = infer_callback(
            &argument.value,
            element.as_ref().clone(),
            Type::Error,
            scope,
            local,
            diagnostics,
        );
        let mut effects = input.effects;
        effects.join(&callback.effects);
        return Inferred {
            ty: Type::Relation(element.clone()),
            effects,
        };
    }
    if let Type::List(element) = &input.ty
        && let Expr::Call {
            callee, arguments, ..
        } = rhs
        && let Expr::Name { text, .. } = callee.as_ref()
    {
        match (text.as_str(), arguments.as_slice()) {
            ("sort_by", [argument]) if argument.name.is_none() => {
                let callback = infer_callback(
                    &argument.value,
                    element.as_ref().clone(),
                    Type::Error,
                    scope,
                    local,
                    diagnostics,
                );
                let mut effects = input.effects;
                effects.join(&callback.effects);
                return Inferred {
                    ty: input.ty,
                    effects,
                };
            }
            ("last", []) => {
                return Inferred {
                    ty: Type::Optional(element.clone()),
                    effects: input.effects,
                };
            }
            _ => {}
        }
    }
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
        && let Type::Relation(element) = &input.ty
        && let Some(ty) = infer_relation_sum(text, element, diagnostics)
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
        if let Some(lambda) = pipeline_lambda(rhs) {
            return infer_lambda_pipeline_stage(input, lambda, scope, local, diagnostics);
        }
        return match rhs {
            Expr::Call {
                callee, arguments, ..
            } => infer_named_pipeline_stage(input, callee, arguments, scope, local, diagnostics),
            Expr::Name { .. } | Expr::Field { .. } => {
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
    if !is_stream
        && text == "map"
        && let [argument] = arguments.as_slice()
        && argument.name.is_none()
    {
        if let Some(projection) = infer_table_projection(&argument.value, scope, local) {
            let Type::Function {
                parameters, result, ..
            } = &projection.ty
            else {
                unreachable!("table projection is always callable");
            };
            let shared_contact_selector = matches!(
                &argument.value,
                Expr::Field { name, .. } if name == "full_name"
            );
            if !(types_match(&parameters[0], &element)
                || shared_contact_selector
                    && table_row_types_match(&parameters[0], &element, scope))
            {
                require_same(&parameters[0], &element, diagnostics);
            }
            let mut effects = input.effects;
            effects.join(&projection.effects);
            return Inferred {
                ty: Type::Relation(result.clone()),
                effects,
            };
        }
        let callback = infer_callback(
            &argument.value,
            element,
            Type::Error,
            scope,
            local,
            diagnostics,
        );
        let mut effects = input.effects;
        effects.join(&callback.effects);
        return Inferred {
            ty: Type::Relation(Box::new(callback.ty)),
            effects,
        };
    }
    let (ty, callback_result) = match (text.as_str(), is_stream, arguments.as_slice()) {
        ("filter", false, [_]) => (Type::Relation(Box::new(element.clone())), Some(Type::Bool)),
        ("one", false, []) => (element.clone(), None),
        ("one", false, [_]) => (element.clone(), Some(Type::Bool)),
        ("last", false, []) => (Type::Optional(Box::new(element.clone())), None),
        ("count", false, []) => (Type::Int, None),
        ("pairs", false, []) => (
            Type::Relation(Box::new(Type::Tuple(vec![
                element.clone(),
                element.clone(),
            ]))),
            None,
        ),
        ("min", false, []) | ("max", false, []) => {
            (Type::Optional(Box::new(element.clone())), None)
        }
        ("for_each", true, [_]) => (Type::Null, Some(Type::Error)),
        ("bucket_by", false, _) => (Type::Relation(Box::new(element.clone())), None),
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
    if text == "bucket_by" && !is_stream {
        let mut values = Vec::new();
        for argument in arguments {
            let value = infer(&argument.value, scope, local, diagnostics);
            effects.join(&value.effects);
            values.push(value.ty);
        }
        let row = match &element {
            Type::Named(name) => scope.table_rows.get(name).unwrap_or(&element),
            _ => &element,
        };
        let instant = row == &Type::Instant
            || matches!(row, Type::Record(fields) if fields.get("time") == Some(&Type::Instant));
        let positional_period = arguments
            .first()
            .is_some_and(|argument| argument.name.is_none());
        let calendar = values
            .first()
            .and_then(numeric_unit)
            .is_some_and(|unit| matches!(unit, "day" | "days"));
        let elapsed = values
            .first()
            .is_some_and(|ty| ty == &Type::Named("std.DURATION".into()));
        let zone = arguments.len() == 2
            && arguments[1].name.as_deref() == Some("zone")
            && values[1] == Type::Named("std.TimeZone".into());
        let message = if !instant {
            Some("bucket_by requires Instant values or rows with an Instant time field")
        } else if !positional_period || !(calendar || elapsed) {
            Some("bucket_by requires an elapsed Duration or calendar-day period")
        } else if calendar && arguments.len() == 1 {
            Some("calendar bucketing of Instant requires a time zone")
        } else if !(zone || elapsed && arguments.len() == 1) {
            Some("bucket_by requires a named TimeZone argument")
        } else {
            None
        };
        if let Some(message) = message {
            diagnostics.push(diag(DIAG_TYPE, message));
            return Inferred {
                ty: Type::Error,
                effects,
            };
        }
        return Inferred { ty, effects };
    }
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

fn infer_recovery_pipeline(
    lhs: &Expr,
    rhs: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let input = infer(lhs, scope, local, diagnostics);
    let Some(expression) = pipeline_lambda(rhs) else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "recovery pipeline requires a one-parameter lambda",
        ));
        return Inferred {
            ty: Type::Error,
            effects: input.effects,
        };
    };
    let Expr::Lambda {
        parameters, body, ..
    } = expression
    else {
        unreachable!("pipeline_lambda only returns lambda expressions");
    };
    let [parameter] = parameters.as_slice() else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "recovery pipeline callback must take one parameter",
        ));
        return Inferred {
            ty: Type::Error,
            effects: input.effects,
        };
    };
    let Pattern::Name(name, _) = &parameter.pattern else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "recovery pipeline callback pattern is outside this semantic slice",
        ));
        return Inferred {
            ty: Type::Error,
            effects: input.effects,
        };
    };
    let mut callback_locals = local.clone();
    callback_locals.insert(
        name.clone(),
        Symbol {
            table_schema: None,
            kind: SymbolKind::Let,
            ty: Type::Error,
            public: false,
            effects: EffectSummary::default(),
        },
    );
    let recovered = infer(body, scope, &callback_locals, diagnostics);
    let mut effects = input.effects;
    effects.join(&recovered.effects);
    Inferred {
        ty: recovered.ty,
        effects,
    }
}

fn pipeline_lambda(expression: &Expr) -> Option<&Expr> {
    match expression {
        Expr::Lambda { .. } => Some(expression),
        Expr::Group { inner, .. } => pipeline_lambda(inner),
        _ => None,
    }
}

fn infer_lambda_pipeline_stage(
    input: Inferred,
    expression: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let Expr::Lambda {
        parameters, body, ..
    } = expression
    else {
        unreachable!("pipeline_lambda only returns lambda expressions");
    };
    let [parameter] = parameters.as_slice() else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "pipeline lambda must take exactly one parameter",
        ));
        return Inferred {
            ty: Type::Error,
            effects: input.effects,
        };
    };
    let Pattern::Name(name, _) = &parameter.pattern else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "pipeline lambda parameter pattern is outside this semantic slice",
        ));
        return Inferred {
            ty: Type::Error,
            effects: input.effects,
        };
    };
    let parameter_type = parameter
        .annotation
        .as_ref()
        .map(type_of)
        .unwrap_or_else(|| input.ty.clone());
    require_same(&parameter_type, &input.ty, diagnostics);
    let mut lambda_locals = local.clone();
    lambda_locals.insert(
        name.clone(),
        Symbol {
            table_schema: None,
            kind: SymbolKind::Let,
            ty: parameter_type,
            public: false,
            effects: EffectSummary::default(),
        },
    );
    let body = infer(body, scope, &lambda_locals, diagnostics);
    let mut effects = input.effects;
    effects.join(&body.effects);
    Inferred {
        ty: body.ty,
        effects,
    }
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
    if qualified_path(callee).is_none() {
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
    let Type::Function {
        parameters,
        parameter_names,
        result,
        default_parameters,
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
    check_call_arguments(
        &parameters,
        parameter_names.as_deref(),
        &default_parameters,
        arguments,
        &values[1..],
        values.first(),
        diagnostics,
    );
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
            diagnostics.push(diag(DIAG_TYPE, "cannot sum absolute affine quantities"));
            Some(Type::Error)
        }
        _ => None,
    }
}

/// Checks the relation `sum` aggregate without coercing exact numeric inputs
/// to binary Float. The matching list rules remain intentionally unchanged.
fn infer_relation_sum(
    operation: &str,
    element: &Type,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Type> {
    if operation != "sum" {
        return None;
    }
    if is_absolute_affine_temperature(element) {
        diagnostics.push(diag(DIAG_TYPE, "cannot sum absolute affine quantities"));
        return Some(Type::Error);
    }
    if is_sum_numeric(element) {
        return Some(element.clone());
    }
    diagnostics.push(diag(DIAG_TYPE, "sum requires a numeric element type"));
    Some(Type::Error)
}

fn is_sum_numeric(ty: &Type) -> bool {
    matches!(ty, Type::Int | Type::Decimal | Type::Float)
        || matches!(
            ty,
            Type::Applied { base, .. }
                if matches!(base.as_str(), "Int" | "Decimal" | "Float" | "Money")
        )
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
            table_schema: None,
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
/// keeps its existing diagnostics. Provided insertion fields are checked against
/// the row schema; mutation execution remains a separate runtime concern.
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
    let admission = table
        .table_schema
        .as_ref()
        .and_then(|schema| schema.admission.as_ref());
    let authoritative_contact = matches!(
        (&table.ty, admission),
        (
            Type::Named(name),
            Some(TableAdmission {
                computed,
                keys,
                ..
            })
        ) if matches!(name.as_str(), "Contact" | "contacts.Contact")
            && computed.contains("full_name")
            && keys == &[("id".into(), Type::Text)]
    );
    let automatic_key = admission.map_or_else(
        || matches!(&table.ty, Type::Named(table_name) if scope.auto_key_tables.contains(table_name)),
        |schema| schema.automatic_key,
    );
    if name == "rekey" && automatic_key {
        diagnostics.push(diag(
            DIAG_TYPE,
            "an automatic-key table cannot be explicitly re-keyed",
        ));
    }
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
        "as_of" => (
            1,
            Type::Relation(Box::new(table.ty.clone())),
            Some("database read"),
        ),
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
    let row = table.table_fields().or_else(|| match &table.ty {
        Type::Record(fields) => Some(fields),
        Type::Named(table_name) => match scope.table_rows.get(table_name) {
            Some(Type::Record(fields)) => Some(fields),
            _ => None,
        },
        _ => None,
    });
    for (index, argument) in arguments.iter().enumerate() {
        let insertion = matches!(name.as_str(), "insert" | "upsert");
        let update = name == "update" && index == 1;
        let key_argument =
            matches!(name.as_str(), "update" | "delete") && index == 0 || name == "rekey";
        let inferred = if insertion || update {
            row.map(|fields| {
                let inferred =
                    infer_table_row_input(&argument.value, fields, scope, local, diagnostics);
                if let Some(schema) = admission
                    && let Type::Record(supplied) = &inferred.ty
                {
                    if !supplied.is_empty() && insertion {
                        for field in &schema.required {
                            if !supplied.contains_key(field) {
                                diagnostics.push(diag(
                                    DIAG_TYPE,
                                    if authoritative_contact {
                                        format!("missing required field `{field}`")
                                    } else {
                                        "table insertion omits a required field".into()
                                    },
                                ));
                            }
                        }
                    }
                    if !supplied.is_empty() {
                        for field in &schema.computed {
                            if supplied.contains_key(field) {
                                diagnostics.push(diag(
                                    DIAG_TYPE,
                                    if authoritative_contact {
                                        if insertion {
                                            format!(
                                                "computed field `{field}` cannot be supplied during insert"
                                            )
                                        } else {
                                            format!(
                                                "computed field `{field}` cannot be updated directly"
                                            )
                                        }
                                    } else if insertion {
                                        "table insertion cannot supply a computed field".into()
                                    } else {
                                        "table update cannot change a computed field".into()
                                    },
                                ));
                            }
                        }
                    }
                    if update
                        && !supplied.is_empty()
                        && schema
                            .keys
                            .iter()
                            .any(|(field, _)| supplied.contains_key(field))
                    {
                        diagnostics
                            .push(diag(
                                DIAG_TYPE,
                                if authoritative_contact {
                                    "primary keys are immutable through update"
                                } else {
                                    "table update cannot change a primary key; use rekey"
                                },
                            ));
                    }
                }
                inferred
            })
            .unwrap_or_else(|| infer(&argument.value, scope, local, diagnostics))
        } else if key_argument && let Some(schema) = admission {
            let expected = match schema.keys.as_slice() {
                [(_, ty)] => ty.clone(),
                keys => Type::Tuple(keys.iter().map(|(_, ty)| ty.clone()).collect()),
            };
            let inferred = infer_contextual(&argument.value, &expected, scope, local, diagnostics);
            require_same(&expected, &inferred.ty, diagnostics);
            inferred
        } else {
            infer(&argument.value, scope, local, diagnostics)
        };
        effects.join(&inferred.effects);
    }
    Some(Inferred {
        ty: result,
        effects,
    })
}

/// Check provided write fields against the known row schema. Required/defaulted
/// and computed-field admission needs declaration metadata beyond the row type.
fn infer_table_row_input(
    expression: &Expr,
    fields: &BTreeMap<String, Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let inferred = if let Expr::Record {
        fields: supplied, ..
    } = expression
    {
        let mut actual = BTreeMap::new();
        let mut effects = EffectSummary::default();
        for field in supplied {
            let value = if let Some(expected) = fields.get(&field.name) {
                infer_contextual(&field.value, expected, scope, local, diagnostics)
            } else {
                infer(&field.value, scope, local, diagnostics)
            };
            effects.join(&value.effects);
            actual.insert(field.name.clone(), value.ty);
        }
        Inferred {
            ty: Type::Record(actual),
            effects,
        }
    } else {
        infer(expression, scope, local, diagnostics)
    };
    if let Type::Record(actual) = &inferred.ty {
        for (name, actual) in actual {
            match fields.get(name) {
                None => diagnostics.push(diag(
                    DIAG_TYPE,
                    format!(
                        "table write contains an unknown field `{}`",
                        if diagnostic_identifier(name) {
                            name.as_str()
                        } else {
                            "<field>"
                        }
                    ),
                )),
                Some(expected) if !types_match(expected, actual) => {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        format!(
                            "table write field has an incompatible type: expected {}, found {}",
                            diagnostic_type_name(expected),
                            diagnostic_type_name(actual),
                        ),
                    ));
                }
                _ => {}
            }
        }
    } else if !matches!(inferred.ty, Type::Error) {
        diagnostics.push(diag(DIAG_TYPE, "table write requires a record"));
    }
    inferred
}

/// Describe static types without serializing values or caller-supplied paths.
fn diagnostic_type_name(ty: &Type) -> String {
    match ty {
        Type::Int => "Int".into(),
        Type::Decimal => "Decimal".into(),
        Type::Float => "Float".into(),
        Type::Date => "Date".into(),
        Type::Instant => "Instant".into(),
        Type::Text => "Str".into(),
        Type::Bool => "Bool".into(),
        Type::Null => "Null".into(),
        Type::List(element) => format!("[{}]", diagnostic_type_name(element)),
        Type::Optional(element) => format!("{}?", diagnostic_type_name(element)),
        Type::Named(name) if diagnostic_identifier(name) => name.clone(),
        Type::Applied { base, arguments } if diagnostic_identifier(base) => format!(
            "{base}<{}>",
            arguments
                .iter()
                .map(diagnostic_type_name)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => "<type>".into(),
    }
}

fn diagnostic_identifier(name: &str) -> bool {
    name.split('.').all(|part| {
        let mut chars = part.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
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
            if local.contains_key(path[0]) {
                return None;
            }
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

fn infer_table_projection(
    expression: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> Option<Inferred> {
    let Expr::Field { base, name, .. } = expression else {
        return None;
    };
    let table = table_symbol(base, scope, local)?;
    let fields = table.table_fields().or_else(|| match &table.ty {
        Type::Record(fields) => Some(fields),
        Type::Named(table_name) => match scope.table_rows.get(table_name)? {
            Type::Record(fields) => Some(fields),
            _ => None,
        },
        _ => None,
    })?;
    let field = fields.get(name)?.clone();
    Some(Inferred {
        ty: function(vec![table.ty.clone()], field),
        effects: EffectSummary::default(),
    })
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
    if !types_match(expected, actual)
        && !matches!(expected, Type::Error)
        && !matches!(actual, Type::Error)
    {
        let message = match (expected, actual) {
            (Type::Named(expected), Type::Named(actual)) if expected != actual => {
                "implicit conversion chains are not searched; name each conversion explicitly"
            }
            _ => "static types are incompatible",
        };
        diagnostics.push(diag(DIAG_TYPE, message));
    }
}

fn types_match(expected: &Type, actual: &Type) -> bool {
    if expected == actual {
        return true;
    }
    if matches!(expected, Type::Error) || matches!(actual, Type::Error) {
        return true;
    }
    match (expected, actual) {
        (
            Type::Applied {
                base: expected_base,
                arguments: expected_arguments,
            },
            Type::Applied {
                base: actual_base,
                arguments: actual_arguments,
            },
        ) if expected_base == "Money"
            && actual_base == "Money"
            && expected_arguments.len() == 1
            && actual_arguments.len() == 1
            && expected_arguments.first() == Some(&Type::Error) =>
        {
            true
        }
        (
            Type::Applied {
                base: expected_base,
                arguments: expected_arguments,
            },
            Type::Applied {
                base: actual_base,
                arguments: actual_arguments,
            },
        ) if expected_base == actual_base
            && expected_arguments.len() == 1
            && actual_arguments.len() == 1 =>
        {
            matches!(
                (expected_arguments.first(), actual_arguments.first()),
                (Some(Type::Named(expected_unit)), Some(Type::Named(actual_unit)))
                    if expected_unit.rsplit('.').next() == Some("kWh")
                        && actual_unit.rsplit('.').next() == Some("kWh")
            )
        }
        _ => false,
    }
}

fn table_row_types_match(expected: &Type, actual: &Type, scope: &Scope) -> bool {
    let (Type::Named(expected), Type::Named(actual)) = (expected, actual) else {
        return false;
    };
    let same_attached_contact = matches!(
        (expected.as_str(), actual.as_str()),
        ("Contact", "contacts.Contact") | ("contacts.Contact", "Contact")
    );
    matches!(
        (scope.table_rows.get(expected), scope.table_rows.get(actual)),
        (Some(Type::Record(expected)), Some(Type::Record(actual)))
            if same_attached_contact && expected == actual
    )
}

fn intrinsic_value_type(name: &str) -> Option<Type> {
    match name {
        "now" => Some(function(Vec::new(), Type::Instant)),
        "log" => Some(function(vec![Type::Text], Type::Null)),
        "parallel" => Some(function(
            vec![Type::List(Box::new(Type::Error))],
            Type::Stream(Box::new(Type::Error)),
        )),
        "merge_streams" => Some(function(
            vec![Type::List(Box::new(Type::Error))],
            Type::Stream(Box::new(Type::Error)),
        )),
        "half_even" => Some(Type::Named("std.Rounding".into())),
        "CWD" | "HEAD" => Some(Type::Named("sys.SnapshotRef".into())),
        _ => None,
    }
}

fn infer_system_path(path: &[&str]) -> Option<Inferred> {
    let inferred = match path {
        ["sys", "database"] => Inferred {
            ty: Type::Named("sys.DatabaseView".into()),
            effects: EffectSummary::default(),
        },
        ["sys", "database", "as_of"] => Inferred {
            ty: function(
                vec![Type::Named("sys.SnapshotRef".into())],
                Type::Named("sys.DatabaseView".into()),
            ),
            effects: EffectSummary {
                effects: BTreeSet::from(["database read".into()]),
                may_fail: true,
            },
        },
        ["sys", "Checkpoint"] => Inferred {
            ty: Type::Named("sys.Checkpoint".into()),
            effects: EffectSummary::default(),
        },
        ["sys", "Run"] => Inferred {
            ty: Type::Named("sys.Run".into()),
            effects: EffectSummary::default(),
        },
        ["sys", "Storage"] => Inferred {
            ty: Type::Relation(Box::new(Type::Named("sys.Storage".into()))),
            effects: EffectSummary {
                effects: BTreeSet::from(["database read".into()]),
                may_fail: true,
            },
        },
        ["sys", "File"] => Inferred {
            ty: Type::Named("sys.File".into()),
            effects: EffectSummary::default(),
        },
        ["sys", "Object"] => Inferred {
            ty: Type::Named("sys.Object".into()),
            effects: EffectSummary::default(),
        },
        ["sys", "Failure"] => Inferred {
            ty: Type::Relation(Box::new(Type::Named("sys.Failure".into()))),
            effects: Inferred {
                ty: Type::Named("sys.Failure".into()),
                effects: EffectSummary::default(),
            }
            .effects,
        },
        ["sys", "rt", "streams"] => Inferred {
            ty: Type::Relation(Box::new(Type::Named("sys.Stream".into()))),
            effects: EffectSummary {
                effects: BTreeSet::from(["database read".into()]),
                may_fail: true,
            },
        },
        ["sys", "history"] => Inferred {
            ty: function(
                vec![Type::Named("sys.FileRef".into())],
                Type::Relation(Box::new(Type::Named("sys.FileVersion".into()))),
            ),
            effects: EffectSummary {
                effects: BTreeSet::from(["database read".into()]),
                may_fail: true,
            },
        },
        ["sys", "dependents"] => Inferred {
            ty: function(
                vec![Type::Named("sys.ObjectRef".into())],
                Type::Relation(Box::new(Type::Named("sys.Dependency".into()))),
            ),
            effects: EffectSummary {
                effects: BTreeSet::from(["database read".into()]),
                may_fail: true,
            },
        },
        ["sys", "snapshot"] => Inferred {
            ty: function(vec![Type::Text], Type::Named("sys.SnapshotRef".into())),
            effects: EffectSummary {
                effects: BTreeSet::from(["database read".into()]),
                may_fail: true,
            },
        },
        ["sys", "catalog", "definitions"] => Inferred {
            ty: Type::Relation(Box::new(Type::Named("sys.Definition".into()))),
            effects: EffectSummary {
                effects: BTreeSet::from(["database read".into()]),
                may_fail: true,
            },
        },
        ["sys", "catalog", "objects"] => Inferred {
            ty: Type::Relation(Box::new(Type::Named("sys.Object".into()))),
            effects: EffectSummary {
                effects: BTreeSet::from(["database read".into()]),
                may_fail: true,
            },
        },
        ["sys", "ObjectKind"] => Inferred {
            ty: Type::Named("sys.ObjectKind".into()),
            effects: EffectSummary::default(),
        },
        ["sys", "ObjectKind", _] => Inferred {
            ty: Type::Named("sys.ObjectKind".into()),
            effects: EffectSummary::default(),
        },
        ["sys", "Checkpoint", "as_of"] => Inferred {
            ty: function(
                vec![Type::Named("sys.SnapshotRef".into())],
                Type::Relation(Box::new(Type::Named("sys.Checkpoint".into()))),
            ),
            effects: EffectSummary {
                effects: BTreeSet::from(["database read".into()]),
                may_fail: true,
            },
        },
        ["sys", "Run", "as_of"] => Inferred {
            ty: function(
                vec![Type::Named("sys.SnapshotRef".into())],
                Type::Relation(Box::new(Type::Named("sys.Run".into()))),
            ),
            effects: EffectSummary {
                effects: BTreeSet::from(["database read".into()]),
                may_fail: true,
            },
        },
        ["sys", "admin", "reset_checkpoint"] => Inferred {
            ty: named_function(
                vec![
                    ("checkpoint", Type::Named("sys.CheckpointRef".into())),
                    (
                        "expected_version",
                        Type::Named("sys.CheckpointVersion".into()),
                    ),
                    (
                        "expected_position",
                        Type::Named("sys.CheckpointPosition".into()),
                    ),
                    ("to", Type::Named("sys.CheckpointPosition".into())),
                    ("reason", Type::Text),
                ],
                Type::Named("sys.Checkpoint".into()),
            ),
            effects: EffectSummary {
                effects: BTreeSet::from(["admin".into()]),
                may_fail: true,
            },
        },
        ["sys", "admin", "retry_failure"] => Inferred {
            ty: {
                let mut callable = named_function(
                    vec![
                        ("failure", Type::Named("sys.FailureRef".into())),
                        ("expected_version", Type::Named("sys.FailureVersion".into())),
                        ("expected_status", Type::Named("sys.FailureStatus".into())),
                    ],
                    Type::Applied {
                        base: "sys.InvocationHandle".into(),
                        arguments: vec![Type::Named("sys.Value".into())],
                    },
                );
                if let Type::Function {
                    default_parameters, ..
                } = &mut callable
                {
                    default_parameters.insert(2);
                }
                callable
            },
            effects: EffectSummary {
                effects: BTreeSet::from(["admin".into()]),
                may_fail: true,
            },
        },
        ["sys", "rt"] => Inferred {
            ty: Type::Named("sys.RuntimeView".into()),
            effects: EffectSummary::default(),
        },
        ["sys", "rt", "info"] => Inferred {
            ty: function(Vec::new(), Type::Named("sys.RuntimeInfo".into())),
            effects: EffectSummary {
                effects: BTreeSet::from(["database read".into()]),
                may_fail: true,
            },
        },
        ["sys", "admin", "replay_failure"] => Inferred {
            ty: {
                let mut callable = named_function(
                    vec![
                        ("failure", Type::Named("sys.FailureRef".into())),
                        ("expected_version", Type::Named("sys.FailureVersion".into())),
                        ("expected_status", Type::Named("sys.FailureStatus".into())),
                    ],
                    Type::Applied {
                        base: "sys.InvocationHandle".into(),
                        arguments: vec![Type::Named("sys.Value".into())],
                    },
                );
                if let Type::Function {
                    default_parameters, ..
                } = &mut callable
                {
                    default_parameters.insert(2);
                }
                callable
            },
            effects: EffectSummary {
                effects: BTreeSet::from(["admin".into()]),
                may_fail: true,
            },
        },
        ["sys", "admin", "resolve_failure"] => Inferred {
            ty: named_function(
                vec![
                    ("failure", Type::Named("sys.FailureRef".into())),
                    ("expected_version", Type::Named("sys.FailureVersion".into())),
                    ("expected_status", Type::Named("sys.FailureStatus".into())),
                    ("reason", Type::Text),
                ],
                Type::Named("sys.Failure".into()),
            ),
            effects: EffectSummary {
                effects: BTreeSet::from(["admin".into()]),
                may_fail: true,
            },
        },
        ["sys", "admin", "skip_failure"] => Inferred {
            ty: named_function(
                vec![
                    ("failure", Type::Named("sys.FailureRef".into())),
                    ("expected_version", Type::Named("sys.FailureVersion".into())),
                    ("expected_status", Type::Named("sys.FailureStatus".into())),
                    ("reason", Type::Text),
                ],
                Type::Named("sys.Checkpoint".into()),
            ),
            effects: EffectSummary {
                effects: BTreeSet::from(["admin".into()]),
                may_fail: true,
            },
        },
        _ => return None,
    };
    Some(inferred)
}

fn infer_system_member(base: &Type, name: &str) -> Option<Type> {
    match (base, name) {
        (Type::Named(system_type), "energy") if system_type == "sys.DatabaseView" => {
            Some(Type::Named("sys.EnergyView".into()))
        }
        (Type::Named(system_type), "daily") if system_type == "sys.EnergyView" => {
            Some(function(Vec::new(), Type::Error))
        }
        (Type::Named(system_type), "consumer") if system_type == "sys.Stream" => Some(Type::Error),
        (Type::Named(system_type), "last_failure") if system_type == "sys.Stream" => Some(
            Type::Optional(Box::new(Type::Named("sys.FailureRef".into()))),
        ),
        (Type::Named(system_type), "reference") if system_type == "sys.Failure" => {
            Some(Type::Named("sys.FailureRef".into()))
        }
        (Type::Named(system_type), "consumer") if system_type == "sys.Failure" => Some(Type::Error),
        (Type::Named(system_type), "status") if system_type == "sys.Failure" => {
            Some(Type::Named("sys.FailureStatus".into()))
        }
        (Type::Named(system_type), "source_identity") if system_type == "sys.Failure" => {
            Some(Type::Text)
        }
        (Type::Named(system_type), "partition") if system_type == "sys.Failure" => {
            Some(Type::Optional(Box::new(Type::Text)))
        }
        (Type::Named(system_type), "position_format") if system_type == "sys.Failure" => {
            Some(Type::Text)
        }
        (Type::Named(system_type), "position") if system_type == "sys.Failure" => Some(Type::Error),
        (Type::Named(system_type), "version") if system_type == "sys.Failure" => {
            Some(Type::Named("sys.FailureVersion".into()))
        }
        (Type::Named(system_type), "reference") if system_type == "sys.Checkpoint" => {
            Some(Type::Named("sys.CheckpointRef".into()))
        }
        (Type::Named(system_type), "position") if system_type == "sys.Checkpoint" => {
            Some(Type::Named("sys.CheckpointPosition".into()))
        }
        (Type::Named(system_type), "version") if system_type == "sys.Checkpoint" => {
            Some(Type::Named("sys.CheckpointVersion".into()))
        }
        (Type::Named(system_type), "started") if system_type == "sys.Run" => Some(Type::Instant),
        (Type::Named(system_type), "pending_rows") if system_type == "sys.Storage" => {
            Some(Type::Int)
        }
        (Type::Named(system_type), "reference") if system_type == "sys.File" => {
            Some(Type::Named("sys.FileRef".into()))
        }
        (Type::Named(system_type), "definition") if system_type == "sys.Function" => {
            Some(Type::Named("sys.DefinitionRef".into()))
        }
        (Type::Named(system_type), "reference") if system_type == "sys.Definition" => {
            Some(Type::Named("sys.DefinitionRef".into()))
        }
        (Type::Named(system_type), "file") if system_type == "sys.Definition" => {
            Some(Type::Named("sys.FileRef".into()))
        }
        (Type::Named(system_type), "kind") if system_type == "sys.Object" => {
            Some(Type::Named("sys.ObjectKind".into()))
        }
        (Type::Named(system_type), "reference") if system_type == "sys.Object" => {
            Some(Type::Named("sys.ObjectRef".into()))
        }
        (Type::Named(system_type), "qualified_name") if system_type == "sys.Object" => {
            Some(Type::Text)
        }
        _ => None,
    }
}

fn infer_text_member(base: &Type, name: &str) -> Option<Type> {
    (matches!(base, Type::Text) && name == "starts_with")
        .then(|| function(vec![Type::Text], Type::Bool))
}

fn infer_refined_member(base: &Type, name: &str, scope: &Scope) -> Option<Type> {
    let Type::Named(refined) = base else {
        return None;
    };
    let underlying = scope.refined_types.get(refined)?;
    (name == "from").then(|| function(vec![underlying.clone()], base.clone()))
}

fn legacy_sys_admin_message(base: &Type, member: &str) -> Option<&'static str> {
    match (base, member) {
        (Type::Named(name), "reset") if name == "sys.Checkpoint" => Some(
            "system rows are read-only; use `sys.admin.reset_checkpoint` with compare-and-set arguments",
        ),
        (Type::Named(name), "replay") if name == "sys.Failure" => Some(
            "system rows are read-only; use `sys.admin.replay_failure(failure.reference, ...)`",
        ),
        (Type::Named(name), "resolve") if name == "sys.Failure" => Some(
            "system rows are read-only; use `sys.admin.resolve_failure(failure.reference, ...)`",
        ),
        (Type::Named(name), "retry") if name == "sys.Stream" => {
            Some("system rows are read-only; use `sys.admin.retry_failure` on a `sys.FailureRef`")
        }
        (Type::Named(name), "skip") if name == "sys.Stream" => {
            Some("system rows are read-only; use `sys.admin.skip_failure` on a `sys.FailureRef`")
        }
        _ => None,
    }
}

fn infer_numeric_member(base: &Type, name: &str) -> Option<Type> {
    match (base, name) {
        (Type::Decimal, "divide") => Some(Type::Function {
            default_parameters: BTreeSet::new(),
            parameters: vec![Type::Decimal, Type::Int, Type::Named("std.Rounding".into())],
            parameter_names: Some(vec!["value".into(), "scale".into(), "rounding".into()]),
            result: Box::new(Type::Decimal),
        }),
        _ => None,
    }
}

fn infer_numeric_postfix(base: &Type, name: &str, scope: &Scope) -> Option<Type> {
    match base {
        Type::Int | Type::Decimal
            if scope.currency_types.contains(name) || is_currency_code(name) =>
        {
            Some(Type::Applied {
                base: "Money".into(),
                arguments: vec![Type::Named(name.into())],
            })
        }
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

fn is_currency_code(name: &str) -> bool {
    name.len() == 3 && name.bytes().all(|byte| byte.is_ascii_uppercase())
}

fn incompatible_addition_message(left: &Type, right: &Type) -> Option<&'static str> {
    if let (Some(left), Some(right)) = (money_currency(left), money_currency(right))
        && left != right
    {
        return Some("cannot add different currencies without conversion");
    }
    let (Some(left), Some(right)) = (numeric_unit(left), numeric_unit(right)) else {
        return None;
    };
    let (Some(left), Some(right)) = (unit_dimension(left), unit_dimension(right)) else {
        return None;
    };
    (matches!((left, right), ("Time", "Energy") | ("Energy", "Time")))
        .then_some("cannot add Time and Energy")
}

fn money_currency(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Applied { base, arguments } if base == "Money" => arguments.first(),
        _ => None,
    }
}

fn numeric_unit(ty: &Type) -> Option<&str> {
    match ty {
        Type::Applied { base, arguments }
            if matches!(base.as_str(), "Int" | "Decimal" | "Float") && arguments.len() == 1 =>
        {
            match arguments.first() {
                Some(Type::Named(unit)) => Some(unit),
                _ => None,
            }
        }
        _ => None,
    }
}

fn unit_dimension(unit: &str) -> Option<&'static str> {
    match unit.rsplit('.').next().unwrap_or(unit) {
        "day" | "days" | "hour" | "hours" | "minute" | "minutes" => Some("Time"),
        "kWh" => Some("Energy"),
        _ => None,
    }
}

fn is_absolute_affine_temperature(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Applied { base, arguments }
            if matches!(base.as_str(), "Int" | "Decimal" | "Float")
                && matches!(arguments.as_slice(), [Type::Named(unit)] if unit == "C")
    )
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

fn is_binary_float(ty: &Type) -> bool {
    matches!(ty, Type::Float) || matches!(ty, Type::Applied { base, .. } if base == "Float")
}

fn money_constructor_currency(expr: &Expr) -> Option<&str> {
    let Expr::Name { text, .. } = expr else {
        return None;
    };
    let currency = text.strip_prefix("Money<")?.strip_suffix('>')?;
    (!currency.is_empty() && !currency.contains(',')).then_some(currency)
}

fn is_money_rate(ty: &Type) -> bool {
    matches!(ty, Type::MoneyPerUnit { .. })
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

fn infer_type_argument(expression: &Expr) -> Option<Inferred> {
    let path = qualified_path(expression)?;
    Some(Inferred {
        ty: Type::Named(path.join(".")),
        effects: EffectSummary::default(),
    })
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
        let message = if inferred.effects.effects.contains("network") {
            "declaration assertion uses forbidden network effect"
        } else {
            "assertion has forbidden effects or failure"
        };
        diagnostics.push(diag(DIAG_ASSERTION_EFFECT, message));
    }
    if matches!(owner, AssertionOwner::Module) {
        match dependencies.len() {
            0 => diagnostics.push(diag(
                DIAG_ASSERTION_SCOPE,
                "module assertions must depend on at least two distinct tables",
            )),
            1 => diagnostics.push(diag(
                DIAG_ASSERTION_ONE_TABLE,
                "a one-table invariant belongs inside that table",
            )),
            _ => {}
        }
    }
    plans.push(AssertionPlan {
        owner,
        dependencies,
        effects: inferred.effects,
    });
}

fn infer_refined_assertion(
    value: &Expr,
    base: &Type,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let Expr::Unary { op, rhs, .. } = value else {
        return infer(value, scope, &BTreeMap::new(), diagnostics);
    };
    if !matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=" | "in") {
        return infer(value, scope, &BTreeMap::new(), diagnostics);
    }
    let inferred = infer(rhs, scope, &BTreeMap::new(), diagnostics);
    let effects = inferred.effects;
    if op == "in" {
        match inferred.ty {
            Type::Range(element) => require_same(base, &element, diagnostics),
            Type::Error => {}
            _ => diagnostics.push(diag(
                DIAG_TYPE,
                "refined membership assertion requires a numeric range",
            )),
        }
    } else {
        require_same(base, &inferred.ty, diagnostics);
    }
    Inferred {
        ty: Type::Bool,
        effects,
    }
}

/// Elaborates the two owner-local relational predicate constructors used by
/// the frozen reference corpus. They are not evaluator functions: the table
/// supplies the unpublished `Relation<Row>` subject, and the lambda receives
/// one statically shaped row. All other assertion forms retain ordinary
/// inference and therefore remain unsupported unless the general checker can
/// prove them.
fn infer_table_assertion(
    value: &Expr,
    owner_name: &str,
    row: &Type,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if let Expr::Binary { lhs, op, .. } = value
        && op == "|"
        && let Expr::Name { text, .. } = lhs.as_ref()
    {
        let message = match text.as_str() {
            "self" => Some("remove `self |`; the table already supplies its candidate relation"),
            text if text == owner_name => {
                Some("remove the repeated table owner before the assertion predicate")
            }
            _ => None,
        };
        if let Some(message) = message {
            diagnostics.push(diag(DIAG_ASSERTION, message));
            return Inferred {
                ty: Type::Bool,
                effects: EffectSummary::default(),
            };
        }
    }
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
            table_schema: None,
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
            table_schema: None,
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

fn declared_table_schema(item: &Item) -> Option<TableSchema> {
    let Declaration::Table { keys, members, .. } = &item.declaration else {
        return None;
    };
    let Type::Record(fields) = table_row_type(item) else {
        unreachable!()
    };
    let mut required = BTreeSet::new();
    let mut computed = BTreeSet::new();
    for key in keys {
        if key.default.is_none()
            && let Pattern::Name(name, _) = &key.pattern
        {
            required.insert(name.clone());
        }
    }
    for member in members {
        if let orna_syntax_v1::TableMember::Field {
            name, initializer, ..
        } = member
        {
            match initializer {
                None => {
                    required.insert(name.clone());
                }
                Some(FieldInitializer::Computed(_)) => {
                    computed.insert(name.clone());
                }
                Some(FieldInitializer::Default(_)) => {}
            }
        }
    }
    Some(TableSchema {
        fields,
        admission: Some(TableAdmission {
            required,
            computed,
            keys: if keys.is_empty() {
                vec![("id".into(), Type::Int)]
            } else {
                keys.iter()
                    .filter_map(|key| match &key.pattern {
                        Pattern::Name(name, _) => Some((
                            name.clone(),
                            key.annotation.as_ref().map(type_of).unwrap_or(Type::Error),
                        )),
                        _ => None,
                    })
                    .collect()
            },
            automatic_key: keys.is_empty(),
        }),
    })
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
    fn standard_dependency_profile_requires_exact_pinned_source_bytes() {
        let profile = StandardDependencyProfile::from_sources(
            "std-snapshot-1",
            [("std/math.orna".into(), "fn increment() = 1;".into())],
        )
        .unwrap();

        assert_eq!(profile.snapshot(), "std-snapshot-1");
        assert_eq!(profile.module_digests().len(), 1);
        assert_eq!(
            profile.verify_source("std/math.orna", "fn increment() = 1;"),
            Ok(())
        );
        assert_eq!(
            profile.verify_source("std/math.orna", "fn increment() = 2;"),
            Err(StandardProfileError::DigestMismatch)
        );
        assert_eq!(
            profile.verify_source("std/text.orna", "fn trim() = 1;"),
            Err(StandardProfileError::MissingModule)
        );
    }

    #[test]
    fn standard_dependency_profile_rejects_unsafe_or_duplicate_modules() {
        assert_eq!(
            StandardDependencyProfile::empty(""),
            Err(StandardProfileError::EmptySnapshot)
        );
        assert_eq!(
            StandardDependencyProfile::from_sources(
                "std-snapshot-1",
                [("project/math.orna".into(), "fn f() = 1;".into())],
            ),
            Err(StandardProfileError::InvalidModulePath)
        );
        assert_eq!(
            StandardDependencyProfile::from_sources(
                "std-snapshot-1",
                [("std/hidden.module.orna".into(), "fn f() = 1;".into())],
            ),
            Err(StandardProfileError::InvalidModulePath)
        );
        assert_eq!(
            StandardDependencyProfile::from_sources(
                "std-snapshot-1",
                [
                    ("std/math.orna".into(), "fn f() = 1;".into()),
                    ("std/math.orna".into(), "fn f() = 1;".into()),
                ],
            ),
            Err(StandardProfileError::DuplicateModule)
        );
    }

    #[test]
    fn pinned_standard_sources_become_a_semantic_catalogue_without_execution() {
        let source = "pub fn increment(value: Int): Int = value + 1;";
        let profile = StandardDependencyProfile::from_sources(
            "std-snapshot-1",
            [("std/math.orna".into(), source.into())],
        )
        .unwrap();
        let catalogue =
            Catalogue::from_standard_sources(&profile, [("std/math.orna".into(), source.into())])
                .unwrap();
        let analysis = analyze_with_catalogue(
            &[ModuleInput::new(
                "client.orna",
                "use std.math.{increment}; fn next(value: Int): Int = increment(value);",
            )],
            &catalogue,
        );
        assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
        assert_eq!(
            Catalogue::from_standard_sources(
                &profile,
                [("std/math.orna".into(), "pub fn broken( = 1;".into())],
            ),
            Err(StandardCatalogueError::Profile(
                StandardProfileError::DigestMismatch
            ))
        );
        assert_eq!(
            Catalogue::from_standard_sources(&profile, []),
            Err(StandardCatalogueError::MissingModule)
        );
    }

    #[test]
    fn verified_standard_sources_replace_matching_core_modules() {
        let source = "pub fn increment(value: Int): Int = value + 1;";
        let profile = StandardDependencyProfile::from_sources(
            "std-snapshot-1",
            [("std/math.orna".into(), source.into())],
        )
        .unwrap();
        let catalogue = Catalogue::authoritative_core()
            .with_standard_sources(&profile, [("std/math.orna".into(), source.into())])
            .unwrap();
        let analysis = analyze_with_catalogue(
            &[ModuleInput::new(
                "client.orna",
                "use std.math.{increment}; fn next(value: Int): Int = increment(value);",
            )],
            &catalogue,
        );
        assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
        assert_eq!(
            Catalogue::authoritative_core().with_standard_sources(
                &profile,
                [("std/math.orna".into(), "pub fn broken( = 1;".into())],
            ),
            Err(StandardCatalogueError::Profile(
                StandardProfileError::DigestMismatch
            ))
        );
    }

    #[test]
    fn pinned_standard_root_prelude_is_explicit_and_source_defined() {
        let source = "pub fn answer(): Int = 42;";
        let profile = StandardDependencyProfile::from_sources(
            "std-snapshot-1",
            [("std/main.orna".into(), source.into())],
        )
        .unwrap()
        .with_prelude_exports(["answer"]);
        let catalogue =
            Catalogue::from_standard_sources(&profile, [("std/main.orna".into(), source.into())])
                .unwrap();
        let analysis = analyze_with_catalogue(
            &[ModuleInput::new(
                "client.orna",
                "use std as _; pub fn run(): Int = answer();",
            )],
            &catalogue,
        );
        assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);

        let invalid = StandardDependencyProfile::from_sources(
            "std-snapshot-1",
            [("std/main.orna".into(), source.into())],
        )
        .unwrap()
        .with_prelude_exports(["missing"]);
        assert_eq!(
            Catalogue::from_standard_sources(&invalid, [("std/main.orna".into(), source.into())]),
            Err(StandardCatalogueError::Profile(
                StandardProfileError::InvalidPreludeExport,
            ))
        );
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
                default_parameters: BTreeSet::new(),
                result: Box::new(Type::Record(BTreeMap::from([("a".into(), Type::Int)])))
            }
        );
        assert_eq!(
            m.symbols.get("list").expect("list is collected").ty,
            Type::Function {
                parameters: vec![],
                parameter_names: Some(vec![]),
                default_parameters: BTreeSet::new(),
                result: Box::new(Type::List(Box::new(Type::Int)))
            }
        );
        assert!(!has(&a, DIAG_TYPE));
    }

    #[test]
    fn empty_lambda_blocks_match_empty_record_literals() {
        let a = checked(&[ModuleInput::new(
            "m.orna",
            "pub fn makers() = [() => ({}), () => {},];",
        )]);
        assert!(
            !has(&a, DIAG_TYPE),
            "empty lambda records: {:?}",
            a.diagnostics
        );
        let module = a.modules.values().next().expect("module is present");
        let symbol = module.symbols.get("makers").expect("makers is collected");
        assert_eq!(
            symbol.ty,
            Type::Function {
                parameters: vec![],
                parameter_names: Some(vec![]),
                default_parameters: BTreeSet::new(),
                result: Box::new(Type::List(Box::new(Type::Function {
                    parameters: vec![],
                    parameter_names: Some(vec![]),
                    default_parameters: BTreeSet::new(),
                    result: Box::new(Type::Record(BTreeMap::new())),
                }))),
            }
        );
    }

    #[test]
    fn duplicate_durable_consumers_are_rejected_before_execution() {
        let a = analyze_with_catalogue(
            &[ModuleInput::new(
                "m.orna",
                "pub fn main() = parallel([mail.google.sync, mail.google.sync]);",
            )],
            &Catalogue::authoritative_fixture(),
        );
        assert!(a.diagnostics.iter().any(|diagnostic| {
            diagnostic.message()
                == "same durable source consumed twice without distinct consumer identity"
        }));
    }

    #[test]
    fn durable_consumer_cannot_merge_multiple_source_roots() {
        let a = analyze_with_catalogue(
            &[ModuleInput::new(
                "m.orna",
                "pub fn main() = merge_streams([google.mail(), openbanking.transactions()]);",
            )],
            &Catalogue::authoritative_fixture(),
        );
        assert!(a.diagnostics.iter().any(|diagnostic| {
            diagnostic.message()
                == "a durable consumer function may own only one checkpointed source root"
        }));
    }

    #[test]
    fn fixture_provider_roots_reach_source_ownership_typechecking() {
        let a = analyze_with_catalogue(
            &[ModuleInput::new(
                "examples/invalid/two-durable-sources.orna",
                "pub fn main() = merge_streams([google.mail(), openbanking.transactions()]);",
            )],
            &Catalogue::authoritative_fixture(),
        );
        assert!(
            !has(&a, DIAG_UNRESOLVED),
            "provider roots resolved: {:?}",
            a.diagnostics
        );
        assert!(a.diagnostics.iter().any(|diagnostic| {
            diagnostic.message()
                == "a durable consumer function may own only one checkpointed source root"
        }));
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

    #[test]
    fn shape_only_catalogue_does_not_invent_table_admission_rules() {
        let catalogue = Catalogue::authoritative_fixture();
        let order = &catalogue.attached_symbols["Order"];
        assert_eq!(order.ty, Type::Named("Order".into()));
        let schema = order.table_schema.as_ref().unwrap();
        assert!(schema.fields.contains_key("customer"));
        assert!(schema.admission.is_none());
        let contact = &catalogue.modules[&Namespace(vec!["contacts".into()])].exports["Contact"];
        assert_eq!(contact.ty, Type::Named("contacts.Contact".into()));
        let admission = contact
            .table_schema
            .as_ref()
            .unwrap()
            .admission
            .as_ref()
            .unwrap();
        assert_eq!(admission.required, BTreeSet::from(["name".into()]));
        assert_eq!(admission.computed, BTreeSet::from(["full_name".into()]));
        assert_eq!(admission.keys, vec![("id".into(), Type::Text)]);
        assert!(!admission.automatic_key);
    }

    #[test]
    fn type_diagnostics_do_not_serialize_non_identifier_catalogue_names() {
        assert_eq!(
            diagnostic_type_name(&Type::Named("bad/type".into())),
            "<type>"
        );
        assert_eq!(
            diagnostic_type_name(&Type::Applied {
                base: "Float".into(),
                arguments: vec![Type::Named("bad/type".into())],
            }),
            "Float<<type>>"
        );
    }
}
