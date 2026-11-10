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
    lower,
};
use orna_standard_sources::reference_standard_sources_v1;
use orna_syntax::{
    Declaration, EntryPoint, FunctionSignature, Item, ItemKind, SyntaxTree, Visibility,
    parse_expression_with_file, parse_module_with_file,
};

use crate::Route;
use crate::case::{Case, CaseKind};

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

/// The namespace a logical path declares (`ORNA-NS-001` to `ORNA-NS-004`).
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

/// One case lowered to HIR, with the bookkeeping the comparisons need.
pub struct LoweredProgram {
    /// The lowered program.
    pub program: Program,
    /// The logical path of each module, when it came from a source.
    pub logical_paths: Vec<Option<String>>,
    /// Whether the module belongs to the case rather than the standard
    /// dependency its imports reach.
    pub owned: Vec<bool>,
}

/// Lowers one case, or explains why the stage cannot.
pub fn lower_case(case: &Case) -> Result<LoweredProgram, String> {
    match case.kind {
        CaseKind::Module | CaseKind::StandardBundle => lower_module_case(case),
        CaseKind::Expression => lower_expression_case(case),
        CaseKind::Repl => Err(format!(
            "{}: a REPL session is an incremental module and stage H8 lowers it",
            case.id
        )),
    }
}

fn lower_module_case(case: &Case) -> Result<LoweredProgram, String> {
    let mut parsed = Vec::new();
    for source in &case.sources {
        let tree = parse_module_with_file(&source.text, source.logical_path.clone()).value;
        parsed.push((source.clone(), tree));
    }
    // Every standard module is available to a case, as the catalogue makes it
    // available to the old pipeline: a qualified `std.x.y` path needs no
    // `use`, so the reached-imports closure would miss it.
    let standard = STANDARD_MODULES.iter();
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
    let program = lower(&units);
    let (logical_paths, owned) = bookkeeping(&program, &logical_paths, &owned);
    Ok(LoweredProgram {
        program,
        logical_paths,
        owned,
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
    let span = orna_syntax::SyntaxSpan::new(0, source.text.len());
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
