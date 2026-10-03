use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::{Raw, Value};

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected value is canonical")
}

fn ints(values: &[i64]) -> CanonicalValue {
    canonical(Raw::Array(
        values
            .iter()
            .map(|value| Raw::Int((*value).into()))
            .collect(),
    ))
}

fn optional_int(value: i64) -> CanonicalValue {
    canonical(
        Value::option(Some(Value::int(value.into())))
            .expect("integer option is valid")
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

#[test]
fn query_preserves_named_percentile_interpolation_inside_each_window() {
    let actual = session()
        .submit(include_str!(
            "fixtures/stdlib-query-window-percentile-azb4m.orna"
        ))
        .unwrap_or_else(|error| panic!("window percentile failed with {}", error.code()));
    assert_eq!(
        actual,
        Some(canonical(Raw::Array(vec![
            Value::option(Some(Value::int(3.into())))
                .unwrap()
                .raw()
                .clone(),
            Value::option(Some(Value::int(7.into())))
                .unwrap()
                .raw()
                .clone(),
        ])))
    );
}

#[test]
fn stats_percentile_endpoints_and_histogram_bounds_return_exact_values() {
    let mut session = session();
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-stats-percentile-lower-edge-azb4m.orna"
        )),
        Ok(Some(optional_int(2)))
    );
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-stats-percentile-upper-edge-azb4m.orna"
        )),
        Ok(Some(optional_int(8)))
    );
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-stats-histogram-boundaries-azb4m.orna"
        )),
        Ok(Some(ints(&[2, 3]))),
        "bins include their lower bounds, exclude intermediate upper bounds, and optionally include only the final upper bound"
    );
}

#[test]
fn sparse_windows_still_validate_percentile_and_histogram_contracts() {
    let mut session = session();
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-query-window-percentile-sparse-neuwd.orna"
        )),
        Ok(Some(ints(&[]))),
        "valid percentile options return an empty result when no complete windows exist"
    );
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-query-window-histogram-sparse-neuwd.orna"
        )),
        Ok(Some(ints(&[]))),
        "valid bins return an empty result when no complete windows exist"
    );

    for source in [
        include_str!("fixtures/stdlib-query-window-percentile-invalid-probability-neuwd.orna"),
        include_str!("fixtures/stdlib-query-window-percentile-invalid-method-neuwd.orna"),
        include_str!("fixtures/stdlib-query-window-histogram-overlapping-bins-neuwd.orna"),
    ] {
        let error = session
            .submit(source)
            .expect_err("invalid statistic options must fail without complete windows");
        assert_eq!(error.code(), "ORNA-EVAL-VALUE", "fixture: {source}");
    }
}
