use orna_sys_v1::{
    ExplainError, ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef, explain_query_with_disjunct_limit_conjunct_chain,
};

const DISJUNCT_LIMIT_FIXTURE: &str =
    include_str!("fixtures/planner_disjunct_limits_then_conjuncts.orna");

fn explain_disjunct_limits_then_conjuncts(
    rows: Option<u64>,
    disjunct_count: u64,
    nested_limits: &[u64],
    conjunct_count: u64,
    additional_limits: &[u64],
) -> Result<orna_sys_v1::ExplainedPlan, ExplainError> {
    explain_query_with_disjunct_limit_conjunct_chain(
        &QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:disjunct-limits-then-conjuncts"),
            source: ObjectRef::descriptive("table:PlannerDisjunctLimitsThenConjuncts"),
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
        nested_limits,
        ExpressionRef::descriptive("expr:third-and-fourth"),
        conjunct_count,
        additional_limits,
    )
}

#[test]
fn conjunct_work_runs_after_disjunct_expansion_limits_and_cannot_refund_prior_work() {
    let parsed = orna_syntax_v1::parse_module(DISJUNCT_LIMIT_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // ORNA-PLAN leaves nested predicate estimates and cost aggregation
    // unspecified. Each OR arm uses the documented 50% fallback and is
    // charged against the full source; AND work starts only after both LIMITs.
    let source_rows = u64::MAX / 3;
    let disjunct_rows = u64::try_from((u128::from(source_rows) * 3).div_ceil(4))
        .expect("two-arm OR estimate fits u64");
    let disjunct_work = source_rows * 2;
    assert_eq!(
        u128::from(source_rows) + u128::from(disjunct_work),
        u128::from(u64::MAX)
    );

    let conjunct_input_rows = 3u64;
    let conjunct_work = conjunct_input_rows + conjunct_input_rows.div_ceil(2);
    let conjunct_rows = conjunct_input_rows.div_ceil(2).div_ceil(2);

    let explained = explain_disjunct_limits_then_conjuncts(
        Some(source_rows),
        2,
        &[u64::MAX, conjunct_input_rows],
        2,
        &[0],
    )
    .expect("conjunct stage after nested disjunct limits");
    assert_eq!(explained.plan().estimated_cost(), None);
    assert_eq!(explained.root().estimated_rows(), Some(0));
    assert_eq!(
        explained.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "nested expansion caps and later conjunct work cannot refund the prefix"
    );

    let disjunct_filter = explained
        .nodes()
        .iter()
        .find(|node| node.details().contains_key("disjunct_count"))
        .expect("expanded disjunction filter");
    assert_eq!(disjunct_filter.kind(), PlanNodeKind::Filter);
    assert_eq!(disjunct_filter.estimated_rows(), Some(disjunct_rows));
    assert_eq!(disjunct_filter.estimated_work(), Some(disjunct_work));
    let disjunct_filter_surface =
        serde_json::to_value(disjunct_filter).expect("disjunct filter serializes");
    assert_eq!(
        disjunct_filter_surface["predicate"],
        "expr:first-or-second",
        "the pre-limit filter is the disjunctive predicate"
    );
    assert_eq!(
        disjunct_filter.details().get("estimated_work_overflow"),
        None
    );

    let conjunct_filter = explained
        .nodes()
        .iter()
        .find(|node| node.details().contains_key("conjunct_count"))
        .expect("post-limit conjunct filter");
    assert_eq!(conjunct_filter.kind(), PlanNodeKind::Filter);
    assert_eq!(conjunct_filter.estimated_rows(), Some(conjunct_rows));
    assert_eq!(conjunct_filter.estimated_work(), Some(conjunct_work));
    let conjunct_filter_surface =
        serde_json::to_value(conjunct_filter).expect("conjunct filter serializes");
    assert_eq!(
        conjunct_filter_surface["predicate"],
        "expr:third-and-fourth",
        "the post-limit filter is the conjunct predicate"
    );
    assert_eq!(
        conjunct_filter.details().get("conjunct_order"),
        Some(&PlanDetail::Text("left_to_right_short_circuit".to_owned()))
    );
    assert_eq!(conjunct_filter.inputs().len(), 1);
    let conjunct_input = explained
        .nodes()
        .iter()
        .find(|node| node.reference() == &conjunct_filter.inputs()[0])
        .expect("nested limit feeding conjunct filter");
    assert_eq!(conjunct_input.kind(), PlanNodeKind::Limit);
    assert_eq!(conjunct_input.estimated_rows(), Some(conjunct_input_rows));

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
            Some(disjunct_rows),
            Some(conjunct_input_rows),
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
            Some(disjunct_rows),
            Some(disjunct_rows),
            Some(conjunct_rows),
            Some(conjunct_rows),
        ]
    );
}

#[test]
fn unknown_predicate_statistics_stay_unknown_and_empty_or_invalid_chains_fail() {
    let unknown = explain_disjunct_limits_then_conjuncts(None, 2, &[0, u64::MAX], 2, &[0])
        .expect("unknown source statistics across separated predicate stages");
    assert_eq!(unknown.plan().estimated_cost(), None);
    assert_eq!(unknown.root().estimated_rows(), None);
    assert_eq!(
        unknown.root().details().get("estimated_cost_overflow"),
        None
    );
    let filters = unknown
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Filter)
        .collect::<Vec<_>>();
    assert_eq!(filters.len(), 2);
    assert!(
        filters
            .iter()
            .all(|filter| filter.estimated_work().is_none())
    );
    assert!(filters.iter().all(|filter| {
        filter.details().get("estimated_work_overflow") != Some(&PlanDetail::Boolean(true))
    }));

    assert_eq!(
        explain_disjunct_limits_then_conjuncts(Some(10), 0, &[2], 2, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_disjunct_limits_then_conjuncts(Some(10), 2, &[], 2, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_disjunct_limits_then_conjuncts(Some(10), 2, &[2], 0, &[]),
        Err(ExplainError::InvalidExpression)
    );
}
