use orna_sys_v1::{
    ExplainError, ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef, explain_query_with_disjunct_conjunct_limit_chain,
};

const DISJUNCT_CONJUNCT_FIXTURE: &str =
    include_str!("fixtures/planner_disjunct_conjunct_limits.orna");

fn explain_disjunct_conjunct_chain(
    rows: Option<u64>,
    conjunct_count: u64,
    additional_limits: &[u64],
) -> Result<orna_sys_v1::ExplainedPlan, ExplainError> {
    explain_query_with_disjunct_conjunct_limit_chain(
        &QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:disjunct-conjunct-limits"),
            source: ObjectRef::descriptive("table:PlannerDisjunctConjunctLimits"),
            source_statistics: rows.map(|estimated_rows| QuerySourceStatistics {
                estimated_rows: Some(estimated_rows),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: Some(ExpressionRef::descriptive("expr:or-then-conjunct-chain")),
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: Some(u64::MAX),
            mutations: Vec::new(),
            materialize_into: None,
        },
        2,
        conjunct_count,
        additional_limits,
    )
}

#[test]
fn conjunct_pressure_closes_overflow_across_nested_limits_after_representable_prefix() {
    let parsed = orna_syntax_v1::parse_module(DISJUNCT_CONJUNCT_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // The reference specifies left-to-right short-circuiting, but not planner
    // selectivity or nested work aggregation. Use its documented 50% fallback:
    // expand both OR arms first, then charge the two AND terms for surviving
    // rows. This boundary fits through the first LIMIT and overflows at the
    // next nested LIMIT.
    let source_rows = (u64::MAX / 9) * 2 + 1;
    let disjunct_rows = u64::try_from((u128::from(source_rows) * 3).div_ceil(4))
        .expect("OR output estimate fits u64");
    let first_conjunct_rows = disjunct_rows.div_ceil(2);
    let final_rows = first_conjunct_rows.div_ceil(2);
    let filter_work =
        u128::from(source_rows) * 2 + u128::from(disjunct_rows) + u128::from(first_conjunct_rows);
    let prefix_cost = u128::from(source_rows) + filter_work + u128::from(final_rows);
    assert!(filter_work <= u128::from(u64::MAX));
    assert!(prefix_cost < u128::from(u64::MAX));
    assert_eq!(
        prefix_cost + u128::from(final_rows),
        u128::from(u64::MAX) + 1,
        "the next nested LIMIT crosses the boundary by exactly one work unit"
    );

    let prefix = explain_disjunct_conjunct_chain(Some(source_rows), 2, &[])
        .expect("one representable limit prefix");
    let filter = prefix
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("compound predicate filter");
    assert_eq!(filter.estimated_rows(), Some(final_rows));
    assert_eq!(filter.estimated_work(), Some(filter_work as u64));
    assert_eq!(
        filter.details().get("conjunct_order"),
        Some(&PlanDetail::Text("left_to_right_short_circuit".to_owned()))
    );
    assert_eq!(
        filter.details().get("conjunct_count"),
        Some(&PlanDetail::Integer(2))
    );
    let expected_prefix_cost = prefix_cost.to_string();
    assert_eq!(
        prefix.plan().estimated_cost(),
        Some(expected_prefix_cost.as_str())
    );
    assert_eq!(prefix.root().details().get("estimated_cost_overflow"), None);

    let crossing = explain_disjunct_conjunct_chain(Some(source_rows), 2, &[u64::MAX])
        .expect("nested limit closes overflow");
    assert_eq!(
        crossing.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );

    let nested = explain_disjunct_conjunct_chain(Some(source_rows), 2, &[u64::MAX, 1, 0])
        .expect("nested limits preserve compound-predicate pressure");
    assert_eq!(nested.plan().estimated_cost(), None);
    assert_eq!(nested.root().estimated_rows(), Some(0));
    assert_eq!(
        nested.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "later one-row and zero-row caps cannot refund nested predicate pressure"
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
        vec![Some(final_rows), Some(final_rows), Some(1), Some(0)]
    );
    assert_eq!(
        limits
            .iter()
            .rev()
            .map(|node| node.estimated_work())
            .collect::<Vec<_>>(),
        vec![
            Some(final_rows),
            Some(final_rows),
            Some(final_rows),
            Some(1)
        ]
    );
}

#[test]
fn conjunct_work_overflow_is_proven_but_unknown_inputs_stay_unknown() {
    let overflow_rows = u64::MAX / 3 + 1;
    let overflow = explain_disjunct_conjunct_chain(Some(overflow_rows), 2, &[0])
        .expect("compound predicate work overflows before zero limit");
    let filter = overflow
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("compound predicate filter");
    assert_eq!(filter.estimated_work(), None);
    assert_eq!(
        filter.details().get("estimated_work_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    assert_eq!(overflow.root().estimated_rows(), Some(0));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "a zero-row tail cannot erase proven compound-filter work overflow"
    );

    let unknown = explain_disjunct_conjunct_chain(None, 2, &[0, u64::MAX, 0])
        .expect("unknown source statistics through nested limits");
    assert_eq!(unknown.plan().estimated_cost(), None);
    assert_eq!(unknown.root().estimated_rows(), None);
    assert_eq!(
        unknown.root().details().get("estimated_cost_overflow"),
        None
    );

    assert_eq!(
        explain_disjunct_conjunct_chain(Some(10), 0, &[]),
        Err(ExplainError::InvalidExpression)
    );
}
