use orna_sys_v1::{
    DisjunctStormBranchDescription, DisjunctStormCascadeDescription,
    DisjunctStormLimitRebindDescription, ExplainError, ExpressionRef, ObjectRef, PlanDetail,
    PlanNodeKind, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_disjunct_storm_branch_limit_chains,
};

const FIXTURE: &str = include_str!("fixtures/planner_storm_limit_chain_rebinds.orna");
const BRANCH_CAP_FIXTURE: &str = include_str!("fixtures/planner_storm_rebind_branch_caps.orna");

fn branch(
    limits: &[u64],
    conjuncts: u64,
    limit_rebinds: Vec<DisjunctStormLimitRebindDescription>,
) -> DisjunctStormBranchDescription {
    DisjunctStormBranchDescription {
        nested_limits: limits.to_vec(),
        conjunct_count: conjuncts,
        limit_rebinds,
        nested_storms: Vec::new(),
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

fn rebind(
    after_limit: usize,
    storms: Vec<DisjunctStormCascadeDescription>,
) -> DisjunctStormLimitRebindDescription {
    DisjunctStormLimitRebindDescription {
        after_limit,
        storms,
    }
}

fn query(rows: Option<u64>, bytes: Option<u64>) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:planner-storm-limit-chain-rebinds"),
        source: ObjectRef::descriptive("table:PlannerStormLimitChainRebinds"),
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
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    }
}

#[test]
fn branch_rebinds_run_after_their_limit_and_feed_the_remaining_chain() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // ORNA-PLAN leaves rebind placement inside a branch-local limit chain
    // unspecified. Each explicit rebind sees that branch's current rows, and
    // its cascade output becomes the next limit's input.
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(100), Some(4096)),
        &[storm(
            "expr:outer-storm",
            vec![
                branch(
                    &[20, 6],
                    1,
                    vec![
                        rebind(
                            1,
                            vec![storm(
                                "expr:first-limit-rebind",
                                vec![branch(&[5], 1, vec![])],
                            )],
                        ),
                        rebind(
                            2,
                            vec![storm(
                                "expr:second-limit-rebind",
                                vec![branch(&[2], 1, vec![])],
                            )],
                        ),
                    ],
                ),
                branch(&[8], 1, vec![]),
            ],
        )],
        &[],
    )
    .expect("branch-local rebinds compose with later limits and sibling branches");

    let filter = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("storm is represented by an aggregate filter");
    assert_eq!(filter.estimated_rows(), Some(5));
    assert_eq!(filter.estimated_bytes(), Some(182));
    assert_eq!(filter.estimated_work(), Some(242));
    assert_eq!(
        filter.details().get("limit_chain_rebind_shapes"),
        Some(&PlanDetail::Text("1@1:[1:[5]/1];1@2:[1:[2]/1]".to_owned()))
    );
    assert_eq!(
        filter.details().get("limit_chain_rebind_predicates"),
        Some(&PlanDetail::Expressions(vec![
            ExpressionRef::descriptive("expr:first-limit-rebind"),
            ExpressionRef::descriptive("expr:second-limit-rebind"),
        ]))
    );
}

