//! Focused negative evidence for protocol-member and overlap diagnostics.

use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};

const GENERIC_FAILURES: &str = include_str!("fixtures/traceability-generic-failures.orna");

fn diagnostics() -> Vec<orna_foundation_v1::Diagnostic> {
    analyze_with_catalogue(
        &[ModuleInput::new("main.orna", GENERIC_FAILURES)],
        &Catalogue::authoritative_fixture(),
    )
    .diagnostics
}

fn diagnostic_text(diagnostics: &[orna_foundation_v1::Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
        .collect::<Vec<_>>()
        .join(" | ")
}

// Specified: ORNA-GENERIC-011. Exists: a fixture with a required member
// omitted by a nested implementation. Passed: its semantic rejection only.
#[test]
fn incomplete_nested_protocol_implementation_is_rejected() {
    let diagnostics = diagnostics();
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == "ORNA-S021-TYPE"
                && diagnostic.message() == "missing protocol implementation member"
        }),
        "ORNA-GENERIC-011 boundary: {}",
        diagnostic_text(&diagnostics)
    );
}

// Specified: ORNA-GENERIC-014. Exists: a fixture with duplicate identical
// implementations. Passed: overlap is rejected independent of selection.
#[test]
fn overlapping_protocol_implementations_are_rejected() {
    let diagnostics = diagnostics();
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == "ORNA-S021-TYPE"
                && diagnostic.message() == "overlapping protocol implementations are invalid"
        }),
        "ORNA-GENERIC-014 boundary: {}",
        diagnostic_text(&diagnostics)
    );
}
