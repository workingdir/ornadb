use orna_sys_v1::{
    ExplainError, ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef, explain_query_with_disjunct_branch_limit_conjunct_cascade,
};

const FIXTURE: &str = include_str!("fixtures/planner_disjunct_branch_limit_conjunct_cascade.orna");

fn explain_branch_limit_conjunct_cascade(
    rows: Option<u64>,
    disjunct_count: u64,
    nested_branch_limits: &[u64],
    conjunct_count_per_disjunct: u64,
    additional_limits: &[u64],
    has_predicate: bool,
) -> Result<orna_sys_v1::ExplainedPlan, ExplainError> {
    explain_query_with_disjunct_branch_limit_conjunct_cascade(
        &QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:disjunct-branch-limit-conjunct-cascade"),
            source: ObjectRef::descriptive("table:PlannerDisjunctBranchLimitConjunctCascade"),
            source_statistics: rows.map(|estimated_rows| QuerySourceStatistics {
                estimated_rows: Some(estimated_rows),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: has_predicate
                .then(|| ExpressionRef::descriptive("expr:two-limited-conjunctive-branches")),
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: Some(12),
            mutations: Vec::new(),
            materialize_into: None,
        },
        disjunct_count,
        nested_branch_limits,
        conjunct_count_per_disjunct,
        additional_limits,
    )
}

#[test]
fn branch_limit_chains_feed_conjuncts_before_the_disjunct_limit_cascade() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // ORNA-PLAN leaves the selectivity of nested branch limits and expanded
    // conjunctions unspecified. This fallback caps each branch independently,
    // charges each cap for every branch, applies AND terms left-to-right, and
    // only then combines the OR estimates and applies the output LIMIT chain.
    let explained =
        explain_branch_limit_conjunct_cascade(Some(100), 2, &[80, 50], 2, &[8, 3], true)
            .expect("nested branch caps, branch conjuncts, and disjunct limit cascade");

    let filters = explained
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Filter)
        .collect::<Vec<_>>();
    assert_eq!(filters.len(), 1);
    let filter = filters[0];
    assert_eq!(filter.estimated_rows(), Some(23));
    assert_eq!(filter.estimated_work(), Some(150));
    assert_eq!(filter.inputs().len(), 1);
    assert_eq!(
        serde_json::to_value(filter).expect("filter serializes")["predicate"],
        "expr:two-limited-conjunctive-branches"
    );
    assert_eq!(
        filter.details().get("conjunct_count_per_disjunct"),
        Some(&PlanDetail::Integer(2))
    );

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
            .map(|node| node.details().get("limit").cloned())
            .collect::<Vec<_>>(),
        vec![
            Some(PlanDetail::Integer(80)),
            Some(PlanDetail::Integer(50)),
            Some(PlanDetail::Integer(12)),
            Some(PlanDetail::Integer(8)),
            Some(PlanDetail::Integer(3)),
        ]
    );
    let branch_limits = limits
        .iter()
        .filter(|limit| limit.details().contains_key("limit_scope"))
        .collect::<Vec<_>>();
    assert_eq!(branch_limits.len(), 2);
    assert!(branch_limits.iter().all(|limit| {
        limit.estimated_work().is_some()
            && limit.details().get("limit_scope")
                == Some(&PlanDetail::Text("per_disjunct_branch".to_owned()))
            && limit.details().get("disjunct_count") == Some(&PlanDetail::Integer(2))
    }));
    assert_eq!(
        limits
            .iter()
            .rev()
            .map(|node| node.estimated_rows())
            .collect::<Vec<_>>(),
        vec![Some(80), Some(50), Some(12), Some(8), Some(3)]
    );
    assert_eq!(
        limits
            .iter()
            .rev()
            .map(|node| node.estimated_work())
            .collect::<Vec<_>>(),
        vec![Some(200), Some(160), Some(23), Some(12), Some(8)]
    );
    assert_eq!(explained.plan().estimated_cost(), Some("653"));
}

#[test]
fn branch_limit_overflow_survives_zero_caps_and_unknowns_stay_unknown() {
    let overflow_rows = u64::MAX / 2 + 1;
    let overflow = explain_branch_limit_conjunct_cascade(
        Some(overflow_rows),
        2,
        &[u64::MAX, 0],
        2,
        &[0],
        true,
    )
    .expect("known branch limit work overflow through zero caps");
    let first_branch_limit = overflow
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Limit
                && node.details().get("limit") == Some(&PlanDetail::Integer(u64::MAX))
        })
        .expect("uncapped first branch limit");
    assert_eq!(first_branch_limit.estimated_work(), None);
    assert_eq!(
        first_branch_limit.details().get("estimated_work_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    assert_eq!(overflow.root().estimated_rows(), Some(0));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );

    let unknown = explain_branch_limit_conjunct_cascade(None, 2, &[80, 0], 2, &[0], true)
        .expect("unknown source counts through branch and output limits");
    assert_eq!(unknown.root().estimated_rows(), None);
    assert_eq!(unknown.plan().estimated_cost(), None);
    assert_eq!(
        unknown.root().details().get("estimated_cost_overflow"),
        None
    );
}

#[test]
fn branch_cascade_requires_all_stages_and_respects_expression_bound() {
    assert_eq!(
        explain_branch_limit_conjunct_cascade(Some(10), 0, &[4], 2, &[], true),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_branch_limit_conjunct_cascade(Some(10), 2, &[], 2, &[], true),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_branch_limit_conjunct_cascade(Some(10), 2, &[4], 0, &[], true),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_branch_limit_conjunct_cascade(Some(10), 2, &[4], 2, &[], false),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_branch_limit_conjunct_cascade(Some(10), 2, &[4], u64::MAX, &[], true),
        Err(ExplainError::TooManyExpressions)
    );
}
