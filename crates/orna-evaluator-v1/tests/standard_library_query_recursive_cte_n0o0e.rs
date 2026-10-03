use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn integers(values: &[i64]) -> CanonicalValue {
    CanonicalValue::new(Raw::Array(
        values.iter().copied().map(|value| Raw::Int(value.into())).collect(),
    ))
    .expect("expected integer list is canonical")
}

fn raw_integers(values: &[i64]) -> Raw {
    Raw::Array(
        values.iter().copied().map(|value| Raw::Int(value.into())).collect(),
    )
}

#[test]
fn recursive_cte_folds_identity_across_sparse_anchor_and_recursive_rounds() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile is admitted");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-query-2213.orna")),
        Ok(None)
    );
    let actual = session
        .submit(include_str!("fixtures/stdlib-query-recursive-cte-n0o0e.orna"))
        .unwrap_or_else(|error| panic!("recursive CTE evaluation failed with {}", error.code()));

    assert_eq!(actual, Some(integers(&[0, 2, 1, 3, 4, 5, 6, 7, 8])));
}

#[test]
fn paired_recursive_cte_folds_keep_independent_first_seen_identities() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile is admitted");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-query-2213.orna")),
        Ok(None)
    );

    let actual = session
        .submit(include_str!("fixtures/stdlib-query-paired-recursive-cte-o2f65.orna"))
        .unwrap_or_else(|error| panic!("paired recursive CTE evaluation failed with {}", error.code()));
    let expected = CanonicalValue::new(Raw::Tag(
        60015,
        Box::new(Raw::Array(vec![
            raw_integers(&[0, 2, 1, 3, 4, 5, 6, 7, 8]),
            raw_integers(&[3, 1, 4, 5, 2, 6, 7]),
        ])),
    ))
    .expect("paired recursive result is canonical");

    assert_eq!(
        actual,
        Some(expected),
        "each recursive fold keeps its own first anchor and recursive value for overlapping identities"
    );
}
