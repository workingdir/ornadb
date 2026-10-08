use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("boolean is canonical")
}

fn session() -> AdmittedReplSession {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile is admitted");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-money-rl767.orna")),
        Ok(None)
    );
    session
}

// A tie whose rounded quotient is zero rounds to the even neighbour, zero, for
// both signs; a tie at 1.5 rounds up to the even 2.
#[test]
fn decimal_half_even_rounds_zero_quotient_ties_to_even() {
    let mut session = session();
    let source = include_str!("fixtures/stdlib-decimal-half-even-zero-quotient-ln4y7.orna");
    assert_eq!(session.submit(source), Ok(Some(bool_value(true))), "{source}");
}
