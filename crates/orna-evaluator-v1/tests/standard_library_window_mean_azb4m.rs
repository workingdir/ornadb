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

fn option_int(value: i64) -> Raw {
    Value::option(Some(Value::int(value.into())))
        .expect("integer option is valid")
        .raw()
        .clone()
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
fn query_aggregates_only_complete_stepped_windows_to_real_mean_values() {
    let mut session = session();
    let actual = session
        .submit(include_str!(
            "fixtures/stdlib-query-window-mean-azb4m.orna"
        ))
        .unwrap_or_else(|error| panic!("window mean failed with {}", error.code()));
    assert_eq!(
        actual,
        Some(canonical(Raw::Array(vec![
            option_int(4),
            option_int(10),
        ])))
    );
    let short = session
        .submit(include_str!(
            "fixtures/stdlib-query-window-mean-short-azb4m.orna"
        ))
        .unwrap_or_else(|error| panic!("short window aggregate failed with {}", error.code()));
    assert_eq!(
        short,
        Some(ints(&[])),
        "an input shorter than the window has no complete windows"
    );
}
