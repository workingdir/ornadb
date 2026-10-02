use orna_sys_v1::{
    DisjunctStormBranchDescription, DisjunctStormCascadeDescription,
    DisjunctStormLimitRebindDescription, ExplainError, ExpressionRef, ObjectRef,
    PlanByteCapHandoffRoute, PlanByteCapScopeSegment, PlanDetail, PlanNodeKind, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef,
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
const PARTIAL_ESTIMATE_REBIND_STAGES_FIXTURE: &str =
    include_str!("fixtures/planner_storm_partial_estimate_rebind_stages.orna");
const UNKNOWN_REBIND_NESTED_CAPS_FIXTURE: &str =
    include_str!("fixtures/planner_storm_unknown_rebind_nested_caps.orna");
const DEEP_UNKNOWN_REBIND_CAPS_FIXTURE: &str =
    include_str!("fixtures/planner_storm_deep_unknown_rebind_caps.orna");
const REBIND_CAP_SCOPE_DEPTH_FIXTURE: &str =
    include_str!("fixtures/planner_storm_rebind_cap_scope_depth.orna");
const REBIND_CAP_DEPTH_GAPS_FIXTURE: &str =
    include_str!("fixtures/planner_storm_rebind_cap_depth_gaps.orna");
const REBIND_CAP_HANDOFFS_FIXTURE: &str =
    include_str!("fixtures/planner_storm_rebind_cap_handoffs.orna");
const REBIND_CAP_HANDOFF_ROUTES_FIXTURE: &str =
    include_str!("fixtures/planner_storm_scoped_handoff_routes.orna");

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
fn known_byte_caps_stay_bounded_across_stages_when_row_estimates_are_unknown() {
    let parsed = orna_syntax_v1::parse_module(PARTIAL_ESTIMATE_REBIND_STAGES_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let first_rebind = storm(
        "expr:partial-first-rebind",
        (0..3).map(|_| branch(&[40], 2, vec![])).collect(),
    );
    let second_rebind = storm(
        "expr:partial-second-rebind",
        (0..2).map(|_| branch(&[30], 3, vec![])).collect(),
    );
    let first_stage = storm(
        "expr:partial-first-stage",
        vec![
            branch(
                &[20, 10],
                1,
                vec![
                    rebind(1, vec![first_rebind, second_rebind]),
                    rebind(
                        2,
                        vec![storm(
                            "expr:partial-after-second-limit",
                            (0..2).map(|_| branch(&[20], 2, vec![])).collect(),
                        )],
                    ),
                ],
            ),
            branch(&[8], 2, vec![]),
        ],
    );
    let second_stage = storm(
        "expr:partial-second-stage",
        vec![
            branch(
                &[6, 3],
                1,
                vec![rebind(
                    1,
                    vec![
                        storm(
                            "expr:partial-second-stage-rebind-one",
                            (0..2).map(|_| branch(&[24], 2, vec![])).collect(),
                        ),
                        storm(
                            "expr:partial-second-stage-rebind-two",
                            (0..3).map(|_| branch(&[16], 3, vec![])).collect(),
                        ),
                    ],
                )],
            ),
            branch(&[4], 2, vec![]),
        ],
    );
    let third_stage = storm(
        "expr:partial-third-stage",
        vec![branch(&[2], 1, vec![]), branch(&[1], 1, vec![])],
    );
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(None, Some(1_000)),
        &[first_stage, second_stage, third_stage],
        &[],
    )
    .expect("known bytes remain capped even when row counts are unavailable");

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
        vec![(None, Some(297)), (None, Some(104)), (None, Some(104))]
    );
    for pair in cardinalities.windows(2) {
        assert_eq!(pair[1].0, None);
        assert!(pair[1].1.unwrap() <= pair[0].1.unwrap());
    }

    let first_stage = &stages[0];
    assert_eq!(
        first_stage.details().get("limit_chain_rebind_dimension_scope"),
        Some(&PlanDetail::Text(
            "rows_and_bytes_capped_independently_unknown_dimensions_remain_unknown".to_owned()
        ))
    );
    assert_eq!(
        first_stage.details().get("limit_chain_rebind_stage_input_scope"),
        Some(&PlanDetail::Text(
            "post_limit_branch_input_then_previous_rebind_stage_bounded_rows_and_bytes"
                .to_owned()
        ))
    );
}

