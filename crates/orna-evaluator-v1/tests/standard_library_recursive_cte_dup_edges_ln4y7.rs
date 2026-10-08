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

// One round returns duplicate edges (2, 2, 3, 3). The closure's identity set
// admits each value once, so the duplicates collapse and the output is 1, 2, 3.
#[test]
fn recursive_cte_collapses_duplicate_edges_within_a_round() {
    let mut session = session();
    let source = include_str!("fixtures/stdlib-query-recursive-cte-dup-edges-ln4y7.orna");
    assert_eq!(session.submit(source), Ok(Some(bool_value(true))), "{source}");
}
