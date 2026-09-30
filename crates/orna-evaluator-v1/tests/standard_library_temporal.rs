use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).unwrap()
}

#[test]
fn pinned_time_exports_use_the_captured_timezone_edition() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-time-7q1e.orna")),
        Ok(None)
    );
    for source in [
        include_str!("fixtures/stdlib-time-version-7q1e.orna"),
        include_str!("fixtures/stdlib-time-offsets-7q1e.orna"),
        include_str!("fixtures/stdlib-time-overlap-7q1e.orna"),
    ] {
        assert_eq!(
            session.submit(source),
            Ok(Some(canonical(Raw::Bool(true)))),
            "{source}"
        );
    }
    for source in [
        include_str!("fixtures/stdlib-time-ambiguous-reject-7q1e.orna"),
        include_str!("fixtures/stdlib-time-gap-7q1e.orna"),
    ] {
        assert_eq!(
            session.submit(source).unwrap_err().code(),
            "ORNA-EVAL-VALUE",
            "{source}"
        );
    }
}

#[test]
fn time_remains_an_optional_explicit_standard_module() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-without-snapshot-7q1e.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
}
