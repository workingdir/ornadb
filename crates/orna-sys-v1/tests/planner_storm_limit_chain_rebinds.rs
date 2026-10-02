use orna_sys_v1::{
    DisjunctStormBranchDescription, DisjunctStormCascadeDescription,
    DisjunctStormLimitRebindDescription, ExplainError, ExpressionRef, ObjectRef, PlanDetail,
    PlanNodeKind, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_disjunct_storm_branch_limit_chains,
};

const FIXTURE: &str = include_str!("fixtures/planner_storm_limit_chain_rebinds.orna");
const BRANCH_CAP_FIXTURE: &str = include_str!("fixtures/planner_storm_rebind_branch_caps.orna");
const NESTED_BRANCH_CAP_FIXTURE: &str =
    include_str!("fixtures/planner_storm_nested_rebind_caps.orna");
const SEQUENTIAL_BRANCH_CAP_FIXTURE: &str =
    include_str!("fixtures/planner_storm_sequential_rebind_caps.orna");
const REBIND_STAGE_CAP_FIXTURE: &str =
    include_str!("fixtures/planner_storm_rebind_stage_caps.orna");
const SEQUENTIAL_REBIND_POSITIONS_FIXTURE: &str =
    include_str!("fixtures/planner_storm_sequential_rebind_positions.orna");
const REBIND_CAP_STABILITY_FIXTURE: &str =
    include_str!("fixtures/planner_storm_rebind_cap_stability.orna");

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
                        rebind(
                            1,
                            vec![storm("expr:earlier", vec![branch(&[1], 1, vec![])])]
                        ),
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
            "each_cascade_output_capped_to_immediate_input_rows_and_bytes_at_every_nesting_depth"
                .to_owned()
        ))
    );
    assert_eq!(
        filter.details().get("limit_chain_rebind_cap_scope"),
        Some(&PlanDetail::Text(
            "post_limit_branch_rows_and_bytes_at_every_rebind_nesting_depth".to_owned()
        ))
    );
}

#[test]
fn nested_rebind_cascades_keep_caps_isolated_across_each_limit_chain() {
    let parsed = orna_syntax_v1::parse_module(NESTED_BRANCH_CAP_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // Each nested cascade starts from its parent's bounded branch estimate.
    // Three half-selective arms can request more rows than that local input;
    // their union stays within the cap before the following limit runs.
    let deep_storm = storm(
        "expr:deep-three-arm-storm",
        (0..3).map(|_| branch(&[90], 1, vec![])).collect(),
    );
    let mut middle_branch = branch(&[50, 100], 1, vec![rebind(1, vec![deep_storm.clone()])]);
    middle_branch.nested_storms.push(storm(
        "expr:post-rebind-three-arm-storm",
        (0..3).map(|_| branch(&[90], 1, vec![])).collect(),
    ));
    let mut outer_branch = branch(
        &[6, 100],
        1,
        vec![rebind(
            1,
            vec![storm(
                "expr:middle-rebind-storm",
                vec![
                    middle_branch,
                    branch(&[50], 1, vec![]),
                    branch(&[50], 1, vec![]),
                ],
            )],
        )],
    );
    outer_branch.nested_storms.push(storm(
        "expr:outer-post-limit-storm",
        (0..3).map(|_| branch(&[90], 1, vec![])).collect(),
    ));

    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(100), Some(1_000)),
        &[storm(
            "expr:outer-storm",
            vec![outer_branch, branch(&[20], 1, vec![])],
        )],
        &[],
    )
    .expect("nested rebinds preserve every enclosing branch cap");

    let filter = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("storm is represented by an aggregate filter");
    assert_eq!(filter.estimated_rows(), Some(13));
    assert_eq!(filter.estimated_bytes(), Some(130));
    assert_eq!(filter.estimated_work(), Some(346));
    assert_eq!(
        filter.details().get("branch_limit_chains"),
        Some(&PlanDetail::Text("1:[6,100];2:[20]".to_owned()))
    );
    assert_eq!(
        filter.details().get("branch_local_storm_cap_scope"),
        Some(&PlanDetail::Text(
            "each_cascade_output_capped_to_immediate_input_rows_and_bytes_at_every_nesting_depth"
                .to_owned()
        ))
    );
    assert_eq!(
        filter.details().get("limit_chain_rebind_cap_scope"),
        Some(&PlanDetail::Text(
            "post_limit_branch_rows_and_bytes_at_every_rebind_nesting_depth".to_owned()
        ))
    );
    assert_eq!(
        filter.details().get("limit_chain_rebind_shapes"),
        Some(&PlanDetail::Text(
            "1@1:[1:[50,100]/1@1=[1:[90]/1;2:[90]/1;3:[90]/1]{[1:[90]/1;2:[90]/1;3:[90]/1]};2:[50]/1;3:[50]/1]".to_owned()
        ))
    );
}

