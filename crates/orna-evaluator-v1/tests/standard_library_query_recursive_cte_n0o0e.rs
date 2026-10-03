use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn integers(values: &[i64]) -> CanonicalValue {
    CanonicalValue::new(Raw::Array(
        values.iter().copied().map(|value| Raw::Int(value.into())).collect(),
    ))
    .expect("expected integer list is canonical")
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
