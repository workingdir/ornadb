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

// The recursive closure keeps one identity set across rounds, so the back-edge
// 2 -> 1 and the self-loop at 3 cannot re-add a value: the walk terminates with
// each reachable value exactly once, in breadth-first order.
#[test]
fn recursive_cte_cycle_is_cut_by_the_identity_set() {
    let mut session = session();
    let source = include_str!("fixtures/stdlib-query-recursive-cte-cycle-guard-ln4y7.orna");
    assert_eq!(session.submit(source), Ok(Some(bool_value(true))), "{source}");
}
