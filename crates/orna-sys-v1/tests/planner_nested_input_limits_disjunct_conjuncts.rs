use orna_sys_v1::{
    ExplainError, ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef, explain_query_with_input_limit_disjunct_conjunct_chain,
};

const FIXTURE: &str = include_str!("fixtures/planner_nested_input_limits_disjunct_conjuncts.orna");

fn explain_nested_input_limits_disjunct_conjuncts(
    rows: Option<u64>,
    disjunct_count: u64,
    nested_input_limits: &[u64],
    conjunct_count: u64,
    additional_limits: &[u64],
) -> Result<orna_sys_v1::ExplainedPlan, ExplainError> {
    explain_query_with_input_limit_disjunct_conjunct_chain(
        &QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:nested-input-limits-disjunct-conjuncts"),
            source: ObjectRef::descriptive("table:PlannerNestedInputLimitsDisjunctConjuncts"),
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
        ExpressionRef::descriptive("expr:third-and-fourth"),
        conjunct_count,
        additional_limits,
    )
}

#[test]
fn nested_input_limits_feed_disjunct_work_before_the_conjunct_chain() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // ORNA-PLAN leaves estimate aggregation unspecified. Apply independent
    // 50% selectivity to the two OR arms, then left-to-right 50% selectivity
    // to the AND terms. The source and nested limits plus OR expansion reach
    // MAX exactly before the distinct post-expansion conjunct filter.
    let source_rows = u64::MAX / 5;
    let capped_rows = source_rows - 1;
    let disjunct_rows = u64::try_from((u128::from(capped_rows) * 3).div_ceil(4))
        .expect("two-arm OR estimate fits u64");
    let disjunct_work = capped_rows * 2;
    let conjunct_work = disjunct_rows + disjunct_rows.div_ceil(2);
    let conjunct_rows = disjunct_rows.div_ceil(2).div_ceil(2);
    let prefix_work = u128::from(source_rows) * 3 + u128::from(disjunct_work);
    assert_eq!(prefix_work, u128::from(u64::MAX) - 2);
    assert!(prefix_work + u128::from(conjunct_work) > u128::from(u64::MAX));

    let explained = explain_nested_input_limits_disjunct_conjuncts(
        Some(source_rows),
        2,
        &[u64::MAX, capped_rows],
        2,
        &[0],
    )
    .expect("input limits, disjunct expansion, then conjunct chain");
    assert_eq!(explained.plan().estimated_cost(), None);
    assert_eq!(explained.root().estimated_rows(), Some(0));
    assert_eq!(
        explained.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the later conjunct and limits cannot refund the exact-limit prefix"
    );

    let disjunct_filter = explained
        .nodes()
        .iter()
        .find(|node| node.details().contains_key("disjunct_count"))
        .expect("expanded disjunction filter");
    assert_eq!(disjunct_filter.kind(), PlanNodeKind::Filter);
    assert_eq!(disjunct_filter.estimated_rows(), Some(disjunct_rows));
    assert_eq!(disjunct_filter.estimated_work(), Some(disjunct_work));
    let disjunct_surface = serde_json::to_value(disjunct_filter).expect("filter serializes");
    assert_eq!(disjunct_surface["predicate"], "expr:first-or-second");
    let capped_input = explained
        .nodes()
        .iter()
        .find(|node| node.reference() == &disjunct_filter.inputs()[0])
        .expect("nested input cap feeds disjunct expansion");
    assert_eq!(capped_input.kind(), PlanNodeKind::Limit);
    assert_eq!(capped_input.estimated_rows(), Some(capped_rows));

    let conjunct_filter = explained
        .nodes()
        .iter()
        .find(|node| node.details().contains_key("conjunct_count"))
        .expect("post-expansion conjunct filter");
    assert_eq!(conjunct_filter.kind(), PlanNodeKind::Filter);
    assert_eq!(conjunct_filter.estimated_rows(), Some(conjunct_rows));
    assert_eq!(conjunct_filter.estimated_work(), Some(conjunct_work));
    let conjunct_surface = serde_json::to_value(conjunct_filter).expect("filter serializes");
    assert_eq!(conjunct_surface["predicate"], "expr:third-and-fourth");
    assert_eq!(
        conjunct_filter.inputs(),
        &[disjunct_filter.reference().clone()]
    );

    let limits = explained
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
        vec![
            Some(source_rows),
            Some(capped_rows),
            Some(conjunct_rows),
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
            Some(conjunct_rows),
            Some(conjunct_rows),
        ]
    );
}

#[test]
fn unknown_input_statistics_remain_unknown_and_invalid_chains_fail() {
    let unknown = explain_nested_input_limits_disjunct_conjuncts(None, 2, &[0, u64::MAX], 2, &[0])
        .expect("unknown input statistics across both predicate stages");
    assert_eq!(unknown.plan().estimated_cost(), None);
    assert_eq!(unknown.root().estimated_rows(), None);
    assert_eq!(
        unknown.root().details().get("estimated_cost_overflow"),
        None
    );
    assert!(
        unknown
            .nodes()
            .iter()
            .filter(|node| node.kind() == PlanNodeKind::Filter)
            .all(|filter| filter.estimated_rows().is_none() && filter.estimated_work().is_none())
    );

    assert_eq!(
        explain_nested_input_limits_disjunct_conjuncts(Some(10), 0, &[2], 2, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_nested_input_limits_disjunct_conjuncts(Some(10), 2, &[], 2, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_nested_input_limits_disjunct_conjuncts(Some(10), 2, &[2], 0, &[]),
        Err(ExplainError::InvalidExpression)
    );
}
