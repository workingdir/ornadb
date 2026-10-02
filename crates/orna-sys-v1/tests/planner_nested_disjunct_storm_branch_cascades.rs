use orna_sys_v1::{
    DisjunctStormDescription, ExplainError, ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_disjunct_storm_chain,
};

const FIXTURE: &str = include_str!("fixtures/planner_nested_disjunct_storm_branch_cascades.orna");

fn storm(
    predicate: &str,
    disjunct_count: u64,
    limits: &[u64],
    conjuncts: u64,
) -> DisjunctStormDescription {
    DisjunctStormDescription {
        predicate: ExpressionRef::descriptive(predicate),
        disjunct_count,
        nested_branch_limits: limits.to_vec(),
        conjunct_count_per_disjunct: conjuncts,
    }
}

fn explain_storms(
    rows: Option<u64>,
    storms: &[DisjunctStormDescription],
    additional_limits: &[u64],
    has_query_predicate: bool,
) -> Result<orna_sys_v1::ExplainedPlan, ExplainError> {
    explain_query_with_disjunct_storm_chain(
        &QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:nested-disjunct-storm-branch-cascades"),
            source: ObjectRef::descriptive("table:PlannerNestedDisjunctStormBranchCascades"),
            source_statistics: rows.map(|estimated_rows| QuerySourceStatistics {
                estimated_rows: Some(estimated_rows),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: has_query_predicate
                .then(|| ExpressionRef::descriptive("expr:unexpected-base-predicate")),
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: Some(4),
            mutations: Vec::new(),
            materialize_into: None,
        },
        storms,
        additional_limits,
    )
}

#[test]
fn storm_stages_preserve_branch_limit_conjunct_order_and_outer_limit_work() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 3);

    // The reference leaves nested disjunct storm aggregation unspecified.
    // Model two stages in order: the first expands two AND branches after
    // their 80/40 caps; its output feeds a three-branch storm with 12/6 caps.
    let explained = explain_storms(
        Some(100),
        &[
            storm("expr:first-storm", 2, &[80, 40], 2),
            storm("expr:second-storm", 3, &[12, 6], 1),
        ],
        &[2, 0],
        false,
    )
    .expect("nested branch limit and conjunct cascades across two storms");

    let filters = explained
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Filter)
        .collect::<Vec<_>>();
    assert_eq!(filters.len(), 2);
    assert_eq!(
        filters
            .iter()
            .rev()
            .map(|node| node.estimated_rows())
            .collect::<Vec<_>>(),
        vec![Some(18), Some(6)]
    );
    assert_eq!(
        filters
            .iter()
            .rev()
            .map(|node| node.estimated_work())
            .collect::<Vec<_>>(),
        vec![Some(120), Some(18)]
    );
    assert_eq!(
        filters
            .iter()
            .rev()
            .map(|node| node.details().get("disjunct_storm").cloned())
            .collect::<Vec<_>>(),
        vec![Some(PlanDetail::Integer(1)), Some(PlanDetail::Integer(2))]
    );

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
            .map(|node| node.details().get("limit").cloned())
            .collect::<Vec<_>>(),
        vec![
            Some(PlanDetail::Integer(80)),
            Some(PlanDetail::Integer(40)),
            Some(PlanDetail::Integer(12)),
            Some(PlanDetail::Integer(6)),
            Some(PlanDetail::Integer(4)),
            Some(PlanDetail::Integer(2)),
            Some(PlanDetail::Integer(0)),
        ]
    );
    assert_eq!(
        limits
            .iter()
            .rev()
            .map(|node| node.estimated_rows())
            .collect::<Vec<_>>(),
        vec![
            Some(80),
            Some(40),
            Some(12),
            Some(6),
            Some(4),
            Some(2),
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
            Some(200),
            Some(160),
            Some(54),
            Some(36),
            Some(6),
            Some(4),
            Some(2),
        ]
    );
    assert_eq!(explained.plan().estimated_cost(), Some("700"));
}

#[test]
fn known_overflow_survives_later_storms_while_unknown_counts_stay_unknown() {
    let overflow = explain_storms(
        Some(u64::MAX / 2 + 1),
        &[
            storm("expr:overflowing-first-storm", 2, &[u64::MAX], 1),
            storm("expr:zero-row-second-storm", 2, &[0], 2),
        ],
        &[0],
        false,
    )
    .expect("nested storm work overflow persists through a zero output limit");
    assert_eq!(overflow.root().estimated_rows(), Some(0));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    assert!(overflow.nodes().iter().any(|node| {
        node.kind() == PlanNodeKind::Limit
            && node.details().get("disjunct_storm") == Some(&PlanDetail::Integer(1))
            && node.details().get("estimated_work_overflow") == Some(&PlanDetail::Boolean(true))
    }));

    let unknown = explain_storms(
        None,
        &[
            storm("expr:unknown-first-storm", 2, &[80, 0], 1),
            storm("expr:unknown-second-storm", 3, &[u64::MAX], 2),
        ],
        &[0],
        false,
    )
    .expect("unknown rows remain unknown through nested storms");
    assert_eq!(unknown.root().estimated_rows(), None);
    assert_eq!(unknown.plan().estimated_cost(), None);
    assert_eq!(
        unknown.root().details().get("estimated_cost_overflow"),
        None
    );
}

#[test]
fn storm_chain_requires_stages_and_rejects_invalid_or_oversized_shapes() {
    assert_eq!(
        explain_storms(Some(10), &[], &[], false),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_storms(
            Some(10),
            &[storm("expr:empty-limits", 2, &[], 1)],
            &[],
            false
        ),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_storms(
            Some(10),
            &[storm("expr:zero-branches", 0, &[4], 1)],
            &[],
            false
        ),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_storms(
            Some(10),
            &[storm("expr:zero-conjuncts", 2, &[4], 0)],
            &[],
            false
        ),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_storms(
            Some(10),
            &[storm("expr:wrong-base-predicate", 2, &[4], 1)],
            &[],
            true,
        ),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_storms(
            Some(10),
            &[
                storm("expr:large-first-storm", u64::MAX, &[4], 2),
                storm("expr:large-second-storm", 1, &[4], 1),
            ],
            &[],
            false,
        ),
        Err(ExplainError::TooManyExpressions)
    );
}
