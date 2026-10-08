use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("boolean is canonical")
}

fn session() -> AdmittedReplSession {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile is admitted");
    for import in [
        include_str!("fixtures/stdlib-use-money-rl767.orna"),
    ] {
        assert_eq!(session.submit(import), Ok(None), "import: {import}");
    }
    session
}

// An exact decimal has no remainder to round, so every rounding mode returns
// it unchanged; a half-up tie at scale zero moves away from zero.
#[test]
fn decimal_rounding_keeps_exact_values_and_breaks_scale_zero_ties_up() {
    let mut session = session();
    let source = include_str!("fixtures/stdlib-decimal-rounding-exact-and-scale0-ln4y7.orna");
    assert_eq!(session.submit(source), Ok(Some(bool_value(true))), "{source}");
}
