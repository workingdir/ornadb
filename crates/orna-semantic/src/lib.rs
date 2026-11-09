//! Read-only semantic loading for the frozen Orna 1.0 syntax tree.
//!
//! This crate accepts source bytes supplied by its caller.  It deliberately
//! does not read the host filesystem, execute expressions, or install a
//! standard library.  Its result is a deterministic module graph, headers,
//! imports, conservative type summaries, effect/failure summaries, and
//! declaration-assertion plans.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::{One, Signed, Zero};
use orna_foundation::{Diagnostic, SafeText};
use orna_syntax::{
    AssignmentOperator, AssignmentTarget, ControlKind, Declaration, DimensionExpr, Expr,
    FieldInitializer, Item, LambdaParameter, LiteralKind, Pattern, ProtocolMember, Statement,
    StringSegment, SyntaxSpan, SyntaxTree, TypeExpr, TypeMember, TypeRepresentation,
    UnitDefinition, UseTail, parse_module_with_file,
};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

mod diagnostics;
mod effects;
mod located;
mod repl;
mod semantic_payload;
mod system_api;
pub use effects::EffectKind;
pub use system_api::system_enum_constant;
mod source_row;

pub use located::{DiagnosticOrigin, LocatedAnalysis, analyze_with_catalogue_located};

pub use source_row::{
    LoweredRowUnit, lower_data_expression, lower_row_unit, lower_row_unit_with_key_spelling,
    lower_row_unit_with_owner_fields,
};

pub use semantic_payload::{
    DeclarationExplicitness, LocalSourceOrigin, NamedSemanticParameter, SEMANTIC_PAYLOAD_DOMAIN,
    SEMANTIC_PAYLOAD_VERSION, SemanticDeclaration, SemanticDeclarationKind, SemanticParameter,
    SemanticPayloadAnalysis, SemanticPayloadError, SemanticPayloadErrorKind,
    analyze_semantic_payloads,
};

pub use repl::{NameHints, ReplAdmission, ReplCommitError, ReplContext};

pub use diagnostics::*;
mod types;
pub use types::*;
mod catalogue;
pub use catalogue::*;
mod scope;
pub(crate) use scope::*;
mod std_support;
pub(crate) use std_support::*;
mod units;
pub use units::*;
mod imports;
pub(crate) use imports::*;
mod item_checks;
pub(crate) use item_checks::*;
mod flow;
pub(crate) use flow::*;
mod inference;
pub(crate) use inference::*;
mod collections;
pub(crate) use collections::*;
mod collection_calls;
pub(crate) use collection_calls::*;
mod calls;
pub(crate) use calls::*;
mod system_calls;
pub(crate) use system_calls::*;
mod checkpoint;
pub(crate) use checkpoint::*;
mod assertions;
pub(crate) use assertions::*;
mod secrets;
pub(crate) use secrets::*;
mod analysis_support;
pub use analysis_support::*;

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
    module_prelude_exports: BTreeMap<String, BTreeSet<String>>,
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
            module_prelude_exports: BTreeMap::new(),
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

    /// Returns the canonical source-bundle revision for this profile.
    ///
    /// The revision binds the captured snapshot label, every module path and
    /// source digest, the root prelude export set, and any module-specific
    /// prelude export sets. It is deliberately
    /// derived from this descriptor rather than from caller iteration order,
    /// so a catalogue consumer can retain one stable provenance coordinate
    /// without retaining source bytes. This is a source identity, not a
    /// semantic declaration revision.
    pub fn revision_digest(&self) -> [u8; 32] {
        standard_dependency_revision_digest(
            &self.snapshot,
            &self.module_digests,
            &self.prelude_exports,
            &self.module_prelude_exports,
        )
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

    /// Records the exact `as _` exports for one pinned standard source module.
    /// The path and names are checked against captured module bytes when the
    /// profile is admitted into a catalogue.
    pub fn with_module_prelude_exports(
        mut self,
        logical_path: impl Into<String>,
        names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let logical_path = logical_path.into();
        let exports = names.into_iter().map(Into::into).collect::<BTreeSet<_>>();
        if exports.is_empty() {
            self.module_prelude_exports.remove(&logical_path);
        } else {
            self.module_prelude_exports.insert(logical_path, exports);
        }
        self
    }

    pub fn module_prelude_exports(&self) -> &BTreeMap<String, BTreeSet<String>> {
        &self.module_prelude_exports
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

const STANDARD_DEPENDENCY_REVISION_DOMAIN_V1: &[u8] = b"orna.std.dependency.v1";
const STANDARD_DEPENDENCY_REVISION_DOMAIN_V2: &[u8] = b"orna.std.dependency.v2";

fn standard_dependency_revision_digest(
    snapshot: &str,
    module_digests: &BTreeMap<String, [u8; 32]>,
    prelude_exports: &BTreeSet<String>,
    module_prelude_exports: &BTreeMap<String, BTreeSet<String>>,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    let revision_domain: &[u8] = if module_prelude_exports.is_empty() {
        STANDARD_DEPENDENCY_REVISION_DOMAIN_V1
    } else {
        STANDARD_DEPENDENCY_REVISION_DOMAIN_V2
    };
    let revision_version = if module_prelude_exports.is_empty() {
        1u32
    } else {
        2u32
    };
    hasher.update(revision_domain);
    hasher.update([0]);
    hasher.update(revision_version.to_be_bytes());
    update_canonical_text(&mut hasher, snapshot);
    hasher.update((module_digests.len() as u64).to_be_bytes());
    for (logical_path, digest) in module_digests {
        update_canonical_text(&mut hasher, logical_path);
        hasher.update(digest);
    }
    hasher.update((prelude_exports.len() as u64).to_be_bytes());
    for export in prelude_exports {
        update_canonical_text(&mut hasher, export);
    }
    if !module_prelude_exports.is_empty() {
        hasher.update((module_prelude_exports.len() as u64).to_be_bytes());
        for (logical_path, exports) in module_prelude_exports {
            update_canonical_text(&mut hasher, logical_path);
            hasher.update((exports.len() as u64).to_be_bytes());
            for export in exports {
                update_canonical_text(&mut hasher, export);
            }
        }
    }
    hasher.finalize().into()
}

fn update_canonical_text(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
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

/// Compiler-local identity for a nominal or table declaration.
///
/// This is intentionally a qualified semantic key, not the durable object
/// identity assigned by the catalogue/revision layer. It keeps source-level
/// names from unrelated modules distinct while preserving the existing public
/// `Type::Named` representation. For a table it is also the qualified name
/// under which the committed identity manifest records the table's ObjectId,
/// so two `Item` tables in different modules never share a row type.
fn nominal_identity(namespace: &Namespace, name: &str) -> String {
    let prefix = namespace.display();
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}.{name}")
    }
}

impl EffectSummary {
    fn join(&mut self, other: &Self) {
        self.effects.extend(other.effects.iter().copied());
        self.opaque |= other.opaque;
        self.may_fail |= other.may_fail;
    }
    /// Whether this summary performs any effect, including opaque ones.
    pub fn is_effectful(&self) -> bool {
        self.opaque || !self.effects.is_empty()
    }
    fn forbidden_for_assertion(&self) -> bool {
        self.is_effectful()
    }
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

/// The primitive, root and operation modules every catalogue exposes.
fn core_root_modules(modules: &mut BTreeMap<Namespace, ModuleHeader>) {
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
    modules.insert(
        Namespace(vec!["std".into(), "concurrent".into()]),
        catalogue_module(
            Namespace(vec!["std".into(), "concurrent".into()]),
            [
                (
                    "parallel",
                    function(
                        vec![Type::List(Box::new(Type::Error))],
                        Type::List(Box::new(Type::Error)),
                    ),
                ),
                (
                    "race",
                    function(vec![Type::List(Box::new(Type::Error))], Type::Error),
                ),
                (
                    "timeout",
                    named_function(
                        vec![
                            ("callback", function(Vec::new(), Type::Error)),
                            ("duration", Type::Named("std.DURATION".into())),
                        ],
                        Type::Error,
                    ),
                ),
                (
                    "sleep",
                    named_function(
                        vec![("duration", Type::Named("std.DURATION".into()))],
                        Type::Null,
                    ),
                ),
            ],
            std::iter::empty::<&str>(),
        ),
    );
}

/// The remaining standard modules: values, UI, text, concurrency and data.
fn core_std_modules(modules: &mut BTreeMap<Namespace, ModuleHeader>) {
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
                ("Action", Type::Named("std.ui.Action".into())),
                ("text", function(vec![Type::Text], ui.clone())),
                ("button", function(vec![Type::Text, Type::Bool], ui.clone())),
                (
                    "action",
                    named_function(
                        vec![
                            ("action_id", Type::Text),
                            ("as", Type::Error),
                            ("debug_kind", Type::Optional(Box::new(Type::Text))),
                        ],
                        Type::Named("std.ui.Action".into()),
                    ),
                ),
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
}

/// The row, table and connector types the fixture environment attaches.
struct FixtureRows {
    reading: Type,
    message: Type,
    email: Type,
    note: Type,
    customer: Type,
    order: Type,
    payment: Type,
    audit: Type,
    contact_table: (String, Symbol),
}

fn fixture_rows() -> FixtureRows {
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
            private_required: false,
            computed: BTreeSet::from(["full_name".into()]),
            keys: vec![("id".into(), Type::Text)],
            automatic_key: false,
        },
    );
    FixtureRows {
        reading,
        message,
        email,
        note,
        customer,
        order,
        payment,
        audit,
        contact_table,
    }
}

/// The fixture rows and the attached tables/connectors they name.
fn fixture_attached_symbols(catalogue: &mut Catalogue, rows: &FixtureRows) {
    catalogue.attached_symbols = [
        fixture_type("Account", Type::Named("Account".into())),
        fixture_table("Audit", rows.audit.clone()),
        rows.contact_table.clone(),
        fixture_type("Customer", rows.customer.clone()),
        // The corpus snippets use `GBP` and `EUR` without declaring them;
        // the manifest's fixture environment supplies that support.
        fixture_currency("EUR"),
        fixture_currency("GBP"),
        fixture_table("Message", Type::Named("Message".into())),
        fixture_nominal_type(
            "EmailHeader",
            Type::Record(BTreeMap::from([("address".into(), Type::Text)])),
        ),
        fixture_function(
            "valid_email",
            named_function(vec![("value", Type::Text)], Type::Bool),
        ),
        fixture_table("Note", rows.note.clone()),
        fixture_table("Order", rows.order.clone()),
        fixture_table("Payment", rows.payment.clone()),
    ]
    .into_iter()
    .collect();
}

