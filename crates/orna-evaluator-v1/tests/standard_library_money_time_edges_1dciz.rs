use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).unwrap()
}

fn pinned_session() -> AdmittedReplSession {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for source in [
        include_str!("fixtures/stdlib-money-use-1dciz.orna"),
        include_str!("fixtures/stdlib-time-use-1dciz.orna"),
    ] {
        let parsed = orna_syntax_v1::parse_repl(source);
        assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
        assert_eq!(session.submit(source), Ok(None));
    }
    session
}

#[test]
fn exact_money_rounding_covers_signed_ties_all_modes_and_allocation() {
    let mut session = pinned_session();
    let source = include_str!("fixtures/stdlib-money-rounding-edges-1dciz.orna");
    for assertion in source.split("&&").map(str::trim) {
        let parsed = orna_syntax_v1::parse_repl(assertion);
        assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
        let result = session.submit(assertion);
        assert_eq!(
            result,
            Ok(Some(canonical(Raw::Bool(true)))),
            "failed value assertion: {assertion}\n{result:?}"
        );
    }
}

#[test]
fn timezone_gap_and_overlap_boundaries_preserve_offsets_and_nanoseconds() {
    let mut session = pinned_session();
    let source = include_str!("fixtures/stdlib-timezone-edge-values-1dciz.orna");
    for assertion in source.split("&&").map(str::trim) {
        let parsed = orna_syntax_v1::parse_repl(assertion);
        assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
        let result = session.submit(assertion);
        assert_eq!(
            result,
            Ok(Some(canonical(Raw::Bool(true)))),
            "failed value assertion: {assertion}\n{result:?}"
        );
    }
}

#[test]
fn nonexistent_local_times_still_require_a_named_gap_adjustment() {
    let mut session = pinned_session();
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-timezone-gap-reject-1dciz.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-timezone-gap-invalid-policy-1dciz.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
}
