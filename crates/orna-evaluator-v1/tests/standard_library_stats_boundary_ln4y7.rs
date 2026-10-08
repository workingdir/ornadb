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
        include_str!("fixtures/stdlib-use-query-2213.orna"),
        include_str!("fixtures/stdlib-use-stats-z09xc.orna"),
    ] {
        assert_eq!(session.submit(import), Ok(None), "import: {import}");
    }
    session
}

// Equal values have zero population spread: the mean equals each value, so
// every squared deviation is zero and the variance is exactly 0.
#[test]
fn stats_population_spread_of_equal_values_is_zero() {
    let mut session = session();
    let source = include_str!("fixtures/stdlib-stats-population-boundary-ln4y7.orna");
    assert_eq!(
        session.submit(source),
        Ok(Some(bool_value(true))),
        "{source}"
    );
}
