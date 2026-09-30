use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).unwrap()
}

fn ints(values: &[i64]) -> Raw {
    Raw::Array(values.iter().map(|value| Raw::Int((*value).into())).collect())
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
            Raw::Array(vec![ints(&[10, 11])]),
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
        "ORNA-EVAL-VALUE"
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
