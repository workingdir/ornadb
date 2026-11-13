//! The new pipeline's observer for stage H1a: lowered HIR, call
//! classification and resolution.
//!
//! There is no executor yet, so this observer does not produce an outcome,
//! step count or table write. It produces the two things H1 can compare with the
//! old pipeline (`docs/design/hir.md` section 6.3):
//!
//! * **calls**: every call in the lowered program, keyed by the callee's span
//!   and the route its [`Callee`] maps to by the H1 table, compared with the old
//!   evaluator's route trace;
//! * **resolution**: the names the HIR resolver leaves unresolved or ambiguous,
//!   compared with the old checker's diagnostics.
//!
//! # The program a case lowers to
//!
//! A module case lowers to its own source plus the standard modules its imports
//! reach, because the old pipeline resolves `std` through the catalogue and the
//! HIR resolves it through lowered source. Those standard modules are marked as
//! not owned: their names are not part of the resolution comparison, which is
//! about the case's own source.
//!
//! An expression case lowers to one synthetic module holding `fn run() = <expr>`
//! without moving the expression's spans, so a route record and a HIR call share
//! byte offsets.
//!
//! A REPL case is not lowered: a REPL session is an incremental module and stage
//! H8 owns it. The classification test counts those cases as uncompared and
//! names the stage instead of silently skipping them.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use orna_hir::{
    Callee, ExprKind, Idx, ModulePath, Name, NameTarget, Program, RelOp, SourceId, SourceUnit,
    lower, lower_with_admission, lower_with_attached,
};
use orna_standard_sources::reference_standard_sources_v1;
use orna_syntax::{parse_expression_with_file, parse_module_with_file};
use orna_syntax_ast::{
    Declaration, EntryPoint, FunctionSignature, Item, ItemKind, SyntaxTree, Visibility,
};

use crate::Route;
use crate::case::{Case, CaseKind, Family};
use crate::fixture_environment::{ATTACHED_SUPPORT, CURRENCY_SUPPORT, DATABASE_SUPPORT};

/// The repository's standard modules, parsed once per test process.
struct StandardModule {
    /// The logical path the old pipeline parses the module under.
    logical: String,
    /// The module path lowering registers.
    path: ModulePath,
    /// The parsed tree.
    tree: SyntaxTree,
}

static STANDARD_MODULES: LazyLock<Vec<StandardModule>> = LazyLock::new(|| {
    reference_standard_sources_v1()
        .into_iter()
        .map(|(logical, text)| {
            let parsed = parse_module_with_file(&text, logical.clone());
            StandardModule {
                path: module_path_of(&logical),
                logical,
                tree: parsed.value,
            }
        })
        .collect()
});

/// The fixture's unit namespace, which the old catalogue supplies to reference
/// cases only (`catalogue_for`). A reference case reaches it through `use
/// fixture.units.*;`, so it is lowered as an unowned module.
static FIXTURE_UNITS: LazyLock<StandardModule> = LazyLock::new(|| {
    let logical = "fixture/units.orna".to_owned();
    StandardModule {
        path: module_path_of(&logical),
        tree: parse_module_with_file(orna_check::FIXTURE_UNIT_SUPPORT, logical.clone()).value,
        logical,
    }
});

/// The fixture currencies (`fixture_environment::CURRENCY_SUPPORT`), reached through `use fixture.currencies.*;`.
static FIXTURE_CURRENCIES: LazyLock<StandardModule> = LazyLock::new(|| {
    let logical = "fixture/currencies.orna".to_owned();
    StandardModule {
        path: module_path_of(&logical),
        tree: parse_module_with_file(CURRENCY_SUPPORT, logical.clone()).value,
        logical,
    }
});

/// The attached fixture declarations (`fixture_environment::ATTACHED_SUPPORT`),
/// reached by reference cases through `use fixture.attached.*;`.
static FIXTURE_ATTACHED: LazyLock<StandardModule> = LazyLock::new(|| {
    let logical = "fixture/attached.orna".to_owned();
    StandardModule {
        path: module_path_of(&logical),
        tree: parse_module_with_file(ATTACHED_SUPPORT, logical.clone()).value,
        logical,
    }
});

