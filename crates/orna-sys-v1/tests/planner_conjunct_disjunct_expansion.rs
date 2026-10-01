use orna_sys_v1::{
    ExplainError, ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef, explain_query_with_conjunct_disjunct_limit_chain,
};

const CONJUNCT_DISJUNCT_FIXTURE: &str =
    include_str!("fixtures/planner_conjunct_disjunct_expansion.orna");

fn explain_conjunct_disjunct_chain(
    rows: Option<u64>,
    disjunct_count: u64,
    conjunct_count_per_disjunct: u64,
    additional_limits: &[u64],
) -> Result<orna_sys_v1::ExplainedPlan, ExplainError> {
    explain_query_with_conjunct_disjunct_limit_chain(
        &QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:conjunct-disjunct-expansion"),
            source: ObjectRef::descriptive("table:PlannerConjunctDisjunctExpansion"),
            source_statistics: rows.map(|estimated_rows| QuerySourceStatistics {
                estimated_rows: Some(estimated_rows),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: Some(ExpressionRef::descriptive("expr:and-branches-or-expanded")),
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: Some(u64::MAX),
            mutations: Vec::new(),
            materialize_into: None,
        },
        disjunct_count,
        conjunct_count_per_disjunct,
        additional_limits,
    )
}

#[test]
fn conjunct_branch_work_overflows_only_after_disjunct_expansion() {
    let parsed = orna_syntax_v1::parse_module(CONJUNCT_DISJUNCT_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // ORNA-PLAN leaves expanded predicate estimates unspecified. Apply the
    // documented independent 50%-per-conjunct fallback and charge each arm
    // for the full input, with later AND terms seeing the rounded-up survivors.
    // One two-conjunct arm fits; the second expanded arm crosses MAX by 3.
    let source_rows = u64::MAX / 3 + 1;
    let first_conjunct_rows = source_rows.div_ceil(2);
    let one_arm_work = u128::from(source_rows) + u128::from(first_conjunct_rows);
    let expanded_work = one_arm_work * 2;
    assert!(one_arm_work <= u128::from(u64::MAX));
    assert_eq!(expanded_work, u128::from(u64::MAX) + 3);

    let one_arm_rows = source_rows.div_ceil(4);
    let second_arm_rows = (source_rows - one_arm_rows).div_ceil(4);
    let final_rows = one_arm_rows + second_arm_rows;
    let nested = explain_conjunct_disjunct_chain(Some(source_rows), 2, 2, &[u64::MAX, 0])
        .expect("expanded conjunct branches under nested limits");
    let filter = nested
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("expanded conjunct filter");
    assert_eq!(filter.estimated_work(), None);
    assert_eq!(filter.estimated_rows(), Some(final_rows));
    assert_eq!(
        filter.details().get("estimated_work_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    assert_eq!(
        filter.details().get("conjunct_count_per_disjunct"),
        Some(&PlanDetail::Integer(2))
    );
    assert_eq!(
        filter.details().get("expansion_work"),
        Some(&PlanDetail::Text("full_input_per_disjunct".to_owned()))
    );
    assert_eq!(nested.root().estimated_rows(), Some(0));
    assert_eq!(
        nested.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "nested limits cannot erase expanded branch-work overflow"
    );

    let limits = nested
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Limit)
        .collect::<Vec<_>>();
    assert_eq!(limits.len(), 3);
    assert_eq!(
        limits
            .iter()
            .rev()
            .map(|node| node.estimated_rows())
            .collect::<Vec<_>>(),
        vec![Some(final_rows), Some(final_rows), Some(0)]
    );
}

#[test]
fn unknown_conjunct_branch_inputs_do_not_claim_overflow() {
    let unknown = explain_conjunct_disjunct_chain(None, 2, 2, &[0, u64::MAX, 0])
        .expect("unknown expanded-conjunct source statistics");
    assert_eq!(unknown.plan().estimated_cost(), None);
    assert_eq!(unknown.root().estimated_rows(), None);
    assert_eq!(
        unknown.root().details().get("estimated_cost_overflow"),
        None
    );

    assert_eq!(
        explain_conjunct_disjunct_chain(Some(10), 0, 2, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_conjunct_disjunct_chain(Some(10), 2, 0, &[]),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_conjunct_disjunct_chain(Some(10), 2, u64::MAX, &[]),
        Err(ExplainError::TooManyExpressions)
    );
}