#[test]
fn known_byte_caps_survive_unknown_work_in_nested_rebind_storm_stages() {
    let parsed = orna_syntax_v1::parse_module(UNKNOWN_REBIND_NESTED_CAPS_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let first_rebind = storm(
        "expr:unknown-work-first-rebind",
        (0..3).map(|_| branch(&[40], 2, vec![])).collect(),
    );
    let second_rebind = storm(
        "expr:unknown-work-second-rebind",
        (0..2).map(|_| branch(&[30], 3, vec![])).collect(),
    );
    let mut first_branch = branch(
        &[20, 10],
        1,
        vec![
            rebind(1, vec![first_rebind, second_rebind]),
            rebind(
                2,
                vec![storm(
                    "expr:unknown-work-after-limit-two",
                    (0..2).map(|_| branch(&[20], 2, vec![])).collect(),
                )],
            ),
        ],
    );
    first_branch.nested_storms = vec![
        storm(
            "expr:unknown-work-nested-after-rebind",
            (0..2).map(|_| branch(&[18], 2, vec![])).collect(),
        ),
        storm(
            "expr:unknown-work-second-nested-stage",
            vec![branch(&[12], 3, vec![])],
        ),
    ];
    let first_stage = storm(
        "expr:unknown-work-first-stage",
        vec![first_branch, branch(&[8], 2, vec![])],
    );

    let mut second_branch = branch(
        &[6, 3],
        1,
        vec![rebind(
            1,
            vec![
                storm(
                    "expr:unknown-work-second-stage-rebind-one",
                    (0..2).map(|_| branch(&[24], 2, vec![])).collect(),
                ),
                storm(
                    "expr:unknown-work-second-stage-rebind-two",
                    (0..3).map(|_| branch(&[16], 3, vec![])).collect(),
                ),
            ],
        )],
    );
    second_branch.nested_storms.push(storm(
        "expr:unknown-work-second-stage-nested",
        (0..2).map(|_| branch(&[10], 2, vec![])).collect(),
    ));
    let second_stage = storm(
        "expr:unknown-work-second-stage",
        vec![second_branch, branch(&[4], 2, vec![])],
    );
    let third_stage = storm(
        "expr:unknown-work-third-stage",
        vec![branch(&[2], 1, vec![]), branch(&[1], 1, vec![])],
    );

    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(None, Some(1_000)),
        &[first_stage, second_stage, third_stage],
        &[],
    )
    .expect("unknown row work does not erase nested known byte caps");
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
    assert!(cardinalities.iter().all(|(rows, bytes)| rows.is_none() && bytes.is_some()));
    for pair in cardinalities.windows(2) {
        assert_eq!(pair[1].0, None);
        assert!(pair[1].1.unwrap() <= pair[0].1.unwrap());
    }
    assert!(cardinalities[0].1.unwrap() <= 1_000);
    assert!(stages.iter().all(|stage| stage.estimated_work().is_none()));
    assert_eq!(
        stages[0].details().get("limit_chain_rebind_work_scope"),
        Some(&PlanDetail::Text(
            "unknown_row_work_does_not_erase_known_byte_estimates".to_owned()
        ))
    );
    assert!(matches!(
        stages[0].details().get("nested_cascade_shapes"),
        Some(PlanDetail::Text(shapes)) if !shapes.is_empty()
    ));
}

#[test]
fn byte_caps_stay_local_through_deep_unknown_rebind_chains() {
    let parsed = orna_syntax_v1::parse_module(DEEP_UNKNOWN_REBIND_CAPS_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let leaf_first = storm(
        "expr:deep-leaf-first",
        (0..2).map(|_| branch(&[80], 2, vec![])).collect(),
    );
    let leaf_second = storm(
        "expr:deep-leaf-second",
        (0..3).map(|_| branch(&[60], 3, vec![])).collect(),
    );
    let mut deep_branch = branch(
        &[20, 8],
        1,
        vec![
            rebind(1, vec![leaf_first]),
            rebind(2, vec![leaf_second]),
        ],
    );
    deep_branch.nested_storms.push(storm(
        "expr:deep-post-rebind-storm",
        (0..2).map(|_| branch(&[40], 2, vec![])).collect(),
    ));
    let nested_rebind = storm(
        "expr:nested-rebind-chain",
        vec![deep_branch, branch(&[6], 2, vec![])],
    );

    let middle_rebind = storm(
        "expr:middle-rebind-chain",
        vec![
            branch(
                &[24, 12],
                1,
                vec![
                    rebind(1, vec![nested_rebind.clone()]),
                    rebind(2, vec![storm(
                        "expr:middle-position-two",
                        (0..2).map(|_| branch(&[30], 2, vec![])).collect(),
                    )]),
                ],
            ),
            branch(&[8], 2, vec![]),
        ],
    );
    let mut outer_branch = branch(
        &[32, 18],
        1,
        vec![
            rebind(1, vec![middle_rebind.clone()]),
            rebind(2, vec![storm(
                "expr:outer-position-two",
                (0..2).map(|_| branch(&[22], 2, vec![])).collect(),
            )]),
        ],
    );
    outer_branch.nested_storms.push(storm(
        "expr:outer-post-chain-nested",
        vec![branch(&[14, 7], 1, vec![rebind(1, vec![nested_rebind.clone()])])],
    ));

    let stages = [
        storm(
            "expr:deep-unknown-stage-one",
            vec![outer_branch.clone(), branch(&[10], 2, vec![])],
        ),
        storm(
            "expr:deep-unknown-stage-two",
            vec![
                branch(&[20, 9], 1, vec![rebind(1, vec![middle_rebind.clone()])]),
                branch(&[7], 2, vec![]),
            ],
        ),
        storm(
            "expr:deep-unknown-stage-three",
            vec![
                branch(&[5], 1, vec![rebind(1, vec![nested_rebind])]),
                branch(&[3], 1, vec![]),
            ],
        ),
    ];
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(None, Some(2_048)),
        &stages,
        &[],
    )
    .expect("nested rebinds use only their immediate known-byte branch cap");
    let mut filters = explained
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Filter && node.details().contains_key("disjunct_storm")
        })
        .collect::<Vec<_>>();
    assert_eq!(filters.len(), 3);
    filters.sort_by_key(|filter| match filter.details().get("disjunct_storm") {
        Some(PlanDetail::Integer(index)) => *index,
        _ => unreachable!("top-level storm stages carry their one-based index"),
    });

    let estimates = filters
        .iter()
        .map(|filter| (filter.estimated_rows(), filter.estimated_bytes(), filter.estimated_work()))
        .collect::<Vec<_>>();
    assert!(estimates.iter().all(|(rows, bytes, work)| {
        rows.is_none() && bytes.is_some() && work.is_none()
    }));
    assert!(estimates[0].1.unwrap() <= 2_048);
    for pair in estimates.windows(2) {
        assert_eq!(pair[1].0, None);
        assert!(pair[1].1.unwrap() <= pair[0].1.unwrap());
        assert_eq!(pair[1].2, None);
    }
    assert!(matches!(
        filters[0].details().get("limit_chain_rebind_shapes"),
        Some(PlanDetail::Text(shapes)) if shapes.matches('@').count() >= 3
    ));
    assert_eq!(
        filters[0]
            .details()
            .get("nested_limit_chain_rebind_byte_cap_scope"),
        Some(&PlanDetail::Text(
            "immediate_post_limit_branch_bytes_at_every_rebind_nesting_depth".to_owned()
        ))
    );
}