#[test]
fn rebind_overflow_is_preserved_and_positions_are_validated() {
    let overflowing_child = storm(
        "expr:overflowing-rebind",
        (0..8).map(|_| branch(&[u64::MAX], 1, vec![])).collect(),
    );
    let overflow = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(u64::MAX / 4), Some(0)),
        &[storm(
            "expr:outer-storm",
            vec![branch(
                &[u64::MAX, 0],
                1,
                vec![rebind(1, vec![overflowing_child])],
            )],
        )],
        &[],
    )
    .expect("rebind work overflow remains visible after the chain reaches zero");
    assert_eq!(overflow.root().estimated_rows(), Some(0));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    assert!(overflow.nodes().iter().any(|node| {
        node.details().get("estimated_work_overflow") == Some(&PlanDetail::Boolean(true))
    }));

    for invalid_rebind in [
        rebind(
            0,
            vec![storm("expr:zero-position", vec![branch(&[1], 1, vec![])])],
        ),
        rebind(
            2,
            vec![storm("expr:past-chain", vec![branch(&[1], 1, vec![])])],
        ),
        rebind(1, vec![]),
    ] {
        assert_eq!(
            explain_query_with_disjunct_storm_branch_limit_chains(
                &query(Some(10), Some(0)),
                &[storm(
                    "expr:invalid-rebind",
                    vec![branch(&[4], 1, vec![invalid_rebind])],
                )],
                &[],
            ),
            Err(ExplainError::InvalidExpression)
        );
    }

    assert_eq!(
        explain_query_with_disjunct_storm_branch_limit_chains(
            &query(Some(10), Some(0)),
            &[storm(
                "expr:unordered-rebinds",
                vec![branch(
                    &[4, 2],
                    1,
                    vec![
                        rebind(2, vec![storm("expr:later", vec![branch(&[1], 1, vec![])])]),
                        rebind(1, vec![storm("expr:earlier", vec![branch(&[1], 1, vec![])])]),
                    ],
                )],
            )],
            &[],
        ),
        Err(ExplainError::InvalidExpression)
    );
}

#[test]
fn nested_rebind_cascades_stay_inside_each_branch_local_storm_cap() {
    let parsed = orna_syntax_v1::parse_module(BRANCH_CAP_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // ORNA-PLAN does not define how nested cascade estimates interact with a
    // parent branch's local cap. Keep each rebind inside its immediate bounded
    // branch input, then combine sibling branches under their shared input cap.
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(100), Some(1_000)),
        &[storm(
            "expr:outer-cap-storm",
            vec![
                branch(
                    &[4, 100],
                    1,
                    vec![rebind(
                        1,
                        vec![storm(
                            "expr:two-arm-rebind",
                            vec![branch(&[20], 1, vec![]), branch(&[20], 1, vec![])],
                        )],
                    )],
                ),
                branch(&[12], 1, vec![]),
            ],
        )],
        &[],
    )
    .expect("nested rebind branches remain isolated by their parent caps");

    let filter = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("storm is represented by an aggregate filter");
    assert_eq!(filter.estimated_rows(), Some(8));
    assert_eq!(filter.estimated_bytes(), Some(80));
    assert_eq!(filter.estimated_work(), Some(236));
    assert_eq!(
        filter.details().get("branch_limit_chains"),
        Some(&PlanDetail::Text("1:[4,100];2:[12]".to_owned()))
    );
    assert_eq!(
        filter.details().get("limit_chain_rebind_shapes"),
        Some(&PlanDetail::Text("1@1:[1:[20]/1;2:[20]/1]".to_owned()))
    );
    assert_eq!(
        filter.details().get("branch_local_storm_cap_scope"),
        Some(&PlanDetail::Text(
            "each_cascade_output_capped_to_immediate_input_rows_and_bytes".to_owned()
        ))
    );
    assert_eq!(
        filter.details().get("limit_chain_rebind_cap_scope"),
        Some(&PlanDetail::Text(
            "post_limit_branch_rows_and_bytes".to_owned()
        ))
    );
}

#[test]
fn overflowing_rebind_work_keeps_the_parent_branch_cap() {
    let source_rows = u64::MAX / 4;
    let overflowing_rebind = storm(
        "expr:wide-rebind-storm",
        (0..8)
            .map(|_| branch(&[u64::MAX], 1, vec![]))
            .collect(),
    );
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(source_rows), Some(0)),
        &[storm(
            "expr:outer-cap-storm",
            vec![branch(
                &[u64::MAX, u64::MAX],
                1,
                vec![rebind(1, vec![overflowing_rebind])],
            )],
        )],
        &[],
    )
    .expect("the branch cap and nested work overflow are both retained");

    let rows = explained.root().estimated_rows().expect("known source rows");
    assert!(rows > 0);
    assert!(rows <= source_rows);
    assert_eq!(
        explained.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    assert!(explained.nodes().iter().any(|node| {
        node.details().get("estimated_work_overflow") == Some(&PlanDetail::Boolean(true))
    }));
}
