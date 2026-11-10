//! The old checker's resolution oracle for stage H1a.
//!
//! The comparison H1 asks for is "for every name in every corpus program, the
//! HIR resolver and the old checker agree on resolved or unresolved, and on
//! `ORNA-S011` ambiguity". The checker's only per-name answer that is public is
//! its diagnostics: an unresolved name carries the spelling in a
//! `unresolved name: NAME` note, and an ambiguity is counted. Neither carries a
//! source span, so the comparison is by name and by count, not by span.
//!
//! The checker analyses the case's own sources, exactly as the old pipeline's
//! admission does; the standard library reaches it through the catalogue, so
//! standard source is not re-analysed here and is not part of this comparison.

use std::collections::BTreeSet;

use orna_check::{DIAG_AMBIGUOUS, DIAG_UNRESOLVED, ModuleInput, analyze_with_catalogue};

use crate::case::{Case, CaseKind};
use crate::observe_new::ResolutionObservation;
use crate::observe_old::catalogue_for;

/// The checker's unresolved and ambiguous answers for one case.
pub fn resolution(case: &Case) -> Result<ResolutionObservation, String> {
    if case.kind == CaseKind::Repl {
        return Err(format!(
            "{}: checker resolution is a module analysis; the REPL path is stage H8",
            case.id
        ));
    }
    let inputs = case
        .sources
        .iter()
        .map(|source| ModuleInput {
            logical_path: source.logical_path.clone(),
            // An expression fixture is not a module; the checker analyses
            // modules, so the expression is wrapped without moving its names.
            source: match case.kind {
                CaseKind::Expression => format!("fn run() = {};\n", source.text),
                _ => source.text.clone(),
            },
            prelude_exports: BTreeSet::new(),
        })
        .collect::<Vec<_>>();
    let catalogue = catalogue_for(case)?;
    let analysis = analyze_with_catalogue(&inputs, catalogue);
    let mut observation = ResolutionObservation::default();
    for diagnostic in &analysis.diagnostics {
        match diagnostic.code() {
            DIAG_UNRESOLVED => match orna_check::unresolved_name(diagnostic) {
                Some(name) => {
                    *observation.unresolved.entry(name.to_owned()).or_insert(0) += 1;
                }
                None => observation.unresolved_without_name += 1,
            },
            DIAG_AMBIGUOUS => observation.ambiguous += 1,
            _ => {}
        }
    }
    Ok(observation)
}