#[test]
fn sequential_storms_rebind_from_the_previous_bounded_branch_cascade() {
    let parsed = orna_syntax_v1::parse_module(SEQUENTIAL_BRANCH_CAP_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let first_rebind = storm(
        "expr:first-stage-rebind",
        (0..3).map(|_| branch(&[100], 1, vec![])).collect(),
    );
    let second_rebind = storm(
        "expr:second-stage-rebind",
        (0..3).map(|_| branch(&[90], 1, vec![])).collect(),
    );
    let stages = [
        storm(
            "expr:first-stage",
            vec![
                branch(&[10, 80], 1, vec![rebind(1, vec![first_rebind])]),
                branch(&[40], 1, vec![]),
            ],
        ),
        storm(
            "expr:second-stage",
            vec![
                branch(&[4, 100], 1, vec![rebind(1, vec![second_rebind])]),
                branch(&[20], 1, vec![]),
            ],
        ),
    ];
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(100), Some(1_000)),
        &stages,
        &[],
    )
    .expect("later storm rebinds consume the preceding stage's bounded branches");

    let filters = explained
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Filter)
        .collect::<Vec<_>>();
    assert_eq!(filters.len(), 2);
    let stage = |index| {
        *filters
            .iter()
            .find(|filter| {
                filter.details().get("disjunct_storm") == Some(&PlanDetail::Integer(index))
            })
            .expect("storm stage is identified by its one-based index")
    };
    let first_stage = stage(1);
    let second_stage = stage(2);
    assert_eq!(first_stage.estimated_rows(), Some(25));
    assert_eq!(first_stage.estimated_bytes(), Some(250));
    assert_eq!(first_stage.estimated_work(), Some(320));
    assert_eq!(second_stage.estimated_rows(), Some(12));
    assert_eq!(second_stage.estimated_bytes(), Some(120));
    assert_eq!(second_stage.estimated_work(), Some(102));
    for filter in [&first_stage, &second_stage] {
        assert_eq!(
            filter.details().get("storm_stage_input_scope"),
            Some(&PlanDetail::Text(
                "query_input_then_previous_stage_bounded_rows_and_bytes".to_owned()
            ))
        );
        assert_eq!(
            filter.details().get("branch_local_storm_cap_scope"),
            Some(&PlanDetail::Text(
                "each_cascade_output_capped_to_immediate_input_rows_and_bytes_at_every_nesting_depth"
                    .to_owned()
            ))
        );
    }
    assert_eq!(
        first_stage.details().get("branch_limit_chains"),
        Some(&PlanDetail::Text("1:[10,80];2:[40]".to_owned()))
    );
    assert_eq!(
        second_stage.details().get("branch_limit_chains"),
        Some(&PlanDetail::Text("1:[4,100];2:[20]".to_owned()))
    );
    assert_eq!(
        second_stage.details().get("limit_chain_rebind_shapes"),
        Some(&PlanDetail::Text(
            "1@1:[1:[90]/1;2:[90]/1;3:[90]/1]".to_owned()
        ))
    );
}

