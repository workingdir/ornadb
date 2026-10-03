use orna_evaluator_v1::{Environment, Limits, evaluate_expression};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn list(values: impl IntoIterator<Item = Raw>) -> Raw {
    Raw::Array(values.into_iter().collect())
}

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected runtime proof value is canonical")
}

#[test]
fn stream_and_concurrency_runtime_primitives_compute_documented_values() {
    let actual = evaluate_expression(
        include_str!("fixtures/stdlib-real-bodies4-runtime-primitives.orna"),
        &Environment::new(),
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("runtime primitive fixture failed with {}", error.code()));

    assert_eq!(
        actual,
        canonical(list([
            list([
                list([Raw::Int(1.into()), Raw::Int(2.into())]),
                list([Raw::Int(3.into())])
            ]),
            list([Raw::Int(17.into()), Raw::Int(23.into())]),
        ]))
    );
}
