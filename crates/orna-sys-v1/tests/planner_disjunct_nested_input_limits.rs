use orna_sys_v1::{
    ExplainError, ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef, explain_query_with_input_limit_disjunct_chain,
};

const NESTED_INPUT_FIXTURE: &str =
    include_str!("fixtures/planner_disjunct_nested_input_limits.orna");

fn explain_nested_input_disjuncts(
    rows: Option<u64>,
    disjunct_count: u64,
    nested_input_limits: &[u64],
    additional_limits: &[u64],
) -> Result<orna_sys_v1::ExplainedPlan, ExplainError> {
    explain_query_with_input_limit_disjunct_chain(
        &QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:disjunct-nested-input-limits"),
            source: ObjectRef::descriptive("table:PlannerDisjunctNestedInputLimits"),
            source_statistics: rows.map(|estimated_rows| QuerySourceStatistics {
                estimated_rows: Some(estimated_rows),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: Some(ExpressionRef::descriptive("expr:first-or-second")),
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: Some(u64::MAX),
            mutations: Vec::new(),
            materialize_into: None,
        },
        disjunct_count,
        nested_input_limits,
        additional_limits,
    )
}

#[test]
fn disjunct_work_uses_rows_after_nested_input_limits_and_keeps_prior_work() {
    let parsed = orna_syntax_v1::parse_module(NESTED_INPUT_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // ORNA-PLAN leaves nested expansion estimates unspecified. Keep the
    // planner's documented independent 50%-per-arm estimate, apply nested
    // input caps in order, and charge each expanded arm for the rows left.
    let source_rows = u64::MAX / 3;
    let expanded_input_rows = source_rows - 1;
    let disjunct_rows = u64::try_from((u128::from(expanded_input_rows) * 3).div_ceil(4))
        .expect("two-arm disjunction estimate fits u64");
    let expansion_work = expanded_input_rows * 2;
    assert!(expansion_work <= u64::MAX);
    assert_eq!(u128::from(source_rows) * 3, u128::from(u64::MAX));
    assert!(u128::from(source_rows) * 3 + u128::from(expansion_work) > u128::from(u64::MAX));

    let explained = explain_nested_input_disjuncts(
        Some(source_rows),
        2,
        &[u64::MAX, expanded_input_rows],
        &[1, 0],
    )
    .expect("nested input caps before disjunct expansion");
    assert_eq!(explained.plan().estimated_cost(), None);
    assert_eq!(explained.root().estimated_rows(), Some(0));
    assert_eq!(
        explained.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "post-filter limits cannot refund nested scan, cap, or expansion work"
    );

    let filter = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("expanded disjunction filter");
    assert_eq!(filter.estimated_rows(), Some(disjunct_rows));
    assert_eq!(filter.estimated_work(), Some(expansion_work));
    assert_eq!(filter.details().get("estimated_work_overflow"), None);
    assert_eq!(filter.inputs().len(), 1);
    let filter_input = explained
        .nodes()
        .iter()
        .find(|node| node.reference() == &filter.inputs()[0])
        .expect("nested input limit feeding filter");
    assert_eq!(filter_input.kind(), PlanNodeKind::Limit);
    assert_eq!(filter_input.estimated_rows(), Some(expanded_input_rows));

    let limits = explained
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Limit)
        .collect::<Vec<_>>();
    assert_eq!(limits.len(), 5);
    assert_eq!(
        limits
            .iter()
            .rev()
            .map(|node| node.estimated_rows())
            .collect::<Vec<_>>(),
        vec![
            Some(source_rows),
            Some(expanded_input_rows),
            Some(disjunct_rows),
            Some(1),
            Some(0),
        ]
    );
    assert_eq!(
        limits
            .iter()
            .rev()
            .map(|node| node.estimated_work())
            .collect::<Vec<_>>(),
        vec![
            Some(source_rows),
            Some(source_rows),
            Some(disjunct_rows),
            Some(disjunct_rows),
            Some(1),
        ]
    );
}

#[test]
fn nested_input_limit_expansion_keeps_unknown_statistics_unknown() {
    let unknown = explain_nested_input_disjuncts(None, 2, &[0, u64::MAX], &[0])
        .expect("unknown source statistics through nested input caps");
    assert_eq!(unknown.plan().estimated_cost(), None);
    assert_eq!(unknown.root().estimated_rows(), None);
    assert_eq!(
        unknown.root().details().get("estimated_cost_overflow"),
        None
    );
    let filter = unknown
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("expanded disjunction filter");
    assert_eq!(filter.estimated_rows(), None);
    assert_eq!(filter.estimated_work(), None);
    assert_eq!(filter.details().get("estimated_work_overflow"), None);

    assert_eq!(
        explain_nested_input_disjuncts(Some(10), 0, &[2], &[]),
        Err(ExplainError::InvalidExpression)
    );
}
