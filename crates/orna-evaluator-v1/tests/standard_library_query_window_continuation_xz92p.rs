use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::{Raw, Value};

fn optional_int(value: i64) -> Raw {
    Value::option(Some(Value::int(value.into())))
        .expect("integer option is valid")
        .raw()
        .clone()
}

fn optional_ints(values: &[i64]) -> CanonicalValue {
    CanonicalValue::new(Raw::Array(
        values.iter().copied().map(optional_int).collect(),
    ))
    .expect("expected values are canonical")
}

fn session() -> AdmittedReplSession {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference standard profile failed with {}", error.code()));
    for import in [
        include_str!("fixtures/stdlib-use-query-2213.orna"),
        include_str!("fixtures/stdlib-use-stats-z09xc.orna"),
    ] {
        assert_eq!(session.submit(import), Ok(None), "import: {import}");
    }
    session
}

#[test]
fn sparse_windows_continue_across_deeper_steps_with_exact_statistics() {
    let mut session = session();
    let rate = session
        .submit(include_str!(
            "fixtures/stdlib-query-window-rate-continuation-xz92p.orna"
        ))
        .unwrap_or_else(|error| panic!("continuation window rates failed with {}", error.code()));
    assert_eq!(rate, Some(optional_ints(&[2, 2, 2, 2])));

    let integral = session
        .submit(include_str!(
            "fixtures/stdlib-query-window-integrate-continuation-xz92p.orna"
        ))
        .unwrap_or_else(|error| {
            panic!("continuation window integrals failed with {}", error.code())
        });
    assert_eq!(integral, Some(optional_ints(&[9, 56, 75, 111])));

    let derivative = session
        .submit(include_str!(
            "fixtures/stdlib-query-window-derivative-continuation-xz92p.orna"
        ))
        .unwrap_or_else(|error| {
            panic!(
                "continuation window derivatives failed with {}",
                error.code()
            )
        });
    assert_eq!(
        derivative,
        Some(CanonicalValue::new(Raw::Bool(true)).unwrap())
    );
}

#[test]
fn sparse_window_order_validation_reaches_reversal_inside_later_skipped_points() {
    let error = session()
        .submit(include_str!(
            "fixtures/stdlib-query-window-reversal-in-continuation-xz92p.orna"
        ))
        .expect_err("ordering must be checked through points skipped between windows");
    assert_eq!(error.code(), "ORNA-EVAL-ERROR");
}