#[test]
fn nested_rebind_byte_cap_scope_exposes_deepest_unknown_cascade_level() {
    let parsed = orna_syntax_v1::parse_module(REBIND_CAP_SCOPE_DEPTH_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let wrap_rebind = |predicate: &str, child| {
        storm(
            predicate,
            vec![branch(&[100, 50], 1, vec![rebind(1, vec![child])])],
        )
    };
    let leaf = storm(
        "expr:scope-leaf",
        vec![branch(&[80], 1, vec![])],
    );
    let level_four = wrap_rebind("expr:scope-level-four", leaf);
    let level_three = wrap_rebind("expr:scope-level-three", level_four);
    let level_two = wrap_rebind("expr:scope-level-two", level_three.clone());
    let deepest = wrap_rebind("expr:scope-deepest", level_two);
    let medium = wrap_rebind("expr:scope-medium", level_three);
    let stages = [
        deepest.clone(),
        medium,
        deepest,
    ];
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(None, Some(2_048)),
        &stages,
        &[],
    )
    .expect("nested cap scope remains visible with unknown row estimates");

    let mut filters = explained
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Filter && node.details().contains_key("disjunct_storm")
        })
        .collect::<Vec<_>>();
    assert_eq!(filters.len(), 3);
    filters.sort_by_key(|filter| match filter.details().get("disjunct_storm") {
        Some(PlanDetail::Integer(index)) => *index,
        _ => unreachable!("top-level storm stages carry their one-based index"),
    });
    assert_eq!(
        filters
            .iter()
            .map(|filter| filter.estimated_rows())
            .collect::<Vec<_>>(),
        vec![None, None, None]
    );
    let byte_estimates = filters
        .iter()
        .map(|filter| filter.estimated_bytes().expect("known byte estimate"))
        .collect::<Vec<_>>();
    assert!(byte_estimates[0] <= 2_048);
    assert!(byte_estimates.windows(2).all(|pair| pair[1] <= pair[0]));
    assert_eq!(
        filters
            .iter()
            .map(|filter| match filter
                .details()
                .get("limit_chain_rebind_max_nested_depth")
            {
                Some(PlanDetail::Integer(depth)) => Some(*depth),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![Some(4), Some(3), Some(4)]
    );
    assert!(filters.iter().all(|filter| matches!(
        filter
            .details()
            .get("nested_limit_chain_rebind_byte_cap_scope"),
        Some(PlanDetail::Text(scope))
            if scope == "immediate_post_limit_branch_bytes_at_every_rebind_nesting_depth"
    )));
}

#[test]
fn unknown_row_byte_caps_report_each_nested_rebind_scope() {
    let parsed = orna_syntax_v1::parse_module(REBIND_CAP_DEPTH_GAPS_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let leaf = storm("expr:scope-depth-leaf", vec![branch(&[80], 1, vec![])]);
    let depth_three = storm(
        "expr:scope-depth-three",
        vec![branch(&[100], 1, vec![rebind(1, vec![leaf.clone()])])],
    );
    let mut depth_two_branch = branch(&[100], 1, vec![]);
    depth_two_branch.nested_storms.push(depth_three);
    let depth_two = storm("expr:scope-depth-two", vec![depth_two_branch]);
    let first_stage = storm(
        "expr:scope-depth-first-stage",
        vec![branch(&[100], 1, vec![rebind(1, vec![depth_two])])],
    );

    let depth_two_rebind = storm(
        "expr:scope-depth-two-rebind",
        vec![branch(&[100], 1, vec![rebind(1, vec![leaf])])],
    );
    let second_stage = storm(
        "expr:scope-depth-second-stage",
        vec![branch(&[100], 1, vec![rebind(1, vec![depth_two_rebind])])],
    );

    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(None, Some(2_048)),
        &[first_stage, second_stage],
        &[],
    )
    .expect("known byte caps stay scoped while row estimates remain unknown");
    let mut filters = explained
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Filter && node.details().contains_key("disjunct_storm")
        })
        .collect::<Vec<_>>();
    assert_eq!(filters.len(), 2);
    filters.sort_by_key(|filter| match filter.details().get("disjunct_storm") {
        Some(PlanDetail::Integer(index)) => *index,
        _ => unreachable!("top-level storm stages carry their one-based index"),
    });

    assert_eq!(
        filters
            .iter()
            .map(|filter| (filter.estimated_rows(), filter.estimated_bytes()))
            .collect::<Vec<_>>(),
        vec![(None, Some(128)), (None, Some(16))]
    );
    assert_eq!(
        filters
            .iter()
            .map(|filter| filter.details().get("limit_chain_rebind_byte_cap_depths"))
            .collect::<Vec<_>>(),
        vec![
            Some(&PlanDetail::Text("1,3".to_owned())),
            Some(&PlanDetail::Text("1,2".to_owned())),
        ]
    );
    assert_eq!(
        filters
            .iter()
            .map(|filter| filter.details().get("limit_chain_rebind_max_nested_depth"))
            .collect::<Vec<_>>(),
        vec![Some(&PlanDetail::Integer(3)), Some(&PlanDetail::Integer(2))]
    );
}

