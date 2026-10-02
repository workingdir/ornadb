use orna_sys_v1::{
    DisjunctStormBranchDescription, DisjunctStormCascadeDescription, ExplainError, ExpressionRef,
    ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_disjunct_storm_branch_limit_chains,
};

const FIXTURE: &str = include_str!("fixtures/planner_storm_branch_limit_chains.orna");

fn branch(limits: &[u64], conjuncts: u64) -> DisjunctStormBranchDescription {
    DisjunctStormBranchDescription {
        nested_limits: limits.to_vec(),
        conjunct_count: conjuncts,
    }
}

fn cascade(
    predicate: &str,
    branches: Vec<DisjunctStormBranchDescription>,
) -> DisjunctStormCascadeDescription {
    DisjunctStormCascadeDescription {
        predicate: ExpressionRef::descriptive(predicate),
        branches,
    }
}

fn query(
    rows: Option<u64>,
    bytes: Option<u64>,
    limit: Option<u64>,
    has_predicate: bool,
) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:planner-storm-branch-limit-chains"),
        source: ObjectRef::descriptive("table:PlannerStormBranchLimitChains"),
        source_statistics: (rows.is_some() || bytes.is_some()).then_some(QuerySourceStatistics {
            estimated_rows: rows,
            estimated_bytes: bytes,
            mutable_branch: None,
        }),
        joins: Vec::new(),
        predicate: has_predicate.then(|| ExpressionRef::descriptive("expr:unexpected-base")),
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit,
        mutations: Vec::new(),
        materialize_into: None,
    }
}

#[test]
fn nested_storms_keep_distinct_branch_limit_chains_and_feed_the_next_stage() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 3);

    // ORNA-PLAN leaves this branch-local cost composition unspecified. Apply
    // each limit/conjunct chain independently, then combine disjoint matches
    // in source order; each later storm consumes the prior storm's output.
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(240), Some(0), Some(6), false),
        &[
            cascade(
                "expr:first-cascade",
                vec![
                    branch(&[120, 30], 2),
                    branch(&[90, 20, 10], 1),
                    branch(&[60], 3),
                ],
            ),
            cascade(
                "expr:second-cascade",
                vec![branch(&[12, 7], 1), branch(&[9], 2), branch(&[5, 2], 2)],
            ),
        ],
        &[4, 0],
    )
    .expect("branch-local chains cascade across two nested storms");

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
        vec![Some(21), Some(8)]
    );
    assert_eq!(
        filters
            .iter()
            .rev()
            .map(|node| node.estimated_work())
            .collect::<Vec<_>>(),
        vec![Some(1_110), Some(104)]
    );
    assert_eq!(
        filters[1].details().get("branch_limit_chains"),
        Some(&PlanDetail::Text(
            "1:[120,30];2:[90,20,10];3:[60]".to_owned()
        ))
    );
    assert_eq!(
        filters[1].details().get("branch_conjunct_counts"),
        Some(&PlanDetail::Text("2,1,3".to_owned()))
    );
    assert_eq!(
        filters[0].details().get("branch_limit_chains"),
        Some(&PlanDetail::Text("1:[12,7];2:[9];3:[5,2]".to_owned()))
    );

    let limits = explained
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
        vec![Some(6), Some(4), Some(0)]
    );
    assert_eq!(explained.root().estimated_rows(), Some(0));
    assert_eq!(explained.plan().estimated_cost(), Some("1472"));
}

#[test]
fn known_work_overflow_survives_later_zero_limits_and_unknown_rows_stay_unknown() {
    let overflow = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(u64::MAX / 2 + 1), Some(0), None, false),
        &[
            cascade(
                "expr:overflowing-branch-chains",
                vec![branch(&[u64::MAX], 1), branch(&[u64::MAX], 1)],
            ),
            cascade("expr:later-zero-branch", vec![branch(&[0], 1)]),
        ],
        &[0],
    )
    .expect("known branch-work overflow remains part of the plan");
    assert_eq!(overflow.root().estimated_rows(), Some(0));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    assert!(overflow.nodes().iter().any(|node| {
        node.details().get("disjunct_storm") == Some(&PlanDetail::Integer(1))
            && node.details().get("estimated_work_overflow") == Some(&PlanDetail::Boolean(true))
    }));

    let unknown = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(None, Some(4096), None, false),
        &[cascade(
            "expr:unknown-branch-chains",
            vec![branch(&[u64::MAX], 2), branch(&[1, 0], 1)],
        )],
        &[],
    )
    .expect("unknown source rows remain unknown");
    assert_eq!(unknown.root().estimated_rows(), None);
    assert_eq!(unknown.root().estimated_bytes(), Some(3_072));
    assert_eq!(unknown.plan().estimated_cost(), None);
    assert_eq!(
        unknown.root().details().get("estimated_cost_overflow"),
        None
    );
}

#[test]
fn branch_chain_cascades_reject_empty_invalid_and_oversized_shapes() {
    assert_eq!(
        explain_query_with_disjunct_storm_branch_limit_chains(
            &query(Some(10), Some(0), None, false),
            &[],
            &[]
        ),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_query_with_disjunct_storm_branch_limit_chains(
            &query(Some(10), Some(0), None, false),
            &[cascade("expr:no-branches", vec![])],
            &[],
        ),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_query_with_disjunct_storm_branch_limit_chains(
            &query(Some(10), Some(0), None, false),
            &[cascade("expr:no-limits", vec![branch(&[], 1)])],
            &[],
        ),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_query_with_disjunct_storm_branch_limit_chains(
            &query(Some(10), Some(0), None, false),
            &[cascade("expr:no-conjuncts", vec![branch(&[4], 0)])],
            &[],
        ),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_query_with_disjunct_storm_branch_limit_chains(
            &query(Some(10), Some(0), None, true),
            &[cascade(
                "expr:duplicate-query-predicate",
                vec![branch(&[4], 1)]
            )],
            &[],
        ),
        Err(ExplainError::InvalidExpression)
    );
    assert_eq!(
        explain_query_with_disjunct_storm_branch_limit_chains(
            &query(Some(10), Some(0), None, false),
            &[cascade(
                "expr:too-many-conjuncts",
                vec![branch(&[4], 8_193)]
            )],
            &[],
        ),
        Err(ExplainError::TooManyExpressions)
    );
}
