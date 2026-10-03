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

#[test]
fn paired_lateral_anchors_keep_distinct_identity_across_recursive_folds() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile is admitted");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-query-2213.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-collection-4tksn.orna")),
        Ok(None)
    );
    let actual = session
        .submit(include_str!("fixtures/stdlib-query-paired-lateral-recursive-cte-4tksn.orna"))
        .unwrap_or_else(|error| panic!("lateral recursive CTE evaluation failed with {}", error.code()));

    let expected = CanonicalValue::new(Raw::Tag(
        60015,
        Box::new(Raw::Array(vec![
            raw_integers(&[101, 201]),
            raw_integers(&[301, 201]),
        ])),
    ))
    .expect("paired lateral recursive results are canonical");
    assert_eq!(
        actual,
        Some(expected),
        "each sparse lateral anchor preserves flattened first-seen values in its own recursive fold"
    );
}

#[test]
fn paired_recursive_anchors_keep_identity_through_sparse_window_frame_folds() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile is admitted");
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
            "fixtures/stdlib-query-paired-recursive-window-fold-i7cej.orna"
        ))
        .unwrap_or_else(|error| panic!("paired recursive window folds failed with {}", error.code()));
    let expected = CanonicalValue::new(Raw::Tag(
        60015,
        Box::new(Raw::Array(vec![
            raw_integers(&[101, 201, 301]),
            raw_integers(&[301, 201, 401]),
        ])),
    ))
    .expect("paired sparse window fold result is canonical");

    assert_eq!(
        actual,
        Some(expected),
        "each recursive anchor keeps its own breadth-first identity order through overlapping complete frames and both distinct folds"
    );
}

#[test]
fn paired_recursive_window_folds_keep_empty_short_lane_independent() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile is admitted");
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
            "fixtures/stdlib-query-paired-recursive-window-short-fold-i7cej.orna"
        ))
        .unwrap_or_else(|error| panic!("short paired recursive window folds failed with {}", error.code()));
    let expected = CanonicalValue::new(Raw::Tag(
        60015,
        Box::new(Raw::Array(vec![
            raw_integers(&[]),
            raw_integers(&[20, 10]),
        ])),
    ))
    .expect("short paired window fold result is canonical");

    assert_eq!(
        actual,
        Some(expected),
        "an anchor shorter than the frame width folds to an empty result without suppressing a sibling's complete frame"
    );
}
