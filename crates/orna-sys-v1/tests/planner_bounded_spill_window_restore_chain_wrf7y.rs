use orna_sys_v1::{
    ExpressionRef, MutableBranchSnapshot, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    PlanWindowFrameBound, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryPairedCheckpointSegmentCompactionChainDescription,
    QueryPairedCheckpointSegmentCompactionStepDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, QueryWindowSpillDescription,
    SnapshotRef, explain_query_with_paired_cost_restoration_and_window_spill_pushdowns,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_bounded_spill_window_restore_chain_wrf7y.orna");
const WINDOW_CHAIN_FIXTURE: &str =
    include_str!("fixtures/planner_bounded_spill_window_chain_v0x8t.orna");

fn object(value: &str) -> ObjectRef {
    ObjectRef::descriptive(value)
}

fn expression(value: &str) -> ExpressionRef {
    ExpressionRef::descriptive(value)
}

fn branch(name: &str, generation: u64) -> MutableBranchSnapshot {
    MutableBranchSnapshot {
        name: name.to_owned(),
        generation,
    }
}

fn statistics(
    rows: u64,
    bytes: u64,
    mutable_branch: Option<MutableBranchSnapshot>,
) -> QuerySourceStatistics {
    QuerySourceStatistics {
        estimated_rows: Some(rows),
        estimated_bytes: Some(bytes),
        mutable_branch,
    }
}

fn pair(source: &str) -> QueryJoinPairIdentityDescription {
    QueryJoinPairIdentityDescription {
        identity: object(&format!("pair:{source}")),
        left_source: object("table:Anchor"),
        right_source: object(&format!("table:{source}")),
        predicate: Some(expression(&format!("expr:join-{source}"))),
    }
}

fn aggregate(
    identity: &str,
    source: &str,
    frame_start: PlanWindowFrameBound,
    frame_end: PlanWindowFrameBound,
) -> QueryWindowAggregatePushdownDescription {
    QueryWindowAggregatePushdownDescription {
        identity: object(identity),
        source: object(source),
        aggregate: expression("expr:sum-value"),
        frame_identity: expression(&format!("frame:{identity}")),
        frame_start,
        frame_end,
    }
}

fn spill(
    identity: &str,
    pair_identity: &str,
    source: &str,
    aggregate_identity: &str,
    working_set: Option<u64>,
    memory_budget: u64,
) -> QueryWindowSpillDescription {
    QueryWindowSpillDescription {
        identity: object(identity),
        join_pair_identity: object(pair_identity),
        source: object(source),
        window_aggregate_identity: object(aggregate_identity),
        estimated_working_set_bytes: working_set,
        memory_budget_bytes: memory_budget,
    }
}

fn restore_chain(
    checkpoint_identity: &str,
) -> QueryPairedCheckpointSegmentCompactionChainDescription {
    QueryPairedCheckpointSegmentCompactionChainDescription {
        join_pair_identity: object("pair:Restore"),
        branch: branch("branch:restore", 12),
        steps: vec![QueryPairedCheckpointSegmentCompactionStepDescription {
            checkpoint_identity: object(checkpoint_identity),
            left_segment_identity: Some(object("segment:restore:left")),
            right_segment_identity: Some(object("segment:restore:right")),
            left_compaction_identity: Some(object("compaction:restore:left")),
            right_compaction_identity: Some(object("compaction:restore:right")),
        }],
    }
}

fn plan(
    first_memory_budget: u64,
    restore_checkpoint_identity: &str,
    reverse_descriptors: bool,
    unknown_restore_working_set: bool,
    unbound_all_frames: bool,
    include_restore: bool,
    include_sparse_tail: bool,
    middle_window_identity: &str,
) -> orna_sys_v1::ExplainedPlan {
    let mut joins = vec![
        QueryJoinDescription {
            source: object("table:First"),
            statistics: Some(statistics(2, 2_000, Some(branch("branch:first", 5)))),
            predicate: Some(expression("expr:join-First")),
        },
        QueryJoinDescription {
            source: object("table:Gap"),
            statistics: Some(statistics(3, 3_000, None)),
            predicate: Some(expression("expr:join-Gap")),
        },
        QueryJoinDescription {
            source: object("table:Middle"),
            statistics: Some(statistics(4, 4_000, Some(branch("branch:middle", 8)))),
            predicate: Some(expression("expr:join-Middle")),
        },
        QueryJoinDescription {
            source: object("table:Restore"),
            statistics: Some(statistics(30, 30_000, Some(branch("branch:restore", 12)))),
            predicate: Some(expression("expr:join-Restore")),
        },
    ];
    if include_sparse_tail {
        joins.push(QueryJoinDescription {
            source: object("table:Tail"),
            statistics: Some(statistics(200, 200_000, None)),
            predicate: Some(expression("expr:join-Tail")),
        });
    }
    let query = QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:bounded-spill-window-restore-chain-wrf7y"),
        source: object("table:Anchor"),
        source_statistics: Some(statistics(100, 40_000, Some(branch("branch:anchor", 3)))),
        joins,
        predicate: None,
        projections: vec![expression("expr:window-output")],
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    };
    let mut pair_names = vec!["First", "Gap", "Middle", "Restore"];
    if include_sparse_tail {
        pair_names.push("Tail");
    }
    let mut pairs = pair_names.into_iter().map(pair).collect::<Vec<_>>();
    let mut aggregates = vec![
        aggregate(
            "window:first-bounded",
            "table:First",
            PlanWindowFrameBound::Preceding(2),
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            middle_window_identity,
            "table:Middle",
            PlanWindowFrameBound::UnboundedPreceding,
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:restore-bounded",
            "table:Restore",
            PlanWindowFrameBound::Preceding(4),
            PlanWindowFrameBound::Following(1),
        ),
    ];
    if unbound_all_frames {
        for aggregate in &mut aggregates {
            aggregate.frame_start = PlanWindowFrameBound::UnboundedPreceding;
            aggregate.frame_end = PlanWindowFrameBound::CurrentRow;
        }
    }
    let mut spills = vec![
        spill(
            "spill:first",
            "pair:First",
            "table:First",
            "window:first-bounded",
            Some(9_000),
            first_memory_budget,
        ),
        spill(
            "spill:middle-unbounded",
            "pair:Middle",
            "table:Middle",
            middle_window_identity,
            Some(80_000),
            1_000,
        ),
        spill(
            "spill:restore",
            "pair:Restore",
            "table:Restore",
            "window:restore-bounded",
            (!unknown_restore_working_set).then_some(17_000),
            8_000,
        ),
    ];
    let mut checkpoint_chains = if include_restore {
        vec![restore_chain(restore_checkpoint_identity)]
    } else {
        Vec::new()
    };
    if reverse_descriptors {
        pairs.reverse();
        aggregates.reverse();
        spills.reverse();
        checkpoint_chains.reverse();
    }
    explain_query_with_paired_cost_restoration_and_window_spill_pushdowns(
        &query,
        &pairs,
        &aggregates,
        &spills,
        &checkpoint_chains,
        &[],
        &[],
        &[],
        &[],
        &[],
    )
    .expect("paired bounded spills and window restore chains explain together")
}

