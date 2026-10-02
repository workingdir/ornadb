use orna_sys_v1::{
    DisjunctStormBranchDescription, DisjunctStormCascadeDescription, ExplainError, ExpressionRef,
    ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_disjunct_storm_branch_limit_chains,
};

const FIXTURE: &str = include_str!("fixtures/planner_storm_nested_branch_cascades.orna");

fn branch(
    limits: &[u64],
    conjuncts: u64,
    nested_storms: Vec<DisjunctStormCascadeDescription>,
) -> DisjunctStormBranchDescription {
    DisjunctStormBranchDescription {
        nested_limits: limits.to_vec(),
        conjunct_count: conjuncts,
        limit_rebinds: Vec::new(),
        nested_storms,
    }
}

fn storm(
    predicate: &str,
    branches: Vec<DisjunctStormBranchDescription>,
) -> DisjunctStormCascadeDescription {
    DisjunctStormCascadeDescription {
        predicate: ExpressionRef::descriptive(predicate),
        branches,
    }
}

fn query(rows: Option<u64>, bytes: Option<u64>, limit: Option<u64>) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:planner-storm-nested-branch-cascades"),
        source: ObjectRef::descriptive("table:PlannerStormNestedBranchCascades"),
        source_statistics: (rows.is_some() || bytes.is_some()).then_some(QuerySourceStatistics {
            estimated_rows: rows,
            estimated_bytes: bytes,
            mutable_branch: None,
        }),
        joins: Vec::new(),
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit,
        mutations: Vec::new(),
        materialize_into: None,
    }
}

#[test]
fn nested_branch_storms_apply_local_chains_before_cascading_to_later_stages() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 3);

    // The reference does not prescribe nested branch-cascade aggregation.
    // Apply each child cascade to its parent's filtered branch, then combine
    // sibling matches in declaration order, capped by their common input.
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(100), Some(0), Some(2)),
        &[
            storm(
                "expr:outer-cascade",
                vec![
                    branch(
                        &[40, 10],
                        1,
                        vec![storm(
                            "expr:first-nested-cascade",
                            vec![branch(&[6], 1, vec![]), branch(&[12, 4], 2, vec![])],
                        )],
                    ),
                    branch(
                        &[30],
                        2,
                        vec![storm(
                            "expr:second-nested-cascade",
                            vec![branch(&[10], 1, vec![]), branch(&[4, 2], 1, vec![])],
                        )],
                    ),
                ],
            ),
            storm(
                "expr:final-cascade",
                vec![
                    branch(
                        &[5],
                        1,
                        vec![storm(
                            "expr:third-nested-cascade",
                            vec![branch(&[2], 1, vec![]), branch(&[1], 1, vec![])],
                        )],
                    ),
                    branch(
                        &[3, 2],
                        2,
                        vec![storm(
                            "expr:fourth-nested-cascade",
                            vec![branch(&[1], 1, vec![]), branch(&[1, 1], 2, vec![])],
                        )],
                    ),
                ],
            ),
        ],
        &[1, 0],
    )
    .expect("nested branch-local cascades compose through later storms");

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
        vec![Some(9), Some(3)]
    );
    assert_eq!(
        filters
            .iter()
            .rev()
            .map(|node| node.estimated_work())
            .collect::<Vec<_>>(),
        vec![Some(351), Some(44)]
    );
    assert_eq!(
        filters[1].details().get("nested_cascade_shapes"),
        Some(&PlanDetail::Text(
            "1:[1:[6]/1;2:[12,4]/2];2:[1:[10]/1;2:[4,2]/1]".to_owned()
        ))
    );
    assert_eq!(
        filters[1].details().get("nested_cascade_predicates"),
        Some(&PlanDetail::Expressions(vec![
            ExpressionRef::descriptive("expr:first-nested-cascade"),
            ExpressionRef::descriptive("expr:second-nested-cascade"),
        ]))
    );
    assert_eq!(explained.root().estimated_rows(), Some(0));
    assert_eq!(explained.plan().estimated_cost(), Some("501"));
}

#[test]
fn nested_work_overflow_survives_later_limits_and_unknown_rows_remain_unknown() {
    let inner = storm(
        "expr:overflowing-nested-cascade",
        (0..8).map(|_| branch(&[u64::MAX], 1, vec![])).collect(),
    );
    let overflow = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(u64::MAX / 4), Some(0), None),
        &[
            storm(
                "expr:parent-cascade",
                vec![branch(&[u64::MAX], 1, vec![inner])],
            ),
            storm(
                "expr:zero-after-nested-overflow",
                vec![branch(&[0], 1, vec![])],
            ),
        ],
        &[0],
    )
    .expect("nested overflow is retained through later zero limits");
    assert_eq!(overflow.root().estimated_rows(), Some(0));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    assert!(overflow.nodes().iter().any(|node| {
        node.details().get("estimated_work_overflow") == Some(&PlanDetail::Boolean(true))
    }));

    let unknown = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(None, Some(4096), None),
        &[storm(
            "expr:unknown-parent",
            vec![branch(
                &[u64::MAX],
                1,
                vec![storm(
                    "expr:unknown-child",
                    vec![branch(&[0], 1, vec![]), branch(&[u64::MAX], 2, vec![])],
                )],
            )],
        )],
        &[],
    )
    .expect("nested storms preserve unknown row counts");
    assert_eq!(unknown.root().estimated_rows(), None);
    assert_eq!(unknown.root().estimated_bytes(), Some(1536));
    assert_eq!(unknown.plan().estimated_cost(), None);
    assert_eq!(
        unknown.root().details().get("estimated_cost_overflow"),
        None
    );
}

#[test]
fn nested_storm_shapes_are_bounded_and_validated_recursively() {
    assert_eq!(
        explain_query_with_disjunct_storm_branch_limit_chains(
            &query(Some(10), Some(0), None),
            &[storm(
                "expr:empty-nested-cascade",
                vec![branch(&[5], 1, vec![storm("expr:empty-child", vec![])])],
            )],
            &[],
        ),
        Err(ExplainError::InvalidExpression)
    );

    let mut too_deep = storm("expr:deep-leaf", vec![branch(&[1], 1, vec![])]);
    for depth in 0..64 {
        too_deep = storm(
            &format!("expr:deep-{depth}"),
            vec![branch(&[1], 1, vec![too_deep])],
        );
    }
    assert_eq!(
        explain_query_with_disjunct_storm_branch_limit_chains(
            &query(Some(10), Some(0), None),
            &[too_deep],
            &[],
        ),
        Err(ExplainError::TooManyNodes)
    );
}
