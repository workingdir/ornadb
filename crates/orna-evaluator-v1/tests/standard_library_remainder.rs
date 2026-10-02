use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::{Raw, Value};

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).unwrap()
}

fn ints(values: &[i64]) -> Raw {
    Raw::Array(values.iter().map(|value| Raw::Int((*value).into())).collect())
}

fn option_int(value: i64) -> CanonicalValue {
    CanonicalValue::new(
        Value::option(Some(Value::int(value.into())))
            .unwrap()
            .raw()
            .clone(),
    )
    .unwrap()
}

fn option_decimal(coefficient: i64, exponent10: i64) -> CanonicalValue {
    let value = Value::decimal(coefficient.into(), exponent10.into()).unwrap();
    CanonicalValue::new(Value::option(Some(value)).unwrap().raw().clone()).unwrap()
}

fn pinned_collection_session() -> AdmittedReplSession {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-collection-alias-yfifu.orna")),
        Ok(None)
    );
    session
}

fn assert_collection_proofs(session: &mut AdmittedReplSession, source: &str, proofs: usize) {
    let parsed = orna_syntax_v1::parse_repl(source);
    assert!(parsed.is_ok(), "collection fixture syntax: {:?}", parsed.diagnostics);
    let actual = session
        .submit(source)
        .unwrap_or_else(|error| panic!("pinned collection callback fixture rejected: {}", error.code()));
    assert_eq!(
        actual,
        Some(canonical(Raw::Array(vec![Raw::Bool(true); proofs])))
    );
}

#[test]
fn pinned_collection_helpers_bind_for_collection_and_query_exports() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-collection-2213.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-query-2213.orna")),
        Ok(None)
    );
    let proofs = [
        ("chunk", include_str!("fixtures/stdlib-collection-chunk-2213.orna")),
        ("flatten", include_str!("fixtures/stdlib-collection-flatten-2213.orna")),
        ("unique", include_str!("fixtures/stdlib-collection-unique-2213.orna")),
        ("window", include_str!("fixtures/stdlib-collection-window-2213.orna")),
    ];
    for (name, source) in proofs {
        assert_eq!(
            session.submit(source),
            Ok(Some(CanonicalValue::new(Raw::Bool(true)).unwrap())),
            "pinned {name} intrinsic proof failed"
        );
    }
    let tuple_results = [
        (
            "partition",
            include_str!("fixtures/stdlib-collection-partition-2213.orna"),
            Raw::Array(vec![ints(&[1, 3]), ints(&[2])]),
        ),
        (
            "zip",
            include_str!("fixtures/stdlib-collection-zip-2213.orna"),
            Raw::Array(vec![Raw::Array(vec![Raw::Int(1.into()), Raw::Text("a".into())])]),
        ),
        (
            "zip_exact",
            include_str!("fixtures/stdlib-collection-zip-exact-2213.orna"),
            Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), Raw::Text("a".into())]),
                Raw::Array(vec![Raw::Int(2.into()), Raw::Text("b".into())]),
            ]),
        ),
        (
            "group_by",
            include_str!("fixtures/stdlib-collection-group-by-2213.orna"),
            Raw::Array(vec![
                Raw::Array(vec![Raw::Int(1.into()), ints(&[31])]),
                Raw::Array(vec![Raw::Int(2.into()), ints(&[12, 22])]),
                Raw::Array(vec![Raw::Int(3.into()), ints(&[13, 33])]),
            ]),
        ),
        (
            "pairs",
            include_str!("fixtures/stdlib-collection-pairs-2213.orna"),
            Raw::Array(vec![ints(&[1, 2]), ints(&[2, 3])]),
        ),
        (
            "rank",
            include_str!("fixtures/stdlib-collection-rank-2213.orna"),
            Raw::Array(vec![ints(&[1, 1]), ints(&[1, 1]), ints(&[2, 3]), ints(&[3, 4])]),
        ),
        (
            "asof_join",
            include_str!("fixtures/stdlib-collection-asof-join-2213.orna"),
            Raw::Array(vec![Raw::Array(vec![
                Raw::Int(10.into()),
                Value::option(Some(Value::int(9.into())))
                    .unwrap()
                    .raw()
                    .clone(),
            ])]),
        ),
        (
            "split_when",
            include_str!("fixtures/stdlib-collection-split-when-2213.orna"),
            Raw::Array(vec![ints(&[1]), ints(&[2]), ints(&[2, 3])]),
        ),
    ];
    for (name, source, expected) in tuple_results {
        assert_eq!(
            session.submit(source),
            Ok(Some(canonical(expected))),
            "pinned {name} intrinsic result"
        );
    }
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-zip-exact-fails-2213.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );
}

