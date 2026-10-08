use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("boolean is canonical")
}

fn session() -> AdmittedReplSession {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile is admitted");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-query-2213.orna")),
        Ok(None)
    );
    session
}

// A self-edge leads a value back to itself. The closure's identity set already
// holds the anchor, so the self-edge adds nothing, however many times it is
// returned, and the anchor appears exactly once.
#[test]
fn recursive_cte_self_edge_returns_the_anchor_once() {
    let mut session = session();
    let source = include_str!("fixtures/stdlib-query-recursive-cte-self-edge-ln4y7.orna");
    assert_eq!(session.submit(source), Ok(Some(bool_value(true))), "{source}");
}