fn text<'a>(node: &'a PlanNode, key: &str) -> &'a str {
    match node.details().get(key) {
        Some(PlanDetail::Text(value)) => value,
        other => panic!("{key} should be text, got {other:?}"),
    }
}

fn integer(node: &PlanNode, key: &str) -> u64 {
    match node.details().get(key) {
        Some(PlanDetail::Integer(value)) => *value,
        other => panic!("{key} should be an integer, got {other:?}"),
    }
}

fn project(plan: &orna_sys_v1::ExplainedPlan) -> &PlanNode {
    plan.nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Project)
        .expect("the output expression creates a Project node")
}

fn join_by_pair<'a>(plan: &'a orna_sys_v1::ExplainedPlan, pair: &str) -> &'a PlanNode {
    plan.nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Join
                && node.details().get("join_pair_identity")
                    == Some(&PlanDetail::Text(pair.to_owned()))
        })
        .expect("the exact pair creates a Join node")
}

#[test]
fn bounded_spill_identity_binds_paired_window_restore_chains_and_carries_across_sparse_tail() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = plan(
        5_000,
        "checkpoint:restore-v1",
        false,
        false,
        false,
        true,
        true,
        "window:middle-unbounded",
    );
    let reordered = plan(
        5_000,
        "checkpoint:restore-v1",
        true,
        false,
        false,
        true,
        true,
        "window:middle-unbounded",
    );
    let changed_spill = plan(
        4_999,
        "checkpoint:restore-v1",
        false,
        false,
        false,
        true,
        true,
        "window:middle-unbounded",
    );
    let changed_restore = plan(
        5_000,
        "checkpoint:restore-v2",
        false,
        false,
        false,
        true,
        true,
        "window:middle-unbounded",
    );
    let project_node = project(&baseline);
    let restore = join_by_pair(&baseline, "pair:Restore");
    let tail = join_by_pair(&baseline, "pair:Tail");
    let spill_key = "paired_bounded_window_spill_restore_chain_fold_identity";
    let spill_component_key = "paired_bounded_window_spill_restore_chain_spill_fold_identity";
    let restore_component_key =
        "paired_bounded_window_spill_restore_chain_restore_chain_fold_identity";
    let window_restore_component_key =
        "paired_bounded_window_spill_restore_chain_window_restore_chain_fold_identity";

    assert!(
        text(project_node, spill_key).starts_with("paired-bounded-window-spill-restore-chain:")
    );
    assert!(
        text(project_node, spill_component_key).starts_with("paired-bounded-window-spill-fold:")
    );
    assert_eq!(
        text(project_node, restore_component_key),
        text(
            restore,
            "paired_window_cost_restoration_restore_chain_fold_identity"
        )
    );
    assert_eq!(
        text(project_node, window_restore_component_key),
        text(
            tail,
            "paired_window_cost_restoration_window_restore_chain_fold_identity"
        )
    );
    assert_eq!(
        text(project_node, restore_component_key),
        text(
            tail,
            "paired_window_cost_restoration_restore_chain_fold_identity"
        )
    );
    assert_eq!(
        text(
            tail,
            "paired_window_cost_restoration_window_restore_chain_fold_identity"
        ),
        text(
            restore,
            "paired_window_cost_restoration_window_restore_chain_fold_identity"
        ),
        "the sparse tail carries the exact window and restore-chain fold"
    );
    assert_eq!(
        text(
            tail,
            "paired_window_cost_restoration_window_restore_chain_transition"
        ),
        "carried_across_sparse_input"
    );
    assert_eq!(
        integer(
            project_node,
            "paired_bounded_window_spill_restore_chain_spill_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(
            project_node,
            "paired_bounded_window_spill_restore_chain_spill_stage_count"
        ),
        2,
        "the unbounded middle window does not enter the bounded spill fold"
    );
    assert_eq!(
        integer(
            project_node,
            "paired_bounded_window_spill_restore_chain_window_pair_count"
        ),
        3
    );
    assert_eq!(
        integer(
            project_node,
            "paired_bounded_window_spill_restore_chain_restore_fold_count"
        ),
        1
    );
    assert_eq!(
        integer(
            project_node,
            "paired_bounded_window_spill_restore_chain_window_identity_count"
        ),
        3
    );
    let reordered_project = project(&reordered);
    assert_eq!(
        text(project_node, spill_key),
        text(reordered_project, spill_key),
        "descriptor ordering does not change the composite identity"
    );
    let changed_spill_project = project(&changed_spill);
    assert_ne!(
        text(project_node, spill_key),
        text(changed_spill_project, spill_key),
        "a bounded spill budget change advances the composite identity"
    );
    assert_ne!(
        text(project_node, spill_component_key),
        text(changed_spill_project, spill_component_key)
    );
    let changed_restore_project = project(&changed_restore);
    assert_ne!(
        text(project_node, spill_key),
        text(changed_restore_project, spill_key),
        "a restore-chain change advances the composite identity"
    );
    assert_eq!(
        text(project_node, spill_component_key),
        text(changed_restore_project, spill_component_key),
        "a restore change leaves the bounded spill component stable"
    );
    assert_ne!(
        text(project_node, restore_component_key),
        text(changed_restore_project, restore_component_key)
    );
}