/// The attached database modules (`fixture_environment::DATABASE_SUPPORT`),
/// reached by reference cases as `contacts.Contact` with no `use`.
static FIXTURE_DATABASES: LazyLock<Vec<StandardModule>> = LazyLock::new(|| {
    DATABASE_SUPPORT
        .iter()
        .map(|(logical, text)| {
            let logical = (*logical).to_owned();
            StandardModule {
                path: module_path_of(&logical),
                tree: parse_module_with_file(text, logical.clone()).value,
                logical,
            }
        })
        .collect()
});

/// The database roots a reference case may attach, with the names that make a
/// case need the root: the root itself, plus the bare table name for `contacts`,
/// whose `Contact` a reference module reaches through `use contacts.*;`.
const ATTACHABLE_DATABASES: [(&str, &[&str]); 3] = [
    ("contacts", &["contacts", "Contact"]),
    ("energy", &["energy"]),
    ("mail", &["mail"]),
];

/// Connector paths the source-only environment cannot supply (bead ornadb-csenl.15).
/// A case that reaches one keeps that connector's root unattached: attaching `mail`
/// would turn `mail.google.sync`, a name the fixture-catalogue environment supplies,
/// into an owned unresolved export of an attached database.
const UNSUPPLIED_CONNECTORS: [&str; 1] = ["mail.google"];

/// The attached database roots one reference case sees: a root is attached when the
/// case names it and does not reach one of its [`UNSUPPLIED_CONNECTORS`].
fn attached_databases(case: &Case) -> Vec<&'static str> {
    let tokens: Vec<&str> = case
        .sources
        .iter()
        .flat_map(|source| {
            source
                .text
                .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        })
        .filter(|token| !token.is_empty())
        .collect();
    let first_segment = |token: &str| token.split('.').next().unwrap_or(token).to_owned();
    let reaches = |prefix: &str| {
        tokens
            .iter()
            .any(|token| *token == prefix || token.starts_with(&format!("{prefix}.")))
    };
    ATTACHABLE_DATABASES
        .iter()
        .filter(|(root, names)| {
            let named = names
                .iter()
                .any(|name| tokens.iter().any(|token| first_segment(token) == *name));
            let connector_reached = UNSUPPLIED_CONNECTORS
                .iter()
                .any(|connector| connector.split('.').next() == Some(*root) && reaches(connector));
            named && !connector_reached
        })
        .map(|(root, _)| *root)
        .collect()
}

/// The namespace a logical path declares (`ORNA-NS-001` to `ORNA-NS-004`).
///
/// This is the authoritative identity rule, `orna-project`'s
/// `object_identity::module_namespace`: a file named `main` declares its
/// directory's namespace in any directory, so `std/concurrent/main.orna` is
/// `std.concurrent`, which the standard library's imports require.
///
/// DIVERGENCE: `orna-conformance`'s `semantic_adapter::module_namespace` drops
/// `main` only at the root and gives `a/main.orna` the namespace `a.main`. That
/// rule applies to owned fixture units only. It is deliberately not applied
/// here: doing so breaks `std.concurrent.parallel` resolution (the
/// `hir_call_classification_matches_the_route_trace` test). Pinned by
/// `module_path_tests`.
#[must_use]
pub fn module_path_of(logical: &str) -> ModulePath {
    let trimmed = logical.strip_suffix(".orna").unwrap_or(logical);
    let mut segments = trimmed
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if segments.last() == Some(&"main") {
        segments.pop();
    }
    ModulePath::new(segments.into_iter().map(Name::new))
}

#[cfg(test)]
mod module_path_tests {
    use super::module_path_of;

    fn names(logical: &str) -> Vec<String> {
        module_path_of(logical)
            .segments()
            .iter()
            .map(|name| name.as_str().to_owned())
            .collect()
    }

    #[test]
    fn root_main_declares_the_empty_namespace() {
        assert!(names("main.orna").is_empty());
    }

    #[test]
    fn a_nested_main_declares_its_directory_in_any_directory() {
        assert_eq!(names("std/concurrent/main.orna"), ["std", "concurrent"]);
        assert_eq!(names("a/main.orna"), ["a"]);
    }

