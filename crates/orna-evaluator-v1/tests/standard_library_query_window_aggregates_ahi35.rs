use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::{Raw, Value};

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected value is canonical")
}

fn int(value: i64) -> Raw {
    Raw::Int(value.into())
}

fn ints(values: &[i64]) -> CanonicalValue {
    canonical(Raw::Array(values.iter().copied().map(int).collect()))
}

fn optional_int(value: i64) -> Raw {
    Value::option(Some(Value::int(value.into())))
        .expect("integer option is valid")
        .raw()
        .clone()
}

fn optional_ints(values: &[i64]) -> CanonicalValue {
    canonical(Raw::Array(values.iter().copied().map(optional_int).collect()))
}

fn nested_ints(values: &[&[i64]]) -> CanonicalValue {
    canonical(Raw::Array(
        values
            .iter()
            .map(|values| Raw::Array(values.iter().copied().map(int).collect()))
            .collect(),
    ))
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
fn query_window_statistics_return_exact_values_for_overlapping_and_trailing_edges() {
    let mut session = session();
    let cases = [
        (
            include_str!("fixtures/stdlib-query-window-sum-ahi35.orna"),
            ints(&[5, 11]),
        ),
        (
            include_str!("fixtures/stdlib-query-window-median-ahi35.orna"),
            optional_ints(&[2, 4]),
        ),
        (
            include_str!("fixtures/stdlib-query-window-min-ahi35.orna"),
            optional_ints(&[1, 3]),
        ),
        (
            include_str!("fixtures/stdlib-query-window-max-ahi35.orna"),
            optional_ints(&[2, 4]),
        ),
        (
            include_str!("fixtures/stdlib-query-window-range-ahi35.orna"),
            optional_ints(&[1, 1]),
        ),
        (
            include_str!("fixtures/stdlib-query-window-mode-ahi35.orna"),
            nested_ints(&[&[2], &[4]]),
        ),
        (
            include_str!("fixtures/stdlib-query-window-variance-ahi35.orna"),
            optional_ints(&[1, 1]),
        ),
        (
            include_str!("fixtures/stdlib-query-window-stddev-ahi35.orna"),
            optional_ints(&[1, 1]),
        ),
        (
            include_str!("fixtures/stdlib-query-window-histogram-ahi35.orna"),
            nested_ints(&[&[2, 1], &[0, 2]]),
        ),
    ];

    for (source, expected) in cases {
        let actual = session
            .submit(source)
            .unwrap_or_else(|error| panic!("query window aggregate failed with {}", error.code()));
        assert_eq!(actual, Some(expected), "fixture: {source}");
    }
}

#[test]
fn query_window_aggregates_omit_input_shorter_than_the_window() {
    let actual = session()
        .submit(include_str!("fixtures/stdlib-query-window-aggregate-short-ahi35.orna"))
        .unwrap_or_else(|error| panic!("short query window aggregate failed with {}", error.code()));
    assert_eq!(actual, Some(ints(&[])));
}