#[test]
fn successive_rebind_storms_preserve_caps_through_nested_branch_limit_chains() {
    let parsed = orna_syntax_v1::parse_module(REBIND_STAGE_CAP_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let first_rebind_stage = storm(
        "expr:first-rebind-stage",
        (0..3).map(|_| branch(&[100], 1, vec![])).collect(),
    );
    let nested_limit_rebind = storm(
        "expr:nested-limit-rebind",
        (0..3).map(|_| branch(&[100], 1, vec![])).collect(),
    );
    let second_rebind_stage = storm(
        "expr:second-rebind-stage",
        vec![
            branch(&[8, 40], 1, vec![rebind(1, vec![nested_limit_rebind])]),
            branch(&[8], 1, vec![]),
        ],
    );
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(100), Some(1_000)),
        &[storm(
            "expr:outer-stage",
            vec![
                branch(
                    &[10, 100],
                    1,
                    vec![rebind(1, vec![first_rebind_stage, second_rebind_stage])],
                ),
                branch(&[20], 1, vec![]),
            ],
        )],
        &[],
    )
    .expect("each rebind storm consumes the preceding bounded branch result");

    let filter = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("outer storm is represented by an aggregate filter");
    assert_eq!(filter.estimated_rows(), Some(14));
    assert_eq!(filter.estimated_bytes(), Some(140));
    assert_eq!(filter.estimated_work(), Some(388));
    assert_eq!(
        filter.details().get("branch_limit_chains"),
        Some(&PlanDetail::Text("1:[10,100];2:[20]".to_owned()))
    );
    assert_eq!(
        filter.details().get("limit_chain_rebind_stage_input_scope"),
        Some(&PlanDetail::Text(
            "post_limit_branch_input_then_previous_rebind_stage_bounded_rows_and_bytes".to_owned()
        ))
    );
    assert_eq!(
        filter.details().get("limit_chain_rebind_shapes"),
        Some(&PlanDetail::Text(
            "1@1:[1:[100]/1;2:[100]/1;3:[100]/1]>[1:[8,40]/1@1=[1:[100]/1;2:[100]/1;3:[100]/1];2:[8]/1]"
                .to_owned()
        ))
    );
}

#[test]
fn later_rebind_positions_consume_only_prior_bounded_branch_outputs() {
    let parsed = orna_syntax_v1::parse_module(SEQUENTIAL_REBIND_POSITIONS_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let first_position_storm = storm(
        "expr:first-position-rebind",
        (0..3).map(|_| branch(&[100], 1, vec![])).collect(),
    );
    let nested_position_storm = storm(
        "expr:nested-position-rebind",
        (0..3).map(|_| branch(&[100], 1, vec![])).collect(),
    );
    let second_position_storm = storm(
        "expr:second-position-rebind",
        vec![
            branch(&[5, 80], 1, vec![rebind(1, vec![nested_position_storm])]),
            branch(&[4], 1, vec![]),
        ],
    );
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(100), Some(1_000)),
        &[storm(
            "expr:positioned-rebind-outer",
            vec![
                branch(
                    &[12, 40, 100],
                    1,
                    vec![
                        rebind(1, vec![first_position_storm]),
                        rebind(2, vec![second_position_storm]),
                    ],
                ),
                branch(&[20], 1, vec![]),
            ],
        )],
        &[],
    )
    .expect("later rebind positions receive the preceding branch-local result");

    let filter = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("storm is represented by an aggregate filter");
    assert_eq!(filter.estimated_rows(), Some(13));
    assert_eq!(filter.estimated_bytes(), Some(123));
    assert_eq!(filter.estimated_work(), Some(382));
    assert_eq!(
        filter.details().get("branch_limit_chains"),
        Some(&PlanDetail::Text("1:[12,40,100];2:[20]".to_owned()))
    );
    assert_eq!(
        filter
            .details()
            .get("limit_chain_rebind_position_input_scope"),
        Some(&PlanDetail::Text(
            "current_branch_rows_and_bytes_after_prior_limits_and_rebind_cascades".to_owned()
        ))
    );
    assert_eq!(
        filter.details().get("limit_chain_rebind_stage_input_scope"),
        Some(&PlanDetail::Text(
            "post_limit_branch_input_then_previous_rebind_stage_bounded_rows_and_bytes".to_owned()
        ))
    );
    assert_eq!(
        filter.details().get("limit_chain_rebind_shapes"),
        Some(&PlanDetail::Text(
            "1@1:[1:[100]/1;2:[100]/1;3:[100]/1];1@2:[1:[5,80]/1@1=[1:[100]/1;2:[100]/1;3:[100]/1];2:[4]/1]".to_owned()
        ))
    );
}