    #[test]
    fn other_modules_keep_every_segment() {
        assert_eq!(names("a/b.orna"), ["a", "b"]);
        assert_eq!(names("valid/enum.orna"), ["valid", "enum"]);
    }
}

/// One case lowered to HIR, with the bookkeeping the comparisons need.
pub struct LoweredProgram {
    /// The lowered program.
    pub program: Program,
    /// The logical path of each module, when it came from a source.
    pub logical_paths: Vec<Option<String>>,
    /// Whether the module belongs to the case rather than the standard
    /// dependency its imports reach.
    pub owned: Vec<bool>,
    /// The syntax diagnostics raised by the case's own sources. HIR lowers the
    /// recovered tree, so these are reported as `ORNA-S000-PARSE`.
    pub parse_diagnostics: usize,
}

/// Lowers one case for the typed and diagnostic observers, or explains why the stage cannot.
///
/// Every `sys` member is linked here (the admission `lower` uses), so the observers see the
/// declared program. The executor reads [`lower_case_admitted`] instead.
pub fn lower_case(case: &Case) -> Result<LoweredProgram, String> {
    lower_case_as(case, false)
}

/// Lowers one case as the executor does, with each `sys` reference checked against the production
/// catalogue (ORNA-SYS-008), or explains why the stage cannot.
pub fn lower_case_admitted(case: &Case) -> Result<LoweredProgram, String> {
    lower_case_as(case, true)
}

fn lower_case_as(case: &Case, admitted: bool) -> Result<LoweredProgram, String> {
    match case.kind {
        CaseKind::Module | CaseKind::StandardBundle => lower_module_case(case, admitted),
        CaseKind::Expression => lower_expression_case(case),
        CaseKind::Repl => Err(format!(
            "{}: a REPL session is an incremental module and stage H8 lowers it",
            case.id
        )),
    }
}

/// Makes the fixture units visible to a reference module without an import.
///
/// The old catalogue supplies the fixture unit names to every reference module
/// (`C`, `mph`, `kWh`), and the corpus uses them with no `use`. HIR resolves a
/// bare name only through an import, so the observer adds the import the
/// catalogue supplies implicitly. The spliced item carries the snippet's spans,
/// which hold no call or diagnostic.
fn import_fixture_units(tree: &mut SyntaxTree, attached_roots: &[&str]) {
    let mut text =
        String::from("use fixture.units.*;\nuse fixture.currencies.*;\nuse fixture.attached.*;\n");
    if attached_roots.contains(&"contacts") {
        text.push_str("use contacts.*;\n");
    }
    let import = parse_module_with_file(&text, "fixture/import.orna").value;
    tree.items.splice(0..0, import.items);
}

fn lower_module_case(case: &Case, admitted: bool) -> Result<LoweredProgram, String> {
    let attached_roots = if case.family == Family::Reference {
        attached_databases(case)
    } else {
        Vec::new()
    };
    let mut parsed = Vec::new();
    let mut parse_diagnostics = 0;
    for source in &case.sources {
        let outcome = parse_module_with_file(&source.text, source.logical_path.clone());
        parse_diagnostics += outcome.diagnostics.len();
        let mut tree = outcome.value;
        if case.family == Family::Reference {
            import_fixture_units(&mut tree, &attached_roots);
        }
        parsed.push((source.clone(), tree));
    }
    // Every standard module is available to a case, as the catalogue makes it
    // available to the old pipeline: a qualified `std.x.y` path needs no
    // `use`, so the reached-imports closure would miss it. Reference cases also
    // get the fixture environment the old pipeline admits them against.
    let mut standard: Vec<&StandardModule> = STANDARD_MODULES.iter().collect();
    if case.family == Family::Reference {
        standard.push(&FIXTURE_UNITS);
        standard.push(&FIXTURE_CURRENCIES);
        standard.push(&FIXTURE_ATTACHED);
        standard.extend(FIXTURE_DATABASES.iter().filter(|module| {
            module
                .logical
                .strip_suffix(".orna")
                .is_some_and(|root| attached_roots.contains(&root))
        }));
    }
    let standard = standard.into_iter();
    let mut units = Vec::new();
    let mut logical_paths = Vec::new();
    let mut owned = Vec::new();
    for (source, tree) in &parsed {
        let path = module_path_of(&source.logical_path);
        units.push(SourceUnit {
            source: SourceId::new(
                u32::try_from(logical_paths.len()).expect("source index fits u32"),
            ),
            tree,
            path,
        });
        logical_paths.push(Some(source.logical_path.clone()));
        owned.push(true);
    }
    for module in standard {
        let index = logical_paths.len();
        units.push(SourceUnit {
            source: SourceId::new(u32::try_from(index).expect("source index fits u32")),
            tree: &module.tree,
            path: module.path.clone(),
        });
        logical_paths.push(Some(module.logical.clone()));
        owned.push(false);
    }
    let prelude =
        orna_check::hir_check::prelude_of_profile(&orna_evaluator::reference_standard_profile());
    let attached: Vec<Name> = attached_roots.iter().copied().map(Name::new).collect();
    let program = if admitted {
        let admission = std::sync::Arc::new(orna_check::hir_check::CatalogueAdmission);
        lower_with_admission(&units, &prelude, &attached, admission)
    } else {
        lower_with_attached(&units, &prelude, &attached)
    };
    let (logical_paths, owned) = bookkeeping(&program, &logical_paths, &owned);
    Ok(LoweredProgram {
        program,
        logical_paths,
        owned,
        parse_diagnostics,
    })
}