/// The fixture's connector and unit modules.
fn fixture_modules(catalogue: &mut Catalogue, rows: &FixtureRows) {
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
        fixture_module(
            Namespace(vec!["contacts".into()]),
            [rows.contact_table.clone()],
            true,
        ),
    );
    catalogue.modules.insert(
        Namespace(vec!["energy".into()]),
        fixture_module(
            Namespace(vec!["energy".into()]),
            [
                fixture_function("daily", function(vec![Type::Error], Type::Error)),
                fixture_table("Reading", rows.reading.clone()),
            ],
            true,
        ),
    );
    // The frozen historical-program example is backed by this explicit
    // retained projection, not by treating every current fixture module as
    // historical code.  Its `daily` declaration is intentionally the
    // historical zero-argument signature.
    let historical_contact = catalogue
        .modules
        .get(&Namespace(vec!["contacts".into()]))
        .and_then(|module| module.exports.get("Contact"))
        .cloned()
        .expect("fixture Contact projection");
    catalogue.historical_modules.insert(
        Namespace(vec!["directory".into()]),
        fixture_module(
            Namespace(vec!["directory".into()]),
            [("Contact".into(), historical_contact)],
            true,
        ),
    );
    catalogue.historical_modules.insert(
        Namespace(vec!["energy".into()]),
        fixture_module(
            Namespace(vec!["energy".into()]),
            [
                fixture_function("daily", function(Vec::new(), Type::Error)),
                fixture_table("Reading", rows.reading.clone()),
            ],
            true,
        ),
    );
    catalogue.modules.insert(
        Namespace(vec!["mail".into()]),
        fixture_module(
            Namespace(vec!["mail".into()]),
            [fixture_table("Email", rows.email.clone())],
            true,
        ),
    );
    catalogue.modules.insert(
        Namespace(vec!["mail".into(), "google".into()]),
        fixture_module(
            Namespace(vec!["mail".into(), "google".into()]),
            [fixture_connector_function("sync", Vec::new(), Type::Null)],
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
            [fixture_connector_function(
                "sync",
                vec![("account", Type::Named("Account".into()))],
                Type::Null,
            )],
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
                    Type::Stream(Box::new(rows.message.clone())),
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
                fixture_table("Reading", rows.reading.clone()),
                fixture_value("trips", Type::Named("vehicle.corsa.trips".into())),
            ],
            true,
        ),
    );
    catalogue.modules.insert(
        Namespace(vec!["vehicle".into(), "corsa".into(), "freematics".into()]),
        fixture_module(
            Namespace(vec!["vehicle".into(), "corsa".into(), "freematics".into()]),
            [fixture_connector_function(
                "sync",
                vec![("source", Type::Text)],
                Type::Null,
            )],
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
            [
                fixture_function(
                    "parallel",
                    function(
                        vec![Type::List(Box::new(Type::Error))],
                        Type::List(Box::new(Type::Error)),
                    ),
                ),
                fixture_function(
                    "timeout",
                    named_function(
                        vec![
                            ("callback", function(Vec::new(), Type::Error)),
                            ("duration", Type::Named("std.DURATION".into())),
                        ],
                        Type::Error,
                    ),
                ),
            ],
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
    // The reference fixtures use these units without declaring them; the
    // conformance manifest requires their semantic support to be supplied
    // explicitly. `work` is the attached database of
    // `examples/valid/unit-cross-database.orna`.
    catalogue.modules.insert(
        Namespace(vec!["work".into()]),
        fixture_module(Namespace(vec!["work".into()]), [], true),
    );
}

/// Dimension and unit support for the reference fixtures, following the
/// declaration block of `05-types.md` "Dimensions and units". Only the units
/// the fixtures name are public, so the private helpers never shadow the
/// intrinsic temporal suffixes such as `90.min`.
const FIXTURE_UNIT_SUPPORT: &str = "\
pub dim Length;
pub dim Mass;
pub dim Time;
pub dim Temperature;
pub dim Information;
pub dim Speed = Length / Time;
pub dim Energy = Mass * Length^2 / Time^2;
unit m : Length base;
unit kg : Mass base;
unit s : Time base;
unit hour : Time = 3600.s;
unit mile : Length = 1609.344.m;
unit J : Energy = kg * m^2 / s^2;
unit B : Information base;
pub unit K : Temperature base;
pub unit C : Temperature = K offset 273.15 affine;
pub unit deltaC : Temperature = K;
pub unit kWh : Energy = 3.6e6.J;
pub unit mph : Speed = mile / hour;
pub unit MiB : Information = 1048576.B;
";

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
            let name: String = name.into();
            let effects = if matches!(&ty, Type::Function { .. }) {
                let path = namespace
                    .0
                    .iter()
                    .map(String::as_str)
                    .chain([name.as_str()])
                    .collect::<Vec<_>>();
                standard_function_effects(&path)
            } else {
                EffectSummary::default()
            };
            (
                name,
                Symbol {
                    table_schema: None,
                    kind: if matches!(ty, Type::Function { .. }) {
                        SymbolKind::Function
                    } else {
                        SymbolKind::Type
                    },
                    public: true,
                    effects,
                    generic_parameters: Vec::new(),
                    enum_variants: BTreeSet::new(),
                    ty,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    ModuleHeader {
        namespace,
        exports: symbols.clone(),
        symbols,
        generic_functions: BTreeMap::new(),
        protocols: BTreeMap::new(),
        prelude_exports: prelude_exports.into_iter().map(Into::into).collect(),
        implicit: false,
    }
}

/// Effect summary of a host-bound standard function, keyed by its fully
/// qualified path. These declarations are host-bound or wrap host operations,
/// so the declared path is the classification and the body is not trusted.
fn standard_function_effects(path: &[&str]) -> EffectSummary {
    EffectSummary {
        effects: effects::host_effect(path).into_iter().collect(),
        opaque: false,
        may_fail: effects::host_effect(path).is_some(),
    }
}

/// A source-declared function of the standard corpus. `callees` holds every
/// syntactic `Name`/`Field` path its signature defaults and body reference.
struct StandardFunction {
    namespace: Namespace,
    callees: Vec<Vec<String>>,
}

/// The `use` declarations of one standard module.
#[derive(Default)]
struct StandardImports {
    aliases: BTreeMap<String, Vec<String>>,
    globs: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssertionOwner {
    Module,
    Table(String),
    RefinedType(String),
}
/// What happens to a stored reference when its target row is deleted or
/// re-keyed (`sys.ReferenceAction`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ReferenceAction {
    Restrict,
    Cascade,
    SetNone,
}

/// A stored reference from one table column to a row of another table
/// (ORNA-REF-001). A table type used as a field denotes a stored reference,
/// not an embedded row.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct TableReference {
    /// The referencing column in the owning table.
    pub from_column: String,
    /// The referenced table, as its symbol name.
    pub to_table: String,
    /// True when the column may hold no reference at all.
    pub optional: bool,
    pub on_delete: ReferenceAction,
    pub on_rekey: ReferenceAction,
}

/// When an assertion is enforced (`sys.AssertionScope`).
///
/// The spec names four values and says which boundary each owner belongs to:
/// a refined-type assertion runs when a base value is constructed or converted
/// into the refined type (ORNA-ASSERT-041), a table assertion is always
/// enforced at activation, and a module assertion is evaluated over the
/// unpublished candidate database before publication (ORNA-ASSERT-026,
/// ORNA-ASSERT-027).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum AssertionScope {
    Activation,
    ValueConstruction,
    TransactionCandidate,
    SnapshotCandidate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssertionPlan {
    pub owner: AssertionOwner,
    pub dependencies: BTreeSet<String>,
    pub effects: EffectSummary,
    /// Source span of the assertion expression. `sys.ExpressionRef` is opaque
    /// and pinned to the snapshot that holds this span, so the span is the
    /// identity source for `sys.Assertion.expression`.
    pub expression_span: orna_syntax::SyntaxSpan,
    /// Position of the assertion among its owner's declarations, which
    /// `sys.Assertion.source_order` reports.
    pub source_order: usize,
    /// When the assertion is enforced, which `sys.Assertion.scope` reports.
    pub scope: AssertionScope,
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct SourceUnitIdentity {
    pub namespace: Namespace,
    pub name: String,
}
impl SourceUnitIdentity {
    pub fn display(&self) -> String {
        format!("{}.{}", self.namespace.display(), self.name)
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct SourceDimensionIdentity {
    pub namespace: Namespace,
    pub name: String,
}
impl SourceDimensionIdentity {
    pub fn display(&self) -> String {
        format!("{}.{}", self.namespace.display(), self.name)
    }
}
/// Reduced exponents over declared base dimensions.
type DimensionBasis = BTreeMap<SourceDimensionIdentity, i64>;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceUnitRational {
    /// Canonical base-10 integer numerator.
    pub numerator: String,
    /// Canonical positive base-10 integer denominator.
    pub denominator: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceUnitProvenance {
    pub logical_path: String,
    pub span: SyntaxSpan,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceUnitMetadata {
    pub identity: SourceUnitIdentity,
    /// Exponents over declared base dimensions; never inferred from names.
    pub dimension_basis: BTreeMap<SourceDimensionIdentity, i64>,
    pub scale: SourceUnitRational,
    pub offset: SourceUnitRational,
    pub affine: bool,
    pub provenance: SourceUnitProvenance,
}
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub modules: BTreeMap<Namespace, ModuleHeader>,
    pub assertions: BTreeMap<Namespace, Vec<AssertionPlan>>,
    pub source_units: BTreeMap<SourceUnitIdentity, SourceUnitMetadata>,
    pub diagnostics: Vec<Diagnostic>,
    /// Payload rows of every admitted module's source enums, keyed by module,
    /// so a REPL session admits the same enum spellings as the project.
    pub(crate) source_enums: BTreeMap<Namespace, BTreeMap<String, EnumVariantRows>>,
    /// Whether `sys` members were admitted against production providers.
    production_sys_providers: bool,
}
impl Analysis {
    pub fn is_ok(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// The provider linkage this analysis admitted `sys` members against.
    pub(crate) fn system_providers(&self) -> SystemProviders {
        if self.production_sys_providers {
            SystemProviders::Production
        } else {
            SystemProviders::DeclaredSurface
        }
    }
}

/// The provider linkage that analysis admits portable `sys` members against.
///
/// A member's static type comes from the declared `api/sys.json` surface;
/// whether it can execute depends on the providers of the host that evaluates
/// the program. Every caller that evaluates analysed source passes
/// [`SystemProviders::Production`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemProviders {
    /// Every member of the declared surface counts as linked, so analysis
    /// checks typing only. For source that is not admitted for evaluation.
    DeclaredSurface,
    /// Only members a production host links a provider for are admitted;
    /// every other member is rejected with `sys.version.incompatible`
    /// (ORNA-SYS-008).
    Production,
}

impl SystemProviders {
    /// A fresh admission record, or `None` when only typing is checked.
    fn admission_record(self) -> Option<Rc<RefCell<BTreeSet<String>>>> {
        match self {
            Self::DeclaredSurface => None,
            Self::Production => Some(Rc::default()),
        }
    }
}

/// Primitive row-unit input supplied by a project/repository adapter.
///
/// The semantic crate deliberately does not depend on `orna-project`.
/// Callers copy the opaque candidate accessors into this borrowed boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RowUnitInput<'a> {
    pub logical_path: &'a str,
    pub table_path: &'a str,
    pub key_path: &'a [String],
    pub source: &'a str,
    pub source_bytes: &'a [u8],
    pub parse_as: &'a str,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RowUnitAdmission {
    /// Slash-separated semantic owner (`namespace/table`).
    pub owner: Option<String>,
    /// Decoded key components in declared key order.
    pub key: Vec<String>,
    /// Statically admitted non-key row fields.
    pub fields: BTreeMap<String, Type>,
    /// Existing parser and semantic classifications, with no source values.
    pub diagnostics: Vec<Diagnostic>,
}

impl RowUnitAdmission {
    pub fn is_ok(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

/// Admit one opaque project row candidate against already checked declarations.
///
/// This is schema admission only: it parses and type-checks the row expression,
/// validates the encoded path key, and retains the existing assertion plans.
/// Evaluation, persistence and assertion execution remain downstream concerns.
pub fn admit_row_unit(analysis: &Analysis, input: &RowUnitInput<'_>) -> RowUnitAdmission {
    admit_row_unit_with_owner_fields(analysis, input, &[])
}

/// How a row candidate's encoded key path becomes canonical key text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowKeySpelling<'a> {
    /// The frozen `key-path-v1` loose-row spelling (`~xx` escapes, `~ff` empty).
    LegacyPathV1,
    /// A VFS-1 projected spelling whose caller already decoded every component
    /// to canonical key text in declared order and verified that exact spelling
    /// (profiles/vfs-1.md:36-56).
    VerifiedVfs1(&'a [String]),
}

/// Admits a row while reserving named stored fields for an authorized owner
/// binding. Reserved fields must be declared stored fields and absent in source.
pub fn admit_row_unit_with_owner_fields(
    analysis: &Analysis,
    input: &RowUnitInput<'_>,
    owner_fields: &[String],
) -> RowUnitAdmission {
    admit_row_unit_with_key_spelling(analysis, input, owner_fields, RowKeySpelling::LegacyPathV1)
}

/// Admits a row with an explicit key spelling. See [`RowKeySpelling`].
pub fn admit_row_unit_with_key_spelling(
    analysis: &Analysis,
    input: &RowUnitInput<'_>,
    owner_fields: &[String],
    spelling: RowKeySpelling<'_>,
) -> RowUnitAdmission {
    let owner_fields = owner_fields
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut result = RowUnitAdmission::default();
    if !analysis.is_ok() {
        result
            .diagnostics
            .extend(analysis.diagnostics.iter().cloned());
        return result;
    }
    if input.parse_as != "row_unit" {
        result.diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "project row candidate is not tagged as a row_unit",
        ));
        return result;
    }
    if std::str::from_utf8(input.source_bytes).is_err()
        || input.source_bytes != input.source.as_bytes()
    {
        result.diagnostics.push(diag(
            DIAG_BAD_PATH,
            "project row source bytes are not valid UTF-8 source",
        ));
        return result;
    }
    if !valid_row_path(input.logical_path)
        || !valid_row_path(input.table_path)
        || input.key_path.is_empty()
        || input
            .key_path
            .iter()
            .any(|component| !valid_row_component(component))
        || !input
            .key_path
            .last()
            .is_some_and(|component| component.ends_with(".orna"))
    {
        result.diagnostics.push(diag(
            DIAG_BAD_PATH,
            "project row candidate has an invalid logical or key path",
        ));
        return result;
    }
    let expected_logical = format!(
        "{}/{}",
        input.table_path.trim_end_matches('/'),
        input.key_path.join("/")
    );
    if input.logical_path != expected_logical {
        result.diagnostics.push(diag(
            DIAG_BAD_PATH,
            "project row logical path is not below its declared table path",
        ));
        return result;
    }
    let mut owners = analysis.modules.iter().flat_map(|(namespace, module)| {
        module.symbols.iter().filter_map(|(name, symbol)| {
            let schema = symbol.table_schema.as_ref()?;
            let owner = if namespace.0.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", namespace.0.join("/"), name)
            };
            (owner == input.table_path).then_some((owner, schema))
        })
    });
    let Some((owner, schema)) = owners.next() else {
        result.diagnostics.push(diag(
            DIAG_UNRESOLVED,
            "project row table path does not resolve to a declared table",
        ));
        return result;
    };
    if owners.next().is_some() {
        result.diagnostics.push(diag(
            DIAG_AMBIGUOUS,
            "project row table path resolves to more than one declared table",
        ));
        return result;
    }
    result.owner = Some(owner);
    let Some(admission) = schema.admission.as_ref() else {
        result.diagnostics.push(diag(
            DIAG_TYPE,
            "declared table has no row admission schema",
        ));
        return result;
    };
    if owner_fields.iter().any(|name| {
        !admission.required.contains(*name)
            || admission.keys.iter().any(|(key, _)| key == name)
            || admission.computed.contains(*name)
    }) {
        result.diagnostics.push(diag(
            DIAG_TYPE,
            "owner-bound field is not a required stored field",
        ));
        return result;
    }
    let mut source_schema_fields = schema.fields.clone();
    for name in &owner_fields {
        source_schema_fields.remove(*name);
    }
    let decoded_key = match spelling {
        RowKeySpelling::LegacyPathV1 => decode_row_key_path(input.key_path),
        RowKeySpelling::VerifiedVfs1(keys) => Some(keys.to_vec()),
    };
    let Some(decoded_key) = decoded_key else {
        result.diagnostics.push(diag(
            DIAG_BAD_PATH,
            "project row key path is not canonically encoded",
        ));
        return result;
    };
    let Some(key_types) = row_key_component_types(admission, analysis) else {
        result.diagnostics.push(diag(
            DIAG_TYPE,
            "project row key path does not match the declared key schema",
        ));
        return result;
    };
    if decoded_key.len() != key_types.len()
        || key_types
            .iter()
            .zip(&decoded_key)
            .any(|(ty, value)| !row_key_matches_type(value, ty, analysis))
    {
        result.diagnostics.push(diag(
            DIAG_TYPE,
            "project row key path does not match the declared key schema",
        ));
        return result;
    }
    result.key = decoded_key;
    let parsed = orna_syntax::parse_row_with_file(input.source, input.logical_path);
    for _ in parsed.diagnostics {
        result.diagnostics.push(diag(
            "ORNA-S000-PARSE",
            "source was not admitted by the frozen syntax parser",
        ));
    }
    if !result.diagnostics.is_empty() {
        return result;
    }
    let orna_syntax::Expr::Record { fields, .. } = &parsed.value else {
        result
            .diagnostics
            .push(diag(DIAG_TYPE, "project row unit must contain one record"));
        return result;
    };
    let key_names = admission
        .keys
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for field in fields {
        if !seen.insert(field.name.as_str()) {
            result
                .diagnostics
                .push(diag(DIAG_DUPLICATE, "project row repeats a field"));
        }
        if key_names.contains(field.name.as_str()) {
            result.diagnostics.push(diag(
                DIAG_TYPE,
                "project row body must not repeat a path key",
            ));
        }
        if admission.computed.contains(&field.name) {
            result.diagnostics.push(diag(
                DIAG_TYPE,
                "project row body cannot supply a computed field",
            ));
        }
        if owner_fields.contains(field.name.as_str()) {
            result.diagnostics.push(diag(
                DIAG_TYPE,
                "project row body cannot supply an owner-bound field",
            ));
        }
    }
    let mut inferred = Vec::new();
    let scope = Scope {
        unlinked_system_members: analysis.system_providers().admission_record(),
        ..Scope::default()
    };
    let input_type = infer_table_row_input(
        &parsed.value,
        &source_schema_fields,
        &scope,
        &BTreeMap::new(),
        &mut inferred,
        true,
    );
    if let Some(record) = &scope.unlinked_system_members {
        report_unlinked_system_members(record, &mut inferred);
    }
    result.diagnostics.extend(inferred);
    if let Type::Record(fields) = input_type.ty {
        result.fields = fields;
    }
    for required in &admission.required {
        if !key_names.contains(required.as_str())
            && !owner_fields.contains(required.as_str())
            && !seen.contains(required.as_str())
        {
            result.diagnostics.push(diag(
                DIAG_MISSING_REQUIRED_FIELD,
                "project row omits a required stored field",
            ));
        }
    }
    result
}

/// Load and check caller-provided modules.  `sys` is a built-in namespace,
/// while `std` remains absent unless an adapter supplies it separately; source
/// modules may not define either reserved root. Portable `sys` members are
/// checked against the declared surface only; see [`analyze_with_providers`].
pub fn analyze(inputs: &[ModuleInput]) -> Analysis {
    analyze_with_catalogue(inputs, &Catalogue::empty())
}

/// Load source modules with an explicit declaration catalogue.  Names not in
/// source or this catalogue remain unresolved; no profile is installed by
/// default. Portable `sys` members are checked against the declared surface
/// only; see [`analyze_with_providers`].
pub fn analyze_with_catalogue(inputs: &[ModuleInput], catalogue: &Catalogue) -> Analysis {
    analyze_with_providers(inputs, catalogue, SystemProviders::DeclaredSurface)
}

/// Load source modules with an explicit declaration catalogue and admit each
/// portable `sys` member against `providers`. A host that evaluates the
/// analysed program passes [`SystemProviders::Production`], so a member with
/// no linked provider fails here with `sys.version.incompatible` rather than
/// mid-evaluation.
pub fn analyze_with_providers(
    inputs: &[ModuleInput],
    catalogue: &Catalogue,
    providers: SystemProviders,
) -> Analysis {
    analyze_retaining_context_with_providers(inputs, catalogue, false, providers).0
}

/// One type error in the body of a pinned standard dependency's source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StandardBodyDiagnostic {
    /// Logical path, under the reserved `std/` root, of the module whose body
    /// raised the diagnostic.
    pub logical_path: String,
    /// One-based line of the top-level declaration whose body raised it.
    pub line: usize,
    /// Published semantic diagnostic code, such as `ORNA-S021-TYPE`.
    pub code: String,
    /// The diagnostic message; it never carries source text.
    pub message: String,
    /// The diagnostic's notes, such as `expected Int, found Text`.
    pub notes: Vec<String>,
}

/// The result of type-checking one pinned standard dependency's own modules.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StandardSourceCheck {
    /// Diagnostics in analysis order, each attributed to its module and item.
    pub diagnostics: Vec<StandardBodyDiagnostic>,
}

impl StandardSourceCheck {
    pub fn is_ok(&self) -> bool {
        self.diagnostics.is_empty()
    }

    pub fn first(&self) -> Option<&StandardBodyDiagnostic> {
        self.diagnostics.first()
    }
}

/// Diagnostic codes that say the semantic slice did not decide an item.
///
/// They force the admission to fall back to the pinned declaration for that
/// item: a type diagnostic raised beside them can be a consequence of the
/// missing analysis rather than evidence about the source.
const UNDECIDED_ITEM_CODES: [&str; 4] = [
    DIAG_IMPORT,
    DIAG_UNRESOLVED,
    DIAG_ANNOTATION,
    DIAG_UNSUPPORTED,
];

/// A diagnostic paired with the byte range of the item that carries it.
type ItemDiagnosticSpan<'a> = (&'a Diagnostic, Range<usize>);

/// Type-checks the bodies of one pinned standard dependency's own modules.
///
/// [`Catalogue::from_standard_sources`] parses the bundle and derives its
/// declarations, but a catalogue cannot check bodies: `std` is a reserved
/// root for project source, so a standard module never reaches ordinary
/// analysis. ORNA-MODULE-003 requires loaded source to resolve, infer and
/// type-check, so a caller holding the exact pinned bundle calls this after
/// building `catalogue` from the same `sources`.
///
/// `catalogue` MUST describe those same sources. Its declarations then stand
/// for every standard module an analysed body imports, while each module's
/// own body is checked here. Nothing is executed and no source bytes are
/// retained.
///
/// # What is decided
///
/// The checker is a semantic slice, not the whole language, and the reference
/// standard library uses forms it does not model: relative `use` paths inside
/// the pinned tree, `case` on a parameter-valued enum, refinement bodies and
/// generic evidence. Its output over the shipped bundle therefore contains
/// diagnostics that do not describe source errors. Admission reports only an
/// item the slice *decided*:
///
/// * the item's own span carries no `ORNA-S010-IMPORT`,
///   `ORNA-S012-UNRESOLVED`, `ORNA-S020-ANNOTATION` or
///   `ORNA-S022-UNSUPPORTED` diagnostic, and
/// * a type-mismatch diagnostic ([`DIAG_TYPE`] or [`DIAG_TYPE_MISMATCH`])
///   names both sides in closed labels that differ.
///
/// A closed label is a primitive or a `List`/`Relation`/`Range` of closed
/// labels. Named, optional, generic and unknown labels stay undecided: the
/// checker compares some spellings by identity, and an identity comparison is
/// not evidence of a source error. Everything else keeps today's behaviour,
/// so the pinned bundle is admitted on its verified declarations.
pub fn check_standard_sources(
    catalogue: &Catalogue,
    sources: impl IntoIterator<Item = (String, String)>,
    providers: SystemProviders,
) -> StandardSourceCheck {
    let sources = sources.into_iter().collect::<Vec<_>>();
    let profile = catalogue.standard_dependency_profile.as_ref();
    let inputs = sources
        .iter()
        .map(|(logical_path, source)| {
            let mut diagnostics = Vec::new();
            let namespace = namespace_for_path(logical_path, &mut diagnostics);
            let prelude_exports = match (namespace.as_ref(), profile) {
                (Some(namespace), Some(profile))
                    if namespace.0.first().map(String::as_str) == Some("std")
                        && namespace.0.len() == 1 =>
                {
                    profile.prelude_exports.clone()
                }
                (_, Some(profile)) => profile
                    .module_prelude_exports
                    .get(logical_path)
                    .cloned()
                    .unwrap_or_default(),
                (_, None) => BTreeSet::new(),
            };
            ModuleInput {
                logical_path: logical_path.clone(),
                source: source.clone(),
                prelude_exports,
            }
        })
        .collect::<Vec<_>>();
    let mut recorder = located::OriginRecorder::enabled();
    let (analysis, _) = analyze_recording_origins(
        &inputs,
        catalogue,
        false,
        providers,
        SourceAdmission::StandardBundle,
        &mut recorder,
    );
    let origins = recorder.align(&analysis.diagnostics);
    let item_spans = sources
        .iter()
        .map(|(logical_path, source)| {
            let parsed = parse_module_with_file(source, logical_path);
            let spans = parsed
                .value
                .items
                .iter()
                .map(|item| item.span.start..item.span.end)
                .collect::<Vec<_>>();
            (logical_path.clone(), spans)
        })
        .collect::<BTreeMap<_, _>>();
    let mut by_item: BTreeMap<(String, usize), Vec<ItemDiagnosticSpan<'_>>> = BTreeMap::new();
    for (diagnostic, origin) in analysis.diagnostics.iter().zip(origins) {
        let (Some(logical_path), Some(primary)) = (origin.logical_path, origin.primary) else {
            continue;
        };
        let Some(spans) = item_spans.get(&logical_path) else {
            continue;
        };
        let Some(index) = spans
            .iter()
            .position(|span| span.start <= primary.start && primary.end <= span.end)
        else {
            continue;
        };
        by_item
            .entry((logical_path, index))
            .or_default()
            .push((diagnostic, primary));
    }
    let mut diagnostics = Vec::new();
    for ((logical_path, _), item) in by_item {
        if item
            .iter()
            .any(|(diagnostic, _)| UNDECIDED_ITEM_CODES.contains(&diagnostic.code()))
        {
            continue;
        }
        let Some(source) = sources
            .iter()
            .find(|(path, _)| path == &logical_path)
            .map(|(_, source)| source)
        else {
            continue;
        };
        for (diagnostic, span) in item {
            if !decided_type_mismatch(diagnostic) {
                continue;
            }
            diagnostics.push(StandardBodyDiagnostic {
                logical_path: logical_path.clone(),
                line: span_line(source, &span),
                code: diagnostic.code().to_owned(),
                message: diagnostic.message().to_owned(),
                notes: diagnostic
                    .notes()
                    .iter()
                    .map(|note| note.as_str().to_owned())
                    .collect(),
            });
        }
    }
    StandardSourceCheck { diagnostics }
}