#[test]
fn unknown_row_byte_caps_report_rebind_handoffs_by_nested_depth() {
    let parsed = orna_syntax_v1::parse_module(REBIND_CAP_HANDOFFS_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let leaf = storm("expr:handoff-leaf", vec![branch(&[40], 1, vec![])]);
    let nested = storm(
        "expr:handoff-depth-two",
        vec![branch(&[80, 40], 1, vec![rebind(1, vec![leaf.clone()])])],
    );
    let first_stage = storm(
        "expr:handoff-first-stage",
        vec![
            branch(
                &[100, 50],
                1,
                vec![
                    rebind(1, vec![nested.clone(), nested.clone()]),
                    rebind(2, vec![nested.clone()]),
                ],
            ),
            branch(&[20], 1, vec![]),
        ],
    );
    let second_stage = storm(
        "expr:handoff-second-stage",
        vec![
            branch(&[30], 1, vec![rebind(1, vec![nested])]),
            branch(&[10], 1, vec![]),
        ],
    );

    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(None, Some(2_048)),
        &[first_stage, second_stage],
        &[],
    )
    .expect("unknown row counts retain byte-cap handoffs through limit rebinds");
    let mut filters = explained
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Filter && node.details().contains_key("disjunct_storm")
        })
        .collect::<Vec<_>>();
    assert_eq!(filters.len(), 2);
    filters.sort_by_key(|filter| match filter.details().get("disjunct_storm") {
        Some(PlanDetail::Integer(index)) => *index,
        _ => unreachable!("top-level storm stages carry their one-based index"),
    });

    let byte_estimates = filters
        .iter()
        .map(|filter| (filter.estimated_rows(), filter.estimated_bytes()))
        .collect::<Vec<_>>();
    assert!(byte_estimates
        .iter()
        .all(|(rows, bytes)| rows.is_none() && bytes.is_some()));
    assert!(byte_estimates[0].1.unwrap() <= 2_048);
    assert!(byte_estimates[1].1.unwrap() <= byte_estimates[0].1.unwrap());
    assert_eq!(
        filters
            .iter()
            .map(|filter| filter.details().get("limit_chain_rebind_byte_cap_depths"))
            .collect::<Vec<_>>(),
        vec![
            Some(&PlanDetail::Text("1,2".to_owned())),
            Some(&PlanDetail::Text("1,2".to_owned())),
        ]
    );
    assert_eq!(
        filters
            .iter()
            .map(|filter| filter
                .details()
                .get("limit_chain_rebind_byte_cap_handoffs_by_depth"))
            .collect::<Vec<_>>(),
        vec![
            Some(&PlanDetail::Text("1:3,2:3".to_owned())),
            Some(&PlanDetail::Text("1:1,2:1".to_owned())),
        ]
    );
    assert!(matches!(
        filters[0].details().get("limit_chain_rebind_shapes"),
        Some(PlanDetail::Text(shapes)) if shapes.contains("1@1:") && shapes.contains("1@2:")
    ));
}

