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
        session.submit(include_str!("fixtures/stdlib-use-math-2189.orna")),
        Ok(None)
    );
    session
}

// Reversed clamp bounds saturate to the lower bound for every value, matching
// the documented rule in std.time (`reversed bounds return the lower bound`).
#[test]
fn math_clamp_with_reversed_bounds_saturates_to_the_lower_bound() {
    let mut session = session();
    let source = include_str!("fixtures/stdlib-math-clamp-inverted-bounds-ln4y7.orna");
    assert_eq!(session.submit(source), Ok(Some(bool_value(true))), "{source}");
}
