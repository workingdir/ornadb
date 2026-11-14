//! The checker behind the semantic diagnostics of a project (`docs/design/hir-cutover.md`,
//! gap G8).
//!
//! The old checker stays the default. `ORNA_LSP_HIR=1` publishes the diagnostics
//! HIR reports (`orna_check::hir_check`) instead. `ORNA_LSP_SHADOW=1` runs both,
//! publishes the old checker's diagnostics, and writes the differences to the
//! server's stderr; it never changes what is published. Shadow takes precedence
//! when both variables are set. The variables are read on every check, as
//! `orna check` reads `ORNA_CHECK_HIR` and `ORNA_CHECK_SHADOW`.

use std::collections::BTreeMap;

use orna_check::hir_check;
use orna_check::{
    Catalogue, DiagnosticOrigin, StandardDependencyProfile, analyze_with_catalogue_located,
};
use orna_hir::PreludeExports;
use orna_project::LoadedProject;
use orna_standard_sources::{
    REFERENCE_STANDARD_PRELUDE_EXPORTS_V1, REFERENCE_STANDARD_PRELUDE_PATH_V1,
};

/// The snapshot name of the reference standard library, as
/// `orna_evaluator::reference_standard_profile` names it.
const REFERENCE_STANDARD_SNAPSHOT: &str = "orna.std/v1-reference-library";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Backend {
    Checker,
    Hir,
    Shadow,
}

/// One semantic diagnostic, with the origin the old checker reports for it.
pub(crate) struct Finding {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) origin: DiagnosticOrigin,
}

/// The semantic diagnostics of `loaded` from the backend the environment selects.
///
/// `sources` maps each module's logical path to its text; the shadow report uses it
/// for line numbers.
pub(crate) fn semantic_findings(
    loaded: &LoadedProject,
    catalogue: &Catalogue,
    sources: &BTreeMap<&str, &str>,
) -> Vec<Finding> {
    match backend_from_env() {
        Backend::Checker => checker_findings(loaded, catalogue),
        Backend::Hir => hir_findings(loaded),
        Backend::Shadow => {
            let checker = checker_findings(loaded, catalogue);
            report_shadow(sources, &checker, &hir_findings(loaded));
            checker
        }
    }
}

fn backend_from_env() -> Backend {
    if env_is_one("ORNA_LSP_SHADOW") {
        Backend::Shadow
    } else if env_is_one("ORNA_LSP_HIR") {
        Backend::Hir
    } else {
        Backend::Checker
    }
}

fn env_is_one(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| value == "1")
}

fn checker_findings(loaded: &LoadedProject, catalogue: &Catalogue) -> Vec<Finding> {
    let located = analyze_with_catalogue_located(loaded.modules(), catalogue);
    located
        .analysis
        .diagnostics
        .iter()
        .zip(located.origins)
        .map(|(diagnostic, origin)| Finding {
            code: diagnostic.code().to_owned(),
            message: diagnostic.message().to_owned(),
            origin,
        })
        .collect()
}

fn hir_findings(loaded: &LoadedProject) -> Vec<Finding> {
    let checked = hir_check::check(
        loaded.modules(),
        loaded.standard_sources(),
        &reference_prelude(),
    );
    let (diagnostics, origins) = hir_check::located_diagnostics(&checked.findings);
    diagnostics
        .iter()
        .zip(origins)
        .map(|(diagnostic, origin)| Finding {
            code: diagnostic.code().to_owned(),
            message: diagnostic.message().to_owned(),
            origin,
        })
        .collect()
}

/// The `as _` exports of the reference standard library, the prelude `orna check`
/// gives HIR. Only the prelude exports of a profile are read, so the profile is
/// built without hashing the standard sources.
fn reference_prelude() -> PreludeExports {
    let profile = StandardDependencyProfile::empty(REFERENCE_STANDARD_SNAPSHOT)
        .expect("the reference standard snapshot name is not empty")
        .with_module_prelude_exports(
            REFERENCE_STANDARD_PRELUDE_PATH_V1,
            REFERENCE_STANDARD_PRELUDE_EXPORTS_V1.iter().copied(),
        );
    hir_check::prelude_of_profile(&profile)
}

/// Writes the diagnostics on which the two checkers differ to stderr, one line
/// per diagnostic that only one side reports.
fn report_shadow(sources: &BTreeMap<&str, &str>, checker: &[Finding], hir: &[Finding]) {
    let checker_keys = shadow_keys(checker, sources);
    let hir_keys = shadow_keys(hir, sources);
    if checker_keys == hir_keys {
        eprintln!(
            "orna-lsp shadow: agree on {} diagnostic(s)",
            checker_keys.len()
        );
        return;
    }
    eprintln!(
        "orna-lsp shadow: differ: checker {} diagnostic(s), hir {} diagnostic(s)",
        checker_keys.len(),
        hir_keys.len()
    );
    for key in multiset_difference(&checker_keys, &hir_keys) {
        eprintln!("orna-lsp shadow: checker-only {key}");
    }
    for key in multiset_difference(&hir_keys, &checker_keys) {
        eprintln!("orna-lsp shadow: hir-only {key}");
    }
}

/// One sorted key per finding: its code, logical path and 1-based line.
///
/// Shared codes can anchor on different tokens of one statement, so the shadow
/// report compares lines rather than byte spans.
fn shadow_keys(findings: &[Finding], sources: &BTreeMap<&str, &str>) -> Vec<String> {
    let mut keys: Vec<String> = findings
        .iter()
        .map(|finding| {
            let path = finding.origin.logical_path.as_deref().unwrap_or("-");
            let line = match (finding.origin.primary.as_ref(), sources.get(path)) {
                (Some(range), Some(source)) => line_at(source, range.start).to_string(),
                _ => "-".to_owned(),
            };
            format!("{} {path} line {line}", finding.code)
        })
        .collect();
    keys.sort();
    keys
}

/// The 1-based line that holds byte `offset` of `source`.
fn line_at(source: &str, offset: usize) -> usize {
    let end = offset.min(source.len());
    source.as_bytes()[..end]
        .split(|byte| *byte == b'\n')
        .count()
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

#[cfg(test)]
mod tests {
    use super::{line_at, multiset_difference};

    #[test]
    fn line_at_counts_newlines_before_the_offset() {
        let source = "a\nbc\n";
        assert_eq!(line_at(source, 0), 1);
        assert_eq!(line_at(source, 2), 2);
        assert_eq!(line_at(source, 99), 3);
    }

    #[test]
    fn multiset_difference_counts_repeated_keys() {
        let left = ["a", "a", "b"].map(str::to_owned);
        let right = ["a", "c"].map(str::to_owned);
        assert_eq!(multiset_difference(&left, &right), ["a", "b"]);
        assert_eq!(multiset_difference(&right, &left), ["c"]);
    }
}
