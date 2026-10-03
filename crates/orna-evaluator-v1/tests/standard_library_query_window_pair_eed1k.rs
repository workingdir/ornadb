use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

#[test]
fn sparse_rate_and_integral_pairs_follow_their_shared_window_order() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference standard profile failed with {}", error.code()));
    for import in [
        include_str!("fixtures/stdlib-use-query-2213.orna"),
        include_str!("fixtures/stdlib-use-stats-z09xc.orna"),
    ] {
        assert_eq!(session.submit(import), Ok(None), "import: {import}");
    }
    let actual = session
        .submit(include_str!(
            "fixtures/stdlib-query-sparse-window-rate-integral-pairs-eed1k.orna"
        ))
        .unwrap_or_else(|error| panic!("paired sparse window statistics failed with {}", error.code()));
    assert_eq!(
        actual,
        Some(CanonicalValue::new(Raw::Bool(true)).expect("expected exact paired values"))
    );
}
