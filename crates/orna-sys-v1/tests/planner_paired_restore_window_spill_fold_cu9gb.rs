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

const FIXTURE: &str = include_str!("fixtures/planner_paired_restore_window_spill_fold_cu9gb.orna");

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
    changed: bool,
) -> QueryPairedCheckpointSegmentCompactionChainDescription {
    QueryPairedCheckpointSegmentCompactionChainDescription {
        join_pair_identity: object(&format!("pair:{source}")),
        branch: branch(branch_name, generation),
        steps: vec![QueryPairedCheckpointSegmentCompactionStepDescription {
            checkpoint_identity: object(&format!(
                "checkpoint:{source}{}",
                if changed { ":rebound" } else { "" }
            )),
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

fn spill(identity: &str, source: &str, aggregate_identity: &str) -> QueryWindowSpillDescription {
    QueryWindowSpillDescription {
        identity: object(identity),
        join_pair_identity: object(&format!("pair:{source}")),
        source: object(source),
        window_aggregate_identity: object(aggregate_identity),
        estimated_working_set_bytes: Some(if source == "table:ChildA" {
            9_000
        } else {
            20_000
        }),
        memory_budget_bytes: if source == "table:ChildA" {
            5_000
        } else {
            10_000
        },
    }
}

fn plan(
    changed_restore: bool,
    reverse_descriptors: bool,
    changed_root_cost: bool,
) -> orna_sys_v1::ExplainedPlan {
    let query = QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-restore-window-spill-cu9gb"),
        source: object("table:Anchor"),
        source_statistics: Some(statistics(
            if changed_root_cost { 101 } else { 100 },
            if changed_root_cost { 41_000 } else { 40_000 },
            Some(branch("branch:anchor", 3)),
        )),
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
        spill("spill:child-a-sum", "table:ChildA", "window:child-a-sum"),
        spill("spill:child-b-sum", "table:ChildB", "window:child-b-sum"),
    ];
    let mut checkpoints = vec![
        checkpoint_chain("table:ChildA", "branch:alpha", 7, false),
        checkpoint_chain("table:ChildB", "branch:beta", 8, changed_restore),
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
    .expect("paired restore and window spill descriptors explain together")
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
fn nested_spill_cost_fold_retains_exact_paired_restore_identities() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = plan(false, false, false);
    let reordered = plan(false, true, false);
    let changed_restore = plan(true, false, false);
    let changed_root_cost = plan(false, false, true);
    let joins = joins_by_pair(&baseline);
    let reordered_joins = joins_by_pair(&reordered);
    let changed_joins = joins_by_pair(&changed_restore);
    let changed_cost_joins = joins_by_pair(&changed_root_cost);
    let child_a = joins["pair:table:ChildA"];
    let child_b = joins["pair:table:ChildB"];
    let tail = joins["pair:table:Tail"];
    let changed_b = changed_joins["pair:table:ChildB"];

    assert_eq!(integer(child_a, "aggregate_spill_estimated_bytes"), 4_000);
    assert_eq!(integer(child_b, "aggregate_spill_estimated_bytes"), 10_000);
    assert_eq!(
        integer(changed_b, "aggregate_spill_estimated_bytes"),
        10_000
    );
    assert_eq!(
        integer(
            changed_cost_joins["pair:table:ChildB"],
            "aggregate_spill_estimated_bytes"
        ),
        10_000,
        "changing the upstream cost input leaves the child's spill estimate intact"
    );
    assert_eq!(
        integer(child_a, "paired_window_spill_rule_cost_restore_pair_count"),
        1
    );
    assert_eq!(
        integer(child_b, "paired_window_spill_rule_cost_restore_pair_count"),
        2
    );
    assert_eq!(
        text(
            child_a,
            "paired_window_spill_rule_cost_restore_pair_identity"
        ),
        text(child_a, "paired_window_cost_restoration_pair_identity")
    );
    assert_eq!(
        text(
            child_b,
            "paired_window_spill_rule_cost_restore_pair_identity"
        ),
        text(child_b, "paired_window_cost_restoration_pair_identity")
    );
    assert_eq!(
        text(child_b, "paired_window_spill_rule_cost_restore_pairing"),
        "exact_restore_pair_identity_bound_into_spill_cost_history"
    );
    assert_eq!(
        text(
            child_b,
            "paired_window_spill_rule_cost_restore_cost_pairing"
        ),
        "nested_rule_costs_bind_left_right_cost_ancestry_and_exact_restore_pair"
    );
    assert_ne!(
        text(
            child_b,
            "paired_window_spill_rule_cost_restore_fold_identity"
        ),
        text(
            child_a,
            "paired_window_spill_rule_cost_restore_fold_identity"
        )
    );
    assert_eq!(
        integer(tail, "paired_window_spill_rule_cost_restore_pair_count"),
        2,
        "sparse tail carries both spill-associated restore pairs"
    );
    assert_eq!(
        text(tail, "paired_window_spill_rule_cost_restore_fold_identity"),
        text(
            child_b,
            "paired_window_spill_rule_cost_restore_fold_identity"
        )
    );
    assert!(
        !tail
            .details()
            .contains_key("paired_window_spill_rule_cost_restore_pair_identity")
    );

    for pair_id in ["pair:table:ChildA", "pair:table:ChildB", "pair:table:Tail"] {
        assert_eq!(
            text(
                joins[pair_id],
                "paired_window_spill_rule_cost_fold_identity"
            ),
            text(
                reordered_joins[pair_id],
                "paired_window_spill_rule_cost_fold_identity"
            ),
            "descriptor reordering preserves the spill/restore fold"
        );
    }
    assert_ne!(
        text(child_b, "paired_window_spill_rule_cost_fold_identity"),
        text(changed_b, "paired_window_spill_rule_cost_fold_identity"),
        "changing a real checkpoint identity changes the nested spill cost fold"
    );
    assert_ne!(
        text(
            child_b,
            "paired_window_spill_rule_cost_restore_cost_fold_identity"
        ),
        text(
            changed_b,
            "paired_window_spill_rule_cost_restore_cost_fold_identity"
        ),
        "changing the exact restore pair changes the nested rule cost restore fold"
    );
    assert_ne!(
        text(
            child_b,
            "paired_window_spill_rule_cost_restore_cost_fold_identity"
        ),
        text(
            changed_cost_joins["pair:table:ChildB"],
            "paired_window_spill_rule_cost_restore_cost_fold_identity"
        ),
        "changing the upstream join cost ancestry changes the nested rule restore fold"
    );
    assert_ne!(
        text(
            child_b,
            "paired_window_spill_rule_cost_restore_pair_identity"
        ),
        text(
            changed_b,
            "paired_window_spill_rule_cost_restore_pair_identity"
        )
    );
    assert_ne!(
        text(tail, "paired_window_spill_rule_cost_restore_fold_identity"),
        text(
            changed_joins["pair:table:Tail"],
            "paired_window_spill_rule_cost_restore_fold_identity"
        ),
        "the sparse tail retains the changed upstream restore identity"
    );
    assert_eq!(
        text(
            tail,
            "paired_window_spill_rule_cost_restore_cost_fold_identity"
        ),
        text(
            child_b,
            "paired_window_spill_rule_cost_restore_cost_fold_identity"
        ),
        "a sparse tail carries the accumulated nested rule cost restore identity"
    );
    assert_ne!(
        text(
            tail,
            "paired_window_spill_rule_cost_restore_cost_fold_identity"
        ),
        text(
            joins_by_pair(&changed_restore)["pair:table:Tail"],
            "paired_window_spill_rule_cost_restore_cost_fold_identity"
        ),
        "the sparse tail carries a changed upstream restore through nested rule costs"
    );
}