/// One-based line of a byte offset in `source`.
fn span_line(source: &str, span: &Range<usize>) -> usize {
    source.get(..span.start).map_or(1, |prefix| {
        prefix.bytes().filter(|byte| *byte == b'\n').count() + 1
    })
}

/// Whether `diagnostic` compares two different, closed, fully-known type
/// labels. Only then is the comparison evidence about the source rather than
/// about the slice's inference.
fn decided_type_mismatch(diagnostic: &Diagnostic) -> bool {
    if !matches!(diagnostic.code(), DIAG_TYPE | DIAG_TYPE_MISMATCH) {
        return false;
    }
    let Some(note) = diagnostic.notes().first() else {
        return false;
    };
    let Some(sides) = note.as_str().strip_prefix("expected ") else {
        return false;
    };
    let Some((expected, actual)) = sides.split_once(", found ") else {
        return false;
    };
    expected != actual && closed_type_label(expected) && closed_type_label(actual)
}

/// Whether a diagnostic's type label is closed: a primitive, or a container of
/// closed labels. Named, optional, generic and unknown labels are open.
fn closed_type_label(label: &str) -> bool {
    let label = label.trim();
    if matches!(
        label,
        "Int" | "Text" | "Bool" | "Float" | "Decimal" | "Date" | "Instant" | "Unit" | "Null"
    ) {
        return true;
    }
    for container in ["List", "Relation", "Range"] {
        if let Some(inner) = label
            .strip_prefix(container)
            .and_then(|rest| rest.strip_prefix('<'))
            .and_then(|rest| rest.strip_suffix('>'))
        {
            return closed_type_label(inner);
        }
    }
    false
}

/// Semantic payload derivation describes declarations rather than admitting
/// a program for evaluation, so it checks the declared `sys` surface only.
fn analyze_retaining_context(
    inputs: &[ModuleInput],
    catalogue: &Catalogue,
    retain_context: bool,
) -> (Analysis, Vec<(Namespace, SyntaxTree, Scope)>) {
    analyze_retaining_context_with_providers(
        inputs,
        catalogue,
        retain_context,
        SystemProviders::DeclaredSurface,
    )
}

fn analyze_retaining_context_with_providers(
    inputs: &[ModuleInput],
    catalogue: &Catalogue,
    retain_context: bool,
    providers: SystemProviders,
) -> (Analysis, Vec<(Namespace, SyntaxTree, Scope)>) {
    let mut origins = located::OriginRecorder::disabled();
    analyze_recording_origins(
        inputs,
        catalogue,
        retain_context,
        providers,
        SourceAdmission::Project,
        &mut origins,
    )
}

/// Which source modules one analysis admits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceAdmission {
    /// Ordinary project source. `sys` and `std` are reserved roots, and a
    /// namespace already present in the catalogue may not be redefined.
    Project,
    /// One pinned standard dependency's own modules. The bundle is the text
    /// that defines the `std` root, so each admitted module replaces the
    /// catalogue entry built from those same bytes; `sys` stays reserved.
    StandardBundle,
}

fn analyze_recording_origins(
    inputs: &[ModuleInput],
    catalogue: &Catalogue,
    retain_context: bool,
    providers: SystemProviders,
    admission: SourceAdmission,
    origins: &mut located::OriginRecorder,
) -> (Analysis, Vec<(Namespace, SyntaxTree, Scope)>) {
    let mut contexts = Vec::new();
    let mut result = Analysis {
        modules: catalogue.modules.clone(),
        production_sys_providers: providers == SystemProviders::Production,
        ..Analysis::default()
    };
    let mut parsed = Vec::new();
    let mut source_unit_items = Vec::new();
    let mut siblings: BTreeMap<Vec<String>, BTreeMap<String, String>> = BTreeMap::new();
    for input in inputs {
        origins.enter_module(&result.diagnostics, &input.logical_path);
        let Some(namespace) = namespace_for_path(&input.logical_path, &mut result.diagnostics)
        else {
            continue;
        };
        let root = namespace.0.first().map(String::as_str);
        // A pinned standard bundle defines the `std` root itself; every other
        // admission keeps both reserved roots closed to source.
        let standard_module = admission == SourceAdmission::StandardBundle && root == Some("std");
        if !standard_module && matches!(root, Some("sys" | "std")) {
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
        if result.modules.contains_key(&namespace) && !standard_module {
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
        source_unit_items.extend(
            parse
                .value
                .items
                .iter()
                .filter(|&item| {
                    matches!(
                        &item.declaration,
                        Declaration::Dimension { .. } | Declaration::Unit { .. }
                    )
                })
                .map(|item| (namespace.clone(), input.logical_path.clone(), item.clone())),
        );
        origins.bind_namespace(&namespace, &input.logical_path);
        parsed.push((namespace, parse.value));
    }
    let (source_units, source_dimensions) = analyze_source_units(&source_unit_items);
    // Catalogue-supplied declarations resolve like source ones; a source
    // declaration with the same identity takes precedence.
    let mut units = catalogue.source_units.clone();
    units.extend(
        source_units
            .iter()
            .map(|(identity, unit)| (identity.clone(), unit.clone())),
    );
    let mut dimensions = catalogue.source_dimensions.clone();
    dimensions.extend(source_dimensions);
    let units = UnitEnvironment {
        units: Rc::new(units),
        dimensions: Rc::new(dimensions),
    };
    result.source_units = source_units;
    let mut dependency_summaries = BTreeMap::new();
    let source_enums = parsed
        .iter()
        .map(|(namespace, tree)| (namespace.clone(), source_enum_variant_rows(namespace, tree)))
        .collect::<BTreeMap<_, _>>();
    stabilize_function_summaries(
        &parsed,
        &mut result.modules,
        &catalogue.historical_modules,
        &catalogue.attached_symbols,
        &catalogue.attached_databases,
        &mut dependency_summaries,
        &units,
    );
    // Every module of one analysis belongs to a single admission, so each
    // unlinked `sys` member is reported once.
    let unlinked_system_members = providers.admission_record();
    // ORNA-SECRET-001 applies to an imported type whatever the importer can see. Every
    // module's nominal rows and transparent aliases are keyed by canonical identity first, so
    // a secret reached through a wrapper or a re-export chain is found in any importer.
    let mut secret_rows: BTreeMap<String, Type> = BTreeMap::new();
    let mut secret_aliases: BTreeMap<String, Type> = BTreeMap::new();
    let mut secret_spellings: BTreeMap<Namespace, BTreeMap<String, String>> = BTreeMap::new();
    for (namespace, tree) in &parsed {
        let Some(header) = result.modules.get(namespace) else {
            continue;
        };
        let mut scratch = result.diagnostics.clone();
        let scope = resolve_imports_with_dependencies(
            namespace,
            tree,
            header,
            ImportInputs {
                modules: &result.modules,
                historical_modules: &catalogue.historical_modules,
                attached_symbols: &catalogue.attached_symbols,
                attached_databases: &catalogue.attached_databases,
                dependency_summaries: &dependency_summaries,
                units: &units,
            },
            &mut scratch,
        );
        let spellings = secret_identity_spellings(namespace, tree, &scope, &result.modules);
        secret_definitions(
            namespace,
            tree,
            &spellings,
            &mut secret_rows,
            &mut secret_aliases,
        );
        secret_spellings.insert(namespace.clone(), spellings);
    }
    for (namespace, tree) in &parsed {
        origins.enter_namespace(&result.diagnostics, namespace);
        let Some(header) = result.modules.get(namespace).cloned() else {
            continue;
        };
        let mut scope = resolve_imports_with_dependencies(
            namespace,
            tree,
            &header,
            ImportInputs {
                modules: &result.modules,
                historical_modules: &catalogue.historical_modules,
                attached_symbols: &catalogue.attached_symbols,
                attached_databases: &catalogue.attached_databases,
                dependency_summaries: &dependency_summaries,
                units: &units,
            },
            &mut result.diagnostics,
        );
        scope.current_namespace = Some(namespace.clone());
        scope.unlinked_system_members = unlinked_system_members.clone();
        let spellings = secret_spellings.get(namespace).cloned().unwrap_or_default();
        scope.secret_types = imported_secret_types(tree, &spellings, &secret_rows, &secret_aliases);
        // Import visibility is enforced by `scope.names`; every unit a name
        // can reach is in the merged environment, including wildcard-imported
        // catalogue units whose module never becomes a `scope.modules` alias.
        scope.source_units = Rc::clone(&units.units);
        let imported_enums = imported_enum_bindings(tree, &source_enums, &result.modules);
        scope.payload_enum_types.extend(
            imported_enums
                .iter()
                .filter(|(_, variants)| variants.values().any(|fields| !fields.is_empty()))
                .map(|(spelling, _)| spelling.clone()),
        );
        scope.imported_enum_variants = imported_enums;
        let mut symbols = header.symbols.clone();
        for symbol in symbols.values_mut() {
            canonicalize_symbol(symbol, &scope.nominal_identities);
            normalize_symbol_quantities(symbol, &scope);
        }
        stabilize_local_function_dependencies(tree, &scope);
        let table_rows = tree
            .items
            .iter()
            .filter_map(|item| match &item.declaration {
                Declaration::Table { name, .. } => Some((
                    name.clone(),
                    normalize_source_quantities(&table_row_type(item), &scope, None),
                )),
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let mut plans = Vec::new();
        for (source_order, item) in tree.items.iter().enumerate() {
            origins.enter_item(&result.diagnostics, namespace, &item.span);
            check_item(
                item,
                source_order,
                &mut symbols,
                &scope,
                &table_rows,
                &mut plans,
                &mut result.diagnostics,
            );
        }
        let conversion_metadata = conversion_metadata(&scope);
        attach_conversion_metadata(&mut symbols, &conversion_metadata);
        attach_currency_metadata(&mut symbols, &scope.currency_types);
        attach_table_references(&mut symbols);
        if let Some(module) = result.modules.get_mut(namespace) {
            module.symbols = symbols;
            module.exports = module
                .symbols
                .iter()
                .filter(|(_, s)| s.public && !type_contains_error(&s.ty))
                .map(|(n, s)| (n.clone(), s.clone()))
                .collect();
        }
        result.assertions.insert(namespace.clone(), plans);
        if retain_context {
            contexts.push((namespace.clone(), tree.clone(), scope));
        }
    }
    result.source_enums = source_enums;
    origins.finish(&result.diagnostics);
    // Admission-wide diagnostics belong to no item, so they are reported after
    // the recorder has attributed the per-item diagnostics.
    if let Some(record) = &unlinked_system_members {
        report_unlinked_system_members(record, &mut result.diagnostics);
    }
    result
        .diagnostics
        .sort_by(|a, b| a.code().cmp(b.code()).then(a.message().cmp(b.message())));
    (result, contexts)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UnitFraction {
    numerator: BigInt,
    denominator: BigInt,
}
impl UnitFraction {
    fn new(mut numerator: BigInt, mut denominator: BigInt) -> Option<Self> {
        if denominator.is_zero() {
            return None;
        }
        if denominator.is_negative() {
            numerator = -numerator;
            denominator = -denominator;
        }
        if numerator.is_zero() {
            return Some(Self {
                numerator,
                denominator: BigInt::one(),
            });
        }
        let mut left = numerator.abs();
        let mut right = denominator.clone();
        while !right.is_zero() {
            let remainder = &left % &right;
            left = right;
            right = remainder;
        }
        if !left.is_zero() {
            numerator /= &left;
            denominator /= left;
        }
        Some(Self {
            numerator,
            denominator,
        })
    }
    fn one() -> Self {
        Self {
            numerator: BigInt::one(),
            denominator: BigInt::one(),
        }
    }
    fn zero() -> Self {
        Self {
            numerator: BigInt::zero(),
            denominator: BigInt::one(),
        }
    }
    fn multiply(&self, other: &Self) -> Option<Self> {
        Self::new(
            &self.numerator * &other.numerator,
            &self.denominator * &other.denominator,
        )
    }
    fn divide(&self, other: &Self) -> Option<Self> {
        Self::new(
            &self.numerator * &other.denominator,
            &self.denominator * &other.numerator,
        )
    }
    fn power(&self, exponent: i64) -> Option<Self> {
        let magnitude = u32::try_from(exponent.unsigned_abs()).ok()?;
        if magnitude > 100_000 {
            return None;
        }
        if exponent < 0 {
            Self::new(
                self.denominator.pow(magnitude),
                self.numerator.pow(magnitude),
            )
        } else {
            Self::new(
                self.numerator.pow(magnitude),
                self.denominator.pow(magnitude),
            )
        }
    }
    fn public(&self) -> SourceUnitRational {
        SourceUnitRational {
            numerator: self.numerator.to_string(),
            denominator: self.denominator.to_string(),
        }
    }
    fn decimal(text: &str) -> Option<Self> {
        let text = text.replace('_', "");
        let exponent_index = text.find('e').or_else(|| text.find('E'));
        let (mantissa, exponent) = exponent_index.map_or((text.as_str(), 0i64), |index| {
            let (mantissa, exponent) = text.split_at(index);
            (mantissa, exponent[1..].parse::<i64>().unwrap_or(i64::MAX))
        });
        if exponent == i64::MAX || exponent.unsigned_abs() > 1_000_000 {
            return None;
        }
        let (negative, mantissa) = mantissa
            .strip_prefix('-')
            .map_or((false, mantissa), |rest| (true, rest));
        let mantissa = mantissa.strip_prefix('+').unwrap_or(mantissa);
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if whole.is_empty() && fraction.is_empty() {
            return None;
        }
        let digits = format!("{whole}{fraction}");
        let mut numerator = digits.parse::<BigInt>().ok()?;
        if negative {
            numerator = -numerator;
        }
        let decimal_exponent = exponent.checked_sub(i64::try_from(fraction.len()).ok()?)?;
        if decimal_exponent >= 0 {
            if decimal_exponent > 1_000_000 {
                return None;
            }
            numerator *= BigInt::from(10u8).pow(decimal_exponent as u32);
            Self::new(numerator, BigInt::one())
        } else {
            let magnitude = decimal_exponent.unsigned_abs();
            if magnitude > 1_000_000 {
                return None;
            }
            Self::new(numerator, BigInt::from(10u8).pow(magnitude as u32))
        }
    }
}

#[derive(Clone)]
struct SourceDimensionDecl {
    expression: Option<DimensionExpr>,
}
#[derive(Clone)]
struct SourceUnitDecl {
    identity: SourceUnitIdentity,
    dimension: TypeExpr,
    definition: UnitDefinition,
    provenance: SourceUnitProvenance,
}

/// The structural meaning of a unit: reduced dimension exponents, exact
/// scale and affine offset (ORNA-UNIT-001).
struct UnitShape {
    dimension_basis: DimensionBasis,
    scale: UnitFraction,
    offset: UnitFraction,
    affine: bool,
}

/// One table declaration: reserved names, key schema, row shape and
/// member assertions.
struct TableCheck<'a> {
    item: &'a Item,
    name: &'a str,
    keys: &'a [orna_syntax::Parameter],
    members: &'a [orna_syntax::TableMember],
    symbols: &'a mut BTreeMap<String, Symbol>,
    scope: &'a Scope,
    plans: &'a mut Vec<AssertionPlan>,
}

fn check_table_item(check: TableCheck<'_>, diagnostics: &mut Vec<Diagnostic>) {
    let TableCheck {
        item,
        name,
        keys,
        members,
        symbols,
        scope,
        plans,
    } = check;
    if name == "std" {
        diagnostics.push(diag(DIAG_RESERVED_STD, "`std` is reserved"));
    } else if name == "sys" {
        diagnostics.push(diag(DIAG_RESERVED_SYS, "`sys` is reserved"));
    }
    let mut key_names = BTreeSet::new();
    for key in keys {
        match &key.pattern {
            Pattern::Name(key_name, _) => {
                if !key_names.insert(key_name.clone()) {
                    diagnostics.push(diag(DIAG_DUPLICATE, "duplicate primary-key field"));
                }
            }
            // Key paths are reconstructed as an ordered list of named
            // columns, so destructuring patterns have no stable schema
            // surface here.
            _ => diagnostics.push(diag(DIAG_TYPE, "primary-key fields must have simple names")),
        }
        let annotation_resolved = match &key.annotation {
            Some(annotation) => {
                let before = diagnostics.len();
                validate_type_annotation(annotation, scope, &BTreeSet::new(), diagnostics);
                diagnostics.len() == before
            }
            None => {
                diagnostics.push(diag(DIAG_TYPE, "primary-key fields must declare a type"));
                false
            }
        };
        let ty = key
            .annotation
            .as_ref()
            .map(|annotation| resolved_type_of(annotation, scope))
            .unwrap_or(Type::Error);
        // A key type that failed annotation validation already carries
        // its diagnostic; it has no encoding left to judge.
        if annotation_resolved
            && let Some((code, message)) = primary_key_type_error(&ty, scope, &mut BTreeSet::new())
        {
            diagnostics.push(diag(code, message));
        }
    }
    // Keep the catalogue's key encoding types transparent through
    // local aliases; path admission has no source Scope to consult.
    if let Some(schema) = symbols
        .get_mut(name)
        .and_then(|symbol| symbol.table_schema.as_mut())
    {
        for key in keys {
            let (Pattern::Name(key_name, _), Some(annotation)) = (&key.pattern, &key.annotation)
            else {
                continue;
            };
            let resolved = resolved_type_of(annotation, scope);
            schema.fields.insert(key_name.clone(), resolved.clone());
            if let Some((_, key_type)) = schema.admission.as_mut().and_then(|admission| {
                admission
                    .keys
                    .iter_mut()
                    .find(|(field, _)| field == key_name)
            }) {
                *key_type = resolved;
            }
        }
    }
    let row = table_row_type(item);
    let target = Type::Named(canonical_nominal_name(name, scope));
    let mut row_locals = match &row {
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
                        generic_parameters: Vec::new(),
                        enum_variants: BTreeSet::new(),
                    },
                )
            })
            .collect(),
        _ => BTreeMap::new(),
    };
    if matches!(&row, Type::Record(_)) {
        if keys.is_empty()
            && members.iter().any(|member| {
                matches!(member, orna_syntax::TableMember::Field { name, .. } if name == "id")
            })
        {
            // `id` is already the implicit logical key, so choosing a
            // body field of the same name would make row identity
            // depend on declaration order.
            diagnostics.push(diag(
                DIAG_DUPLICATE,
                "automatic primary-key field `id` conflicts with a declared field",
            ));
        }
        let mut declared_fields = key_names.clone();
        if keys.is_empty() {
            declared_fields.insert("id".to_owned());
        }
        for member in members {
            if let orna_syntax::TableMember::Field { name, .. } = member
                && !declared_fields.insert(name.clone())
            {
                diagnostics.push(diag(
                    DIAG_DUPLICATE,
                    "table field conflicts with another field or primary key",
                ));
            }
        }
    }
    // `self` is the complete prospective row inside key defaults and
    // row-local field expressions. The table row shape already
    // includes key columns and the implicit `id` where applicable.
    if let Type::Record(fields) = &row {
        row_locals.insert(
            "self".into(),
            Symbol {
                table_schema: None,
                kind: SymbolKind::Let,
                ty: Type::Record(fields.clone()),
                public: false,
                effects: EffectSummary::default(),
                generic_parameters: Vec::new(),
                enum_variants: BTreeSet::new(),
            },
        );
    }
    for key in keys {
        let (Some(annotation), Some(default)) = (&key.annotation, &key.default) else {
            continue;
        };
        let expected = resolved_type_of(annotation, scope);
        let inferred = infer_contextual(default, &expected, scope, &row_locals, diagnostics);
        require_same(&expected, &inferred.ty, diagnostics);
    }
    validate_non_overlapping_implementations(
        members.iter().filter_map(|member| match member {
            orna_syntax::TableMember::Implementation { implementation, .. } => Some(implementation),
            _ => None,
        }),
        scope,
        diagnostics,
    );
    for member in members {
        if let orna_syntax::TableMember::Implementation { implementation, .. } = member {
            validate_nested_implementation_members(
                implementation,
                &scope.local_protocols,
                scope,
                &target,
                Some(&row),
                diagnostics,
            );
            validate_implementation_restrictions(implementation, scope, diagnostics);
        }
    }
    for (source_order, member) in members.iter().enumerate() {
        match member {
            orna_syntax::TableMember::Assertion { value, .. } => {
                let inferred = infer_table_assertion(value, name, &row, scope, diagnostics);
                assertion(
                    AssertionInput {
                        owner: AssertionOwner::Table(name.to_owned()),
                        value,
                        inferred,
                        resolved_tables: None,
                        scope: Some(scope),
                        // A table assertion is always enforced at the
                        // activation boundary.
                        enforcement: AssertionScope::Activation,
                        source_order,
                    },
                    plans,
                    diagnostics,
                );
            }
            orna_syntax::TableMember::Field {
                initializer: None,
                ty,
                ..
            } => {
                validate_type_annotation(ty, scope, &BTreeSet::new(), diagnostics);
                validate_stored_field_type(ty, scope, diagnostics);
            }
            orna_syntax::TableMember::Field {
                initializer: Some(initializer),
                ty,
                ..
            } => {
                validate_type_annotation(ty, scope, &BTreeSet::new(), diagnostics);
                if matches!(initializer, FieldInitializer::Default(_)) {
                    validate_stored_field_type(ty, scope, diagnostics);
                }
                let value = match initializer {
                    FieldInitializer::Default(value) | FieldInitializer::Computed(value) => value,
                };
                let expected = resolved_type_of(ty, scope);
                let inferred = infer_contextual(value, &expected, scope, &row_locals, diagnostics);
                if matches!(initializer, FieldInitializer::Computed(_))
                    && (inferred.effects.is_effectful() || inferred.effects.may_fail)
                {
                    diagnostics.push(diag(
                        DIAG_COMPUTED_NONDETERMINISTIC,
                        "computed field must be deterministic and row-local",
                    ));
                }
                require_same(&expected, &inferred.ty, diagnostics);
            }
            _ => {}
        }
    }
}

