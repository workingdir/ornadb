use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn raw_ints(values: &[i64]) -> Raw {
    Raw::Array(values.iter().copied().map(|value| Raw::Int(value.into())).collect())
}

#[test]
fn paired_recursive_folds_preserve_sparse_distinct_anchor_order() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference standard profile failed with {}", error.code()));
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-query-2213.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-collection-4tksn.orna")),
        Ok(None)
    );

    let actual = session
        .submit(include_str!(
            "fixtures/stdlib-query-paired-recursive-distinct-anchors-t7vp3.orna"
        ))
        .unwrap_or_else(|error| panic!("paired recursive distinct anchors failed with {}", error.code()));
    let expected = CanonicalValue::new(Raw::Tag(
        60015,
        Box::new(Raw::Array(vec![raw_ints(&[10, 20, 30]), raw_ints(&[20, 10, 30])])),
    ))
    .expect("paired recursive distinct anchor output is canonical");

    assert_eq!(
        actual,
        Some(expected),
        "sparse pre-recursion distinct folds retain each lane's first anchor order while recursive duplicate candidates share only that lane's identity set"
    );
}