#[test]
fn sequential_rebind_caps_stay_stable_across_limit_positions_and_storm_stages() {
    let parsed = orna_syntax_v1::parse_module(REBIND_CAP_STABILITY_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let deep_storm = storm(
        "expr:deep-cap-storm",
        (0..3).map(|_| branch(&[40], 2, vec![])).collect(),
    );
    let middle_storm = storm(
        "expr:middle-cap-storm",
        (0..2).map(|_| branch(&[30], 2, vec![])).collect(),
    );
    let first_stage = storm(
        "expr:first-cap-stage",
        vec![
            branch(
                &[19, 11],
                1,
                vec![
                    rebind(1, vec![deep_storm, middle_storm]),
                    rebind(
                        2,
                        vec![storm(
                            "expr:post-limit-cap-storm",
                            (0..2).map(|_| branch(&[20], 2, vec![])).collect(),
                        )],
                    ),
                ],
            ),
            branch(&[7], 2, vec![]),
        ],
    );
    let second_stage = storm(
        "expr:second-cap-stage",
        vec![
            branch(
                &[8, 5],
                1,
                vec![rebind(
                    1,
                    vec![
                        storm(
                            "expr:second-stage-rebind-one",
                            (0..2).map(|_| branch(&[24], 2, vec![])).collect(),
                        ),
                        storm(
                            "expr:second-stage-rebind-two",
                            (0..3).map(|_| branch(&[16], 2, vec![])).collect(),
                        ),
                    ],
                )],
            ),
            branch(&[4], 2, vec![]),
        ],
    );
    let third_stage = storm(
        "expr:third-cap-stage",
        vec![branch(&[3], 1, vec![]), branch(&[2], 1, vec![])],
    );
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(61), Some(257)),
        &[first_stage, second_stage, third_stage],
        &[],
    )
    .expect("each limit and rebind stage consumes its bounded predecessor");

    let mut stages = explained
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Filter && node.details().contains_key("disjunct_storm")
        })
        .collect::<Vec<_>>();
    assert_eq!(stages.len(), 3);
    stages.sort_by_key(|stage| match stage.details().get("disjunct_storm") {
        Some(PlanDetail::Integer(index)) => *index,
        _ => unreachable!("top-level storm stages carry their one-based index"),
    });
    let cardinalities = stages
        .iter()
        .map(|stage| (stage.estimated_rows(), stage.estimated_bytes()))
        .collect::<Vec<_>>();
    assert_eq!(
        cardinalities,
        vec![(Some(4), Some(16)), (Some(2), Some(7)), (Some(2), Some(7))]
    );
    for pair in cardinalities.windows(2) {
        assert!(pair[1].0.unwrap() <= pair[0].0.unwrap());
        assert!(pair[1].1.unwrap() <= pair[0].1.unwrap());
    }

    let first_stage = stages
        .iter()
        .find(|stage| stage.details().get("disjunct_storm") == Some(&PlanDetail::Integer(1)))
        .expect("first storm stage is labeled");
    assert_eq!(
        first_stage.details().get("limit_chain_rebind_stage_order"),
        Some(&PlanDetail::Text(
            "declaration_order_each_rebind_output_capped_before_next_rebind".to_owned()
        ))
    );
    assert_eq!(
        first_stage.details().get("limit_chain_rebind_shapes"),
        Some(&PlanDetail::Text(
            "1@1:[1:[40]/2;2:[40]/2;3:[40]/2]>[1:[30]/2;2:[30]/2];1@2:[1:[20]/2;2:[20]/2]"
                .to_owned()
        ))
    );
}

#[test]
fn overflowing_rebind_work_keeps_the_parent_branch_cap() {
    let source_rows = u64::MAX / 4;
    let overflowing_rebind = storm(
        "expr:wide-rebind-storm",
        (0..8).map(|_| branch(&[u64::MAX], 1, vec![])).collect(),
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

    let rows = explained
        .root()
        .estimated_rows()
        .expect("known source rows");
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