fn check_alias_type_item(
    name: &str,
    ty: &TypeExpr,
    refinements: &[TypeMember],
    scope: &Scope,
    plans: &mut Vec<AssertionPlan>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // A refined alias keeps its own nominal identity, but its
    // representation shape follows the transparent base alias.
    let base = resolved_type_of(ty, scope);
    let target = Type::Named(name.to_owned());
    let target_shape = matches!(&base, Type::Record(_)).then_some(&base);
    validate_non_overlapping_implementations(
        refinements.iter().filter_map(|member| match member {
            TypeMember::Implementation { implementation, .. } => Some(implementation),
            _ => None,
        }),
        scope,
        diagnostics,
    );
    for (source_order, member) in refinements.iter().enumerate() {
        match member {
            TypeMember::Assertion { value, .. } => {
                let inferred = infer_refined_assertion(value, &base, scope, diagnostics);
                assertion(
                    AssertionInput {
                        owner: AssertionOwner::RefinedType(name.to_owned()),
                        value,
                        inferred,
                        resolved_tables: None,
                        scope: Some(scope),
                        // A refined assertion runs whenever a base
                        // value is constructed or converted into the
                        // refined type (ORNA-ASSERT-041).
                        enforcement: AssertionScope::ValueConstruction,
                        source_order,
                    },
                    plans,
                    diagnostics,
                );
            }
            TypeMember::Implementation { implementation, .. } => {
                validate_nested_implementation_members(
                    implementation,
                    &scope.local_protocols,
                    scope,
                    &target,
                    target_shape,
                    diagnostics,
                );
                validate_implementation_restrictions(implementation, scope, diagnostics);
            }
            _ => {}
        }
    }
}

fn check_nominal_type_item(
    name: &str,
    members: &[TypeMember],
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let target = Type::Named(canonical_nominal_name(name, scope));
    let target_name = match &target {
        Type::Named(name) => name,
        _ => unreachable!(),
    };
    let target_shape = scope
        .nominal_rows
        .get(target_name)
        .or_else(|| scope.table_rows.get(target_name));
    let mut field_locals = BTreeMap::new();
    for member in members {
        let TypeMember::Field {
            name,
            ty,
            initializer,
            ..
        } = member
        else {
            continue;
        };
        validate_type_annotation(ty, scope, &BTreeSet::new(), diagnostics);
        let expected = resolved_type_of(ty, scope);
        if let Some(initializer) = initializer {
            let inferred =
                infer_contextual(initializer, &expected, scope, &field_locals, diagnostics);
            require_same(&expected, &inferred.ty, diagnostics);
        }
        field_locals.insert(
            name.clone(),
            Symbol {
                table_schema: None,
                kind: SymbolKind::Let,
                ty: expected,
                public: false,
                effects: EffectSummary::default(),
                generic_parameters: Vec::new(),
                enum_variants: BTreeSet::new(),
            },
        );
    }
    validate_non_overlapping_implementations(
        members.iter().filter_map(|member| match member {
            TypeMember::Implementation { implementation, .. } => Some(implementation),
            _ => None,
        }),
        scope,
        diagnostics,
    );
    for member in members {
        let TypeMember::Implementation { implementation, .. } = member else {
            continue;
        };
        validate_nested_implementation_members(
            implementation,
            &scope.local_protocols,
            scope,
            &target,
            target_shape,
            diagnostics,
        );
        validate_implementation_restrictions(implementation, scope, diagnostics);
    }
}

/// Validates a `From` implementation: source type, the single `fn from(value)`
/// member and its body's result.
fn validate_from_implementation(
    implementation: &orna_syntax::Implementation,
    arguments: &[TypeExpr],
    target: &Type,
    target_shape: Option<&Type>,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(source) = arguments
        .first()
        .filter(|_| arguments.len() == 1)
        .and_then(|source| valid_static_type(source, scope))
    else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "From implementation source type is not a valid static type",
        ));
        return;
    };
    if let Type::Named(target_name) = target
        && scope.refined_types.get(target_name) == Some(&source)
    {
        diagnostics.push(diag(
            DIAG_TYPE,
            "overlapping protocol implementations are invalid",
        ));
        return;
    }
    if let Some(from) = implementation.members.as_slice().first()
        && implementation.members.len() == 1
    {
        let orna_syntax::ImplMember::Function {
            signature, body, ..
        } = from
        else {
            diagnostics.push(diag(
                DIAG_TYPE,
                "From implementation requires exactly one `fn from(value)` member",
            ));
            return;
        };
        if signature.name != "from"
            || !signature.generics.is_empty()
            || signature.parameters.len() != 1
        {
            diagnostics.push(diag(
                DIAG_TYPE,
                "From implementation requires exactly one `fn from(value)` member",
            ));
            return;
        }
        let parameter = &signature.parameters[0];
        if parameter
            .annotation
            .as_ref()
            .is_some_and(|annotation| resolved_type_of(annotation, scope) != source)
        {
            diagnostics.push(diag(
                DIAG_TYPE,
                "From implementation source parameter is incompatible",
            ));
            return;
        }
        if signature
            .result
            .as_ref()
            .is_some_and(|result| resolved_type_of(result, scope) != *target)
        {
            diagnostics.push(diag(
                DIAG_TYPE,
                "From implementation result is incompatible with its target",
            ));
            return;
        }
        let mut local = BTreeMap::new();
        bind_pattern(
            &parameter.pattern,
            source.clone(),
            scope,
            &mut local,
            diagnostics,
        );
        if matches!(target, Type::Named(name) if scope.nominal_rows.contains_key(name)) {
            let inferred =
                infer_from_nominal_target(body, target, target_shape, scope, &local, diagnostics);
            require_same(target, &inferred.ty, diagnostics);
        } else {
            let expected_body = target_shape.unwrap_or(target);
            let inferred = infer_contextual(body, expected_body, scope, &local, diagnostics);
            require_same(expected_body, &inferred.ty, diagnostics);
        }
        return;
    }
    diagnostics.push(diag(
        DIAG_TYPE,
        "From implementation requires exactly one `fn from(value)` member",
    ));
}

/// The protocol members an implementation must provide, or `None` after
/// reporting why the protocol identity cannot be resolved.
fn required_protocol_members<'a>(
    path: &[String],
    local_protocols: &'a BTreeMap<String, Vec<ProtocolMember>>,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<&'a [ProtocolMember]> {
    Some(match resolve_local_protocol_name(&path[0], scope) {
        Some(protocol_name) => {
            let Some(required_members) = local_protocols.get(&protocol_name) else {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "protocol implementation identity cannot be resolved",
                ));
                return None;
            };
            required_members.as_slice()
        }
        None => match scope.names.get(&path[0]) {
            Some(symbol) if symbol.kind == SymbolKind::Protocol => {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "imported protocol identity cannot be resolved",
                ));
                return None;
            }
            Some(_) => {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "protocol implementation identity is not a protocol",
                ));
                return None;
            }
            None if is_builtin_protocol_without_local_surface(&path[0]) => {
                // Display and Present are the two presentation protocol names
                // supplied by the frozen surface without a local member AST.
                return None;
            }
            None => match core_protocol_surface(&path[0]) {
                Some(surface) => surface,
                None => {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "unresolved protocol implementation identity",
                    ));
                    return None;
                }
            },
        },
    })
}

/// These presentation protocol names are part of the frozen builtin surface,
/// but their member AST is not carried in this local semantic scope. They are
/// therefore retained as the only missing-identity residual.
fn is_builtin_protocol_without_local_surface(name: &str) -> bool {
    matches!(name, "Display" | "Present")
}

/// `Currency` is a core protocol so `Money<C>` stays meaningful without `std`
/// (09-standard-library.md). Its member surface is fixed by ORNA-MONEY-004:
/// the stable static properties `code: Str` and `minor_digits: Int`
/// (05-types.md, "Money and currencies"). A module may implement it without
/// declaring it.
const CORE_CURRENCY_PROTOCOL: &str =
    "pub protocol Currency {\n    static code: Str;\n    static minor_digits: Int;\n}\n";

static CORE_CURRENCY_SURFACE: std::sync::LazyLock<Vec<ProtocolMember>> =
    std::sync::LazyLock::new(|| {
        parse_module_with_file(CORE_CURRENCY_PROTOCOL, "core/Currency.orna".to_owned())
            .value
            .items
            .into_iter()
            .find_map(|item| match item.declaration {
                Declaration::Protocol { members, .. } => Some(members),
                _ => None,
            })
            .expect("the core Currency protocol surface parses")
    });

/// How a unit name resolves in the current import context (ORNA-UNIT-008).
enum UnitLookup<'a> {
    Declared(&'a SourceUnitMetadata),
    Ambiguous,
    Unresolved,
}

/// Resolves a unit path through the ordinary lexical environment: local
/// declarations, explicit imports and attached declarations, then wildcard
/// imports, where two candidates are ambiguous. A qualified path names a unit
/// exported by an available module. A unit never resolves from its spelling
/// alone (ORNA-UNIT-001).
fn lookup_unit<'a>(path: &[String], scope: &'a Scope) -> UnitLookup<'a> {
    let symbol = match path {
        [] => None,
        [name] if scope.ambiguous.contains(name) && !scope.names.contains_key(name) => {
            return UnitLookup::Ambiguous;
        }
        [name] => scope.names.get(name),
        [root, members @ .., name] => scope.modules.get(root).and_then(|root| {
            let namespace = Namespace(root.0.iter().chain(members).cloned().collect());
            scope
                .available_modules
                .get(&namespace)
                .and_then(|module| module.exports.get(name))
        }),
    };
    symbol
        .filter(|symbol| symbol.kind == SymbolKind::Unit)
        .and_then(|symbol| match &symbol.ty {
            Type::Named(identity) => scope.source_units.get(&unit_identity(identity)),
            _ => None,
        })
        .map_or(UnitLookup::Unresolved, UnitLookup::Declared)
}

/// Splits the `namespace.name` identity a unit symbol carries.
fn unit_identity(identity: &str) -> SourceUnitIdentity {
    match identity.rsplit_once('.') {
        Some((namespace, name)) => SourceUnitIdentity {
            namespace: Namespace(namespace.split('.').map(str::to_owned).collect()),
            name: name.to_owned(),
        },
        None => SourceUnitIdentity {
            namespace: Namespace(Vec::new()),
            name: identity.to_owned(),
        },
    }
}

/// The units a module can name without qualification.
fn visible_units(scope: &Scope) -> impl Iterator<Item = &SourceUnitMetadata> {
    scope.names.values().filter_map(|symbol| match &symbol.ty {
        Type::Named(identity) if symbol.kind == SymbolKind::Unit => {
            scope.source_units.get(&unit_identity(identity))
        }
        _ => None,
    })
}

/// Checks a unit written in a type: `m`, `units.m` or a compound such as
/// `m / s`. Each name must resolve to a declared unit in the current import
/// context (ORNA-UNIT-008), and only linear units combine into a compound
/// unit (ORNA-UNIT-005). Returns whether a valid unit is affine.
fn validate_unit_annotation(
    unit: &TypeExpr,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<bool> {
    match unit {
        TypeExpr::Name {
            path, arguments, ..
        } if arguments.is_empty() => match lookup_unit(path, scope) {
            UnitLookup::Declared(unit) => Some(unit.affine),
            UnitLookup::Ambiguous => {
                diagnostics.push(diag(DIAG_AMBIGUOUS, AMBIGUOUS_UNIT));
                None
            }
            UnitLookup::Unresolved => {
                diagnostics.push(diag(DIAG_UNRESOLVED, "type name cannot be resolved"));
                None
            }
        },
        TypeExpr::Product { lhs, op, rhs, .. } if matches!(op.as_str(), "*" | "/") => {
            let left = validate_unit_annotation(lhs, scope, diagnostics);
            let right = validate_unit_annotation(rhs, scope, diagnostics);
            if left == Some(true) || right == Some(true) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "an absolute affine unit cannot form a compound unit",
                ));
                return None;
            }
            left.and(right)
        }
        _ => {
            diagnostics.push(diag(DIAG_UNRESOLVED, "type name cannot be resolved"));
            None
        }
    }
}

const AMBIGUOUS_UNIT: &str = "unit has multiple wildcard-import candidates";

fn annotation_type_name_is_declared(
    path: &[String],
    scope: &Scope,
    generic_names: &BTreeSet<String>,
) -> bool {
    let Some(last) = path.last() else {
        return false;
    };
    if path.len() == 1 {
        return generic_names.contains(last)
            || primitive(last).is_some()
            || system_api::embedded_system_api().describes_type(last)
            || matches!(
                last.as_str(),
                "List"
                    | "Range"
                    | "Relation"
                    | "Query"
                    | "Stream"
                    | "Money"
                    | "Quantity"
                    | "Locale"
            )
            || scope.names.get(last).is_some_and(|symbol| {
                matches!(
                    symbol.kind,
                    SymbolKind::Type | SymbolKind::Enum | SymbolKind::Table | SymbolKind::Protocol
                )
            });
    }

    let qualified = path.join(".");
    if matches!(
        qualified.as_str(),
        "std.DURATION"
            | "std.UUID"
            | "std.TIME"
            | "std.BINARY_LARGE_OBJECT"
            | "std.Action"
            | "std.Rows"
            | "std.JsonValue"
            | "std.Document"
            | "std.ByteStream"
            | "std.UI"
    ) || system_api::embedded_system_api().describes_type(&qualified)
    {
        return true;
    }

    // Annotation names resolve only through an explicit `use` binding of the
    // first segment (ORNA-IMPORT-001, ORNA-IMPORT-002). Full namespace paths
    // stay expression-only, so an unimported `vault.Box` remains unresolved.
    if !scope.modules.contains_key(&path[0]) {
        return false;
    }
    let parts = path.iter().map(String::as_str).collect::<Vec<_>>();
    qualified_module_symbol(&parts, scope).is_some_and(|symbol| {
        matches!(
            symbol.kind,
            SymbolKind::Type | SymbolKind::Enum | SymbolKind::Table | SymbolKind::Protocol
        )
    })
}

/// One function body whose `return` statements are being typed. Ownership is
/// lexical: a `return` inside a nested anonymous function exits that
/// function, not the enclosing declaration (ORNA-CFLOW-008).
#[derive(Clone)]
struct ReturnFrame {
    /// Addresses of the `return` statements that this body owns.
    owned: Vec<usize>,
    /// The declared result type, when the signature has one.
    expected: Option<Type>,
    /// Types of the owned `return` values, in source order.
    returned: Vec<Type>,
}

