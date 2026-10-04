use orna_sys_v1::{
    ExpressionRef, MutableBranchSnapshot, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    PlanWindowFrameBound, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryPairedCheckpointSegmentCompactionChainDescription,
    QueryPairedCheckpointSegmentCompactionStepDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, QueryWindowSpillDescription,
    SnapshotRef, explain_query_with_paired_cost_restoration_and_window_spill_pushdowns,
};

const FIXTURE: &str = include_str!("fixtures/planner_bounded_spill_window_restore_axcfm.orna");

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

fn pair(source: &str) -> QueryJoinPairIdentityDescription {
    QueryJoinPairIdentityDescription {
        identity: object(&format!("pair:{source}")),
        left_source: object("table:Anchor"),
        right_source: object(&format!("table:{source}")),
        predicate: Some(expression(&format!("expr:join-{source}"))),
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
) -> orna_sys_v1::ExplainedPlan {
    let query = QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:bounded-spill-window-restore-axcfm"),
        source: object("table:Anchor"),
        source_statistics: Some(statistics(100, 40_000, Some(branch("branch:anchor", 3)))),
        joins: vec![
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
        ],
        predicate: None,
        projections: vec![expression("expr:window-output")],
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    };
    let mut aggregates = vec![
        aggregate(
            "window:first-bounded",
            "table:First",
            PlanWindowFrameBound::Preceding(2),
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:middle-unbounded",
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
    let mut pairs = ["First", "Gap", "Middle", "Restore"]
        .into_iter()
        .map(pair)
        .collect::<Vec<_>>();
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
            "window:middle-unbounded",
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
    .expect("exact bounded window spills and restore chains explain together")
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
fn bounded_spill_identity_carries_across_sparse_paired_window_restoration() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = plan(5_000, "checkpoint:restore-v1", false, false, false, true);
    let reordered = plan(5_000, "checkpoint:restore-v1", true, false, false, true);
    let changed_spill = plan(4_999, "checkpoint:restore-v1", false, false, false, true);
    let changed_restore = plan(5_000, "checkpoint:restore-v2", false, false, false, true);

    let project_node = project(&baseline);
    let fold_key = "paired_bounded_window_spill_restore_fold_identity";
    let spill_key = "paired_bounded_window_spill_restore_spill_fold_identity";
    let restore_key = "paired_bounded_window_spill_restore_window_cost_restoration_fold_identity";
    let gap = join_by_pair(&baseline, "pair:Gap");
    let restore = join_by_pair(&baseline, "pair:Restore");

    assert!(text(project_node, fold_key).starts_with("paired-bounded-window-spill-restore:"));
    assert!(text(project_node, spill_key).starts_with("paired-bounded-window-spill-fold:"));
    assert_eq!(
        text(project_node, restore_key),
        text(restore, "paired_window_cost_restoration_fold_identity")
    );
    assert_eq!(
        integer(
            project_node,
            "paired_bounded_window_spill_restore_spill_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(
            project_node,
            "paired_bounded_window_spill_restore_spill_stage_count"
        ),
        2,
        "the unbounded middle window does not enter the bounded spill fold"
    );
    assert_eq!(
        integer(
            project_node,
            "paired_bounded_window_spill_restore_window_pair_count"
        ),
        3
    );
    assert_eq!(
        integer(
            project_node,
            "paired_bounded_window_spill_restore_restore_fold_count"
        ),
        1
    );
    assert_eq!(
        integer(
            project_node,
            "paired_bounded_window_spill_restore_estimated_bytes"
        ),
        13_000
    );
    assert_eq!(
        integer(
            project_node,
            "paired_bounded_window_spill_restore_estimated_io_blocks"
        ),
        4
    );
    assert_eq!(
        integer(
            project_node,
            "paired_bounded_window_spill_restore_estimated_io_work"
        ),
        8
    );
    assert_eq!(
        text(
            project_node,
            "paired_bounded_window_spill_restore_estimate_status"
        ),
        "computed"
    );
    assert_eq!(
        text(gap, "paired_window_cost_restoration_transition"),
        "carry_across_sparse_pair"
    );
    assert_eq!(
        text(restore, "paired_window_cost_restoration_transition"),
        "append_window_and_cost_restore_pair"
    );

    assert_eq!(
        text(project_node, fold_key),
        text(project(&reordered), fold_key),
        "reordering pair, aggregate, spill, and restore descriptors preserves the fold"
    );
    assert_ne!(
        text(project_node, spill_key),
        text(project(&changed_spill), spill_key),
        "a changed bounded memory budget changes the bounded spill identity"
    );
    assert_ne!(
        text(project_node, fold_key),
        text(project(&changed_spill), fold_key)
    );
    assert_eq!(
        text(project_node, spill_key),
        text(project(&changed_restore), spill_key),
        "restore lineage does not rewrite the bounded spill component"
    );
    assert_ne!(
        text(project_node, restore_key),
        text(project(&changed_restore), restore_key)
    );
    assert_ne!(
        text(project_node, fold_key),
        text(project(&changed_restore), fold_key)
    );
}

#[test]
fn bounded_spill_restore_fold_reports_unknown_estimates_and_requires_finite_frames_and_restore() {
    let unknown = plan(5_000, "checkpoint:restore-v1", false, true, false, true);
    let unknown_project = project(&unknown);
    assert_eq!(
        integer(
            unknown_project,
            "paired_bounded_window_spill_restore_unknown_working_set_count"
        ),
        1
    );
    assert_eq!(
        text(
            unknown_project,
            "paired_bounded_window_spill_restore_estimate_status"
        ),
        "unknown_working_set"
    );
    assert!(
        !unknown_project
            .details()
            .contains_key("paired_bounded_window_spill_restore_estimated_bytes")
    );

    let unbounded = plan(5_000, "checkpoint:restore-v1", false, false, true, true);
    assert!(
        !project(&unbounded)
            .details()
            .contains_key("paired_bounded_window_spill_restore_fold_identity")
    );

    let no_restore = plan(5_000, "checkpoint:restore-v1", false, false, false, false);
    assert!(
        !project(&no_restore)
            .details()
            .contains_key("paired_bounded_window_spill_restore_fold_identity")
    );
}
