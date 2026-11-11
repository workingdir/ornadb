//! Backend selection for `orna check` (`docs/design/hir.md` section 6.3, H3).
//!
//! The old checker stays the default. `ORNA_CHECK_HIR=1` checks the project
//! with HIR. `ORNA_CHECK_SHADOW=1` runs both checkers, writes the diagnostics on
//! which they differ (code, logical path and primary span) to stderr, and
//! reports the old checker's verdict; it never fails because the two disagree.
//! Shadow takes precedence when both variables are set.

use std::ops::Range;

use orna_check::hir_check;
use orna_check::{DiagnosticOrigin, SystemProviders};
use orna_project::LoadedProject;
use orna_value::Diagnostic as CheckDiagnostic;

use crate::Diagnostic;

/// The diagnostics of one checker and the origin of each, in the shape
/// `orna_check` reports: `origins[i]` belongs to `diagnostics[i]`.
type Located = (Vec<CheckDiagnostic>, Vec<DiagnosticOrigin>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Backend {
    Old,
    Hir,
    Shadow,
}

/// Checks `project` with the backend the environment selects and reports the
/// verdict exactly as `orna check` always has.
pub(crate) fn check_project(
    project: &LoadedProject,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let (diagnostics, _) = match backend_from_env() {
        Backend::Old => old_located(project)?,
        Backend::Hir => hir_located(project),
        Backend::Shadow => {
            let old = old_located(project)?;
            report_shadow(&old, &hir_located(project));
            old
        }
    };
    let Some(first) = diagnostics.first() else {
        return crate::write_success_line("project valid", color_enabled);
    };
    Err(Diagnostic::target_with_detail(
        "E2101",
        "project semantic analysis failed",
        "fix the first reported source contract error, then run check again",
        format!("{}: {}", first.code(), first.message()),
    ))
}

fn backend_from_env() -> Backend {
    if env_is_one("ORNA_CHECK_SHADOW") {
        Backend::Shadow
    } else if env_is_one("ORNA_CHECK_HIR") {
        Backend::Hir
    } else {
        Backend::Old
    }
}

fn env_is_one(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| value == "1")
}

fn old_located(project: &LoadedProject) -> Result<Located, Diagnostic> {
    let catalogue = crate::semantic_catalogue(project)?;
    let located = orna_check::analyze_with_providers_located(
        project.modules(),
        &catalogue,
        SystemProviders::Production,
    );
    Ok((located.analysis.diagnostics, located.origins))
}

fn hir_located(project: &LoadedProject) -> Located {
    let checked = hir_check::check(project.modules(), project.standard_sources());
    hir_check::located_diagnostics(&checked.findings)
}

/// Writes the difference between the two checkers to stderr, one line per
/// diagnostic that only one side reports.
fn report_shadow(old: &Located, hir: &Located) {
    let old_keys = keys(old);
    let hir_keys = keys(hir);
    if old_keys == hir_keys {
        eprintln!(
            "orna check shadow: agree on {} diagnostic(s)",
            old_keys.len()
        );
        return;
    }
    eprintln!(
        "orna check shadow: differ: checker {} diagnostic(s), hir {} diagnostic(s)",
        old_keys.len(),
        hir_keys.len()
    );
    for key in multiset_difference(&old_keys, &hir_keys) {
        eprintln!("orna check shadow: checker-only {key}");
    }
    for key in multiset_difference(&hir_keys, &old_keys) {
        eprintln!("orna check shadow: hir-only {key}");
    }
}

/// One sorted key per diagnostic: its code, logical path and primary span.
fn keys(located: &Located) -> Vec<String> {
    let (diagnostics, origins) = located;
    let mut keys: Vec<String> = diagnostics
        .iter()
        .zip(origins)
        .map(|(diagnostic, origin)| {
            let path = origin.logical_path.as_deref().unwrap_or("-");
            let span = origin.primary.as_ref().map_or_else(
                || "-".to_owned(),
                |range: &Range<usize>| format!("{}..{}", range.start, range.end),
            );
            format!("{} {path} {span}", diagnostic.code())
        })
        .collect();
    keys.sort();
    keys
}

/// The keys of `left` not matched by a key of `right`, counting repeats.
fn multiset_difference(left: &[String], right: &[String]) -> Vec<String> {
    let mut remaining = right.to_vec();
    left.iter()
        .filter(
            |key| match remaining.iter().position(|other| other == *key) {
                Some(index) => {
                    remaining.swap_remove(index);
                    false
                }
                None => true,
            },
        )
        .cloned()
        .collect()
}