enum LoopTransferContext {
    Typed(Type),
    Unsupported,
}
fn bind_pattern(
    pattern: &Pattern,
    ty: Type,
    scope: &Scope,
    into: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if matches!(ty, Type::Error) {
        return;
    }
    match pattern {
        Pattern::Name(name, _) => {
            insert_local_binding(name, ty, into, diagnostics);
        }
        Pattern::Wildcard(_) => {}
        Pattern::Literal { kind, .. } => {
            let pattern_ty = literal_pattern_type(*kind);
            if !types_match(&pattern_ty, &ty) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "pattern literal type does not match the value type",
                ));
            }
        }
        Pattern::Tuple { elements, .. } => match ty {
            Type::Tuple(element_types) if element_types.len() == elements.len() => {
                for (pattern, element_ty) in elements.iter().zip(element_types) {
                    bind_pattern(pattern, element_ty, scope, into, diagnostics);
                }
            }
            Type::Tuple(_) => diagnostics.push(diag(
                DIAG_TYPE,
                "tuple pattern arity does not match the value type",
            )),
            _ => diagnostics.push(diag(DIAG_TYPE, "tuple pattern requires a tuple value")),
        },
        Pattern::List { elements, .. } => match ty {
            Type::List(element_ty) => {
                for pattern in elements {
                    bind_pattern(pattern, (*element_ty).clone(), scope, into, diagnostics);
                }
            }
            _ => diagnostics.push(diag(DIAG_TYPE, "list pattern requires a list value")),
        },
        Pattern::Record { fields, .. } => {
            bind_record_pattern(fields, &ty, scope, into, diagnostics);
        }
        Pattern::Constructor {
            path,
            arguments,
            fields,
            ..
        } => {
            bind_constructor_pattern(path, arguments, fields, &ty, scope, into, diagnostics);
        }
    }
}

fn bind_parameter_pattern(
    pattern: &Pattern,
    ty: Type,
    scope: &Scope,
    local: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    bind_pattern(pattern, ty, scope, local, diagnostics);
    for name in pattern_binding_names(pattern) {
        if let Some(symbol) = local.get_mut(&name) {
            symbol.kind = SymbolKind::Parameter;
        }
    }
}

fn literal_pattern_type(kind: LiteralKind) -> Type {
    match kind {
        LiteralKind::Integer => Type::Int,
        LiteralKind::Decimal => Type::Decimal,
        LiteralKind::Float => Type::Float,
        LiteralKind::Date => Type::Date,
        LiteralKind::Instant => Type::Instant,
        LiteralKind::String => Type::Text,
        LiteralKind::Boolean => Type::Bool,
        LiteralKind::Null => Type::Null,
    }
}

fn nominal_pattern_fields<'a>(ty: &Type, scope: &'a Scope) -> Option<&'a BTreeMap<String, Type>> {
    let Type::Named(name) = ty else {
        return None;
    };
    let short_name = name.rsplit('.').next().unwrap_or(name);
    match scope
        .nominal_rows
        .get(name)
        .or_else(|| scope.nominal_rows.get(short_name))
    {
        Some(Type::Record(fields)) => Some(fields),
        _ => None,
    }
}

fn bind_record_pattern(
    fields: &[(String, Option<Pattern>, SyntaxSpan)],
    ty: &Type,
    scope: &Scope,
    into: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(expected_fields) = (match ty {
        Type::Record(fields) => Some(fields),
        _ => nominal_pattern_fields(ty, scope),
    }) else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "record pattern requires a record or nominal value",
        ));
        return;
    };
    let mut seen = BTreeSet::new();
    for (name, nested, _) in fields {
        if !seen.insert(name.as_str()) {
            diagnostics.push(diag(DIAG_TYPE, "record pattern field is duplicated"));
            continue;
        }
        if let Type::Named(nominal) = ty
            && scope
                .nominal_private_fields
                .get(nominal)
                .is_some_and(|private| private.contains(name))
            && scope.private_field_owner.as_deref() != Some(nominal.as_str())
        {
            diagnostics.push(diag(
                DIAG_TYPE,
                "private nominal field is inaccessible in a pattern",
            ));
            continue;
        }
        let Some(field_ty) = expected_fields.get(name) else {
            diagnostics.push(diag(DIAG_TYPE, "record pattern field cannot be resolved"));
            continue;
        };
        if let Some(nested) = nested {
            bind_pattern(nested, field_ty.clone(), scope, into, diagnostics);
        } else {
            insert_local_binding(name, field_ty.clone(), into, diagnostics);
        }
    }
}

fn bind_constructor_pattern(
    path: &[orna_syntax::NameSegment],
    arguments: &[Pattern],
    fields: &[orna_syntax::PatternField],
    ty: &Type,
    scope: &Scope,
    into: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let path_text = path
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(".");
    if matches!(ty, Type::Optional(_))
        && path.len() == 1
        && path[0].text == "Some"
        && fields.is_empty()
    {
        let Type::Optional(inner) = ty else {
            unreachable!()
        };
        if let [argument] = arguments {
            bind_pattern(argument, (**inner).clone(), scope, into, diagnostics);
        } else {
            diagnostics.push(diag(DIAG_TYPE, "Some pattern requires exactly one payload"));
        }
        return;
    }
    if !arguments.is_empty() {
        diagnostics.push(diag(
            DIAG_TYPE,
            "constructor pattern arguments do not match the value type",
        ));
        return;
    }
    let expected_fields = match ty {
        Type::Record(fields) => Some(fields),
        Type::Named(name) => {
            let short_name = name.rsplit('.').next().unwrap_or(name);
            if let Some(variants) = scope
                .enum_variants
                .get(name)
                .or_else(|| scope.enum_variants.get(short_name))
            {
                let variant = path.last().map(|segment| segment.text.as_str());
                variant.and_then(|variant| variants.get(variant))
            } else {
                nominal_pattern_fields(ty, scope)
            }
        }
        _ => None,
    };
    let target_name = match ty {
        Type::Named(name) => name.rsplit('.').next().unwrap_or(name),
        _ => "",
    };
    let owner_text = if path.len() > 1 {
        path[..path.len() - 1]
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join(".")
    } else {
        path_text
    };
    let owner_name = owner_text.rsplit('.').next().unwrap_or(&owner_text);
    let matches_nominal = !target_name.is_empty() && owner_name == target_name;
    if expected_fields.is_none() || !matches_nominal {
        diagnostics.push(diag(
            DIAG_TYPE,
            "constructor pattern does not match the value type",
        ));
        return;
    }
    let expected_fields = expected_fields.expect("checked above");
    let mut seen = BTreeSet::new();
    for field in fields {
        if !seen.insert(field.name.as_str()) {
            diagnostics.push(diag(DIAG_TYPE, "constructor pattern field is duplicated"));
            continue;
        }
        if let Type::Named(nominal) = ty
            && scope
                .nominal_private_fields
                .get(nominal)
                .is_some_and(|private| private.contains(&field.name))
            && scope.private_field_owner.as_deref() != Some(nominal.as_str())
        {
            diagnostics.push(diag(
                DIAG_TYPE,
                "private nominal field is inaccessible in a pattern",
            ));
            continue;
        }
        let Some(field_ty) = expected_fields.get(&field.name) else {
            diagnostics.push(diag(
                DIAG_TYPE,
                "constructor pattern field cannot be resolved",
            ));
            continue;
        };
        if let Some(nested) = &field.pattern {
            bind_pattern(nested, field_ty.clone(), scope, into, diagnostics);
        } else {
            insert_local_binding(&field.name, field_ty.clone(), into, diagnostics);
        }
    }
}

fn insert_local_binding(
    name: &str,
    ty: Type,
    into: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if rejects_portable_sys_shadow(name, diagnostics) {
        return;
    }
    into.insert(
        name.to_owned(),
        Symbol {
            table_schema: None,
            kind: SymbolKind::Let,
            ty,
            public: false,
            effects: EffectSummary::default(),
            generic_parameters: Vec::new(),
            enum_variants: BTreeSet::new(),
        },
    );
}
#[derive(Clone)]
struct Inferred {
    ty: Type,
    effects: EffectSummary,
}

impl Inferred {
    /// A result that performs no host effect and cannot fail.
    fn pure(ty: Type) -> Self {
        Self {
            ty,
            effects: EffectSummary::default(),
        }
    }

    /// An expression that already produced a diagnostic and has no effects.
    fn error() -> Self {
        Self::pure(Type::Error)
    }

    /// An expression that already produced a diagnostic but whose operands
    /// still carry `effects`.
    fn error_with(effects: EffectSummary) -> Self {
        Self {
            ty: Type::Error,
            effects,
        }
    }
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
    if let Some(inferred) =
        infer_contextual_finite_list_aggregate(expr, expected, scope, local, diagnostics)
    {
        return inferred;
    }
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
    if let (Expr::Range { lower, upper, .. }, Type::Range(element_type)) = (expr, expected) {
        if lower.is_none() && upper.is_none() {
            diagnostics.push(diag(
                DIAG_TYPE,
                "an untyped range needs at least one endpoint or a range context",
            ));
            return Inferred::pure(expected.clone());
        }
        let mut effects = EffectSummary::default();
        for endpoint in [lower.as_deref(), upper.as_deref()].into_iter().flatten() {
            let inferred = infer_contextual(endpoint, element_type, scope, local, diagnostics);
            effects.join(&inferred.effects);
            require_same(element_type, &inferred.ty, diagnostics);
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
        return Inferred::pure(Type::Float);
    }
    if let Expr::Control {
        kind: ControlKind::If,
        binding,
        condition,
        body,
        arms,
        alternate,
        ..
    } = expr
    {
        return infer_if(
            ControlParts::new(
                binding.as_ref(),
                condition.as_deref(),
                body.as_deref(),
                arms,
                alternate.as_deref(),
            ),
            Some(expected),
            scope,
            local,
            diagnostics,
        );
    }
    infer(expr, scope, local, diagnostics)
}

fn contextual_finite_list_aggregate_element(operation: &str, expected: &Type) -> Option<Type> {
    match operation {
        "sum" => match expected {
            Type::Int | Type::Float => Some(expected.clone()),
            _ => None,
        },
        "min" | "max" => match expected {
            Type::Optional(element) if matches!(element.as_ref(), Type::Int | Type::Float) => {
                Some(element.as_ref().clone())
            }
            _ => None,
        },
        _ => None,
    }
}

fn infer_contextual_finite_list_aggregate(
    expr: &Expr,
    expected: &Type,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let (operation, rows, arguments, qualified_element) = match expr {
        Expr::Call {
            callee, arguments, ..
        } => {
            let operation = finite_list_collection_operation(callee, scope, local)?;
            if !matches!(operation, "sum" | "min" | "max")
                || arguments.len() != 1
                || arguments[0]
                    .name
                    .as_deref()
                    .is_some_and(|name| name != "rows")
                || !is_empty_list_literal(&arguments[0].value)
            {
                return None;
            }
            (
                operation,
                &arguments[0].value,
                arguments.as_slice(),
                finite_list_aggregate_element(callee, operation, scope),
            )
        }
        Expr::Binary { lhs, op, rhs, .. } if op == "|" => {
            let (callee, arguments) = match rhs.as_ref() {
                Expr::Call {
                    callee, arguments, ..
                } if arguments.is_empty() => (callee.as_ref(), arguments.as_slice()),
                Expr::Name { .. } | Expr::Field { .. } => {
                    (rhs.as_ref(), &[] as &[orna_syntax::Argument])
                }
                _ => return None,
            };
            let operation = finite_list_collection_operation(callee, scope, local)?;
            if !matches!(operation, "sum" | "min" | "max") || !is_empty_list_literal(lhs) {
                return None;
            }
            (
                operation,
                lhs.as_ref(),
                arguments,
                finite_list_aggregate_element(callee, operation, scope),
            )
        }
        _ => return None,
    };
    let element = qualified_element
        .or_else(|| contextual_finite_list_aggregate_element(operation, expected))?;
    let row = infer_contextual(
        rows,
        &Type::List(Box::new(element.clone())),
        scope,
        local,
        diagnostics,
    );
    Some(infer_finite_list_collection(
        operation,
        (matches!(expr, Expr::Binary { .. })).then_some(row),
        arguments,
        Some(element),
        scope,
        local,
        diagnostics,
    ))
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
        bind_parameter_pattern(
            &parameter.pattern,
            ty.clone(),
            scope,
            &mut callback_locals,
            diagnostics,
        );
        types.push(ty);
    }
    let inferred = infer_function_body(
        body,
        (!matches!(expected_result, Type::Error)).then_some(expected_result),
        scope,
        &callback_locals,
        diagnostics,
    );
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

/// Native stack headroom kept before inference requests a fresh stack segment.
const SEMANTIC_STACK_RED_ZONE: usize = 128 * 1024;
/// Size of each fresh stack segment used by deep expression inference.
const SEMANTIC_STACK_SEGMENT: usize = 2 * 1024 * 1024;

/// Nested expressions recurse once per level, and debug frames for
/// `infer_node`'s large match arms can exhaust a test thread's default stack
/// at a few dozen levels. Grow onto a fresh segment near the limit.
fn infer(
    expr: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    stacker::maybe_grow(SEMANTIC_STACK_RED_ZONE, SEMANTIC_STACK_SEGMENT, || {
        infer_node(expr, scope, local, diagnostics)
    })
}

fn infer_node(
    expr: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    match expr {
        Expr::Literal { kind, .. } => Inferred::pure(match kind {
            LiteralKind::Integer => Type::Int,
            LiteralKind::Decimal => Type::Decimal,
            LiteralKind::Float => Type::Float,
            LiteralKind::Date => Type::Date,
            LiteralKind::Instant => Type::Instant,
            LiteralKind::String => Type::Text,
            LiteralKind::Boolean => Type::Bool,
            LiteralKind::Null => Type::Null,
        }),
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
        Expr::Name { text, .. } | Expr::ReplBinding { text, .. } => {
            infer_name(text, scope, local, diagnostics)
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
        Expr::List { elements, .. } => infer_list(elements, scope, local, diagnostics),
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
        Expr::Field { base, name, .. } => infer_field(expr, base, name, scope, local, diagnostics),
        Expr::Lambda {
            parameters, body, ..
        } => infer_lambda(parameters, body, scope, local, diagnostics),
        Expr::Call {
            callee,
            arguments,
            span,
        } => infer_call(callee, arguments, span, scope, local, diagnostics),
        Expr::GenericCall {
            callee,
            type_arguments,
            arguments,
            ..
        } => infer_generic_call(callee, type_arguments, arguments, scope, local, diagnostics),
        Expr::Unary { op, rhs, .. } => {
            let inferred = infer(rhs, scope, local, diagnostics);
            Inferred {
                ty: infer_unary_operator(op, &inferred.ty, diagnostics),
                effects: inferred.effects,
            }
        }
        Expr::Range { lower, upper, .. } => infer_range(
            lower.as_deref(),
            upper.as_deref(),
            scope,
            local,
            diagnostics,
        ),
        Expr::Binary { lhs, op, rhs, .. } => infer_binary(lhs, op, rhs, scope, local, diagnostics),
        Expr::Block {
            statements, tail, ..
        } => infer_block(statements, tail.as_deref(), scope, local, diagnostics),
        Expr::Control {
            kind,
            binding,
            condition,
            body,
            arms,
            alternate,
            ..
        } => infer_control(
            *kind,
            ControlParts::new(
                binding.as_ref(),
                condition.as_deref(),
                body.as_deref(),
                arms,
                alternate.as_deref(),
            ),
            scope,
            local,
            diagnostics,
        ),
        // The 1.1.0 grammar has no index expression, so `base[index]` has no
        // type rule here; an explicit arm keeps it from being absorbed by a
        // catch-all that would also hide a future control kind.
        Expr::Index { .. } => unsupported_expression(diagnostics),
    }
}

fn infer_name(
    text: &str,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if scope.ambiguous.contains(text) {
        diagnostics.push(diag(
            DIAG_AMBIGUOUS,
            "name has multiple wildcard-import candidates",
        ));
        return Inferred::error();
    }
    let symbol = local.get(text).or_else(|| scope.names.get(text));
    match symbol {
        Some(s) if s.kind == SymbolKind::Table => Inferred {
            ty: Type::Relation(Box::new(s.ty.clone())),
            effects: EffectSummary {
                effects: BTreeSet::from([EffectKind::TableRead]),
                opaque: false,
                may_fail: true,
            },
        },
        Some(s) => Inferred {
            ty: s.ty.clone(),
            effects: s.effects.clone(),
        },
        None => match intrinsic_value_type(text) {
            Some(ty) => Inferred::pure(ty),
            None => {
                let mut diagnostic = diag(
                    if matches!(text, "Ok" | "Err") {
                        DIAG_LEGACY_RESULT
                    } else {
                        DIAG_UNRESOLVED
                    },
                    if matches!(text, "Ok" | "Err") {
                        "Result/Ok/Err control plumbing was removed; return the success type directly"
                    } else {
                        "name cannot be resolved"
                    },
                );
                if !matches!(text, "Ok" | "Err") {
                    diagnostic = diagnostic.with_note(
                        SafeText::new(format!("{UNRESOLVED_NAME_NOTE}{text}"))
                            .expect("identifier text is safe"),
                    );
                }
                diagnostics.push(diagnostic);
                Inferred::error()
            }
        },
    }
}

/// The element fold of a list literal. A list of checkpointed tuples folds
/// transactionally: each later row must reconcile against the accumulated
/// topology instead of being joined pairwise (12-checkpoints). The fold shapes
/// differ in how a rejected parent is recovered, so the mode is explicit
/// instead of a set of booleans read together at every row.
struct ListFold {
    merge: ListMerge,
    element: Option<Type>,
    rejected: bool,
    first_parent: Option<Type>,
}

enum ListMerge {
    /// Ordinary lists join element types pairwise.
    Pairwise,
    /// A transactional fold checked against one topology anchor.
    Checkpoint { topology: Type },
    /// A transactional fold whose rejected nested scopes roll back locally.
    ScopedCheckpoint { topology: Type, rollback: Rollback },
}

/// Where a rejected scoped fold restores its value.
enum Rollback {
    /// Freeze rejected paths at the first row; every later row reuses them.
    ToFirstRow {
        first_row: Type,
        frozen_paths: BTreeSet<Vec<SnapshotTopologyBoundary>>,
    },
    /// Retry every later row against the latest accepted row, which grows into
    /// a checkpoint map as omitted pins are filled.
    ToLatestMap { latest: Type },
}

/// Builds the fold for one list literal's element types, seeding omitted
/// pinned tuple paths before the first row becomes the fold anchor.
fn list_fold(element_types: &mut [Type]) -> ListFold {
    let omitted_pinned_tuple_path =
        type_sequence_has_omitted_pinned_checkpoint_tuple(element_types);
    if omitted_pinned_tuple_path {
        seed_omitted_pinned_checkpoint_tuple_paths(element_types);
    }
    let first_parent = element_types.first().cloned();
    // A transactional fold needs at least three rows: a first row to anchor,
    // a second to establish the topology and a third to cross it.
    let transactional = element_types.len() > 2
        && first_parent.as_ref().is_some_and(|first| {
            type_contains_pinned_checkpoint_tuple(first)
                || (type_contains_omitted_checkpoint_tuple(first)
                    && element_types
                        .iter()
                        .any(type_contains_pinned_checkpoint_tuple))
                || omitted_pinned_tuple_path
        });
    let topology = element_types
        .iter()
        .find(|parent| type_contains_pinned_checkpoint_tuple(parent))
        .cloned()
        .or_else(|| first_parent.clone());
    let merge = match (transactional, topology, first_parent.clone()) {
        (true, Some(topology), Some(first_row)) => {
            // A sparse paired first row has no complete pin map to roll back
            // to. Recover one as the first fully populated row is accepted,
            // then retain the accepted sparse-chain map for later local
            // rollback.
            let paired_omission_recovery = type_contains_paired_checkpoint_omissions(&first_row);
            // A complete anchor, or one concrete lane of a partial first pair,
            // can grow into a recoverable checkpoint map as later rows fill
            // omitted pins. Retain the latest accepted map so a later crossed
            // row cannot erase that sparse-chain identity.
            let first_parent_can_grow_checkpoint_map =
                type_contains_pinned_checkpoint_tuple(&first_row)
                    && (!type_contains_omitted_checkpoint_tuple(&first_row)
                        || type_contains_partially_omitted_pinned_tuple(&first_row));
            // If sparse compaction starts before any tuple pin exists, use the
            // first concrete topology as the recovery point. The reference is
            // silent on local compaction rollback, so keep accepted identities
            // when a later parent conflicts instead of restoring empty slots.
            let checkpoint_reanchor_recovery = paired_omission_recovery
                || (omitted_pinned_tuple_path
                    && (first_parent_can_grow_checkpoint_map
                        || (type_contains_omitted_checkpoint_tuple(&first_row)
                            && type_contains_pinned_checkpoint_tuple(&topology))));
            // Any omitted row can expose a cross-sibling selector collision.
            // Sparse pinned tuples anywhere in the fold need a local scope so
            // a later rebind can roll back only the crossed tuple path.
            let scoped = element_types
                .iter()
                .any(type_contains_omitted_checkpoint_tuple)
                || element_types
                    .iter()
                    .any(type_contains_partially_omitted_pinned_tuple)
                || omitted_pinned_tuple_path
                || element_types
                    .iter()
                    .any(type_contains_paired_pinned_checkpoint_siblings);
            if !scoped {
                ListMerge::Checkpoint { topology }
            } else if checkpoint_reanchor_recovery {
                ListMerge::ScopedCheckpoint {
                    topology,
                    rollback: Rollback::ToLatestMap { latest: first_row },
                }
            } else {
                ListMerge::ScopedCheckpoint {
                    topology,
                    rollback: Rollback::ToFirstRow {
                        first_row,
                        frozen_paths: BTreeSet::new(),
                    },
                }
            }
        }
        _ => ListMerge::Pairwise,
    };
    ListFold {
        merge,
        element: None,
        rejected: false,
        first_parent,
    }
}

impl ListFold {
    /// Folds every row after the first into the element type.
    fn fold(&mut self, element_types: Vec<Type>, diagnostics: &mut Vec<Diagnostic>) {
        for value in element_types {
            let Some(prior) = self.element.take() else {
                self.element = Some(value);
                continue;
            };
            if let Some(merged) = self.merge_parent(&prior, &value, diagnostics) {
                self.element = Some(merged);
            } else {
                require_same(&prior, &value, diagnostics);
                // A transactional fold that cannot reconcile keeps the whole
                // first parent as the recovery anchor in `finish`.
                self.rejected = matches!(
                    &self.merge,
                    ListMerge::Checkpoint { .. } | ListMerge::ScopedCheckpoint { .. }
                );
                self.element = Some(prior);
            }
        }
    }

    /// Reconciles one later row into the accumulated element type.
    fn merge_parent(
        &mut self,
        prior: &Type,
        value: &Type,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Type> {
        match &mut self.merge {
            ListMerge::Pairwise => merge_list_element_types(prior, value),
            ListMerge::Checkpoint { topology } => {
                merge_multi_parent_checkpoint_value(prior, value, topology)
            }
            ListMerge::ScopedCheckpoint { topology, rollback } => {
                let mut parent_rollback_paths = match &*rollback {
                    Rollback::ToFirstRow { frozen_paths, .. } => frozen_paths.clone(),
                    Rollback::ToLatestMap { .. } => BTreeSet::new(),
                };
                let rollback_anchor = match &*rollback {
                    Rollback::ToFirstRow { first_row, .. } => first_row,
                    Rollback::ToLatestMap { latest } => latest,
                };
                let mut fold = merge_multi_parent_checkpoint_value_scoped(
                    prior,
                    value,
                    rollback_anchor,
                    topology,
                    &mut Vec::new(),
                    &mut parent_rollback_paths,
                )?;
                let reanchor = matches!(&*rollback, Rollback::ToLatestMap { .. });
                if fold.rejected_scope {
                    require_same(prior, value, diagnostics);
                    if reanchor {
                        fold.merged =
                            merge_unpaired_checkpoint_record_siblings(&fold.merged, value)
                                .unwrap_or(fold.merged);
                        fold.effective_parent = merge_unpaired_checkpoint_record_siblings(
                            &fold.effective_parent,
                            value,
                        )
                        .unwrap_or(fold.effective_parent);
                    }
                }
                // An omitted tuple path has no identity topology in the
                // original anchor. Remember the first accepted parent's
                // concrete pins at those paths so later parents cannot
                // silently rebind them differently.
                let next_topology =
                    checkpoint_topology_anchor_with_first_pins(topology, &fold.effective_parent);
                *topology = next_topology;
                match rollback {
                    // A re-anchored fold retries later rows against the latest
                    // accepted map; an ordinary scoped fold keeps the paths it
                    // rejected at the first row frozen for the whole fold.
                    Rollback::ToLatestMap { latest } => {
                        if let Some(next) =
                            merge_checkpoint_field_map_multi_parent(latest, &fold.effective_parent)
                        {
                            *latest = next;
                        }
                    }
                    Rollback::ToFirstRow { frozen_paths, .. } => {
                        *frozen_paths = parent_rollback_paths;
                    }
                }
                Some(fold.merged)
            }
        }
    }

    /// The folded element type, after a rejected transactional parent restores
    /// the whole first row.
    fn finish(self, diagnostics: &mut Vec<Diagnostic>) -> Option<Type> {
        let element = if self.rejected {
            self.first_parent
        } else {
            self.element
        };
        if element.is_none() {
            diagnostics.push(diag(
                DIAG_ANNOTATION,
                "empty list needs an expected element type",
            ));
        }
        element
    }
}

fn infer_list(
    elements: &[Expr],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut element_types = Vec::with_capacity(elements.len());
    let mut effects = EffectSummary::default();
    for element in elements {
        let value = infer(element, scope, local, diagnostics);
        effects.join(&value.effects);
        element_types.push(value.ty);
    }
    let mut fold = list_fold(&mut element_types);
    fold.fold(element_types, diagnostics);
    let element = fold.finish(diagnostics).unwrap_or(Type::Error);
    Inferred {
        ty: Type::List(Box::new(element)),
        effects,
    }
}

/// The shape fields shared by every `Expr::Control` form. Grouping them keeps
/// each control form's inference to the parts plus one inference context
/// instead of five positional options behind an allow.
struct ControlParts<'a> {
    binding: Option<&'a Pattern>,
    condition: Option<&'a Expr>,
    body: Option<&'a Expr>,
    arms: &'a [orna_syntax::CaseArm],
    alternate: Option<&'a Expr>,
}

/// Declared payload rows of one enum, by variant name. A payload-free
/// variant has an empty row.
type EnumVariantRows = BTreeMap<String, BTreeMap<String, Type>>;

/// A closed source enum named by a value path, with the static type its
/// constructed values carry.
struct EnumConstructor<'a> {
    ty: Type,
    variants: &'a EnumVariantRows,
}

// The pinned collection source declares each aggregate. A generic export such
// as `sum<T>(rows: [T]): T` takes its element type from the rows; a
// monomorphic export such as `sum(rows: [Decimal]): Decimal` pins the element
// type every call must supply and, with it, the declared result.

/// One finite-list helper call's bound arguments and the values they produced.
struct FiniteListCall<'a> {
    operation: &'a str,
    values: &'a [Option<Type>],
    slots: &'a [Option<usize>],
    arguments: &'a [orna_syntax::Argument],
    scope: &'a Scope,
    local: &'a BTreeMap<String, Symbol>,
}

