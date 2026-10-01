use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef, explain_query,
};

const CONJUNCT_FIXTURE: &str = include_str!("fixtures/planner_conjunct_overflow.orna");

fn explain_conjunct_filter(rows: Option<u64>) -> orna_sys_v1::ExplainedPlan {
    explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:conjunct-overflow"),
        source: ObjectRef::descriptive("table:PlannerConjunctOverflow"),
        source_statistics: rows.map(|estimated_rows| QuerySourceStatistics {
            estimated_rows: Some(estimated_rows),
            estimated_bytes: Some(0),
            mutable_branch: None,
        }),
        joins: Vec::new(),
        predicate: Some(ExpressionRef::descriptive("expr:active-and-verified")),
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: Some(0),
        mutations: Vec::new(),
        materialize_into: None,
    })
    .expect("conjunctive filter plan")
}

#[test]
fn conjunctive_predicate_keeps_input_work_and_propagates_overflow_past_limit() {
    let parsed = orna_syntax_v1::parse_module(CONJUNCT_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // Scan + filter + limit work is rows + rows + ceil(rows / 2). This
    // even-valued boundary makes the exact subtotal equal u64::MAX.
    let exact_rows = (u64::MAX / 5) * 2;
    let exact = explain_conjunct_filter(Some(exact_rows));
    let filter = exact
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("conjunctive predicate filter");
    let filter_surface = serde_json::to_value(filter).expect("serialized conjunct predicate");
    assert_eq!(filter_surface["predicate"], "expr:active-and-verified");
    assert_eq!(filter.estimated_work(), Some(exact_rows));
    assert_eq!(exact.root().estimated_rows(), Some(0));
    assert_eq!(exact.root().estimated_work(), Some(exact_rows / 2));
    assert_eq!(
        exact.nodes().last().unwrap().estimated_work(),
        Some(exact_rows)
    );
    let expected_exact_cost = u64::MAX.to_string();
    assert_eq!(
        exact.plan().estimated_cost(),
        Some(expected_exact_cost.as_str())
    );
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);

    let overflow_rows = exact_rows + 1;
    let overflow = explain_conjunct_filter(Some(overflow_rows));
    let filter = overflow
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("conjunctive predicate filter");
    assert_eq!(filter.estimated_work(), Some(overflow_rows));
    assert_eq!(overflow.root().kind(), PlanNodeKind::Limit);
    assert_eq!(overflow.root().estimated_rows(), Some(0));
    assert_eq!(
        overflow.root().estimated_work(),
        Some(overflow_rows.div_ceil(2))
    );
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "filter output reduction and LIMIT 0 cannot refund work already required"
    );
    assert!(filter.details().get("estimated_work_overflow").is_none());

    let unknown = explain_conjunct_filter(None);
    assert_eq!(unknown.plan().estimated_cost(), None);
    assert_eq!(
        unknown.root().details().get("estimated_cost_overflow"),
        None
    );
}
