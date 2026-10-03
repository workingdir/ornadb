use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_test_expectations_are_importable_and_core_assertions_need_no_std() {
    let mut with_std = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-use-test-fqjge.orna")),
        Ok(None)
    );
    let expectations = with_std.submit(include_str!("fixtures/stdlib-test-expectations-fqjge.orna"));
    let expectation_error = expectations.as_ref().err().map(|error| error.code().to_owned());
    assert_eq!(
        expectations,
        Ok(Some(bool_value(true))),
        "std.test expectations failed: {:?}",
        expectation_error
    );

    let mut without_std = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_std.submit(include_str!("fixtures/stdlib-core-assertion-function-without-test-fqjge.orna")),
        Ok(None)
    );
    let core_result = without_std.submit(include_str!("fixtures/stdlib-core-assertion-call-without-test-fqjge.orna"));
    let core_error = core_result.as_ref().err().map(|error| error.code().to_owned());
    assert_eq!(
        core_result,
        Ok(Some(bool_value(true))),
        "core assert without std failed: {:?}",
        core_error
    );
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-use-test-without-snapshot-fqjge.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        without_std.submit(include_str!("fixtures/stdlib-core-assertion-call-without-test-fqjge.orna")),
        Ok(Some(bool_value(true)))
    );
}