/// The slot metadata and bound values one statistics call supplies.
struct StatisticsOutcome<'a> {
    operation: &'a str,
    parameters: &'a [&'a str],
    values: &'a [Option<Inferred>],
    scope: &'a Scope,
}

enum DescriptorPathInference {
    Resolved(Inferred),
    Rejected,
}

/// Substitute a selector parameter in a function result with the caller's
/// selector identity. Literal and built-in selectors retain canonical
/// identities. Dynamic arguments use the call occurrence because this pass
/// does not evaluate selector values. Returned function parameters shadow
/// same-named selectors from the caller throughout the returned signature;
/// shadowing SnapshotRef lambda parameters also retain a binder identity so a
/// captured alias is specialized at the depth where it was introduced.
/// The seven inputs are one call site's formal parameters and its arguments,
/// which the two callers read from the same inference frame.
/// The call shape a dynamic result's snapshot contexts specialize against.
#[derive(Clone, Copy)]
struct DynamicParameters<'a> {
    formal_parameters: &'a [Type],
    default_parameters: &'a BTreeSet<usize>,
    parameter_names: Option<&'a [String]>,
    arguments: &'a [orna_syntax::Argument],
    argument_types: &'a [Type],
    local: &'a BTreeMap<String, Symbol>,
    call_span: &'a SyntaxSpan,
    historical_context: Option<&'a Type>,
}

fn specialize_dynamic_parameter_snapshot_contexts(ty: &Type, call: DynamicParameters<'_>) -> Type {
    let DynamicParameters {
        formal_parameters,
        default_parameters,
        parameter_names,
        arguments,
        argument_types,
        local,
        call_span,
        historical_context,
    } = call;
    let mut binder_contexts = BTreeMap::new();
    let nonreturning_argument = argument_types
        .iter()
        .any(contains_nonreturning_aggregate_component);
    let incomplete_argument =
        formal_parameters
            .iter()
            .enumerate()
            .any(|(parameter_index, formal)| {
                let Some(argument_index) =
                    call_argument_index_for_position(parameter_names, parameter_index, arguments)
                else {
                    return !default_parameters.contains(&parameter_index);
                };
                let Some(actual) = argument_types.get(argument_index) else {
                    return true;
                };
                contains_tuple_type(formal)
                    && (contains_type_error(formal)
                        || contains_type_error(actual)
                        || !types_match(formal, actual))
            });
    let paired_direct_binders = formal_parameters
        .iter()
        .filter_map(|formal| snapshot_ref_context_key(formal).and_then(snapshot_ref_binder_id))
        .collect::<BTreeSet<_>>();
    let incomplete_paired_direct_rebind = paired_direct_binders.len() > 1
        && formal_parameters
            .iter()
            .enumerate()
            .any(|(parameter_index, formal)| {
                if snapshot_ref_context_key(formal)
                    .and_then(snapshot_ref_binder_id)
                    .is_none()
                {
                    return false;
                }
                let Some(argument_index) =
                    call_argument_index_for_position(parameter_names, parameter_index, arguments)
                else {
                    return false;
                };
                argument_types
                    .get(argument_index)
                    .is_none_or(|actual| !is_contextual_snapshot_ref(actual))
            });
    // The reference specifies pin identity but leaves recovery after a call
    // that cannot reach the callee unspecified. Treat its binding wave
    // transactionally:
    // a malformed tuple at any depth in an argument signature, or any missing
    // required argument, must not rebind otherwise valid siblings captured by
    // the returned callable. Paired direct pin parameters share that wave too;
    // an unknown sibling must not promote the other pin alone. Omitted defaults
    // are valid.
    let suppress_rebinding =
        nonreturning_argument || incomplete_argument || incomplete_paired_direct_rebind;
    if !suppress_rebinding {
        for (parameter_index, formal) in formal_parameters.iter().enumerate() {
            let Some(argument_index) =
                call_argument_index_for_position(parameter_names, parameter_index, arguments)
            else {
                continue;
            };
            let Some(actual) = argument_types.get(argument_index) else {
                continue;
            };
            // A destructured parameter is one callback argument. Do not partially
            // bind its pins if recovery or shape checking found an invalid leaf.
            if matches!(formal, Type::Tuple(_))
                && (contains_type_error(formal)
                    || contains_type_error(actual)
                    || !types_match(formal, actual))
            {
                continue;
            }
            collect_snapshot_binder_contexts(formal, actual, &mut binder_contexts);
        }
    }

    // A call with a non-returning argument, malformed tuple, or missing required
    // argument never reaches the callee. Keep its result symbolic instead of
    // leaking sibling pins.
    let (parameter_names, arguments, argument_types) = if suppress_rebinding {
        (None, &[][..], &[][..])
    } else {
        (parameter_names, arguments, argument_types)
    };
    let scoped_call = SpecializationCall {
        parameter_names,
        arguments,
        argument_types,
        local,
        call_span,
        historical_context,
    };
    specialize_dynamic_parameter_snapshot_contexts_scoped(
        ty,
        &scoped_call,
        &BTreeSet::new(),
        &binder_contexts,
    )
}

/// The call inputs every scoped recursion needs, so the two scope inputs that
/// vary per node stand alone.
#[derive(Clone, Copy)]
struct SpecializationCall<'a> {
    parameter_names: Option<&'a [String]>,
    arguments: &'a [orna_syntax::Argument],
    argument_types: &'a [Type],
    local: &'a BTreeMap<String, Symbol>,
    call_span: &'a SyntaxSpan,
    historical_context: Option<&'a Type>,
}

struct ScopedCheckpointFold {
    merged: Type,
    effective_parent: Type,
    rejected_scope: bool,
}

/// Sparse parent folds may contribute different counts of selector labels to
/// otherwise paired tuple slots or sibling record fields. Keep paired paths
/// stable, but let singleton labels remain in the concrete lane where each
/// sparse source supplied them. The enclosing compaction guard still rejects
/// new cross-path identity. The reference does not specify these local sparse
/// multi-parent folds.
/// A paired snapshot shape: the expected and actual boundary sets, counted.
type PairedShapeCounts = BTreeMap<
    BTreeSet<(
        Vec<SnapshotTopologyBoundary>,
        BTreeSet<SnapshotTopologyBoundary>,
    )>,
    usize,
>;

/// Structural boundaries for checkpoint selector maps. ORNA-CP-003 exposes
/// historical checkpoint selection and ORNA-SYS-136 resolves explicit
/// selectors, but neither specifies local record/tuple folds; preserve the
/// complete boundary path as the conservative local rule.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SnapshotTopologyBoundary {
    ListElement,
    RangeElement,
    RelationElement,
    StreamElement,
    OptionalValue,
    TupleElement(usize),
    RecordField(String),
    FunctionParameter(usize),
    FunctionResult,
    AppliedArgument { base: String, index: usize },
    MoneyCurrency,
    MoneyUnit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SnapshotContextMapPair {
    boundary_path: Vec<SnapshotTopologyBoundary>,
    expected: BTreeSet<String>,
    actual: BTreeSet<String>,
}

fn tuple_pin_identity_topology_matches(expected: &[Type], actual: &[Type]) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    let mut pin_maps = Vec::new();
    let mut path = Vec::new();
    for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
        if !collect_corresponding_snapshot_context_maps_at_boundary(
            expected,
            actual,
            &mut pin_maps,
            &mut path,
            SnapshotTopologyBoundary::TupleElement(index),
        ) {
            return false;
        }
    }

    snapshot_context_topology_matches(&pin_maps)
}

/// Record fields form sibling slots just like tuple positions. A rebind may
/// rename concrete selectors, but it must preserve which fields share a
/// captured pin across every nested depth. The reference is silent about this
/// local record-rebind edge, so use the same conservative topology rule as
/// tuple rebinding rather than allowing equal-width fields to split or merge.
fn record_pin_identity_topology_matches(
    expected: &BTreeMap<String, Type>,
    actual: &BTreeMap<String, Type>,
) -> bool {
    let mut pin_maps = Vec::new();
    let mut path = Vec::new();
    for (name, expected) in expected {
        let Some(actual) = actual.get(name) else {
            return false;
        };
        if !collect_corresponding_snapshot_context_maps_at_boundary(
            expected,
            actual,
            &mut pin_maps,
            &mut path,
            SnapshotTopologyBoundary::RecordField(name.clone()),
        ) {
            return false;
        }
    }

    snapshot_context_topology_matches(&pin_maps)
}

fn snapshot_context_topology_matches(pin_maps: &[SnapshotContextMapPair]) -> bool {
    fn membership_signatures(
        pin_maps: &[SnapshotContextMapPair],
        expected: bool,
    ) -> BTreeMap<Vec<Vec<SnapshotTopologyBoundary>>, usize> {
        let mut labels = BTreeMap::<String, BTreeSet<Vec<SnapshotTopologyBoundary>>>::new();
        for pair in pin_maps {
            let selectors = if expected {
                &pair.expected
            } else {
                &pair.actual
            };
            for selector in selectors {
                labels
                    .entry(selector.clone())
                    .or_default()
                    .insert(pair.boundary_path.clone());
            }
        }

        let mut signatures = BTreeMap::new();
        for membership in labels.into_values() {
            let membership = membership.into_iter().collect::<Vec<_>>();
            *signatures.entry(membership).or_insert(0) += 1;
        }
        signatures
    }

    pin_maps
        .iter()
        .all(|pair| pair.expected.len() == pair.actual.len())
        && membership_signatures(pin_maps, true) == membership_signatures(pin_maps, false)
}

