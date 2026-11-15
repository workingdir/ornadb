//! The HIR checker entry point (`docs/design/hir.md` section 3.1).
//!
//! [`check`] lowers the project sources together with the pinned standard
//! sources, runs the HIR type checker, and returns the program with the
//! diagnostics of the project's own modules. [`located_diagnostics`] converts
//! those findings to the diagnostics and origins the old `Analysis` reports,
//! so a caller can compare the two checkers code by code.

use std::collections::BTreeMap;
use std::sync::Arc;

use orna_hir::{
    Idx, ModuleOrigin, ModulePath, Name, PreludeExports, Program, SourceId, SourceUnit,
    SysAdmission, SysMemberStatus, lower_with_admission, typeck,
};
use orna_syntax::parse_module_with_file;
use orna_syntax_ast::SourceSpan as SyntaxSpan;
use orna_syntax_ast::SyntaxTree;
use orna_value::{Diagnostic, SafeText};

use crate::StandardDependencyProfile;
use crate::located::DiagnosticOrigin;
use crate::span_index::ItemSpans;
use crate::system_api::{PathResolution, SystemMember, embedded_system_api};
use crate::{DIAG_PARSE, DIAG_UNRESOLVED, ModuleInput, UNRESOLVED_NAME_NOTE, diag};

/// The host's `sys` admission, read from the embedded catalogue and the
/// production providers linked into this build (`system_api.rs`).
///
/// A catalogued member is `Unlinked` when no provider serves it. The member
/// mapping is the one `system_calls::infer_descriptor_system_path` uses, so the
/// HIR gate and the descriptor checker refuse the same members.
#[derive(Clone, Copy, Debug, Default)]
pub struct CatalogueAdmission;

impl SysAdmission for CatalogueAdmission {
    fn status(&self, path: &[&str]) -> SysMemberStatus {
        let api = embedded_system_api();
        let name = path.join(".");
        let status = |linked: bool| {
            if linked {
                SysMemberStatus::Linked
            } else {
                SysMemberStatus::Unlinked
            }
        };
        if api.historical_relation(path).is_some() {
            let view = name.strip_suffix(".as_of").unwrap_or(&name);
            return status(api.provider_linked(SystemMember::HistoricalView(view)));
        }
        if path == ["sys", "database", "as_of"] {
            return status(api.provider_linked(SystemMember::HistoricalView("sys.database")));
        }
        if let Some(singleton) = api.historical_singleton(path) {
            return status(api.provider_linked(SystemMember::HistoricalView(&singleton.name)));
        }
        if let Some(functions) = api.function(&name) {
            return status(
                functions
                    .iter()
                    .any(|function| api.provider_linked(SystemMember::Callable(&function.label))),
            );
        }
        if api.relation(&name).is_some() {
            return status(api.provider_linked(SystemMember::Relation(&name)));
        }
        if api.enum_value(path).is_some() {
            return SysMemberStatus::Linked;
        }
        match api.resolve(path) {
            PathResolution::ReadOnly(_) => match api.singleton_view(path) {
                Some(singleton) => status(api.provider_linked(SystemMember::Singleton(singleton))),
                // The admitted table catalogue is served by the evaluator.
                None => SysMemberStatus::Linked,
            },
            PathResolution::Removed(_)
            | PathResolution::KnownUnsupported
            | PathResolution::Unknown => SysMemberStatus::Unknown,
        }
    }
}

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
    let mut program = lower_with_admission(&units, prelude, &[], Arc::new(CatalogueAdmission));
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