#[test]
fn bounded_spill_restore_chain_fold_preserves_unknown_estimates_and_requires_bounded_spills() {
    let unknown_plan = plan(
        5_000,
        "checkpoint:restore-v1",
        false,
        true,
        false,
        true,
        true,
        "window:middle-unbounded",
    );
    let unknown = project(&unknown_plan);
    assert_eq!(
        integer(
            unknown,
            "paired_bounded_window_spill_restore_chain_unknown_working_set_count"
        ),
        1
    );
    assert_eq!(
        text(
            unknown,
            "paired_bounded_window_spill_restore_chain_estimate_status"
        ),
        "unknown_working_set"
    );
    assert!(
        !unknown
            .details()
            .contains_key("paired_bounded_window_spill_restore_chain_estimated_bytes")
    );
    assert_eq!(
        integer(
            unknown,
            "paired_bounded_window_spill_window_chain_unknown_working_set_count"
        ),
        1
    );
    assert_eq!(
        text(
            unknown,
            "paired_bounded_window_spill_window_chain_estimate_status"
        ),
        "unknown_working_set"
    );
    assert!(
        !unknown
            .details()
            .contains_key("paired_bounded_window_spill_window_chain_estimated_bytes")
    );

    let unbounded_plan = plan(
        5_000,
        "checkpoint:restore-v1",
        false,
        false,
        true,
        true,
        true,
        "window:middle-unbounded",
    );
    let unbounded = project(&unbounded_plan);
    assert!(
        !unbounded
            .details()
            .contains_key("paired_bounded_window_spill_restore_chain_fold_identity")
    );
    assert!(
        !unbounded
            .details()
            .contains_key("paired_bounded_window_spill_window_chain_fold_identity")
    );
    let no_restore_plan = plan(
        5_000,
        "checkpoint:restore-v1",
        false,
        false,
        false,
        false,
        true,
        "window:middle-unbounded",
    );
    let no_restore = project(&no_restore_plan);
    assert!(
        !no_restore
            .details()
            .contains_key("paired_bounded_window_spill_restore_chain_fold_identity")
    );
    assert!(
        text(
            no_restore,
            "paired_bounded_window_spill_window_chain_fold_identity"
        )
        .starts_with("paired-bounded-window-spill-window-chain:")
    );
    assert_eq!(
        integer(
            no_restore,
            "paired_bounded_window_spill_window_chain_window_pair_count"
        ),
        3,
        "paired window chains produce this fold without a restore descriptor"
    );
}

