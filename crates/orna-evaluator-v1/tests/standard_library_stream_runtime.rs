use num_traits::ToPrimitive;
use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn int(value: i64) -> Raw {
    Raw::Int(value.into())
}

fn list(values: impl IntoIterator<Item = Raw>) -> Raw {
    Raw::Array(values.into_iter().collect())
}

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected value is canonical")
}

fn int_values(raw: &Raw) -> Vec<i64> {
    let Raw::Array(values) = raw else {
        panic!("expected an array, got {raw:?}");
    };
    values
        .iter()
        .map(|value| match value {
            Raw::Int(value) => value.to_i64().expect("fixture integer fits i64"),
            _ => panic!("expected an integer, got {value:?}"),
        })
        .collect()
}

fn is_subsequence(actual: &[i64], expected: &[i64]) -> bool {
    let mut expected = expected.iter();
    let mut next = expected.next();
    for value in actual {
        if next == Some(value) {
            next = expected.next();
        }
    }
    next.is_none()
}

#[test]
fn list_stream_batch_preserves_order_and_emits_the_short_final_batch() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile loads");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-stream-n8phe.orna")),
        Ok(None)
    );

    let actual = session
        .submit(include_str!("fixtures/stdlib-stream-runtime-m45oc.orna"))
        .unwrap_or_else(|error| panic!("stream batch failed with {}", error.code()));

    assert_eq!(
        actual,
        Some(canonical(list([
            list([int(1), int(2)]),
            list([int(3), int(4)]),
            list([int(5)]),
        ])))
    );
}

#[test]
fn finite_stream_operators_compute_real_sequences() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile loads");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-stream-n8phe.orna")),
        Ok(None)
    );

    let actual = session
        .submit(include_str!("fixtures/stdlib-stream-operators-m45oc.orna"))
        .unwrap_or_else(|error| panic!("stream operators failed with {}", error.code()));

    let actual = actual.expect("stream operators return a value");
    let Raw::Array(outputs) = actual.raw() else {
        panic!("stream operator result is not an array: {actual:?}");
    };
    assert_eq!(outputs.len(), 6);
    assert_eq!(outputs[0], list([int(1), int(2), int(3)]));
    let merged = int_values(&outputs[1]);
    assert_eq!(merged.len(), 5);
    let mut sorted = merged.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, [1, 2, 3, 4, 6]);
    assert!(
        is_subsequence(&merged, &[1, 3]),
        "left source order: {merged:?}"
    );
    assert!(
        is_subsequence(&merged, &[2, 4, 6]),
        "right source order: {merged:?}"
    );
    assert_eq!(outputs[2], list([int(7)]));
    assert_eq!(outputs[3], list([int(9)]));
    assert_eq!(outputs[4], list([int(10), int(11)]));
    assert_eq!(outputs[5], list([int(12), int(13)]));
}

#[test]
fn for_each_runs_each_callback_and_returns_unit() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile loads");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-stream-n8phe.orna")),
        Ok(None)
    );

    let actual = session
        .submit(include_str!("fixtures/stdlib-stream-for-each-m45oc.orna"))
        .unwrap_or_else(|error| panic!("stream consumer failed with {}", error.code()));

    assert_eq!(
        actual,
        Some(canonical(Raw::Tag(60014, Box::new(Raw::Array(vec![])))))
    );
}
