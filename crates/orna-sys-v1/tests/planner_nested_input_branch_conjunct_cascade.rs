use orna_sys_v1::{
    ExplainError, ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef,
    explain_query_with_input_limit_conjunct_disjunct_limit_conjunct_chain,
};

const FIXTURE: &str = include_str!("fixtures/planner_nested_input_branch_conjunct_cascade.orna");

fn explain_nested_input_branch_conjunct_cascade(
    rows: Option<u64>,
    disjunct_count: u64,
    conjunct_count_per_disjunct: u64,
    nested_input_limits: &[u64],
    nested_disjunct_limits: &[u64],
    conjunct_count: u64,
    additional_limits: &[u64],
) -> Result<orna_sys_v1::ExplainedPlan, ExplainError> {
    explain_query_with_input_limit_conjunct_disjunct_limit_conjunct_chain(
        &QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:nested-input-branch-conjunct-cascade"),
            source: ObjectRef::descriptive("table:PlannerNestedInputBranchConjunctCascade"),
            source_statistics: rows.map(|estimated_rows| QuerySourceStatistics {
                estimated_rows: Some(estimated_rows),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: Some(ExpressionRef::descriptive("expr:two-conjunct-or-arms")),
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: Some(100),
            mutations: Vec::new(),
            materialize_into: None,
        },
        disjunct_count,
        conjunct_count_per_disjunct,
        nested_input_limits,
        nested_disjunct_limits,
        ExpressionRef::descriptive("expr:fifth-and-sixth"),
        conjunct_count,
        additional_limits,
    )
}

#[test]
fn nested_input_arms_and_post_disjunct_conjunct_cross_limit_cascades() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // ORNA-PLAN leaves estimate aggregation unspecified for this composition.
    // The documented fallback applies the two-term AND inside each OR arm,
    // caps the expanded rows through a nested limit cascade, then charges the
    // separate post-cascade AND chain for its own surviving input.
    let explained = explain_nested_input_branch_conjunct_cascade(
        Some(100),
        2,
        2,
        &[80, 50],
        &[12, 8],
        2,
        &[1, 0],
    )
    .expect("nested input caps, branch conjuncts, limit cascade, and final conjuncts");

    let branch_filter = explained
        .nodes()
        .iter()
        .find(|node| node.details().contains_key("conjunct_count_per_disjunct"))
        .expect("expanded filter with conjuncts in each OR arm");
    assert_eq!(branch_filter.kind(), PlanNodeKind::Filter);
    assert_eq!(branch_filter.estimated_rows(), Some(23));
    assert_eq!(branch_filter.estimated_work(), Some(150));
    assert_eq!(
        serde_json::to_value(branch_filter).expect("branch filter serializes")["predicate"],
        "expr:two-conjunct-or-arms"
    );
    assert_eq!(
        branch_filter.details().get("conjunct_order"),
        Some(&PlanDetail::Text("left_to_right_short_circuit".to_owned()))
    );

    let final_filter = explained
        .nodes()
        .iter()
        .find(|node| node.details().contains_key("conjunct_count"))
        .expect("separate conjunct after the nested disjunct limits");
    assert_eq!(final_filter.kind(), PlanNodeKind::Filter);
    assert_eq!(final_filter.estimated_rows(), Some(2));
    assert_eq!(final_filter.estimated_work(), Some(12));
    assert_eq!(
        serde_json::to_value(final_filter).expect("final filter serializes")["predicate"],
        "expr:fifth-and-sixth"
    );

    let branch_input = explained
        .nodes()
        .iter()
        .find(|node| node.reference() == &branch_filter.inputs()[0])
        .expect("last nested input cap feeds the expanded arms");
    assert_eq!(branch_input.kind(), PlanNodeKind::Limit);
    assert_eq!(branch_input.estimated_rows(), Some(50));
    let final_input = explained
        .nodes()
        .iter()
        .find(|node| node.reference() == &final_filter.inputs()[0])
        .expect("last disjunct cap feeds the separate conjunct");
    assert_eq!(final_input.kind(), PlanNodeKind::Limit);
    assert_eq!(final_input.estimated_rows(), Some(8));

    let limits = explained
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Limit)
        .collect::<Vec<_>>();
    assert_eq!(limits.len(), 7);
    assert_eq!(
        limits
            .iter()
            .rev()
            .map(|node| node.estimated_rows())
            .collect::<Vec<_>>(),
        vec![
            Some(80),
            Some(50),
            Some(12),
            Some(8),
            Some(2),
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
            Some(100),
            Some(80),
            Some(23),
            Some(12),
            Some(2),
            Some(2),
            Some(1),
        ]
    );
}

#[test]
fn branch_overflow_survives_caps_and_unknown_statistics_stay_unknown() {
    let source_rows = u64::MAX / 3 + 1;
    let overflow = explain_nested_input_branch_conjunct_cascade(
        Some(source_rows),
        2,
        2,
        &[u64::MAX],
        &[u64::MAX, 2],
        2,
        &[0],
    )
    .expect("branch work overflow through nested input and disjunct limits");
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    let branch_filter = overflow
        .nodes()
        .iter()
        .find(|node| node.details().contains_key("conjunct_count_per_disjunct"))
        .expect("expanded branch filter with proven local overflow");
    assert_eq!(branch_filter.estimated_work(), None);
    assert_eq!(
        branch_filter.details().get("estimated_work_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    let final_filter = overflow
        .nodes()
        .iter()
        .find(|node| node.details().contains_key("conjunct_count"))
        .expect("separate conjunct remains after the limit cascade");
    assert_eq!(final_filter.estimated_rows(), Some(1));
    assert_eq!(final_filter.estimated_work(), Some(3));

    let unknown =
        explain_nested_input_branch_conjunct_cascade(None, 2, 2, &[80, 50], &[12, 8], 2, &[0])
            .expect("unknown source statistics through both conjunct stages");
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
fn every_stage_is_required_and_branch_expansion_is_bounded() {
    assert_eq!(
        explain_nested_input_branch_conjunct_cascade(Some(10), 2, 2, &[], &[4], 2, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_nested_input_branch_conjunct_cascade(Some(10), 2, 2, &[4], &[], 2, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_nested_input_branch_conjunct_cascade(Some(10), 0, 2, &[4], &[2], 2, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_nested_input_branch_conjunct_cascade(Some(10), 2, 0, &[4], &[2], 2, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_nested_input_branch_conjunct_cascade(Some(10), 2, 2, &[4], &[2], 0, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_nested_input_branch_conjunct_cascade(Some(10), 2, u64::MAX, &[4], &[2], 1, &[],),
        Err(ExplainError::TooManyExpressions)
    );
}