#[test]
fn bounded_spill_identity_binds_paired_window_chains_and_carries_across_sparse_tail_v0x8t() {
    let parsed = orna_syntax_v1::parse_module(WINDOW_CHAIN_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = plan(
        5_000,
        "checkpoint:restore-v1",
        false,
        false,
        false,
        true,
        true,
        "window:middle-unbounded",
    );
    let without_sparse_tail = plan(
        5_000,
        "checkpoint:restore-v1",
        false,
        false,
        false,
        true,
        false,
        "window:middle-unbounded",
    );
    let reordered = plan(
        5_000,
        "checkpoint:restore-v1",
        true,
        false,
        false,
        true,
        true,
        "window:middle-unbounded",
    );
    let changed_spill = plan(
        4_999,
        "checkpoint:restore-v1",
        false,
        false,
        false,
        true,
        true,
        "window:middle-unbounded",
    );
    let changed_window = plan(
        5_000,
        "checkpoint:restore-v1",
        false,
        false,
        false,
        true,
        true,
        "window:middle-unbounded-v2",
    );
    let changed_restore = plan(
        5_000,
        "checkpoint:restore-v2",
        false,
        false,
        false,
        true,
        true,
        "window:middle-unbounded",
    );

    let baseline_project = project(&baseline);
    let baseline_restore = join_by_pair(&baseline, "pair:Restore");
    let baseline_tail = join_by_pair(&baseline, "pair:Tail");
    let fold_key = "paired_bounded_window_spill_window_chain_fold_identity";
    let spill_component_key = "paired_bounded_window_spill_window_chain_spill_fold_identity";
    let window_component_key = "paired_bounded_window_spill_window_chain_window_fold_identity";

    assert!(
        text(baseline_project, fold_key).starts_with("paired-bounded-window-spill-window-chain:")
    );
    assert_eq!(
        text(baseline_project, spill_component_key),
        text(
            baseline_project,
            "paired_bounded_window_spill_restore_spill_fold_identity"
        )
    );
    assert!(text(baseline_project, window_component_key).starts_with("paired-window-chain-fold:"));
    assert_eq!(
        integer(
            baseline_project,
            "paired_bounded_window_spill_window_chain_spill_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(
            baseline_project,
            "paired_bounded_window_spill_window_chain_spill_stage_count"
        ),
        2,
        "the unbounded middle window does not enter the bounded spill fold"
    );
    assert_eq!(
        integer(
            baseline_project,
            "paired_bounded_window_spill_window_chain_window_pair_count"
        ),
        3
    );
    assert_eq!(
        integer(
            baseline_project,
            "paired_bounded_window_spill_window_chain_window_identity_count"
        ),
        3
    );

    assert_eq!(
        text(
            baseline_restore,
            "paired_window_cost_restoration_window_fold_identity"
        ),
        text(
            baseline_tail,
            "paired_window_cost_restoration_window_fold_identity"
        ),
        "the sparse tail carries the exact paired window fold"
    );
    assert_eq!(
        text(
            baseline_tail,
            "paired_window_cost_restoration_window_restore_chain_transition"
        ),
        "carried_across_sparse_input"
    );
    assert_eq!(
        text(baseline_project, fold_key),
        text(project(&without_sparse_tail), fold_key),
        "a sparse tail does not advance the bounded spill/window-chain identity"
    );
    assert_eq!(
        text(baseline_project, fold_key),
        text(project(&reordered), fold_key),
        "descriptor ordering does not change the paired window-chain identity"
    );

    let changed_spill_project = project(&changed_spill);
    assert_ne!(
        text(baseline_project, fold_key),
        text(changed_spill_project, fold_key)
    );
    assert_ne!(
        text(baseline_project, spill_component_key),
        text(changed_spill_project, spill_component_key)
    );
    assert_eq!(
        text(baseline_project, window_component_key),
        text(changed_spill_project, window_component_key)
    );

    let changed_window_project = project(&changed_window);
    assert_ne!(
        text(baseline_project, fold_key),
        text(changed_window_project, fold_key)
    );
    assert_eq!(
        text(baseline_project, spill_component_key),
        text(changed_window_project, spill_component_key),
        "changing an unbounded window identity leaves bounded spill history unchanged"
    );
    assert_ne!(
        text(baseline_project, window_component_key),
        text(changed_window_project, window_component_key)
    );

    let changed_restore_project = project(&changed_restore);
    assert_eq!(
        text(baseline_project, fold_key),
        text(changed_restore_project, fold_key),
        "the paired window-chain digest is independent of restore-chain identity"
    );
    assert_ne!(
        text(
            baseline_project,
            "paired_bounded_window_spill_restore_chain_fold_identity"
        ),
        text(
            changed_restore_project,
            "paired_bounded_window_spill_restore_chain_fold_identity"
        )
    );
}
