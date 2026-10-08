use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("boolean is canonical")
}

fn session() -> AdmittedReplSession {
    AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile is admitted")
}

// Ordinary float equality treats the two zeros as equal, while neither is
// strictly less than the other; both orderings hold in the non-strict forms.
#[test]
fn ordinary_float_comparison_treats_signed_zeros_as_equal() {
    let mut session = session();
    let source = include_str!("fixtures/stdlib-float-compare-signed-zero-ln4y7.orna");
    assert_eq!(session.submit(source), Ok(Some(bool_value(true))), "{source}");
}
