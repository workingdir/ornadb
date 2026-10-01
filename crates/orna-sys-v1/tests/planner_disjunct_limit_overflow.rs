use orna_sys_v1::{
    ExplainError, ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef, explain_query_with_disjunct_limit_chain,
};

const DISJUNCT_FIXTURE: &str = include_str!("fixtures/planner_disjunct_limit_overflow.orna");

fn explain_disjunct_limit_chain(
    rows: Option<u64>,
    disjunct_count: u64,
    first_limit: Option<u64>,
    following_limits: &[u64],
) -> Result<orna_sys_v1::ExplainedPlan, ExplainError> {
    explain_query_with_disjunct_limit_chain(
        &QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:disjunct-limit-chain"),
            source: ObjectRef::descriptive("table:PlannerDisjunctLimitOverflow"),
            source_statistics: rows.map(|estimated_rows| QuerySourceStatistics {
                estimated_rows: Some(estimated_rows),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: Some(ExpressionRef::descriptive("expr:active-or-verified")),
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: first_limit,
            mutations: Vec::new(),
            materialize_into: None,
        },
        disjunct_count,
        following_limits,
    )
}

#[test]
fn disjunct_expansion_overflow_crosses_an_exact_maximum_limit_prefix() {
    let parsed = orna_syntax_v1::parse_module(DISJUNCT_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let source_rows = (u64::MAX / 15) * 4;
    let matched_rows = u64::try_from((u128::from(source_rows) * 3).div_ceil(4))
        .expect("three-quarter disjunction estimate fits u64");
    let first = explain_disjunct_limit_chain(Some(source_rows), 2, Some(u64::MAX), &[])
        .expect("one representable disjunct limit stage");
    let filter = first
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("expanded disjunction filter");
    assert_eq!(filter.estimated_rows(), Some(matched_rows));
    assert_eq!(filter.estimated_work(), Some(source_rows * 2));
    assert_eq!(
        filter.details().get("selectivity_assumption"),
        Some(&PlanDetail::Text(
            "0.5_per_disjunct_independent_or".to_owned()
        ))
    );
    assert_eq!(
        filter.details().get("disjunct_count"),
        Some(&PlanDetail::Integer(2))
    );

    let first_cost_work = u128::from(source_rows) * 3 + u128::from(matched_rows);
    assert_eq!(first_cost_work, u128::from(u64::MAX));
    let first_cost = first_cost_work.to_string();
    assert_eq!(first.plan().estimated_cost(), Some(first_cost.as_str()));
    assert_eq!(first.root().details().get("estimated_cost_overflow"), None);

    let overflow = explain_disjunct_limit_chain(Some(source_rows), 2, Some(u64::MAX), &[u64::MAX])
        .expect("second limit crosses MAX after the representable prefix");
    assert_eq!(overflow.root().estimated_work(), Some(matched_rows));
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the second limit's input work closes overflow after disjunct expansion"
    );

    let zero_tail =
        explain_disjunct_limit_chain(Some(source_rows), 2, Some(u64::MAX), &[u64::MAX, 0])
            .expect("zero limit follows disjunct-chain overflow");
    assert_eq!(zero_tail.root().estimated_rows(), Some(0));
    assert_eq!(
        zero_tail.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "a later zero limit cannot erase overflow from expanded branches"
    );
}

#[test]
fn disjunct_work_overflow_is_retained_while_unknown_inputs_stay_unproven() {
    let rows = u64::MAX / 2 + 1;
    let overflow = explain_disjunct_limit_chain(Some(rows), 2, Some(u64::MAX), &[u64::MAX])
        .expect("disjunct multiplication overflow beneath a limit chain");
    let filter = overflow
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("expanded disjunction filter");
    assert_eq!(filter.estimated_work(), None);
    assert_eq!(
        filter.details().get("estimated_work_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );

    let unknown = explain_disjunct_limit_chain(None, 2, Some(u64::MAX), &[u64::MAX])
        .expect("unknown input rows stay unknown across expanded limits");
    assert_eq!(unknown.plan().estimated_cost(), None);
    assert_eq!(unknown.root().estimated_rows(), None);
    assert_eq!(
        unknown.root().details().get("estimated_cost_overflow"),
        None
    );
}

#[test]
fn disjunct_expansion_requires_at_least_one_branch() {
    assert!(matches!(
        explain_disjunct_limit_chain(Some(4), 0, None, &[]),
        Err(ExplainError::InvalidExpression)
    ));
}
