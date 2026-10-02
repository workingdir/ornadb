use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn int(value: i64) -> Raw {
    Raw::Int(value.into())
}

fn row(root: i64, middle: i64, leaf: i64, total: i64) -> Raw {
    Raw::Array(vec![int(root), int(middle), int(leaf), int(total)])
}

#[test]
fn paired_record_rebind_returns_each_saved_and_rebound_depth_value() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session.submit(include_str!(
            "fixtures/paired-rebind-depth-label-values.orna"
        )),
        Ok(None),
        "the executable fixture must admit real function bodies"
    );

    let actual = session
        .submit("paired_rebind_depth_values()")
        .unwrap_or_else(|error| panic!("paired depth computation failed: {}", error.code()));
    let expected = CanonicalValue::new(Raw::Array(vec![
        row(10, 1, 5, 16),
        row(20, 2, 6, 28),
        row(100, 3, 7, 110),
        row(200, 4, 8, 212),
    ]))
    .expect("computed values are canonical");
    assert_eq!(actual, Some(expected));
}