/// Maps module ids to the logical paths and ownership the sources recorded.
///
/// Module ids are not source indices: lowering registers the built-in `sys`
/// module first and may add the implicit `std` root, so the mapping goes
/// through the module's canonical path, which is unique.
fn bookkeeping(
    program: &Program,
    sources: &[Option<String>],
    owned: &[bool],
) -> (Vec<Option<String>>, Vec<bool>) {
    let mut logical_paths = vec![None; program.modules.len()];
    let mut owned_modules = vec![false; program.modules.len()];
    for (source, is_owned) in sources.iter().zip(owned) {
        let Some(logical) = source else {
            continue;
        };
        let path = module_path_of(logical);
        if let Some((module, _)) = program
            .modules
            .iter_enumerated()
            .find(|(_, candidate)| candidate.path == path)
        {
            logical_paths[module.index()] = Some(logical.clone());
            owned_modules[module.index()] = *is_owned;
        }
    }
    (logical_paths, owned_modules)
}

fn lower_expression_case(case: &Case) -> Result<LoweredProgram, String> {
    let source = case
        .sources
        .first()
        .ok_or_else(|| format!("{}: case has no source", case.id))?;
    let parsed = parse_expression_with_file(&source.text, source.logical_path.clone());
    let span = orna_syntax_ast::SourceSpan::new(0, source.text.len());
    let tree = SyntaxTree {
        entry: EntryPoint::Module,
        items: vec![Item {
            kind: ItemKind::Function,
            visibility: Visibility::Private,
            span: span.clone(),
            declaration: Declaration::Function {
                signature: FunctionSignature {
                    name: "run".to_owned(),
                    generics: Vec::new(),
                    parameters: Vec::new(),
                    result: None,
                    span: span.clone(),
                },
                body: parsed.value,
            },
            docs: Vec::new(),
            clauses: orna_syntax_ast::InterfaceClauses::default(),
        }],
        span,
    };
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path: module_path_of(&source.logical_path),
    }]);
    let (logical_paths, owned) =
        bookkeeping(&program, &[Some(source.logical_path.clone())], &[true]);
    // The synthetic tree borrows nothing, so the program owns everything it
    // records; the tree is dropped with this frame.
    Ok(LoweredProgram {
        program,
        logical_paths,
        owned,
        parse_diagnostics: parsed.diagnostics.len(),
    })
}

/// One call in a lowered case, with the old route its `Callee` maps to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassifiedCall {
    /// Logical path of the module the call is written in.
    pub logical_path: String,
    /// Byte range of the callee expression in that module's source.
    pub start: usize,
    /// End of the callee expression.
    pub end: usize,
    /// The old route the classified callee maps to, when the old trace has a
    /// branch for it.
    pub route: Option<Route>,
    /// The classification, for evidence messages.
    pub kind: &'static str,
}

