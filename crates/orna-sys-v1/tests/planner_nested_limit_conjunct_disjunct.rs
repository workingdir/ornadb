use orna_sys_v1::{
    ExplainError, ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef, explain_query_with_input_limit_conjunct_disjunct_chain,
};

const NESTED_LIMIT_FIXTURE: &str =
    include_str!("fixtures/planner_nested_limit_conjunct_disjunct.orna");

fn explain_nested_limit_conjunct_disjunct(
    rows: Option<u64>,
    disjunct_count: u64,
    conjunct_count_per_disjunct: u64,
    nested_input_limits: &[u64],
    additional_limits: &[u64],
) -> Result<orna_sys_v1::ExplainedPlan, ExplainError> {
    explain_query_with_input_limit_conjunct_disjunct_chain(
        &QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:nested-limit-conjunct-disjunct"),
            source: ObjectRef::descriptive("table:PlannerNestedLimitConjunctDisjunct"),
            source_statistics: rows.map(|estimated_rows| QuerySourceStatistics {
                estimated_rows: Some(estimated_rows),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: Some(ExpressionRef::descriptive("expr:two-conjunct-arms")),
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: Some(u64::MAX),
            mutations: Vec::new(),
            materialize_into: None,
        },
        disjunct_count,
        conjunct_count_per_disjunct,
        nested_input_limits,
        additional_limits,
    )
}

#[test]
fn conjunct_work_runs_on_nested_capped_input_and_survives_outer_limits() {
    let parsed = orna_syntax_v1::parse_module(NESTED_LIMIT_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // ORNA-PLAN leaves expansion estimates unspecified. Keep the documented
    // independent 50%-per-conjunct fallback and left-to-right AND charging;
    // each arm starts with the rows left by the nested input caps. The source
    // scan plus both LIMIT inputs exactly reaches MAX before filter work.
    let source_rows = u64::MAX / 3;
    let filter_input_rows = source_rows - 1;
    let first_conjunct_rows = filter_input_rows.div_ceil(2);
    let per_arm_work = u128::from(filter_input_rows) + u128::from(first_conjunct_rows);
    let filter_work = per_arm_work * 2;
    assert_eq!(u128::from(source_rows) * 3, u128::from(u64::MAX));
    assert_eq!(filter_work, u128::from(u64::MAX) - 3);
    assert!(u128::from(source_rows) * 3 + filter_work > u128::from(u64::MAX));

    let first_arm_rows = u64::try_from(u128::from(filter_input_rows).div_ceil(4))
        .expect("first arm row estimate fits u64");
    let remaining_rows = filter_input_rows - first_arm_rows;
    let second_arm_rows =
        u64::try_from(u128::from(remaining_rows).div_ceil(4)).expect("second arm estimate fits");
    let filter_rows = first_arm_rows + second_arm_rows;

    let explained = explain_nested_limit_conjunct_disjunct(
        Some(source_rows),
        2,
        2,
        &[u64::MAX, filter_input_rows],
        &[1, 0],
    )
    .expect("nested input caps before expanded conjunct branches");
    assert_eq!(explained.plan().estimated_cost(), None);
    assert_eq!(explained.root().estimated_rows(), Some(0));
    assert_eq!(
        explained.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "post-filter limits cannot refund nested input and conjunct work"
    );

    let filter = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("expanded conjunct filter");
    assert_eq!(filter.estimated_rows(), Some(filter_rows));
    assert_eq!(filter.estimated_work(), Some(filter_work as u64));
    assert_eq!(filter.details().get("estimated_work_overflow"), None);
    assert_eq!(
        filter.details().get("conjunct_count_per_disjunct"),
        Some(&PlanDetail::Integer(2))
    );
    assert_eq!(
        filter.details().get("conjunct_order"),
        Some(&PlanDetail::Text("left_to_right_short_circuit".to_owned()))
    );
    assert_eq!(
        filter.details().get("expansion_work"),
        Some(&PlanDetail::Text("full_input_per_disjunct".to_owned()))
    );
    let filter_input = explained
        .nodes()
        .iter()
        .find(|node| node.reference() == &filter.inputs()[0])
        .expect("nested cap feeding expanded filter");
    assert_eq!(filter_input.kind(), PlanNodeKind::Limit);
    assert_eq!(filter_input.estimated_rows(), Some(filter_input_rows));

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
            Some(filter_input_rows),
            Some(filter_rows),
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
            Some(filter_rows),
            Some(filter_rows),
            Some(1),
        ]
    );
}

#[test]
fn unknown_nested_inputs_stay_unknown_and_invalid_expansion_shapes_are_rejected() {
    let unknown =
        explain_nested_limit_conjunct_disjunct(None, 2, 2, &[u64::MAX, 0], &[0, u64::MAX])
            .expect("unknown source statistics through nested caps and conjunct branches");
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
        .expect("expanded conjunct filter");
    assert_eq!(filter.estimated_rows(), None);
    assert_eq!(filter.estimated_work(), None);
    assert_eq!(filter.details().get("estimated_work_overflow"), None);

    assert_eq!(
        explain_nested_limit_conjunct_disjunct(Some(10), 0, 2, &[2], &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_nested_limit_conjunct_disjunct(Some(10), 2, 0, &[2], &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_nested_limit_conjunct_disjunct(Some(10), 2, u64::MAX, &[2], &[]),
        Err(ExplainError::TooManyExpressions)
    );
}
