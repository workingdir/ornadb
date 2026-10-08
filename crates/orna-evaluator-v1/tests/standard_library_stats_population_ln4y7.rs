use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::{Raw, Value};

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected value is canonical")
}

fn optional_int(value: i64) -> CanonicalValue {
    canonical(
        Value::option(Some(Value::int(value.into())))
            .expect("integer option is valid")
            .raw()
            .clone(),
    )
}

fn optional_none() -> CanonicalValue {
    canonical(
        Value::option(None)
            .expect("absent option is valid")
            .raw()
            .clone(),
    )
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

// The population variance and standard deviation documented in stats.orna:
// one value has zero spread, and empty input has no estimate and returns null
// rather than dividing by zero.
#[test]
fn stats_population_spread_of_one_value_is_zero_and_empty_input_is_null() {
    let mut session = session();
    for (source, expected) in [
        ("stats.variance([5])", optional_int(0)),
        ("stats.standard_deviation([7])", optional_int(0)),
        ("stats.variance([])", optional_none()),
        ("stats.standard_deviation([])", optional_none()),
    ] {
        assert_eq!(session.submit(source), Ok(Some(expected)), "{source}");
    }
}