/// Every classified call of a lowered case, keyed the way the route trace is.
#[must_use]
pub fn calls(lowered: &LoweredProgram) -> Vec<ClassifiedCall> {
    let mut records = Vec::new();
    for body in lowered.program.bodies.iter() {
        let module = lowered.program.defs[body.owner].module;
        let Some(Some(logical)) = lowered.logical_paths.get(module.index()) else {
            continue;
        };
        for expr in body.exprs.iter() {
            let ExprKind::Call(call) = &expr.kind else {
                continue;
            };
            records.push(ClassifiedCall {
                logical_path: logical.clone(),
                start: call.callee_span.start,
                end: call.callee_span.end,
                route: route_of(&call.callee),
                kind: kind_of(&call.callee),
            });
        }
    }
    records
}

/// The classification's name, for evidence messages.
#[must_use]
pub fn kind_of(callee: &Callee) -> &'static str {
    match callee {
        Callee::Source { .. } => "source",
        Callee::Native { .. } => "native",
        Callee::Sys { .. } => "sys",
        Callee::TableOp { .. } => "table-op",
        Callee::Relational { .. } => "relational",
        Callee::Intrinsic(_) => "intrinsic",
        Callee::Ctor { .. } => "ctor",
        Callee::Value { .. } => "value",
        Callee::Pending { .. } => "pending",
    }
}

/// The old route a classified callee maps to (`docs/design/hir.md` H1).
///
/// `None` means the old route trace records no branch for this kind: a table
/// operation is served inside the source-function branch, a constructor has no
/// branch of its own, and the concurrency forms are not in the root-intrinsic
/// set. Those calls are counted, not compared, until the stage owns them.
#[must_use]
pub fn route_of(callee: &Callee) -> Option<Route> {
    match callee {
        Callee::Source { .. } | Callee::Value { .. } => Some(Route::SourceFunction),
        Callee::Native { .. } => Some(Route::Native),
        Callee::Sys { .. } => Some(Route::SysHost),
        Callee::Relational { op } => Some(match op {
            RelOp::SourceScan => Route::RelationSource,
            _ => Route::RelationStage,
        }),
        Callee::Intrinsic(form) => match form {
            orna_hir::IntrinsicForm::Fail
            | orna_hir::IntrinsicForm::OptionSome
            | orna_hir::IntrinsicForm::Now
            | orna_hir::IntrinsicForm::Uuid7
            | orna_hir::IntrinsicForm::Error => Some(Route::RootIntrinsic),
            orna_hir::IntrinsicForm::Parallel
            | orna_hir::IntrinsicForm::Race
            | orna_hir::IntrinsicForm::Timeout
            | orna_hir::IntrinsicForm::MergeStreams => None,
        },
        Callee::Pending { name, .. } => {
            if name.as_str() == "divide" {
                Some(Route::DecimalDivide)
            } else if relation_stage(name.as_str()) {
                Some(Route::RelationStage)
            } else {
                None
            }
        }
        Callee::TableOp { .. } | Callee::Ctor { .. } => None,
    }
}

/// Whether a member name is a relation pipeline stage.
fn relation_stage(name: &str) -> bool {
    matches!(
        name,
        "filter"
            | "map"
            | "sort_by"
            | "bucket_by"
            | "distinct"
            | "pairs"
            | "window"
            | "take"
            | "drop"
            | "flat_map"
    )
}

/// The names a lowered case resolves and does not resolve.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolutionObservation {
    /// Unresolved names and how often each occurs.
    pub unresolved: BTreeMap<String, usize>,
    /// Unresolved names the other pipeline reports with no name attached.
    pub unresolved_without_name: usize,
    /// Ambiguity diagnostics.
    pub ambiguous: usize,
}

/// The HIR resolver's answer for the case's own modules.
#[must_use]
pub fn resolution(lowered: &LoweredProgram) -> ResolutionObservation {
    let mut observation = ResolutionObservation::default();
    for (module_id, module) in lowered.program.modules.iter_enumerated() {
        if !lowered
            .owned
            .get(module_id.index())
            .copied()
            .unwrap_or(false)
        {
            continue;
        }
        for use_ in &module.name_uses {
            // The old checker's unresolved diagnostic names a value use; type,
            // pattern and member names are not attributable on its side, so the
            // comparison is over value positions.
            if use_.context == orna_hir::UseContext::Value && use_.target == NameTarget::Unresolved
            {
                *observation
                    .unresolved
                    .entry(use_.name.as_str().to_owned())
                    .or_insert(0) += 1;
            }
        }
        observation.ambiguous += module
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == orna_hir::diag::AMBIGUOUS)
            .count();
    }
    observation
}