#[test]
fn unknown_row_byte_caps_report_scoped_handoff_routes_by_nested_depth() {
    let parsed = orna_syntax_v1::parse_module(REBIND_CAP_HANDOFF_ROUTES_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let leaf = storm("expr:handoff-estimate-leaf", vec![branch(&[40], 1, vec![])]);
    let nested = storm(
        "expr:handoff-estimate-depth-two",
        vec![branch(&[80, 40], 1, vec![rebind(1, vec![leaf.clone()])])],
    );
    let first_stage = storm(
        "expr:handoff-estimate-first-stage",
        vec![
            branch(
                &[100, 50],
                1,
                vec![
                    rebind(1, vec![nested.clone(), nested.clone()]),
                    rebind(2, vec![nested.clone()]),
                ],
            ),
            branch(&[20], 1, vec![]),
        ],
    );
    let second_stage = storm(
        "expr:handoff-estimate-second-stage",
        vec![
            branch(&[30], 1, vec![rebind(1, vec![nested])]),
            branch(&[10], 1, vec![]),
        ],
    );

    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(None, Some(2_048)),
        &[first_stage, second_stage],
        &[],
    )
    .expect("byte estimates remain attached to each unknown-row rebind handoff");
    let mut filters = explained
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Filter && node.details().contains_key("disjunct_storm")
        })
        .collect::<Vec<_>>();
    assert_eq!(filters.len(), 2);
    filters.sort_by_key(|filter| match filter.details().get("disjunct_storm") {
        Some(PlanDetail::Integer(index)) => *index,
        _ => unreachable!("top-level storm stages carry their one-based index"),
    });

    assert!(filters.iter().all(|filter| filter.estimated_rows().is_none()));
    let byte_estimates = filters
        .iter()
        .map(|filter| filter.estimated_bytes().expect("known byte estimate"))
        .collect::<Vec<_>>();
    assert!(byte_estimates[0] <= 2_048);
    assert!(byte_estimates[1] <= byte_estimates[0]);
    assert_eq!(
        filters
            .iter()
            .map(|filter| filter
                .details()
                .get("limit_chain_rebind_byte_cap_handoff_estimates_by_depth"))
            .collect::<Vec<_>>(),
        vec![
            Some(&PlanDetail::Text(
                "1:2048>512,512>128,128>32;2:2048>1024,512>256,128>64".to_owned()
            )),
            Some(&PlanDetail::Text("1:1040>260;2:1040>520".to_owned())),
        ]
    );
    assert_eq!(
        filters
            .iter()
            .map(|filter| filter
                .details()
                .get("limit_chain_rebind_byte_cap_handoff_scopes_by_depth"))
            .collect::<Vec<_>>(),
        vec![
            Some(&PlanDetail::Text(
                "1:root/branch1/limit1/rebind1/cascade1=2048>512,root/branch1/limit1/rebind1/cascade2=512>128,root/branch1/limit2/rebind2/cascade1=128>32;2:root/branch1/limit1/rebind1/cascade1/branch1/limit1/rebind1/cascade1=2048>1024,root/branch1/limit1/rebind1/cascade2/branch1/limit1/rebind1/cascade1=512>256,root/branch1/limit2/rebind2/cascade1/branch1/limit1/rebind1/cascade1=128>64".to_owned()
            )),
            Some(&PlanDetail::Text(
                "1:root/branch1/limit1/rebind1/cascade1=1040>260;2:root/branch1/limit1/rebind1/cascade1/branch1/limit1/rebind1/cascade1=1040>520".to_owned()
            )),
        ]
    );
    assert_eq!(
        filters
            .iter()
            .map(|filter| filter
                .details()
                .get("limit_chain_rebind_byte_cap_handoff_routes_by_depth"))
            .collect::<Vec<_>>(),
        vec![
            Some(&PlanDetail::Text(
                "1:root/branch1/limit1=>root/branch1/limit1/rebind1/cascade1=2048>512,root/branch1/limit1/rebind1/cascade1=>root/branch1/limit1/rebind1/cascade2=512>128,root/branch1/limit1/rebind1/cascade2/limit2=>root/branch1/limit2/rebind2/cascade1=128>32;2:root/branch1/limit1/rebind1/cascade1/branch1/limit1=>root/branch1/limit1/rebind1/cascade1/branch1/limit1/rebind1/cascade1=2048>1024,root/branch1/limit1/rebind1/cascade2/branch1/limit1=>root/branch1/limit1/rebind1/cascade2/branch1/limit1/rebind1/cascade1=512>256,root/branch1/limit2/rebind2/cascade1/branch1/limit1=>root/branch1/limit2/rebind2/cascade1/branch1/limit1/rebind1/cascade1=128>64".to_owned()
            )),
            Some(&PlanDetail::Text(
                "1:root/branch1/limit1=>root/branch1/limit1/rebind1/cascade1=1040>260;2:root/branch1/limit1/rebind1/cascade1/branch1/limit1=>root/branch1/limit1/rebind1/cascade1/branch1/limit1/rebind1/cascade1=1040>520".to_owned()
            )),
        ]
    );
    let first_routes = match filters[0]
        .details()
        .get("limit_chain_rebind_byte_cap_handoff_route_records")
    {
        Some(PlanDetail::ByteCapHandoffRoutes(routes)) => routes,
        other => panic!("expected typed byte-cap handoff routes, got {other:?}"),
    };
    assert_eq!(first_routes.len(), 6);
    for route in first_routes {
        assert_eq!(
            &route.output_path[..route.input_path.len()],
            route.input_path.as_slice(),
            "each typed destination descends from its actual bounded input"
        );
        assert_eq!(route.output_path.len(), route.input_path.len() + 2);
        assert!(matches!(
            route.output_path[route.input_path.len()],
            PlanByteCapScopeSegment::Rebind { .. }
        ));
        assert!(matches!(
            route.output_path[route.input_path.len() + 1],
            PlanByteCapScopeSegment::Cascade { .. }
        ));
    }
    assert_eq!(
        first_routes[0],
        PlanByteCapHandoffRoute {
            depth: 1,
            input_path: vec![
                PlanByteCapScopeSegment::StormStage { index: 1 },
                PlanByteCapScopeSegment::Branch { index: 1 },
                PlanByteCapScopeSegment::Limit { position: 1 },
            ],
            output_path: vec![
                PlanByteCapScopeSegment::StormStage { index: 1 },
                PlanByteCapScopeSegment::Branch { index: 1 },
                PlanByteCapScopeSegment::Limit { position: 1 },
                PlanByteCapScopeSegment::Rebind { position: 1 },
                PlanByteCapScopeSegment::Cascade { index: 1 },
            ],
            input_scope: "root/branch1/limit1".to_owned(),
            output_scope: "root/branch1/limit1/rebind1/cascade1".to_owned(),
            input_bytes: Some(2_048),
            output_bytes: Some(512),
        }
    );
    assert_eq!(
        first_routes[1].output_path,
        vec![
            PlanByteCapScopeSegment::StormStage { index: 1 },
            PlanByteCapScopeSegment::Branch { index: 1 },
            PlanByteCapScopeSegment::Limit { position: 1 },
            PlanByteCapScopeSegment::Rebind { position: 1 },
            PlanByteCapScopeSegment::Cascade { index: 1 },
            PlanByteCapScopeSegment::Rebind { position: 1 },
            PlanByteCapScopeSegment::Cascade { index: 2 },
        ]
    );
    assert_eq!(
        first_routes[2],
        PlanByteCapHandoffRoute {
            depth: 1,
            input_path: vec![
                PlanByteCapScopeSegment::StormStage { index: 1 },
                PlanByteCapScopeSegment::Branch { index: 1 },
                PlanByteCapScopeSegment::Limit { position: 1 },
                PlanByteCapScopeSegment::Rebind { position: 1 },
                PlanByteCapScopeSegment::Cascade { index: 1 },
                PlanByteCapScopeSegment::Rebind { position: 1 },
                PlanByteCapScopeSegment::Cascade { index: 2 },
                PlanByteCapScopeSegment::Limit { position: 2 },
            ],
            output_path: vec![
                PlanByteCapScopeSegment::StormStage { index: 1 },
                PlanByteCapScopeSegment::Branch { index: 1 },
                PlanByteCapScopeSegment::Limit { position: 1 },
                PlanByteCapScopeSegment::Rebind { position: 1 },
                PlanByteCapScopeSegment::Cascade { index: 1 },
                PlanByteCapScopeSegment::Rebind { position: 1 },
                PlanByteCapScopeSegment::Cascade { index: 2 },
                PlanByteCapScopeSegment::Limit { position: 2 },
                PlanByteCapScopeSegment::Rebind { position: 2 },
                PlanByteCapScopeSegment::Cascade { index: 1 },
            ],
            input_scope: "root/branch1/limit1/rebind1/cascade2/limit2".to_owned(),
            output_scope: "root/branch1/limit2/rebind2/cascade1".to_owned(),
            input_bytes: Some(128),
            output_bytes: Some(32),
        }
    );
    assert_eq!(first_routes[3].depth, 2);
    assert_eq!(
        first_routes[3].input_path,
        vec![
            PlanByteCapScopeSegment::StormStage { index: 1 },
            PlanByteCapScopeSegment::Branch { index: 1 },
            PlanByteCapScopeSegment::Limit { position: 1 },
            PlanByteCapScopeSegment::Rebind { position: 1 },
            PlanByteCapScopeSegment::Cascade { index: 1 },
            PlanByteCapScopeSegment::Branch { index: 1 },
            PlanByteCapScopeSegment::Limit { position: 1 },
        ]
    );

    let second_routes = match filters[1]
        .details()
        .get("limit_chain_rebind_byte_cap_handoff_route_records")
    {
        Some(PlanDetail::ByteCapHandoffRoutes(routes)) => routes,
        other => panic!("expected typed byte-cap handoff routes, got {other:?}"),
    };
    assert_eq!(second_routes.len(), 2);
    let second_stage_ancestry = [
        PlanByteCapScopeSegment::StormStage { index: 1 },
        PlanByteCapScopeSegment::StormStageOutput { index: 1 },
        PlanByteCapScopeSegment::StormStage { index: 2 },
    ];
    assert_eq!(
        &second_routes[0].input_path[..second_stage_ancestry.len()],
        second_stage_ancestry
    );
    assert_eq!(
        &second_routes[1].output_path[..second_stage_ancestry.len()],
        second_stage_ancestry
    );

    let serialized = serde_json::to_value(&explained).expect("explained plans serialize");
    let serialized_stage = serialized["nodes"]
        .as_array()
        .expect("plan nodes serialize as an array")
        .iter()
        .find(|node| node["details"]["disjunct_storm"] == 1)
        .expect("first storm node is serialized");
    assert_eq!(
        serialized_stage["details"]["limit_chain_rebind_byte_cap_handoff_route_records"][0],
        serde_json::json!({
            "depth": 1,
            "input_path": [
                { "kind": "storm_stage", "index": 1 },
                { "kind": "branch", "index": 1 },
                { "kind": "limit", "position": 1 },
            ],
            "output_path": [
                { "kind": "storm_stage", "index": 1 },
                { "kind": "branch", "index": 1 },
                { "kind": "limit", "position": 1 },
                { "kind": "rebind", "position": 1 },
                { "kind": "cascade", "index": 1 },
            ],
            "input_scope": "root/branch1/limit1",
            "output_scope": "root/branch1/limit1/rebind1/cascade1",
            "input_bytes": 2048,
            "output_bytes": 512,
        })
    );
}

