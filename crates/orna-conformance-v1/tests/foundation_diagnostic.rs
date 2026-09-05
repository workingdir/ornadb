use num_bigint::BigInt;
use orna_conformance_v1::{StageOutcome, shared_diagnostic_outcome};
use orna_foundation_v1::{
    CanonicalSnapshot, Diagnostic, DiagnosticSeverity, DiagnosticSpan, GitHash, SafeText,
};

#[test]
fn shared_foundation_diagnostic_survives_stage_outcome_matching_and_evidence_json() {
    let snapshot = CanonicalSnapshot::Commit {
        database: [1; 16],
        algorithm: GitHash::Sha256,
        oid: vec![2; 32],
    };
    let diagnostic = Diagnostic::new(
        SafeText::new("ORNA091-E-VAR").unwrap(),
        DiagnosticSeverity::Error,
        SafeText::new("use let").unwrap(),
    )
    .unwrap()
    .with_span(
        DiagnosticSpan::new(
            snapshot,
            "src/main.orna",
            BigInt::from(u64::MAX) + 1,
            BigInt::from(u64::MAX) + 2,
        )
        .unwrap(),
    );
    let outcome = shared_diagnostic_outcome(diagnostic);
    let StageOutcome::Failed(actual) = outcome else {
        panic!("shared helper must retain a failed diagnostic")
    };
    assert_eq!(actual.code(), "ORNA091-E-VAR");
    assert!(actual.message().contains("use let"));
    let evidence = serde_json::to_value(actual).unwrap();
    assert_eq!(evidence["code"], "ORNA091-E-VAR");
    assert_eq!(evidence["spans"][0]["start-byte"], "18446744073709551616");
}
