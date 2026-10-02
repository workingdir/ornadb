use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

#[test]
fn pinned_concurrent_module_imports_and_requires_its_clock_binding() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-concurrent-zhw5h.orna")),
        Ok(None)
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-call-concurrent-sleep-zhw5h.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-UNSUPPORTED"
    );
}

#[test]
fn core_remains_available_without_std_and_concurrent_import_is_not_filled_in() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-core-without-concurrent-zhw5h.orna")),
        Ok(Some(CanonicalValue::new(Raw::Bool(true)).unwrap()))
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-concurrent-without-snapshot-zhw5h.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-core-without-concurrent-zhw5h.orna")),
        Ok(Some(CanonicalValue::new(Raw::Bool(true)).unwrap()))
    );
}
