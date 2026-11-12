//! The HIR checker entry point (`docs/design/hir.md` section 3.1).
//!
//! [`check`] lowers the project sources together with the pinned standard
//! sources, runs the HIR type checker, and returns the program with the
//! diagnostics of the project's own modules. [`located_diagnostics`] converts
//! those findings to the diagnostics and origins the old `Analysis` reports,
//! so a caller can compare the two checkers code by code.

use orna_hir::{
    Idx, ModuleOrigin, ModulePath, Name, PreludeExports, Program, SourceId, SourceUnit,
    lower_with_prelude, typeck,
};
use orna_syntax::{SyntaxSpan, SyntaxTree, parse_module_with_file};
use orna_value::Diagnostic;

use crate::StandardDependencyProfile;
use crate::located::DiagnosticOrigin;
use crate::{DIAG_PARSE, ModuleInput, diag};

/// One pinned standard source, as `(logical path, text)`. The logical path is
/// under the reserved `std/` root, as the project loader supplies it.
pub type StandardSource = (String, String);

/// The result of checking a project on HIR.
pub struct Checked {
    /// The lowered and type-checked program, standard modules included.
    pub program: Program,
    /// The diagnostics of the project's own modules, in module order.
    pub findings: Vec<HirFinding>,
}

/// One HIR diagnostic raised in a project module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirFinding {
    /// The stable diagnostic code.
    pub code: &'static str,
    /// The explanation, without source text.
    pub message: String,
    /// The logical path of the module that raised the diagnostic.
    pub logical_path: String,
    /// The byte range of the diagnostic inside that module's source.
    pub span: SyntaxSpan,
}

/// Lowers `inputs` with `standard` and type-checks the result on HIR.
///
/// `inputs` are the project's modules; `standard` are the pinned standard
/// sources the project may import; `prelude` gives the names each standard module
/// exports through `use module as _;` (see [`prelude_of_profile`]). Every source
/// goes through the frozen syntax parser. Only the project's own modules
/// contribute findings.
#[must_use]
pub fn check(
    inputs: &[ModuleInput],
    standard: &[StandardSource],
    prelude: &PreludeExports,
) -> Checked {
    let (trees, parse_findings) = parse_all(inputs, standard);
    let units: Vec<SourceUnit<'_>> = trees
        .iter()
        .enumerate()
        .map(|(index, (logical, tree))| SourceUnit {
            source: SourceId::new(u32::try_from(index).expect("source index fits u32")),
            tree,
            path: module_path_of(logical),
        })
        .collect();
    let mut program = lower_with_prelude(&units, prelude);
    typeck(&mut program);
    let mut findings = parse_findings;
    findings.extend(project_findings(&program, &trees));
    Checked { program, findings }
}

/// The `use module as _;` exports of a pinned standard profile (ORNA-IMPORT-005).
///
/// The profile's logical paths, such as `std/prelude.orna`, map to the module
/// paths the lowering keys on, such as `std.prelude`.
#[must_use]
pub fn prelude_of_profile(profile: &StandardDependencyProfile) -> PreludeExports {
    profile
        .module_prelude_exports()
        .iter()
        .fold(PreludeExports::default(), |prelude, (logical, names)| {
            prelude.with_module(module_path_of(logical), names)
        })
}

/// The explanation the old checker gives every syntax diagnostic of a project module.
const PARSE_MESSAGE: &str = "source was not admitted by the frozen syntax parser";

/// Parses every source, project first and then standard, with its logical path.
///
/// The syntax diagnostics of project modules become `ORNA-S000-PARSE` findings.
/// Lowering still runs on the recovered tree, as the old checker keeps going
/// after a parse error. Standard sources are pinned and must parse; their
/// diagnostics are not reported.
fn parse_all(
    inputs: &[ModuleInput],
    standard: &[StandardSource],
) -> (Vec<(String, SyntaxTree)>, Vec<HirFinding>) {
    let project = inputs
        .iter()
        .map(|input| (&input.logical_path, &input.source, true));
    let standard = standard
        .iter()
        .map(|(logical, text)| (logical, text, false));
    let mut findings = Vec::new();
    let trees = project
        .chain(standard)
        .map(|(logical, text, is_project)| {
            let parsed = parse_module_with_file(text, logical.clone());
            if is_project {
                findings.extend(parsed.diagnostics.iter().map(|diagnostic| HirFinding {
                    code: DIAG_PARSE,
                    message: PARSE_MESSAGE.to_owned(),
                    logical_path: logical.clone(),
                    span: diagnostic.span.clone(),
                }));
            }
            (logical.clone(), parsed.value)
        })
        .collect();
    (trees, findings)
}

/// Maps a logical source path to its canonical module path (ORNA-NS-001..004).
///
/// The `.orna` suffix is dropped, and a trailing `main` segment names its
/// directory, so `app/main.orna` is the module `app`.
fn module_path_of(logical: &str) -> ModulePath {
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

/// The diagnostics raised by project modules, with the logical path of each.
fn project_findings(program: &Program, trees: &[(String, SyntaxTree)]) -> Vec<HirFinding> {
    let mut findings = Vec::new();
    for (module, diagnostic) in program.diagnostics() {
        let record = &program.modules[module];
        if record.origin != ModuleOrigin::Project {
            continue;
        }
        let logical_path = trees
            .get(record.source.index())
            .map(|(logical, _)| logical.clone())
            .unwrap_or_default();
        findings.push(HirFinding {
            code: diagnostic.code,
            message: diagnostic.message.to_string(),
            logical_path,
            span: diagnostic.span.clone(),
        });
    }
    findings
}

/// Converts findings to the diagnostics and origins the old `Analysis` reports.
/// `origins[i]` belongs to `diagnostics[i]`, as in [`crate::LocatedAnalysis`].
#[must_use]
pub fn located_diagnostics(findings: &[HirFinding]) -> (Vec<Diagnostic>, Vec<DiagnosticOrigin>) {
    let diagnostics = findings
        .iter()
        .map(|finding| diag(finding.code, finding.message.clone()))
        .collect();
    let origins = findings
        .iter()
        .map(|finding| DiagnosticOrigin {
            logical_path: Some(finding.logical_path.clone()),
            primary: Some(finding.span.start..finding.span.end),
        })
        .collect();
    (diagnostics, origins)
}
