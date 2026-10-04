use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, MutableBranchSnapshot, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    PlanWindowFrameBound, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryPairedCheckpointSegmentCompactionChainDescription,
    QueryPairedCheckpointSegmentCompactionStepDescription,
    QueryPairedSegmentRotationChainDescription, QueryPairedSegmentRotationDescription,
    QueryPlanDescription, QuerySourceStatistics, QueryWindowAggregatePushdownDescription,
    QueryWindowSpillDescription, SnapshotRef,
    explain_query_with_paired_cost_restoration_and_window_spill_pushdowns,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_window_spill_restore_fold_rooh4.orna");

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
        right_source: object(source),
        predicate: Some(expression(&format!("expr:join-{source}"))),
    }
}

fn checkpoint_chain(
    source: &str,
    branch_name: &str,
    generation: u64,
) -> QueryPairedCheckpointSegmentCompactionChainDescription {
    QueryPairedCheckpointSegmentCompactionChainDescription {
        join_pair_identity: object(&format!("pair:{source}")),
        branch: branch(branch_name, generation),
        steps: vec![QueryPairedCheckpointSegmentCompactionStepDescription {
            checkpoint_identity: object(&format!("checkpoint:{source}")),
            left_segment_identity: Some(object(&format!("segment:{source}:left"))),
            right_segment_identity: Some(object(&format!("segment:{source}:right"))),
            left_compaction_identity: Some(object(&format!("compact:{source}:left"))),
            right_compaction_identity: Some(object(&format!("compact:{source}:right"))),
        }],
    }
}

fn rotation_chain(
    source: &str,
    branch_name: &str,
    generation: u64,
) -> QueryPairedSegmentRotationChainDescription {
    QueryPairedSegmentRotationChainDescription {
        join_pair_identity: object(&format!("pair:{source}")),
        branch: branch(branch_name, generation),
        rotations: vec![QueryPairedSegmentRotationDescription {
            checkpoint_identity: object(&format!("rotation-checkpoint:{source}")),
            source_stream_ordinal: 1,
            fold_ordinal: 2,
            order: 3,
            left_segment_identity: object(&format!("rotation-left:{source}")),
            right_segment_identity: object(&format!("rotation-right:{source}")),
        }],
    }
}

fn aggregate(identity: &str, source: &str) -> QueryWindowAggregatePushdownDescription {
    QueryWindowAggregatePushdownDescription {
        identity: object(identity),
        source: object(source),
        aggregate: expression("expr:sum-value"),
        frame_identity: expression("frame:running"),
        frame_start: PlanWindowFrameBound::UnboundedPreceding,
        frame_end: PlanWindowFrameBound::CurrentRow,
    }
}

fn spill(
    identity: &str,
    source: &str,
    aggregate_identity: &str,
    working_set: u64,
    memory_budget: u64,
) -> QueryWindowSpillDescription {
    QueryWindowSpillDescription {
        identity: object(identity),
        join_pair_identity: object(&format!("pair:{source}")),
        source: object(source),
        window_aggregate_identity: object(aggregate_identity),
        estimated_working_set_bytes: Some(working_set),
        memory_budget_bytes: memory_budget,
    }
}

fn plan(second_spill_budget: u64, reverse_descriptors: bool) -> orna_sys_v1::ExplainedPlan {
    let query = QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-window-spill-restore-rooh4"),
        source: object("table:Anchor"),
        source_statistics: Some(statistics(100, 40_000, Some(branch("branch:anchor", 3)))),
        joins: vec![
            QueryJoinDescription {
                source: object("table:ChildB"),
                statistics: Some(statistics(5, 8_192, Some(branch("branch:beta", 8)))),
                predicate: Some(expression("expr:join-table:ChildB")),
            },
            QueryJoinDescription {
                source: object("table:Tail"),
                statistics: Some(statistics(30, 24_576, None)),
                predicate: Some(expression("expr:join-table:Tail")),
            },
            QueryJoinDescription {
                source: object("table:ChildA"),
                statistics: Some(statistics(2, 4_096, Some(branch("branch:alpha", 7)))),
                predicate: Some(expression("expr:join-table:ChildA")),
            },
        ],
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    };
    let mut pairs = ["table:ChildA", "table:ChildB", "table:Tail"]
        .into_iter()
        .map(pair)
        .collect::<Vec<_>>();
    let mut windows = vec![
        aggregate("window:child-a-sum", "table:ChildA"),
        aggregate("window:child-b-sum", "table:ChildB"),
    ];
    let mut spills = vec![
        spill(
            "spill:child-a-sum",
            "table:ChildA",
            "window:child-a-sum",
            9_000,
            5_000,
        ),
        spill(
            "spill:child-b-sum",
            "table:ChildB",
            "window:child-b-sum",
            20_000,
            second_spill_budget,
        ),
    ];
    let mut checkpoints = vec![
        checkpoint_chain("table:ChildA", "branch:alpha", 7),
        checkpoint_chain("table:ChildB", "branch:beta", 8),
    ];
    let mut rotations = vec![
        rotation_chain("table:ChildA", "branch:alpha", 7),
        rotation_chain("table:ChildB", "branch:beta", 8),
    ];
    if reverse_descriptors {
        pairs.reverse();
        windows.reverse();
        spills.reverse();
        checkpoints.reverse();
        rotations.reverse();
    }
    explain_query_with_paired_cost_restoration_and_window_spill_pushdowns(
        &query,
        &pairs,
        &windows,
        &spills,
        &checkpoints,
        &rotations,
        &[],
        &[],
        &[],
        &[],
    )
    .expect("paired window, spill, and restore descriptors explain together")
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

