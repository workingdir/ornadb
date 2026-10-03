use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn raw_ints(values: &[i64]) -> Raw {
    Raw::Array(values.iter().copied().map(|value| Raw::Int(value.into())).collect())
}

#[test]
fn paired_sparse_window_distinct_folds_keep_frame_and_lane_identity() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference standard profile failed with {}", error.code()));
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-collection-4tksn.orna")),
        Ok(None)
    );

    for (source, stage) in [
        (
            include_str!("fixtures/stdlib-query-paired-window-fold-left-filter-y5hv5.orna"),
            "left sparse filter",
        ),
        (
            include_str!("fixtures/stdlib-query-paired-window-fold-left-frames-y5hv5.orna"),
            "left window frames",
        ),
        (
            include_str!("fixtures/stdlib-query-paired-window-fold-left-frame-distinct-y5hv5.orna"),
            "left frame distinct fold",
        ),
        (
            include_str!("fixtures/stdlib-query-paired-window-fold-left-final-y5hv5.orna"),
            "left outer distinct fold",
        ),
        (
            include_str!("fixtures/stdlib-query-paired-window-fold-right-filter-y5hv5.orna"),
            "right sparse filter",
        ),
        (
            include_str!("fixtures/stdlib-query-paired-window-fold-right-frames-y5hv5.orna"),
            "right window frames",
        ),
        (
            include_str!("fixtures/stdlib-query-paired-window-fold-right-frame-distinct-y5hv5.orna"),
            "right frame distinct fold",
        ),
        (
            include_str!("fixtures/stdlib-query-paired-window-fold-right-final-y5hv5.orna"),
            "right outer distinct fold",
        ),
    ] {
        assert_eq!(session.submit(source), Ok(None), "{stage}: {source}");
    }

    let frames = session
        .submit(include_str!(
            "fixtures/stdlib-query-paired-window-frame-folds-y5hv5.orna"
        ))
        .unwrap_or_else(|error| panic!("paired frame folds failed with {}", error.code()));
    let integer = |value: i64| Raw::Int(value.into());
    let frame = |values: &[i64]| {
        Raw::Array(values.iter().copied().map(integer).collect())
    };
    let expected_frames = CanonicalValue::new(Raw::Tag(
        60015,
        Box::new(Raw::Array(vec![
            Raw::Array(vec![frame(&[1]), frame(&[1, 2])]),
            Raw::Array(vec![frame(&[2, 1]), frame(&[1, 2])]),
        ])),
    ))
    .expect("paired distinct frame output is canonical");
    assert_eq!(
        frames,
        Some(expected_frames),
        "each sparse frame folds duplicate members in first-seen order before the outer cascade"
    );

    let left = session
        .submit(include_str!(
            "fixtures/stdlib-query-paired-window-fold-result-y5hv5.orna"
        ))
        .unwrap_or_else(|error| panic!("paired sparse window distinct folds failed with {}", error.code()));
    let expected = CanonicalValue::new(Raw::Tag(
        60015,
        Box::new(Raw::Array(vec![raw_ints(&[1, 2]), raw_ints(&[2, 1])])),
    ))
    .expect("paired sparse fold output is canonical");
    assert_eq!(
        left,
        Some(expected),
        "overlapping frame folds retain original members while independent outer folds keep their own first-seen lane order"
    );
}