#[test]
fn typed_handoff_routes_preserve_nested_storm_steps_and_unknown_bytes() {
    let parsed = orna_syntax_v1::parse_module(REBIND_CAP_HANDOFF_ROUTES_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let leaf = storm("expr:typed-route-unknown-leaf", vec![branch(&[10], 1, vec![])]);
    let nested = storm(
        "expr:typed-route-unknown-nested",
        vec![branch(&[20], 1, vec![rebind(1, vec![leaf])])],
    );
    let mut outer_branch = branch(&[30], 1, vec![]);
    outer_branch.nested_storms.push(nested);
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(None, None),
        &[storm("expr:typed-route-unknown-outer", vec![outer_branch])],
        &[],
    )
    .expect("unknown byte estimates remain explicit in typed routes");

    let filter = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter && node.details().contains_key("disjunct_storm"))
        .expect("outer storm is visible");
    let routes = match filter
        .details()
        .get("limit_chain_rebind_byte_cap_handoff_route_records")
    {
        Some(PlanDetail::ByteCapHandoffRoutes(routes)) => routes,
        other => panic!("expected typed byte-cap handoff routes, got {other:?}"),
    };
    assert_eq!(routes.len(), 1);
    assert_eq!(
        routes[0],
        PlanByteCapHandoffRoute {
            depth: 2,
            input_path: vec![
                PlanByteCapScopeSegment::StormStage { index: 1 },
                PlanByteCapScopeSegment::Branch { index: 1 },
                PlanByteCapScopeSegment::Limit { position: 1 },
                PlanByteCapScopeSegment::NestedStorm { index: 1 },
                PlanByteCapScopeSegment::Branch { index: 1 },
                PlanByteCapScopeSegment::Limit { position: 1 },
            ],
            output_path: vec![
                PlanByteCapScopeSegment::StormStage { index: 1 },
                PlanByteCapScopeSegment::Branch { index: 1 },
                PlanByteCapScopeSegment::Limit { position: 1 },
                PlanByteCapScopeSegment::NestedStorm { index: 1 },
                PlanByteCapScopeSegment::Branch { index: 1 },
                PlanByteCapScopeSegment::Limit { position: 1 },
                PlanByteCapScopeSegment::Rebind { position: 1 },
                PlanByteCapScopeSegment::Cascade { index: 1 },
            ],
            input_scope: "root/branch1/nested1/branch1/limit1".to_owned(),
            output_scope:
                "root/branch1/nested1/branch1/limit1/rebind1/cascade1".to_owned(),
            input_bytes: None,
            output_bytes: None,
        }
    );
    let serialized = serde_json::to_value(&explained).expect("explained plans serialize");
    assert_eq!(
        serialized["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["details"]["disjunct_storm"] == 1)
            .unwrap()["details"]["limit_chain_rebind_byte_cap_handoff_route_records"][0]
            ["input_bytes"],
        serde_json::Value::Null
    );
    assert_eq!(
        serialized["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["details"]["disjunct_storm"] == 1)
            .unwrap()["details"]["limit_chain_rebind_byte_cap_handoff_route_records"][0]
            ["input_path"][3],
        serde_json::json!({ "kind": "nested_storm", "index": 1 })
    );
}

#[test]
fn typed_handoff_routes_preserve_prior_storm_stage_outputs() {
    let parsed = orna_syntax_v1::parse_module(REBIND_CAP_HANDOFF_ROUTES_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let stages = (1..=3)
        .map(|stage| {
            let cascade = storm(
                &format!("expr:stage-{stage}-bounded-cascade"),
                vec![branch(&[40], 1, vec![])],
            );
            storm(
                &format!("expr:stage-{stage}-handoff"),
                vec![branch(&[80], 1, vec![rebind(1, vec![cascade])])],
            )
        })
        .collect::<Vec<_>>();
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(256), Some(8_192)),
        &stages,
        &[],
    )
    .expect("three chained storm stages retain typed handoff ancestry");
    let mut filters = explained
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Filter && node.details().contains_key("disjunct_storm")
        })
        .collect::<Vec<_>>();
    filters.sort_by_key(|filter| match filter.details().get("disjunct_storm") {
        Some(PlanDetail::Integer(stage)) => *stage,
        _ => unreachable!("top-level storm filters carry their stage number"),
    });
    assert_eq!(filters.len(), 3);

    for (stage_index, filter) in filters.iter().enumerate() {
        let routes = match filter
            .details()
            .get("limit_chain_rebind_byte_cap_handoff_route_records")
        {
            Some(PlanDetail::ByteCapHandoffRoutes(routes)) => routes,
            other => panic!("expected typed byte-cap handoff routes, got {other:?}"),
        };
        assert_eq!(routes.len(), 1);

        let mut expected_input_path = Vec::new();
        for prior_stage in 1..=stage_index {
            expected_input_path.push(PlanByteCapScopeSegment::StormStage {
                index: prior_stage,
            });
            expected_input_path.push(PlanByteCapScopeSegment::StormStageOutput {
                index: prior_stage,
            });
        }
        expected_input_path.push(PlanByteCapScopeSegment::StormStage {
            index: stage_index + 1,
        });
        expected_input_path.extend([
            PlanByteCapScopeSegment::Branch { index: 1 },
            PlanByteCapScopeSegment::Limit { position: 1 },
        ]);
        assert_eq!(routes[0].input_path, expected_input_path);

        expected_input_path.extend([
            PlanByteCapScopeSegment::Rebind { position: 1 },
            PlanByteCapScopeSegment::Cascade { index: 1 },
        ]);
        assert_eq!(routes[0].output_path, expected_input_path);
        assert!(routes[0].input_bytes.is_some());
        assert!(routes[0].output_bytes.is_some());
    }

    let serialized = serde_json::to_value(&explained).expect("explained plans serialize");
    let third_stage = serialized["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["details"]["disjunct_storm"] == 3)
        .expect("third storm stage is serialized");
    assert_eq!(
        third_stage["details"]["limit_chain_rebind_byte_cap_handoff_route_records"][0]
            ["input_path"][3],
        serde_json::json!({ "kind": "storm_stage_output", "index": 2 })
    );
}

