use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_text_and_numeric_modules_expose_their_documented_behaviour() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for import in [
        include_str!("fixtures/stdlib-use-text-z09xc.orna"),
        include_str!("fixtures/stdlib-use-bits-z09xc.orna"),
        include_str!("fixtures/stdlib-use-math-z09xc.orna"),
        include_str!("fixtures/stdlib-use-stats-z09xc.orna"),
    ] {
        assert_eq!(session.submit(import), Ok(None));
    }
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-text-numeric-z09xc.orna")),
        Ok(Some(bool_value(true)))
    );
}

#[test]
fn exact_core_numeric_types_remain_available_without_std() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-core-numeric-z09xc.orna")),
        Ok(Some(bool_value(true)))
    );
}
