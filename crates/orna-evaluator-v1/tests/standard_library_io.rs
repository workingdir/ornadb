use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_filesystem_module_imports_and_fails_closed_without_a_host_binding() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-io-t7auz.orna")),
        Ok(None)
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-call-io-read-text-t7auz.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );
}

#[test]
fn core_numeric_operations_remain_available_without_optional_io_std() {
    let mut session = AdmittedReplSession::new(Limits::default());
    let core = include_str!("fixtures/stdlib-core-without-io-t7auz.orna");
    assert_eq!(session.submit(core), Ok(Some(bool_value(true))));
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-io-without-snapshot-t7auz.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(session.submit(core), Ok(Some(bool_value(true))));
}