#[test]
fn pinned_collection_transform_callbacks_keep_captured_values() {
    let mut session = pinned_collection_session();
    assert_collection_proofs(
        &mut session,
        include_str!("fixtures/stdlib-collection-captured-transform-callbacks-yfifu.orna"),
        5,
    );
}

#[test]
fn pinned_collection_predicate_callbacks_keep_captured_values() {
    let mut session = pinned_collection_session();
    assert_collection_proofs(
        &mut session,
        include_str!("fixtures/stdlib-collection-captured-predicate-callbacks-yfifu.orna"),
        6,
    );
}

#[test]
fn pinned_collection_asof_selectors_keep_captured_values() {
    let mut session = pinned_collection_session();
    let source = include_str!("fixtures/stdlib-collection-captured-asof-selectors-yfifu.orna");
    let parsed = orna_syntax_v1::parse_repl(source);
    assert!(parsed.is_ok(), "collection fixture syntax: {:?}", parsed.diagnostics);
    let actual = session
        .submit(source)
        .unwrap_or_else(|error| panic!("pinned as-of callback fixture rejected: {}", error.code()));
    assert_eq!(
        actual,
        Some(canonical(Raw::Array(vec![
            Raw::Array(vec![
                Raw::Array(vec![
                    Raw::Int(5.into()),
                    Value::option(Some(Value::int(1.into())))
                        .unwrap()
                        .raw()
                        .clone(),
                ]),
            ]),
            Raw::Array(vec![
                Raw::Array(vec![
                    Raw::Int(11.into()),
                    Value::option(Some(Value::int(1.into())))
                        .unwrap()
                        .raw()
                        .clone(),
                ]),
            ]),
        ])))
    );
}

#[test]
fn pinned_bits_exports_bind_and_remain_optional_without_std() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-bits-2213.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-bits-operations-2213.orna")),
        Ok(Some(CanonicalValue::new(Raw::Bool(true)).unwrap()))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-bits-negative-shift-2213.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );

    let mut without_std = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-bits-without-snapshot-2213.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S012-UNRESOLVED"
    );
}

#[test]
fn pinned_stats_aggregates_require_the_captured_std_module() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-stats-ymou.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-stats-z09xc.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-stats-empty-list-ymou.orna")),
        Ok(None)
    );
    for (source, expected) in [
        (
            include_str!("fixtures/stdlib-stats-mean-exact-ymou.orna"),
            option_int(2),
        ),
        (
            include_str!("fixtures/stdlib-stats-mean-rounded-ymou.orna"),
            option_int(2),
        ),
        (
            include_str!("fixtures/stdlib-stats-median-ymou.orna"),
            option_int(5),
        ),
        (
            include_str!("fixtures/stdlib-stats-median-rounded-ymou.orna"),
            option_decimal(2, 0),
        ),
        (
            include_str!("fixtures/stdlib-stats-percentile-linear-ymou.orna"),
            option_decimal(25, -1),
        ),
        (
            include_str!("fixtures/stdlib-stats-percentile-lower-ymou.orna"),
            option_decimal(0, 0),
        ),
        (
            include_str!("fixtures/stdlib-stats-empty-ymou.orna"),
            CanonicalValue::new(Raw::Null).unwrap(),
        ),
    ] {
        let actual = session
            .submit(source)
            .unwrap_or_else(|error| panic!("{source}: {}", error.code()));
        assert_eq!(actual, Some(expected), "{source}");
    }
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-stats-inexact-ymou.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE",
        "an inexact exact-number mean needs an explicit scale and rounding mode"
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-stats-complete-behavior-yn4vz.orna")),
        Ok(Some(CanonicalValue::new(Raw::Bool(true)).unwrap())),
        "each stats export computes a result through the pinned source wrapper"
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-query-ymou.orna")),
        Ok(None)
    );
    for (source, expected) in [
        (
            include_str!("fixtures/stdlib-query-aggregations-ymou.orna"),
            CanonicalValue::new(Raw::Int(6.into())).unwrap(),
        ),
        (include_str!("fixtures/stdlib-query-min-ymou.orna"), option_int(1)),
        (include_str!("fixtures/stdlib-query-max-ymou.orna"), option_int(3)),
        (
            include_str!("fixtures/stdlib-query-count-ymou.orna"),
            CanonicalValue::new(Raw::Int(2.into())).unwrap(),
        ),
    ] {
        assert_eq!(session.submit(source), Ok(Some(expected)), "{source}");
    }

    let mut without_std = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-stats-without-snapshot-ymou.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S012-UNRESOLVED"
    );
}

#[test]
fn pinned_bits_use_unbounded_signed_twos_complement_operations() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-bits-2213.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-bits-signed-ymou.orna")),
        Ok(Some(CanonicalValue::new(Raw::Bool(true)).unwrap()))
    );
}