fn collect_corresponding_snapshot_context_maps_at_boundary(
    expected: &Type,
    actual: &Type,
    into: &mut Vec<SnapshotContextMapPair>,
    path: &mut Vec<SnapshotTopologyBoundary>,
    boundary: SnapshotTopologyBoundary,
) -> bool {
    path.push(boundary);
    let matched = collect_corresponding_snapshot_context_maps_at_path(expected, actual, into, path);
    path.pop();
    matched
}

fn collect_corresponding_snapshot_context_maps_at_path(
    expected: &Type,
    actual: &Type,
    into: &mut Vec<SnapshotContextMapPair>,
    path: &mut Vec<SnapshotTopologyBoundary>,
) -> bool {
    // Generic SnapshotRef formals have no concrete checkpoint identity to
    // preserve. A Bottom on one side of a compaction fold is different: its
    // sibling slot still exists, so retain an empty map for that side. The
    // empty map lets the fold detect when a live pin from the other row would
    // make two previously distinct depth labels overlap.
    if matches!(expected, Type::Bottom) && matches!(actual, Type::Bottom) {
        return true;
    }
    if matches!(expected, Type::Bottom) {
        return collect_snapshot_context_maps_from_omitted_value_at_path(actual, true, into, path);
    }
    if matches!(actual, Type::Bottom) {
        return collect_snapshot_context_maps_from_omitted_value_at_path(
            expected, false, into, path,
        );
    }
    if expected == &Type::Named("sys.SnapshotRef".into())
        || actual == &Type::Named("sys.SnapshotRef".into())
    {
        return true;
    }
    if is_contextual_snapshot_ref(expected) || is_contextual_snapshot_ref(actual) {
        let (
            Type::Applied {
                arguments: expected,
                ..
            },
            Type::Applied {
                arguments: actual, ..
            },
        ) = (expected, actual)
        else {
            return false;
        };
        let (Some(Type::Named(expected)), Some(Type::Named(actual))) =
            (expected.first(), actual.first())
        else {
            return false;
        };
        into.push(SnapshotContextMapPair {
            boundary_path: path.clone(),
            expected: BTreeSet::from([expected.clone()]),
            actual: BTreeSet::from([actual.clone()]),
        });
        return true;
    }
    if is_snapshot_context_map_shape(expected) || is_snapshot_context_map_shape(actual) {
        let (
            Type::Applied {
                arguments: expected,
                ..
            },
            Type::Applied {
                arguments: actual, ..
            },
        ) = (expected, actual)
        else {
            return false;
        };
        let selectors = |arguments: &[Type]| {
            arguments
                .iter()
                .filter_map(|argument| match argument {
                    Type::Named(selector) => Some(selector.clone()),
                    _ => None,
                })
                .collect::<BTreeSet<_>>()
        };
        let expected = selectors(expected);
        let actual = selectors(actual);
        into.push(SnapshotContextMapPair {
            boundary_path: path.clone(),
            expected,
            actual,
        });
        return true;
    }

    match (expected, actual) {
        (Type::List(expected), Type::List(actual)) => {
            collect_corresponding_snapshot_context_maps_at_boundary(
                expected,
                actual,
                into,
                path,
                SnapshotTopologyBoundary::ListElement,
            )
        }
        (Type::Range(expected), Type::Range(actual)) => {
            collect_corresponding_snapshot_context_maps_at_boundary(
                expected,
                actual,
                into,
                path,
                SnapshotTopologyBoundary::RangeElement,
            )
        }
        (Type::Relation(expected), Type::Relation(actual)) => {
            collect_corresponding_snapshot_context_maps_at_boundary(
                expected,
                actual,
                into,
                path,
                SnapshotTopologyBoundary::RelationElement,
            )
        }
        (Type::Stream(expected), Type::Stream(actual)) => {
            collect_corresponding_snapshot_context_maps_at_boundary(
                expected,
                actual,
                into,
                path,
                SnapshotTopologyBoundary::StreamElement,
            )
        }
        (Type::Optional(expected), Type::Optional(actual)) => {
            collect_corresponding_snapshot_context_maps_at_boundary(
                expected,
                actual,
                into,
                path,
                SnapshotTopologyBoundary::OptionalValue,
            )
        }
        (Type::Tuple(expected), Type::Tuple(actual)) if expected.len() == actual.len() => expected
            .iter()
            .zip(actual)
            .enumerate()
            .all(|(index, (expected, actual))| {
                collect_corresponding_snapshot_context_maps_at_boundary(
                    expected,
                    actual,
                    into,
                    path,
                    SnapshotTopologyBoundary::TupleElement(index),
                )
            }),
        (Type::Record(expected), Type::Record(actual)) if expected.len() == actual.len() => {
            expected.iter().all(|(name, expected)| {
                actual.get(name).is_some_and(|actual| {
                    collect_corresponding_snapshot_context_maps_at_boundary(
                        expected,
                        actual,
                        into,
                        path,
                        SnapshotTopologyBoundary::RecordField(name.clone()),
                    )
                })
            })
        }
        (
            Type::Function {
                parameters: expected_parameters,
                result: expected_result,
                ..
            },
            Type::Function {
                parameters: actual_parameters,
                result: actual_result,
                ..
            },
        ) if expected_parameters.len() == actual_parameters.len() => {
            expected_parameters
                .iter()
                .zip(actual_parameters)
                .enumerate()
                .all(|(index, (expected, actual))| {
                    collect_corresponding_snapshot_context_maps_at_boundary(
                        expected,
                        actual,
                        into,
                        path,
                        SnapshotTopologyBoundary::FunctionParameter(index),
                    )
                })
                && collect_corresponding_snapshot_context_maps_at_boundary(
                    expected_result,
                    actual_result,
                    into,
                    path,
                    SnapshotTopologyBoundary::FunctionResult,
                )
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
        ) if expected_base == actual_base && expected_arguments.len() == actual_arguments.len() => {
            expected_arguments
                .iter()
                .zip(actual_arguments)
                .enumerate()
                .all(|(index, (expected, actual))| {
                    collect_corresponding_snapshot_context_maps_at_boundary(
                        expected,
                        actual,
                        into,
                        path,
                        SnapshotTopologyBoundary::AppliedArgument {
                            base: expected_base.clone(),
                            index,
                        },
                    )
                })
        }
        (
            Type::MoneyPerUnit {
                currency: expected_currency,
                unit: expected_unit,
            },
            Type::MoneyPerUnit {
                currency: actual_currency,
                unit: actual_unit,
            },
        ) => {
            collect_corresponding_snapshot_context_maps_at_boundary(
                expected_currency,
                actual_currency,
                into,
                path,
                SnapshotTopologyBoundary::MoneyCurrency,
            ) && collect_corresponding_snapshot_context_maps_at_boundary(
                expected_unit,
                actual_unit,
                into,
                path,
                SnapshotTopologyBoundary::MoneyUnit,
            )
        }
        _ => true,
    }
}

fn collect_snapshot_context_maps_from_omitted_value_at_path(
    value: &Type,
    omitted_on_left: bool,
    into: &mut Vec<SnapshotContextMapPair>,
    path: &mut Vec<SnapshotTopologyBoundary>,
) -> bool {
    if matches!(value, Type::Bottom) || value == &Type::Named("sys.SnapshotRef".into()) {
        return true;
    }
    if is_contextual_snapshot_ref(value) || is_snapshot_context_map_shape(value) {
        let Type::Applied { arguments, .. } = value else {
            return false;
        };
        let selectors = arguments
            .iter()
            .filter_map(|argument| match argument {
                Type::Named(selector) => Some(selector.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        if selectors.is_empty() {
            return false;
        }
        let empty = BTreeSet::new();
        let (expected, actual) = if omitted_on_left {
            (empty, selectors)
        } else {
            (selectors, empty)
        };
        into.push(SnapshotContextMapPair {
            boundary_path: path.clone(),
            expected,
            actual,
        });
        return true;
    }

    match value {
        Type::List(value) => collect_omitted_at_boundary(
            value,
            omitted_on_left,
            into,
            path,
            SnapshotTopologyBoundary::ListElement,
        ),
        Type::Range(value) => collect_omitted_at_boundary(
            value,
            omitted_on_left,
            into,
            path,
            SnapshotTopologyBoundary::RangeElement,
        ),
        Type::Relation(value) => collect_omitted_at_boundary(
            value,
            omitted_on_left,
            into,
            path,
            SnapshotTopologyBoundary::RelationElement,
        ),
        Type::Stream(value) => collect_omitted_at_boundary(
            value,
            omitted_on_left,
            into,
            path,
            SnapshotTopologyBoundary::StreamElement,
        ),
        Type::Optional(value) => collect_omitted_at_boundary(
            value,
            omitted_on_left,
            into,
            path,
            SnapshotTopologyBoundary::OptionalValue,
        ),
        Type::Record(fields) => fields.iter().all(|(name, value)| {
            collect_omitted_at_boundary(
                value,
                omitted_on_left,
                into,
                path,
                SnapshotTopologyBoundary::RecordField(name.clone()),
            )
        }),
        Type::Tuple(elements) => elements.iter().enumerate().all(|(index, value)| {
            collect_omitted_at_boundary(
                value,
                omitted_on_left,
                into,
                path,
                SnapshotTopologyBoundary::TupleElement(index),
            )
        }),
        Type::Applied { base, arguments } => arguments.iter().enumerate().all(|(index, value)| {
            collect_omitted_at_boundary(
                value,
                omitted_on_left,
                into,
                path,
                SnapshotTopologyBoundary::AppliedArgument {
                    base: base.clone(),
                    index,
                },
            )
        }),
        Type::Function {
            parameters, result, ..
        } => {
            parameters.iter().enumerate().all(|(index, value)| {
                collect_omitted_at_boundary(
                    value,
                    omitted_on_left,
                    into,
                    path,
                    SnapshotTopologyBoundary::FunctionParameter(index),
                )
            }) && collect_omitted_at_boundary(
                result,
                omitted_on_left,
                into,
                path,
                SnapshotTopologyBoundary::FunctionResult,
            )
        }
        Type::MoneyPerUnit { currency, unit } => {
            collect_omitted_at_boundary(
                currency,
                omitted_on_left,
                into,
                path,
                SnapshotTopologyBoundary::MoneyCurrency,
            ) && collect_omitted_at_boundary(
                unit,
                omitted_on_left,
                into,
                path,
                SnapshotTopologyBoundary::MoneyUnit,
            )
        }
        _ => true,
    }
}

fn collect_omitted_at_boundary(
    value: &Type,
    omitted_on_left: bool,
    into: &mut Vec<SnapshotContextMapPair>,
    path: &mut Vec<SnapshotTopologyBoundary>,
    boundary: SnapshotTopologyBoundary,
) -> bool {
    path.push(boundary);
    let collected = collect_snapshot_context_maps_from_omitted_value_at_path(
        value,
        omitted_on_left,
        into,
        path,
    );
    path.pop();
    collected
}

/// Selector identities may change during a local rebind, but the number of
/// captured selector slots is part of the pinned shape. The reference is
/// silent about local map rebinds; preserving cardinality avoids silently
/// adding or dropping captured pins.
fn snapshot_context_map_cardinality(ty: &Type) -> Option<usize> {
    match ty {
        Type::Applied { base, arguments }
            if base == "sys.SnapshotRefContext" || base == "semantic.SnapshotContextMap" =>
        {
            Some(arguments.len())
        }
        _ => None,
    }
}

/// A pin set may narrow across a rebind only by dropping selectors that the
/// wider side already pins. Growth stays a width mismatch.
fn snapshot_context_selectors_narrow(wide: &Type, narrow: &Type) -> bool {
    let (Some(wide_names), Some(narrow_names)) = (
        snapshot_context_selector_names(wide),
        snapshot_context_selector_names(narrow),
    ) else {
        return false;
    };
    narrow_names.len() < wide_names.len()
        && narrow_names
            .iter()
            .all(|selector| wide_names.contains(selector))
}

fn snapshot_context_selector_names(ty: &Type) -> Option<Vec<&str>> {
    match ty {
        Type::Applied { base, arguments }
            if base == "sys.SnapshotRefContext" || base == "semantic.SnapshotContextMap" =>
        {
            arguments
                .iter()
                .map(|argument| match argument {
                    Type::Named(selector) => Some(selector.as_str()),
                    _ => None,
                })
                .collect()
        }
        _ => None,
    }
}

fn is_snapshot_context_map_shape(ty: &Type) -> bool {
    match ty {
        Type::Applied { base, arguments } if base == "sys.SnapshotRefContext" => {
            is_contextual_snapshot_ref(ty)
        }
        Type::Applied { base, arguments } if base == "semantic.SnapshotContextMap" => {
            // SnapshotContextMap is the canonical encoding of a set with at
            // least two selectors; singleton sets use SnapshotRefContext.
            // Require sorted unique entries so repeated pairwise merges do
            // not hide a non-canonical map behind equality.
            arguments.len() > 1
                && arguments
                    .iter()
                    .all(|argument| {
                        matches!(argument, Type::Named(selector) if is_snapshot_selector_context(selector))
                    })
                && arguments.windows(2).all(|pair| {
                    matches!(pair, [Type::Named(left), Type::Named(right)] if left < right)
                })
        }
        _ => false,
    }
}

/// Rebind callable values returned through a historical function to the
/// selected snapshot context, including callables nested in simple products.
fn bind_historical_closure_context(snapshot: &Type, ty: &Type) -> Type {
    match ty {
        Type::Function {
            parameters,
            parameter_names,
            default_parameters,
            result,
        } => historical_callable_type(
            snapshot,
            &Type::Function {
                parameters: parameters.clone(),
                parameter_names: parameter_names.clone(),
                default_parameters: default_parameters.clone(),
                result: Box::new(bind_historical_closure_context(snapshot, result)),
            },
        ),
        Type::List(element) => {
            Type::List(Box::new(bind_historical_closure_context(snapshot, element)))
        }
        Type::Relation(element) => {
            Type::Relation(Box::new(bind_historical_closure_context(snapshot, element)))
        }
        Type::Stream(element) => {
            Type::Stream(Box::new(bind_historical_closure_context(snapshot, element)))
        }
        Type::Optional(element) => {
            Type::Optional(Box::new(bind_historical_closure_context(snapshot, element)))
        }
        Type::Record(fields) => Type::Record(
            fields
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        bind_historical_closure_context(snapshot, value),
                    )
                })
                .collect(),
        ),
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| bind_historical_closure_context(snapshot, element))
                .collect(),
        ),
        Type::Applied { base, arguments } if base != "sys.HistoricalCallable" => Type::Applied {
            base: base.clone(),
            arguments: arguments
                .iter()
                .map(|argument| bind_historical_closure_context(snapshot, argument))
                .collect(),
        },
        _ => ty.clone(),
    }
}

fn historical_namespace_parts(ty: &Type) -> Option<(&Type, &str)> {
    let Type::Applied { base, arguments } = ty else {
        return None;
    };
    (base == "sys.HistoricalNamespace")
        .then_some(match arguments.as_slice() {
            [snapshot, Type::Named(namespace)] => Some((snapshot, namespace.as_str())),
            _ => None,
        })
        .flatten()
}

fn historical_root_available(modules: &BTreeMap<Namespace, ModuleHeader>, root: &str) -> bool {
    modules
        .keys()
        .any(|namespace| namespace.0.first().is_some_and(|part| part == root))
}

fn infer_historical_member(
    base: &Type,
    name: &str,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    if let Type::Applied {
        base: base_name,
        arguments,
    } = base
        && base_name == "sys.DatabaseSnapshot"
        && let [snapshot] = arguments.as_slice()
    {
        let namespace = name.to_owned();
        if historical_root_available(&scope.historical_modules, &namespace) || name == "sys" {
            return Some(Inferred::pure(historical_namespace_type(
                snapshot, &namespace,
            )));
        }
        diagnostics.push(diag(
            DIAG_UNRESOLVED,
            "historical database member cannot be resolved",
        ));
        return Some(Inferred::error());
    }
    let (snapshot, namespace) = historical_namespace_parts(base)?;
    let module_namespace = Namespace(
        namespace
            .split('.')
            .chain(std::iter::once(name))
            .map(str::to_owned)
            .collect(),
    );
    if scope.historical_modules.contains_key(&module_namespace) {
        return Some(Inferred::pure(historical_namespace_type(
            snapshot,
            &module_namespace.display(),
        )));
    }

    // The portable system surface is descriptor-backed rather than a source
    // ModuleHeader.  Project only relations explicitly present in api/sys.json
    // and keep unknown historical system members unresolved. Reading one
    // through a database snapshot is its historical view, which needs its own
    // linked provider.
    if namespace == "sys" {
        let relation = format!("sys.{name}");
        if system_api::embedded_system_api()
            .relation(&relation)
            .is_some()
        {
            require_linked_system_member(
                system_api::SystemMember::HistoricalView(&relation),
                scope,
            );
            return Some(Inferred {
                ty: historical_table_type(snapshot, &relation),
                effects: EffectSummary {
                    effects: BTreeSet::from([EffectKind::TableRead]),
                    opaque: false,
                    may_fail: true,
                },
            });
        }
    }

    let module = scope.historical_modules.get(&Namespace(
        namespace.split('.').map(str::to_owned).collect(),
    ));
    if let Some(symbol) = module.and_then(|module| module.exports.get(name)) {
        let full_name = if namespace.is_empty() {
            name.to_owned()
        } else {
            format!("{namespace}.{name}")
        };
        if symbol.kind == SymbolKind::Table {
            return Some(Inferred {
                ty: historical_table_type(snapshot, &full_name),
                effects: EffectSummary {
                    effects: BTreeSet::from([EffectKind::TableRead]),
                    opaque: false,
                    may_fail: true,
                },
            });
        }
        if symbol.kind == SymbolKind::Function
            && (symbol.effects.opaque
                || symbol
                    .effects
                    .effects
                    .iter()
                    .any(|effect| *effect != EffectKind::TableRead))
        {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "historical callable permits only database-read effects",
            ));
            return Some(Inferred::error());
        }
        return Some(Inferred {
            ty: if symbol.kind == SymbolKind::Function {
                bind_historical_closure_context(snapshot, &symbol.ty)
            } else {
                symbol.ty.clone()
            },
            effects: {
                let mut effects = symbol.effects.clone();
                effects.effects.insert(EffectKind::TableRead);
                effects.may_fail = true;
                effects
            },
        });
    }
    diagnostics.push(diag(
        DIAG_UNRESOLVED,
        "historical database member cannot be resolved",
    ));
    Some(Inferred::error())
}