fn joins_by_pair(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<&str, &PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| (text(node, "join_pair_identity"), node))
        .collect()
}

#[test]
fn paired_spill_identity_is_retained_by_nested_window_restore_folds() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = plan(10_000, false);
    let reordered = plan(10_000, true);
    let changed_spill = plan(12_000, false);
    let joins = joins_by_pair(&baseline);
    let reordered_joins = joins_by_pair(&reordered);
    let changed_joins = joins_by_pair(&changed_spill);
    let child_a = joins["pair:table:ChildA"];
    let child_b = joins["pair:table:ChildB"];
    let tail = joins["pair:table:Tail"];

    assert_eq!(integer(child_a, "aggregate_spill_estimated_bytes"), 4_000);
    assert_eq!(integer(child_a, "aggregate_spill_estimated_io_blocks"), 1);
    assert_eq!(integer(child_a, "aggregate_spill_estimated_io_work"), 2);
    assert_eq!(integer(child_b, "aggregate_spill_estimated_bytes"), 10_000);
    assert_eq!(integer(child_b, "aggregate_spill_estimated_io_blocks"), 3);
    assert_eq!(integer(child_b, "aggregate_spill_estimated_io_work"), 6);

    assert_eq!(
        integer(child_a, "paired_window_cost_restoration_spill_pair_count"),
        1
    );
    assert_eq!(
        text(
            child_a,
            "paired_window_cost_restoration_spill_pair_identity"
        ),
        text(child_a, "paired_aggregate_spill_anchor_fold_identity")
    );
    assert_eq!(
        integer(child_b, "paired_window_cost_restoration_spill_pair_count"),
        2
    );
    assert_eq!(
        text(
            child_b,
            "paired_window_cost_restoration_spill_pair_identity"
        ),
        text(child_b, "paired_aggregate_spill_anchor_fold_identity")
    );
    assert_ne!(
        text(
            child_a,
            "paired_window_cost_restoration_spill_fold_identity"
        ),
        text(
            child_b,
            "paired_window_cost_restoration_spill_fold_identity"
        ),
        "the cumulative spill identity grows when the second exact pair is appended"
    );
    assert_eq!(
        text(child_b, "paired_window_cost_restoration_parent_identity"),
        text(child_a, "paired_window_cost_restoration_fold_identity")
    );
    assert_eq!(
        integer(child_b, "paired_window_cost_restoration_window_pair_count"),
        2
    );
    assert_eq!(
        integer(child_b, "paired_window_cost_restoration_restore_fold_count"),
        2
    );
    assert_eq!(
        integer(tail, "paired_window_cost_restoration_spill_pair_count"),
        2,
        "a sparse join carries the exact two accumulated spill pairs"
    );
    assert_eq!(
        text(
            tail,
            "paired_window_cost_restoration_spill_fold_identity"
        ),
        text(
            child_b,
            "paired_window_cost_restoration_spill_fold_identity"
        ),
        "a sparse join carries the nested spill-fold identity unchanged"
    );
    assert!(
        !tail
            .details()
            .contains_key("paired_window_cost_restoration_spill_pair_identity")
    );
    assert_eq!(
        text(tail, "paired_window_cost_restoration_spill_pairing"),
        "exact_spill_pairs_bound_to_nested_window_cost_restore_folds"
    );

    for pair_id in ["pair:table:ChildA", "pair:table:ChildB", "pair:table:Tail"] {
        assert_eq!(
            text(
                joins[pair_id],
                "paired_window_cost_restoration_fold_identity"
            ),
            text(
                reordered_joins[pair_id],
                "paired_window_cost_restoration_fold_identity"
            ),
            "descriptor order does not change the nested spill/restore fold"
        );
    }
    assert_ne!(
        text(child_b, "paired_window_cost_restoration_pair_identity"),
        text(
            changed_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_pair_identity"
        ),
        "changing the actual spill budget changes the exact paired restore identity"
    );
    assert_ne!(
        text(child_b, "paired_window_cost_restoration_fold_identity"),
        text(
            changed_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_fold_identity"
        )
    );
    assert_eq!(
        integer(
            changed_joins["pair:table:ChildB"],
            "aggregate_spill_estimated_bytes"
        ),
        8_000
    );
    assert_ne!(
        text(
            child_b,
            "paired_window_cost_restoration_spill_fold_identity"
        ),
        text(
            changed_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_spill_fold_identity"
        ),
        "the cumulative spill identity incorporates changed byte costs"
    );
    assert_ne!(
        text(tail, "paired_window_cost_restoration_fold_identity"),
        text(
            changed_joins["pair:table:Tail"],
            "paired_window_cost_restoration_fold_identity"
        ),
        "the sparse tail retains the changed upstream spill identity"
    );
}