/// Converts findings to the diagnostics and origins the checker reports, so the
/// language server publishes HIR findings where it published the checker's.
///
/// The checker reports every diagnostic against the top-level item it checked
/// (`located.rs`), so each origin here is the span of the enclosing item of
/// `modules`; a finding outside every item keeps its own span. An unresolved
/// bare name takes the checker's wording and note (`name cannot be resolved`,
/// `unresolved name: X`). Other findings keep their HIR code and message.
/// `origins[i]` belongs to `diagnostics[i]`.
#[must_use]
pub fn checker_located_diagnostics(
    findings: &[HirFinding],
    modules: &[ModuleInput],
) -> (Vec<Diagnostic>, Vec<DiagnosticOrigin>) {
    let mut items_by_module = BTreeMap::<&str, ItemSpans>::new();
    findings
        .iter()
        .map(|finding| {
            let items = items_by_module
                .entry(finding.logical_path.as_str())
                .or_insert_with(|| {
                    modules
                        .iter()
                        .find(|module| module.logical_path == finding.logical_path)
                        .map_or_else(
                            || ItemSpans::new(Vec::new()),
                            |module| top_level_spans(&module.source, &module.logical_path),
                        )
                });
            let inner = finding.span.start..finding.span.end;
            let anchor = items
                .containing(&inner)
                .map_or(inner, |index| items.span(index).clone());
            let origin = DiagnosticOrigin {
                logical_path: Some(finding.logical_path.clone()),
                primary: Some(anchor),
            };
            (checker_diagnostic(finding), origin)
        })
        .unzip()
}

/// The spans of the top-level items of one module, as the parser recovers them.
fn top_level_spans(source: &str, logical: &str) -> ItemSpans {
    ItemSpans::new(
        parse_module_with_file(source, logical.to_owned())
            .value
            .items
            .iter()
            .map(|item| item.span.start..item.span.end)
            .collect(),
    )
}

/// The checker's diagnostic for one HIR finding.
fn checker_diagnostic(finding: &HirFinding) -> Diagnostic {
    match unresolved_bare_name(finding) {
        Some(name) => diag(DIAG_UNRESOLVED, "name cannot be resolved").with_note(
            SafeText::new(format!("{UNRESOLVED_NAME_NOTE}{name}"))
                .expect("identifier text is safe"),
        ),
        None => diag(finding.code, finding.message.clone()),
    }
}

/// The name of a HIR `` `name` is not in scope `` unresolved-name finding.
fn unresolved_bare_name(finding: &HirFinding) -> Option<&str> {
    if finding.code != DIAG_UNRESOLVED {
        return None;
    }
    let name = finding
        .message
        .strip_prefix('`')?
        .strip_suffix("` is not in scope")?;
    (!name.is_empty() && !name.contains('`')).then_some(name)
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::*;

    /// A finding of `DIAG_UNRESOLVED` in `logical_path` over `start..end`.
    fn finding(logical_path: &str, start: usize, end: usize) -> HirFinding {
        HirFinding {
            code: DIAG_UNRESOLVED,
            message: "unused".to_owned(),
            logical_path: logical_path.to_owned(),
            span: SyntaxSpan::new(start, end),
        }
    }

    #[test]
    fn origins_anchor_each_finding_to_its_first_containing_item() {
        let main = "pub fn one(): Int = 1;\npub fn two(): Int = 2;\n";
        let modules = [ModuleInput::new("main.orna", main)];
        let items: Vec<Range<usize>> = parse_module_with_file(main, "main.orna".to_owned())
            .value
            .items
            .iter()
            .map(|item| item.span.start..item.span.end)
            .collect();
        assert_eq!(items.len(), 2, "{items:?}");
        let two = main.find("pub fn two").expect("second item");
        let findings = [
            finding("main.orna", two + 4, two + 6),
            finding("main.orna", 3, 5),
            finding("main.orna", 0, main.len()),
            finding("main.orna", main.len(), main.len()),
        ];
        let (_, origins) = checker_located_diagnostics(&findings, &modules);
        let anchored: Vec<_> = origins.into_iter().map(|origin| origin.primary).collect();
        let expected: Vec<_> = findings
            .iter()
            .map(|finding| {
                let span = finding.span.start..finding.span.end;
                Some(
                    items
                        .iter()
                        .find(|item| item.start <= span.start && span.end <= item.end)
                        .cloned()
                        .unwrap_or_else(|| span.clone()),
                )
            })
            .collect();
        assert_eq!(anchored, expected);
        assert_eq!(anchored[0], Some(items[1].clone()));
        assert_eq!(anchored[1], Some(items[0].clone()));

        let (_, missing) = checker_located_diagnostics(&[finding("missing.orna", 3, 5)], &modules);
        assert_eq!(missing[0].primary, Some(3..5));
    }
}