fn infer_system_member(base: &Type, name: &str) -> Option<Type> {
    let api = system_api::embedded_system_api();
    match base {
        Type::Named(system_type) => descriptor_member_type(system_type, name, &BTreeMap::new()),
        Type::Applied {
            base: system_type,
            arguments,
        } => {
            let parameters = api.type_parameters(system_type)?;
            if parameters.len() != arguments.len() {
                return None;
            }
            let substitutions = parameters
                .iter()
                .cloned()
                .zip(arguments.iter().cloned())
                .collect::<BTreeMap<_, _>>();
            descriptor_member_type(system_type, name, &substitutions)
        }
        _ => None,
    }
}

fn descriptor_member_type(
    system_type: &str,
    member: &str,
    substitutions: &BTreeMap<String, Type>,
) -> Option<Type> {
    let field = system_api::embedded_system_api().field(system_type, member)?;
    descriptor_type_with_substitutions(field, substitutions)
}

/// A described member keeps the nominal identity of its descriptor type, so a
/// read such as `checkpoint.position` agrees with a `sys.CheckpointPosition`
/// annotation or signature parameter (ORNA-NOMINAL-002), and an opaque
/// `sys.Value` is never unboxed into a structural record. Chained reads resolve
/// each step through `infer_system_member`; only type parameters substitute.
fn descriptor_type_with_substitutions(
    ty: &system_api::SystemType,
    substitutions: &BTreeMap<String, Type>,
) -> Option<Type> {
    match ty {
        system_api::SystemType::Named(name) => Some(
            substitutions
                .get(name)
                .cloned()
                .unwrap_or_else(|| descriptor_type(ty)),
        ),
        system_api::SystemType::Applied { base, arguments }
            if matches!(base.as_str(), "Relation" | "Query") =>
        {
            Some(Type::Relation(Box::new(
                descriptor_type_with_substitutions(arguments.first()?, substitutions)?,
            )))
        }
        system_api::SystemType::Applied { base, arguments } if base == "Stream" => {
            Some(Type::Stream(Box::new(descriptor_type_with_substitutions(
                arguments.first()?,
                substitutions,
            )?)))
        }
        system_api::SystemType::Applied { base, arguments } => Some(Type::Applied {
            base: base.clone(),
            arguments: arguments
                .iter()
                .map(|argument| descriptor_type_with_substitutions(argument, substitutions))
                .collect::<Option<Vec<_>>>()?,
        }),
        system_api::SystemType::List(element) => Some(Type::List(Box::new(
            descriptor_type_with_substitutions(element, substitutions)?,
        ))),
        system_api::SystemType::Optional(element) => Some(Type::Optional(Box::new(
            descriptor_type_with_substitutions(element, substitutions)?,
        ))),
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
    let Type::Named(name) = relation_element_type(base) else {
        return None;
    };
    match (name.as_str(), member) {
        ("sys.Checkpoint", "reset") => Some(
            "system rows are read-only; use `sys.admin.reset_checkpoint` with compare-and-set arguments",
        ),
        ("sys.Failure", "replay") => Some(
            "system rows are read-only; use `sys.admin.replay_failure(failure.reference, ...)`",
        ),
        ("sys.Failure", "resolve") => Some(
            "system rows are read-only; use `sys.admin.resolve_failure(failure.reference, ...)`",
        ),
        ("sys.Stream", "retry") => {
            Some("system rows are read-only; use `sys.admin.retry_failure` on a `sys.FailureRef`")
        }
        ("sys.Stream", "skip") => {
            Some("system rows are read-only; use `sys.admin.skip_failure` on a `sys.FailureRef`")
        }
        _ => None,
    }
}

fn relation_element_type(ty: &Type) -> &Type {
    match ty {
        Type::Relation(element) => element,
        _ => ty,
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

/// `decimal_expression.CurrencyType` constructs exact `Money<CurrencyType>`
/// only when that nominal type implements `Currency` (ORNA-LIT-007,
/// ORNA-MONEY-010). A binary `Float` never becomes money (ORNA-NUM-003).
fn infer_currency_suffix(
    base: &Type,
    name: &str,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Type> {
    if !matches!(base, Type::Int | Type::Decimal | Type::Float) {
        return None;
    }
    let identity = canonical_nominal_name(name, scope);
    if scope.currency_types.contains(&identity) {
        if *base == Type::Float {
            diagnostics.push(diag(
                DIAG_TYPE,
                "Money cannot be constructed from an inexact Float without explicit rounding",
            ));
            return Some(Type::Error);
        }
        return Some(Type::Applied {
            base: "Money".into(),
            arguments: vec![Type::Named(identity)],
        });
    }
    if scope.nominal_identities.contains_key(name) {
        diagnostics.push(diag(
            DIAG_TYPE,
            "currency suffix names a type that does not implement Currency",
        ));
        return Some(Type::Error);
    }
    None
}

fn infer_numeric_postfix(base: &Type, name: &str) -> Option<Type> {
    match base {
        Type::Int | Type::Decimal | Type::Float if name == "decimal" => Some(Type::Decimal),
        _ => None,
    }
}

/// Elapsed `Duration` suffixes of the root intrinsic environment, such as
/// `now() - 1.hour`; the evaluator constructs these values directly.
const INTRINSIC_DURATION_SUFFIXES: &[&str] = &[
    "hour", "hours", "minute", "minutes", "min", "second", "seconds", "s",
];

/// Currency mismatch for addition; time-versus-energy is reported by the
/// dimension check (ORNA-UNIT-004).
fn incompatible_addition_message(
    left: &Type,
    right: &Type,
) -> Option<(&'static str, &'static str)> {
    if let (Some(left), Some(right)) = (money_currency(left), money_currency(right))
        && left != right
    {
        return Some((
            DIAG_CURRENCY_MISMATCH,
            "cannot add different currencies without conversion",
        ));
    }
    None
}

/// Calendar-day period suffixes of the root intrinsic environment, such as
/// `bucket_by(1.day, zone: ...)`; a period is distinct from an elapsed
/// `Duration` (ORNA-TIME-004).
const INTRINSIC_PERIOD_SUFFIXES: &[&str] = &["day", "days"];

/// Types `value.unit` on a numeric value (ORNA-UNIT-008). A unit visible
/// through a declaration or import gives a structural quantity, and naming
/// the unit a quantity already has keeps it; a suffix never converts. An
/// unshadowed intrinsic temporal suffix gives an elapsed `Duration` or a
/// calendar-day period. Any other suffix is rejected: a unit is never
/// inferred from its spelling.
fn infer_unit_suffix(
    base: &Type,
    name: &str,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Type> {
    let numeric = match base {
        Type::Int | Type::Decimal | Type::Float => base,
        Type::Quantity { numeric, .. } => numeric.as_ref(),
        _ => return None,
    };
    match lookup_unit(&[name.to_owned()], scope) {
        UnitLookup::Declared(unit) => {
            let quantity = Type::Quantity {
                numeric: Box::new(numeric.clone()),
                dimension_basis: unit.dimension_basis.clone(),
                scale: unit.scale.clone(),
                offset: unit.offset.clone(),
                affine: unit.affine,
            };
            if matches!(base, Type::Quantity { .. }) && *base != quantity {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "a unit suffix does not convert a quantity to another unit",
                ));
                return Some(Type::Error);
            }
            Some(quantity)
        }
        UnitLookup::Ambiguous => {
            diagnostics.push(diag(DIAG_AMBIGUOUS, AMBIGUOUS_UNIT));
            Some(Type::Error)
        }
        UnitLookup::Unresolved => match base {
            Type::Int | Type::Decimal if INTRINSIC_DURATION_SUFFIXES.contains(&name) => {
                Some(Type::Named("std.DURATION".into()))
            }
            Type::Int if INTRINSIC_PERIOD_SUFFIXES.contains(&name) => Some(Type::Applied {
                base: "Int".into(),
                arguments: vec![Type::Named(name.into())],
            }),
            _ => {
                diagnostics.push(diag(DIAG_UNRESOLVED, "unit suffix cannot be resolved"));
                Some(Type::Error)
            }
        },
    }
}

/// A calendar-day period written `n.day` or `n.days`.
fn is_calendar_period(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Applied { base, arguments }
            if base == "Int"
                && matches!(
                    arguments.as_slice(),
                    [Type::Named(unit)] if INTRINSIC_PERIOD_SUFFIXES.contains(&unit.as_str())
                )
    )
}

fn money_currency(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Applied { base, arguments } if base == "Money" => arguments.first(),
        _ => None,
    }
}

/// An absolute value of an affine unit such as `C` (ORNA-UNIT-005).
fn is_absolute_affine(ty: &Type) -> bool {
    matches!(ty, Type::Quantity { affine: true, .. })
}

/// `Decimal<kWh> * (Money<GBP> / kWh)` reduces to `Money<GBP>`
/// (ORNA-MONEY-003): an exact quantity whose dimensions match the rate's unit
/// leaves only the currency. Units of one dimension with different scales
/// reduce when their exact ratio is a finite decimal, so no rounding is
/// implied.
fn reduce_exact_money_rate(left: &Type, right: &Type) -> Option<Type> {
    let ((
        Type::Quantity {
            numeric,
            dimension_basis,
            scale,
            affine: false,
            ..
        },
        Type::MoneyPerUnit { currency, unit },
    )
    | (
        Type::MoneyPerUnit { currency, unit },
        Type::Quantity {
            numeric,
            dimension_basis,
            scale,
            affine: false,
            ..
        },
    )) = (left, right)
    else {
        return None;
    };
    let Type::Quantity {
        dimension_basis: rate_basis,
        scale: rate_scale,
        affine: false,
        ..
    } = unit.as_ref()
    else {
        return None;
    };
    if !matches!(numeric.as_ref(), Type::Int | Type::Decimal) || dimension_basis != rate_basis {
        return None;
    }
    let ratio = unit_fraction(scale)?.divide(&unit_fraction(rate_scale)?)?;
    is_finite_decimal(&ratio).then(|| Type::Applied {
        base: "Money".into(),
        arguments: vec![currency.as_ref().clone()],
    })
}

/// Whether an exact ratio has a finite decimal expansion.
fn is_finite_decimal(value: &UnitFraction) -> bool {
    let mut denominator = value.denominator.clone();
    for factor in [BigInt::from(2u8), BigInt::from(5u8)] {
        while (&denominator % &factor).is_zero() {
            denominator /= &factor;
        }
    }
    denominator.is_one()
}

fn is_binary_float(ty: &Type) -> bool {
    matches!(ty, Type::Float)
        || matches!(ty, Type::Applied { base, .. } if base == "Float")
        || matches!(ty, Type::Quantity { numeric, .. } if **numeric == Type::Float)
}

fn money_constructor_currency(expr: &Expr) -> Option<&str> {
    let Expr::Name { text, .. } = expr else {
        return None;
    };
    let currency = text.strip_prefix("Money<")?.strip_suffix('>')?;
    (!currency.is_empty() && !currency.contains(',')).then_some(currency)
}

fn money_generic_constructor_currency(
    expr: &Expr,
    type_arguments: &[TypeExpr],
    scope: &Scope,
) -> Option<Type> {
    if !matches!(expr, Expr::Name { text, .. } if text == "Money") || type_arguments.len() != 1 {
        return None;
    }
    match resolved_type_of(&type_arguments[0], scope) {
        Type::Named(currency) if !currency.is_empty() => Some(Type::Named(currency)),
        _ => None,
    }
}

fn infer_money_constructor(
    currency: Type,
    arguments: &[orna_syntax::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    if arguments.len() != 1 || arguments[0].name.is_some() {
        return None;
    }
    let value = infer(&arguments[0].value, scope, local, diagnostics);
    let mut inferred = infer_money_constructor_type(currency, &value.ty, scope, diagnostics);
    inferred.effects = value.effects;
    Some(inferred)
}

/// `Money<C>(value)` constructs exact money from an exact value whose currency
/// `C` implements `Currency` (ORNA-MONEY-004, ORNA-NUM-003).
fn infer_money_constructor_type(
    currency: Type,
    value: &Type,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let currency_is_valid = is_currency_type(&currency, scope);
    if !currency_is_valid {
        diagnostics.push(diag(
            DIAG_TYPE,
            "Money currency must be a nominal type that implements Currency",
        ));
    }
    let value_is_float = is_binary_float(value);
    if value_is_float {
        diagnostics.push(diag(
            DIAG_MONEY_FROM_FLOAT,
            "Money cannot be constructed from an inexact Float without explicit rounding",
        ));
    }
    if !currency_is_valid || value_is_float {
        return Inferred::error();
    }
    Inferred::pure(Type::Applied {
        base: "Money".into(),
        arguments: vec![currency],
    })
}

fn is_money_rate(ty: &Type) -> bool {
    matches!(ty, Type::MoneyPerUnit { .. })
}

fn contains_secret_value_type(ty: &Type) -> bool {
    contains_secret_value_type_in(
        ty,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeSet::new(),
        &mut BTreeSet::new(),
    )
}

/// Finds a nominal row by its canonical identity. Only an exact identity resolves:
/// a bare name must not borrow the secret of an unrelated namespaced row.
fn nominal_row_by_name<'a>(rows: &'a BTreeMap<String, Type>, name: &str) -> Option<&'a Type> {
    rows.get(name)
}

/// Reports whether a value of `ty` can hold a `Secret`. A nominal reference is
/// followed through `nominal_rows` (its canonical record members) and an alias
/// through `aliases` (its target), so a nominal or alias whose member reaches a
/// secret stores that secret. `visiting` holds every name already entered; a
/// self-referential name therefore terminates, and any reachable secret is
/// still found on the first walk that reaches it.
fn contains_secret_value_type_in(
    ty: &Type,
    nominal_rows: &BTreeMap<String, Type>,
    aliases: &BTreeMap<String, Type>,
    imported: &BTreeSet<String>,
    visiting: &mut BTreeSet<String>,
) -> bool {
    let secret = |ty: &Type, visiting: &mut BTreeSet<String>| {
        contains_secret_value_type_in(ty, nominal_rows, aliases, imported, visiting)
    };
    match ty {
        Type::Applied { base, arguments } => {
            base == "Secret" || arguments.iter().any(|argument| secret(argument, visiting))
        }
        Type::List(value)
        | Type::Range(value)
        | Type::Relation(value)
        | Type::Stream(value)
        | Type::Optional(value) => secret(value, visiting),
        Type::Record(fields) => fields.values().any(|field| secret(field, visiting)),
        Type::Tuple(values) => values.iter().any(|value| secret(value, visiting)),
        Type::MoneyPerUnit { currency, unit } => {
            secret(currency, visiting) || secret(unit, visiting)
        }
        Type::Quantity { numeric, .. } => secret(numeric, visiting),
        Type::Function {
            parameters, result, ..
        } => {
            parameters
                .iter()
                .any(|parameter| secret(parameter, visiting))
                || secret(result, visiting)
        }
        Type::Named(name) => {
            name == "Secret"
                || imported.contains(name)
                || (visiting.insert(name.clone())
                    && nominal_row_by_name(nominal_rows, name)
                        .or_else(|| aliases.get(name))
                        .is_some_and(|target| secret(target, visiting)))
        }
        Type::Int
        | Type::Decimal
        | Type::Float
        | Type::Date
        | Type::Instant
        | Type::Text
        | Type::Bool
        | Type::Null
        | Type::Unit
        | Type::Bottom
        | Type::Error => false,
    }
}

fn is_secret_sensitive_presentation_path(path: &[&str]) -> bool {
    path.starts_with(&["std", "ui"]) || path == ["std", "terminal", "present_table"]
}

fn infer_ui_action_call(
    path: &[&str],
    arguments: &[orna_syntax::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    if path != ["std", "ui", "action"] {
        return None;
    }

    let mut effects = EffectSummary::default();
    let mut action_id = None;
    let mut input_type = None;
    let mut debug_kind = None;
    let mut positional = 0usize;
    let mut valid = true;

    for argument in arguments {
        match argument.name.as_deref() {
            Some("action_id") if action_id.is_none() => {
                let inferred = infer(&argument.value, scope, local, diagnostics);
                effects.join(&inferred.effects);
                if inferred.ty != Type::Text {
                    valid = false;
                }
                action_id = Some(inferred.ty);
            }
            Some("as") if input_type.is_none() => {
                let Some(path) = qualified_path(&argument.value) else {
                    valid = false;
                    continue;
                };
                let name = path.join(".");
                let ty = primitive(&name).unwrap_or(Type::Named(name));
                let ty = canonicalize_type(&ty, scope);
                if !static_type_is_known(&ty, scope) {
                    valid = false;
                }
                input_type = Some(ty);
            }
            Some("debug_kind") if debug_kind.is_none() => {
                let inferred = infer(&argument.value, scope, local, diagnostics);
                effects.join(&inferred.effects);
                if inferred.ty != Type::Text && inferred.ty != Type::Null {
                    valid = false;
                }
                debug_kind = Some(inferred.ty);
            }
            None if positional == 0 && action_id.is_none() => {
                let inferred = infer(&argument.value, scope, local, diagnostics);
                effects.join(&inferred.effects);
                if inferred.ty != Type::Text {
                    valid = false;
                }
                action_id = Some(inferred.ty);
                positional += 1;
            }
            _ => valid = false,
        }
    }

    if action_id.is_none() || input_type.is_none() || !valid {
        diagnostics.push(diag(
            DIAG_TYPE,
            "std.ui.action requires explicit action_id provenance and input type",
        ));
        return Some(Inferred::error_with(effects));
    }

    Some(Inferred {
        ty: Type::Applied {
            base: "std.ui.Action".into(),
            arguments: vec![input_type.expect("checked above")],
        },
        effects,
    })
}

fn intrinsic_call_effects(callee: &Expr) -> EffectSummary {
    let Some(path) = qualified_path(callee) else {
        return EffectSummary::default();
    };
    standard_function_effects(&path)
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
    Some(Inferred::pure(Type::Named(path.join("."))))
}

/// One assertion to check: what it is, where it runs and over which tables.
struct AssertionInput<'a> {
    owner: AssertionOwner,
    value: &'a Expr,
    inferred: Inferred,
    resolved_tables: Option<&'a BTreeMap<String, Type>>,
    scope: Option<&'a Scope>,
    enforcement: AssertionScope,
    source_order: usize,
}

/// Identity given to an imported type whose definition cannot be determined. The secret walk
/// treats `Secret` as secret, so such a name is never accepted as a stored value.
const FAIL_CLOSED_SECRET_IDENTITY: &str = "Secret";

/// Dependency marker for an assertion whose table reads cannot be enumerated.
/// Such an assertion is evaluated on every write instead of being skipped.
pub const UNRESOLVED_TABLE_DEPENDENCY: &str = "*";

#[cfg(test)]
mod tests;