#[test]
fn typed_handoff_routes_preserve_nested_storm_outputs_across_rebinds() {
    let parsed = orna_syntax_v1::parse_module(REBIND_CAP_HANDOFF_ROUTES_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let nested_storms = (1..=3)
        .map(|nested_index| {
            let leaf = storm(
                &format!("expr:nested-{nested_index}-bounded-leaf"),
                vec![branch(&[40], 1, vec![])],
            );
            storm(
                &format!("expr:nested-{nested_index}-rebind"),
                vec![branch(&[80], 1, vec![rebind(1, vec![leaf])])],
            )
        })
        .collect::<Vec<_>>();
    let prior_rebind = storm(
        "expr:outer-rebind-before-nested-storms",
        vec![branch(&[60], 1, vec![])],
    );
    let mut outer_branch = branch(&[80], 1, vec![rebind(1, vec![prior_rebind])]);
    outer_branch.nested_storms = nested_storms;
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(Some(256), Some(4_096)),
        &[storm("expr:nested-output-ancestry", vec![outer_branch])],
        &[],
    )
    .expect("nested storm handoffs retain each preceding bounded output");
    let filter = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter && node.details().contains_key("disjunct_storm"))
        .expect("outer storm is visible");
    let routes = match filter
        .details()
        .get("limit_chain_rebind_byte_cap_handoff_route_records")
    {
        Some(PlanDetail::ByteCapHandoffRoutes(routes)) => routes,
        other => panic!("expected typed byte-cap handoff routes, got {other:?}"),
    };
    assert_eq!(routes.len(), 4);

    let prior_route = &routes[0];
    assert_eq!(prior_route.depth, 1);
    let mut prior_route_input_path = vec![
        PlanByteCapScopeSegment::StormStage { index: 1 },
        PlanByteCapScopeSegment::Branch { index: 1 },
        PlanByteCapScopeSegment::Limit { position: 1 },
    ];
    assert_eq!(prior_route.input_path, prior_route_input_path);
    prior_route_input_path.extend([
        PlanByteCapScopeSegment::Rebind { position: 1 },
        PlanByteCapScopeSegment::Cascade { index: 1 },
    ]);
    assert_eq!(prior_route.output_path, prior_route_input_path);
    assert!(prior_route.input_bytes.is_some());
    assert!(prior_route.output_bytes.is_some());

    for (nested_index, route) in routes.iter().skip(1).enumerate() {
        let mut expected_input_path = vec![
            PlanByteCapScopeSegment::StormStage { index: 1 },
            PlanByteCapScopeSegment::Branch { index: 1 },
            PlanByteCapScopeSegment::Limit { position: 1 },
            PlanByteCapScopeSegment::Rebind { position: 1 },
            PlanByteCapScopeSegment::Cascade { index: 1 },
        ];
        for prior_nested in 1..=nested_index {
            expected_input_path.push(PlanByteCapScopeSegment::NestedStorm {
                index: prior_nested,
            });
            expected_input_path.push(PlanByteCapScopeSegment::NestedStormOutput {
                index: prior_nested,
            });
        }
        expected_input_path.extend([
            PlanByteCapScopeSegment::NestedStorm {
                index: nested_index + 1,
            },
            PlanByteCapScopeSegment::Branch { index: 1 },
            PlanByteCapScopeSegment::Limit { position: 1 },
        ]);
        assert_eq!(route.depth, 2);
        assert_eq!(route.input_path, expected_input_path);
        expected_input_path.extend([
            PlanByteCapScopeSegment::Rebind { position: 1 },
            PlanByteCapScopeSegment::Cascade { index: 1 },
        ]);
        assert_eq!(route.output_path, expected_input_path);
        assert!(route.input_bytes.is_some());
        assert!(route.output_bytes.is_some());
    }
    let mut preceding_output_bytes = prior_route.output_bytes.unwrap();
    for route in routes.iter().skip(1) {
        let input_bytes = route.input_bytes.expect("nested input byte cap is known");
        let output_bytes = route.output_bytes.expect("nested output byte cap is known");
        assert!(input_bytes <= preceding_output_bytes);
        assert!(output_bytes <= input_bytes);
        preceding_output_bytes = output_bytes;
    }

    let serialized = serde_json::to_value(&explained).expect("explained plans serialize");
    assert_eq!(
        serialized["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["details"]["disjunct_storm"] == 1)
            .unwrap()["details"]["limit_chain_rebind_byte_cap_handoff_route_records"][3]
            ["input_path"][8],
        serde_json::json!({ "kind": "nested_storm_output", "index": 2 })
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
