use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef, explain_query_with_disjunct_limit_chain,
};

const NESTED_DISJUNCT_FIXTURE: &str = include_str!("fixtures/planner_disjunct_nested_limits.orna");

fn explain_nested_disjuncts(
    rows: Option<u64>,
    additional_limits: &[u64],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_disjunct_limit_chain(
        &QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:disjunct-nested-limits"),
            source: ObjectRef::descriptive("table:PlannerDisjunctNestedLimits"),
            source_statistics: rows.map(|estimated_rows| QuerySourceStatistics {
                estimated_rows: Some(estimated_rows),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: Some(ExpressionRef::descriptive("expr:first-or-second-or-third")),
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: Some(u64::MAX),
            mutations: Vec::new(),
            materialize_into: None,
        },
        3,
        additional_limits,
    )
    .expect("nested disjunct limit plan")
}

#[test]
fn disjunct_overflow_crosses_nested_limits_after_a_representable_prefix() {
    let parsed = orna_syntax_v1::parse_module(NESTED_DISJUNCT_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // ORNA-PLAN requires planner choices to remain introspectable but does
    // not prescribe nested disjunct cost aggregation. Keep the documented
    // independent 50%-per-arm estimate and charge every nested LIMIT for its
    // immediate input rows.
    let source_rows = u64::MAX / 5;
    let disjunct_rows = u64::try_from((u128::from(source_rows) * 7).div_ceil(8))
        .expect("three-arm disjunction estimate fits u64");
    let representable = explain_nested_disjuncts(Some(source_rows), &[]);
    let filter = representable
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("expanded disjunction filter");
    assert_eq!(filter.estimated_rows(), Some(disjunct_rows));
    assert_eq!(filter.estimated_work(), Some(source_rows * 3));
    let prefix_work = u128::from(source_rows) * 4 + u128::from(disjunct_rows);
    assert!(prefix_work < u128::from(u64::MAX));
    assert_eq!(
        representable.plan().estimated_cost(),
        Some(prefix_work.to_string().as_str())
    );

    let nested = explain_nested_disjuncts(Some(source_rows), &[u64::MAX, 1, 0]);
    assert_eq!(nested.plan().estimated_cost(), None);
    assert_eq!(nested.root().estimated_rows(), Some(0));
    assert_eq!(
        nested.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the outer one-row and zero-row caps cannot erase inner expansion overflow"
    );

    let limits = nested
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Limit)
        .collect::<Vec<_>>();
    assert_eq!(limits.len(), 4);
    assert_eq!(
        limits
            .iter()
            .rev()
            .map(|node| node.estimated_rows())
            .collect::<Vec<_>>(),
        vec![Some(disjunct_rows), Some(disjunct_rows), Some(1), Some(0)]
    );
    assert_eq!(
        limits
            .iter()
            .rev()
            .map(|node| node.estimated_work())
            .collect::<Vec<_>>(),
        vec![
            Some(disjunct_rows),
            Some(disjunct_rows),
            Some(disjunct_rows),
            Some(1)
        ]
    );
    assert_eq!(
        limits
            .iter()
            .rev()
            .map(|node| node.details().get("limit").cloned())
            .collect::<Vec<_>>(),
        vec![
            Some(PlanDetail::Integer(u64::MAX)),
            Some(PlanDetail::Integer(u64::MAX)),
            Some(PlanDetail::Integer(1)),
            Some(PlanDetail::Integer(0)),
        ]
    );
}