/// What the H2a type pass did with one lowered case.
///
/// The pass itself is [`orna_hir::typeck`]; this record narrows it to the
/// case's own modules. A dependency's body may be outside the subset (pinned
/// `std` still omits parameter annotations in places) without making the case
/// untyped, exactly as the checker's dependency analysis does not make a
/// project module's signatures unavailable.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TypeObservation {
    /// Whether the HIR accepts the case: no lowering diagnostic in an owned
    /// module and every owned top-level body typed.
    pub accepted: bool,
    /// Owned top-level bodies with finished static types.
    pub typed_bodies: usize,
    /// Owned top-level bodies examined.
    pub total_bodies: usize,
    /// One line per owned body the subset cannot type, with the reason.
    pub not_yet: Vec<String>,
    /// One line per lowering diagnostic in an owned module.
    pub diagnostics: Vec<String>,
    /// The printed type of every public signature, keyed by
    /// `module.path::name`.
    pub signatures: BTreeMap<String, String>,
}

impl TypeObservation {
    /// Whether this case contributes to the typed-share ratchet.
    ///
    /// A program with no top-level body has nothing to type, so it is accepted
    /// but does not claim a share.
    #[must_use]
    pub fn typed_case(&self) -> bool {
        self.accepted && self.total_bodies > 0
    }
}

/// Runs the type pass over one lowered case and records the owned modules'
/// result.
pub fn type_observation(lowered: &mut LoweredProgram) -> TypeObservation {
    let summary = orna_hir::typeck(&mut lowered.program);
    let mut observation = TypeObservation::default();
    for outcome in &summary.outcomes {
        if !owned(lowered, outcome.module) {
            continue;
        }
        observation.total_bodies += 1;
        if outcome.typed {
            observation.typed_bodies += 1;
        } else if let Some(reason) = &outcome.not_yet {
            observation.not_yet.push(format!(
                "{}: {}.{}: {}",
                outcome.body.as_u32(),
                module_name(lowered, outcome.module),
                definition_name(lowered, outcome.owner),
                reason.message()
            ));
        }
    }
    for (module_id, module) in lowered.program.modules.iter_enumerated() {
        if !owned(lowered, module_id) {
            continue;
        }
        for diagnostic in &module.diagnostics {
            observation.diagnostics.push(format!(
                "{}: {}: {}",
                module.path, diagnostic.code, diagnostic.message
            ));
        }
    }
    for (module_id, module) in lowered.program.modules.iter_enumerated() {
        if !owned(lowered, module_id) {
            continue;
        }
        for def in &module.items {
            let definition = &lowered.program.defs[*def];
            if definition.vis != orna_hir::Visibility::Public {
                continue;
            }
            if !matches!(&definition.kind, orna_hir::DefKind::Fn(_)) {
                continue;
            }
            let Some(signature) = orna_hir::print_signature(&lowered.program, *def) else {
                continue;
            };
            observation
                .signatures
                .insert(format!("{}::{}", module.path, definition.name), signature);
        }
    }
    observation.accepted =
        observation.diagnostics.is_empty() && observation.typed_bodies == observation.total_bodies;
    observation
}

fn owned(lowered: &LoweredProgram, module: orna_hir::ModuleId) -> bool {
    lowered.owned.get(module.index()).copied().unwrap_or(false)
}

fn module_name(lowered: &LoweredProgram, module: orna_hir::ModuleId) -> String {
    lowered
        .logical_paths
        .get(module.index())
        .cloned()
        .flatten()
        .unwrap_or_else(|| lowered.program.modules[module].path.to_string())
}

fn definition_name(lowered: &LoweredProgram, def: orna_hir::DefId) -> String {
    lowered.program.defs[def].name.as_str().to_owned()
}
