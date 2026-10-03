use orna_sys_v1::{
    ExplainError, ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef, explain_query_with_input_disjunct_limit_conjunct_chain,
};

const FIXTURE: &str = include_str!("fixtures/planner_nested_input_disjunct_limits_conjunct.orna");

fn explain_nested_input_disjunct_limits_conjunct(
    rows: Option<u64>,
    disjunct_count: u64,
    nested_input_limits: &[u64],
    nested_disjunct_limits: &[u64],
    conjunct_count: u64,
    additional_limits: &[u64],
) -> Result<orna_sys_v1::ExplainedPlan, ExplainError> {
    explain_query_with_input_disjunct_limit_conjunct_chain(
        &QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:nested-input-disjunct-limits-conjunct"),
            source: ObjectRef::descriptive("table:PlannerNestedInputDisjunctLimitsConjunct"),
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
            limit: Some(100),
            mutations: Vec::new(),
            materialize_into: None,
        },
        disjunct_count,
        nested_input_limits,
        nested_disjunct_limits,
        ExpressionRef::descriptive("expr:third-and-fourth"),
        conjunct_count,
        additional_limits,
    )
}

#[test]
fn nested_input_caps_feed_disjunct_limits_before_the_conjunct_chain() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // ORNA-PLAN does not define estimate aggregation for this chain. The
    // fallback charges each OR arm against the input after both source caps,
    // then applies two nested limits before charging the left-to-right AND.
    let explained =
        explain_nested_input_disjunct_limits_conjunct(Some(100), 2, &[80, 50], &[30, 10], 2, &[2])
            .expect("input limits, disjunct expansion, nested limits, and conjuncts");

    let disjunct_filter = explained
        .nodes()
        .iter()
        .find(|node| node.details().contains_key("disjunct_count"))
        .expect("expanded disjunction filter");
    assert_eq!(disjunct_filter.kind(), PlanNodeKind::Filter);
    assert_eq!(disjunct_filter.estimated_rows(), Some(38));
    assert_eq!(disjunct_filter.estimated_work(), Some(100));
    assert_eq!(
        serde_json::to_value(disjunct_filter).expect("disjunct filter serializes")["predicate"],
        "expr:first-or-second"
    );

    let conjunct_filter = explained
        .nodes()
        .iter()
        .find(|node| node.details().contains_key("conjunct_count"))
        .expect("conjunct filter after the disjunct limit chain");
    assert_eq!(conjunct_filter.kind(), PlanNodeKind::Filter);
    assert_eq!(conjunct_filter.estimated_rows(), Some(3));
    assert_eq!(conjunct_filter.estimated_work(), Some(15));
    assert_eq!(
        serde_json::to_value(conjunct_filter).expect("conjunct filter serializes")["predicate"],
        "expr:third-and-fourth"
    );
    let conjunct_input = explained
        .nodes()
        .iter()
        .find(|node| node.reference() == &conjunct_filter.inputs()[0])
        .expect("last disjunct limit feeds conjuncts");
    assert_eq!(conjunct_input.kind(), PlanNodeKind::Limit);
    assert_eq!(conjunct_input.estimated_rows(), Some(10));

    let disjunct_input = explained
        .nodes()
        .iter()
        .find(|node| node.reference() == &disjunct_filter.inputs()[0])
        .expect("second nested input limit feeds disjunct expansion");
    assert_eq!(disjunct_input.kind(), PlanNodeKind::Limit);
    assert_eq!(disjunct_input.estimated_rows(), Some(50));

    let limits = explained
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Limit)
        .collect::<Vec<_>>();
    assert_eq!(limits.len(), 6);
    assert_eq!(
        limits
            .iter()
            .rev()
            .map(|node| node.estimated_rows())
            .collect::<Vec<_>>(),
        vec![Some(80), Some(50), Some(30), Some(10), Some(3), Some(2)]
    );
    assert_eq!(
        limits
            .iter()
            .rev()
            .map(|node| node.estimated_work())
            .collect::<Vec<_>>(),
        vec![Some(100), Some(80), Some(38), Some(30), Some(3), Some(3)]
    );
}

#[test]
fn overflow_and_unknown_rows_survive_the_entire_nested_chain() {
    let source_rows = u64::MAX / 4;
    let capped_rows = source_rows - 1;
    let overflow = explain_nested_input_disjunct_limits_conjunct(
        Some(source_rows),
        2,
        &[u64::MAX, capped_rows],
        &[u64::MAX, 10],
        2,
        &[0],
    )
    .expect("large nested chain preserves accumulated cost overflow");
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    let conjunct_filter = overflow
        .nodes()
        .iter()
        .find(|node| node.details().contains_key("conjunct_count"))
        .expect("conjunct filter remains represented after prior overflow");
    assert_eq!(conjunct_filter.estimated_rows(), Some(3));
    assert_eq!(conjunct_filter.estimated_work(), Some(15));

    let unknown =
        explain_nested_input_disjunct_limits_conjunct(None, 2, &[80, 50], &[30, 10], 2, &[0])
            .expect("unknown source estimates through every stage");
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
}

#[test]
fn all_four_stages_are_required() {
    assert_eq!(
        explain_nested_input_disjunct_limits_conjunct(Some(10), 2, &[], &[4], 2, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_nested_input_disjunct_limits_conjunct(Some(10), 2, &[4], &[], 2, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_nested_input_disjunct_limits_conjunct(Some(10), 0, &[4], &[2], 2, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_nested_input_disjunct_limits_conjunct(Some(10), 2, &[4], &[2], 0, &[]),
        Err(ExplainError::InvalidExpression)
    );
}
