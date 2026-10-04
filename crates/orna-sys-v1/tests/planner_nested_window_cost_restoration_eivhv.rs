use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, MutableBranchSnapshot, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    PlanWindowFrameBound, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryPairedCheckpointSegmentCompactionChainDescription,
    QueryPairedCheckpointSegmentCompactionStepDescription,
    QueryPairedSegmentRotationChainDescription, QueryPairedSegmentRotationDescription,
    QueryPlanDescription, QuerySourceStatistics, QueryWindowAggregatePushdownDescription,
    SnapshotRef, explain_query_with_paired_cost_restoration_and_window_pushdowns,
};

const FIXTURE: &str = include_str!("fixtures/planner_nested_window_cost_restoration_eivhv.orna");

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
    rebound: bool,
) -> QueryPairedCheckpointSegmentCompactionChainDescription {
    QueryPairedCheckpointSegmentCompactionChainDescription {
        join_pair_identity: object(&format!("pair:{source}")),
        branch: branch(branch_name, generation),
        steps: vec![QueryPairedCheckpointSegmentCompactionStepDescription {
            checkpoint_identity: object(&format!(
                "checkpoint:{source}{}",
                if rebound { ":rebound" } else { "" }
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

fn window(
    identity: &str,
    source: &str,
    operation: &str,
) -> QueryWindowAggregatePushdownDescription {
    QueryWindowAggregatePushdownDescription {
        identity: object(identity),
        source: object(source),
        aggregate: expression(operation),
        frame_identity: expression("frame:running"),
        frame_start: PlanWindowFrameBound::UnboundedPreceding,
        frame_end: PlanWindowFrameBound::CurrentRow,
    }
}

fn plan(
    second_window_operation: &str,
    reverse_descriptors: bool,
    changed_cost: Option<&str>,
    changed_restore: bool,
) -> orna_sys_v1::ExplainedPlan {
    let query = QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:nested-window-cost-restoration-eivhv"),
        source: object("table:Anchor"),
        source_statistics: Some(statistics(
            100,
            if changed_cost == Some("anchor") {
                40_001
            } else {
                40_000
            },
            Some(branch("branch:anchor", 3)),
        )),
        joins: vec![
            QueryJoinDescription {
                source: object("table:ChildB"),
                statistics: Some(statistics(
                    5,
                    if changed_cost == Some("child-b") {
                        8_193
                    } else {
                        8_192
                    },
                    Some(branch("branch:beta", 8)),
                )),
                predicate: Some(expression("expr:join-table:ChildB")),
            },
            QueryJoinDescription {
                source: object("table:Tail"),
                statistics: Some(statistics(30, 24_576, None)),
                predicate: Some(expression("expr:join-table:Tail")),
            },
            QueryJoinDescription {
                source: object("table:ChildA"),
                statistics: Some(statistics(
                    2,
                    if changed_cost == Some("child-a") {
                        4_097
                    } else {
                        4_096
                    },
                    Some(branch("branch:alpha", 7)),
                )),
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
    let mut checkpoints = vec![
        checkpoint_chain("table:ChildA", "branch:alpha", 7, false),
        checkpoint_chain("table:ChildB", "branch:beta", 8, changed_restore),
    ];
    let mut rotations = vec![
        rotation_chain("table:ChildA", "branch:alpha", 7),
        rotation_chain("table:ChildB", "branch:beta", 8),
    ];
    let mut windows = vec![
        window("window:child-a-sum", "table:ChildA", "expr:sum-value"),
        window(
            "window:child-b-sum",
            "table:ChildB",
            second_window_operation,
        ),
    ];
    if reverse_descriptors {
        pairs.reverse();
        checkpoints.reverse();
        rotations.reverse();
        windows.reverse();
    }
    explain_query_with_paired_cost_restoration_and_window_pushdowns(
        &query,
        &pairs,
        &windows,
        &checkpoints,
        &rotations,
        &[],
        &[],
        &[],
        &[],
    )
    .expect("paired window and cost-restoration descriptors produce an explain plan")
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
fn nested_window_identities_survive_paired_cost_restore_folds_and_sparse_edges() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = plan("expr:sum-value", false, None, false);
    let reordered = plan("expr:sum-value", true, None, false);
    let changed_window = plan("expr:avg-value", false, None, false);
    let changed_root_cost = plan("expr:sum-value", false, Some("anchor"), false);
    let changed_left_pair_cost = plan("expr:sum-value", false, Some("child-a"), false);
    let changed_right_pair_cost = plan("expr:sum-value", false, Some("child-b"), false);
    let changed_restore = plan("expr:sum-value", false, None, true);
    let joins = joins_by_pair(&baseline);
    let reordered_joins = joins_by_pair(&reordered);
    let changed_joins = joins_by_pair(&changed_window);
    let changed_root_cost_joins = joins_by_pair(&changed_root_cost);
    let changed_left_pair_cost_joins = joins_by_pair(&changed_left_pair_cost);
    let changed_right_pair_cost_joins = joins_by_pair(&changed_right_pair_cost);
    let changed_restore_joins = joins_by_pair(&changed_restore);
    let child_a = joins["pair:table:ChildA"];
    let child_b = joins["pair:table:ChildB"];
    let tail = joins["pair:table:Tail"];

    assert_eq!(
        integer(child_a, "paired_checkpoint_cost_restoration_chain_count"),
        1
    );
    assert_eq!(
        integer(child_b, "paired_checkpoint_cost_restoration_chain_count"),
        2
    );
    assert_eq!(
        integer(tail, "paired_checkpoint_cost_restoration_chain_count"),
        2
    );
    assert_eq!(
        text(tail, "paired_checkpoint_cost_restoration_transition"),
        "carry_through_sparse_cost_fold"
    );

    assert_eq!(
        integer(child_a, "paired_window_cost_restoration_pair_count"),
        1
    );
    assert_eq!(
        integer(child_a, "paired_window_cost_restoration_window_pair_count"),
        1
    );
    assert_eq!(
        integer(child_a, "paired_window_cost_restoration_window_count"),
        1
    );
    assert_eq!(
        integer(
            child_a,
            "paired_window_cost_restoration_window_identity_pair_count"
        ),
        1
    );
    assert_eq!(
        integer(child_a, "paired_window_cost_restoration_restore_fold_count"),
        2
    );
    assert_eq!(
        integer(child_a, "paired_window_cost_restoration_cost_pair_count"),
        1
    );
    assert_eq!(
        text(child_a, "paired_window_cost_restoration_transition"),
        "append_window_and_cost_restore_pair"
    );
    assert_eq!(
        text(child_b, "paired_window_cost_restoration_parent_identity"),
        text(child_a, "paired_window_cost_restoration_fold_identity")
    );
    assert_eq!(
        integer(child_b, "paired_window_cost_restoration_pair_count"),
        2
    );
    assert_eq!(
        integer(child_b, "paired_window_cost_restoration_window_pair_count"),
        2
    );
    assert_eq!(
        integer(child_b, "paired_window_cost_restoration_window_count"),
        2
    );
    assert_eq!(
        integer(
            child_b,
            "paired_window_cost_restoration_window_identity_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(child_b, "paired_window_cost_restoration_restore_fold_count"),
        2
    );
    assert_eq!(
        integer(child_b, "paired_window_cost_restoration_cost_pair_count"),
        2
    );
    assert_eq!(
        text(child_b, "paired_window_cost_restoration_transition"),
        "append_window_and_cost_restore_pair"
    );
    assert_eq!(
        text(tail, "paired_window_cost_restoration_parent_identity"),
        text(child_b, "paired_window_cost_restoration_fold_identity")
    );
    assert_eq!(
        integer(tail, "paired_window_cost_restoration_pair_count"),
        3
    );
    assert_eq!(
        integer(tail, "paired_window_cost_restoration_window_pair_count"),
        2
    );
    assert_eq!(
        integer(tail, "paired_window_cost_restoration_window_count"),
        2
    );
    assert_eq!(
        integer(
            tail,
            "paired_window_cost_restoration_window_identity_pair_count"
        ),
        2,
        "the sparse edge carries both paired window identities"
    );
    assert_eq!(
        text(
            tail,
            "paired_window_cost_restoration_window_identity_pairing"
        ),
        "exact_pair_window_identities_bound_across_nested_cost_restore_folds"
    );
    assert_eq!(
        text(tail, "paired_window_cost_restoration_window_fold_identity"),
        text(
            child_b,
            "paired_window_cost_restoration_window_fold_identity"
        ),
        "a sparse edge carries the last paired window identity fold"
    );
    assert_ne!(
        text(
            child_a,
            "paired_window_cost_restoration_window_restore_chain_fold_identity"
        ),
        text(
            child_b,
            "paired_window_cost_restoration_window_restore_chain_fold_identity"
        ),
        "the nested composite advances across the second window and restore chain pair"
    );
    assert_eq!(
        text(
            child_b,
            "paired_window_cost_restoration_window_restore_chain_pairing"
        ),
        "nested_window_identity_bound_to_exact_paired_cost_restore_chain_folds"
    );
    assert_eq!(
        text(
            child_b,
            "paired_window_cost_restoration_window_restore_chain_transition"
        ),
        "advanced_window_and_restore_chains"
    );
    assert_eq!(
        text(
            tail,
            "paired_window_cost_restoration_window_restore_chain_fold_identity"
        ),
        text(
            child_b,
            "paired_window_cost_restoration_window_restore_chain_fold_identity"
        ),
        "the sparse edge carries the cumulative window and restore-chain identity"
    );
    assert_eq!(
        text(
            tail,
            "paired_window_cost_restoration_window_restore_chain_transition"
        ),
        "carried_across_sparse_input"
    );
    assert_eq!(
        integer(tail, "paired_window_cost_restoration_restore_fold_count"),
        2
    );
    assert_eq!(
        integer(tail, "paired_window_cost_restoration_cost_pair_count"),
        3,
        "the sparse edge contributes its exact left/right cost ancestry"
    );
    assert_eq!(
        integer(
            child_a,
            "paired_window_cost_restoration_pair_envelope_count"
        ),
        1
    );
    assert_eq!(
        integer(
            child_b,
            "paired_window_cost_restoration_pair_envelope_count"
        ),
        2
    );
    assert_eq!(
        integer(tail, "paired_window_cost_restoration_pair_envelope_count"),
        3,
        "the sparse edge gets an envelope around the carried nested chain"
    );
    assert_eq!(
        text(tail, "paired_window_cost_restoration_pair_envelope_pairing"),
        "cumulative_window_restore_chain_identity_bound_to_each_exact_pair_envelope"
    );
    assert_ne!(
        text(
            child_a,
            "paired_window_cost_restoration_pair_envelope_identity"
        ),
        text(
            child_b,
            "paired_window_cost_restoration_pair_envelope_identity"
        ),
        "each exact pair envelope binds its own cumulative nested chain"
    );
    assert_ne!(
        text(
            child_b,
            "paired_window_cost_restoration_pair_envelope_identity"
        ),
        text(
            tail,
            "paired_window_cost_restoration_pair_envelope_identity"
        ),
        "a sparse edge has a distinct envelope while carrying the nested chain"
    );
    assert_ne!(
        text(
            child_a,
            "paired_window_cost_restoration_pair_envelope_fold_identity"
        ),
        text(
            child_b,
            "paired_window_cost_restoration_pair_envelope_fold_identity"
        )
    );
    assert_ne!(
        text(
            child_b,
            "paired_window_cost_restoration_pair_envelope_fold_identity"
        ),
        text(
            tail,
            "paired_window_cost_restoration_pair_envelope_fold_identity"
        ),
        "the cumulative envelope fold includes the sparse pair"
    );
    assert_eq!(
        text(tail, "paired_window_cost_restoration_cost_pairing"),
        "exact_left_and_right_cost_ancestors_bound_to_each_window_restore_pair"
    );
    assert_eq!(
        text(tail, "paired_window_cost_restoration_transition"),
        "carry_across_sparse_pair"
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
            "resolving descriptors in a different order retains identical nested window values"
        );
        assert_eq!(
            text(
                joins[pair_id],
                "paired_window_cost_restoration_window_fold_identity"
            ),
            text(
                reordered_joins[pair_id],
                "paired_window_cost_restoration_window_fold_identity"
            ),
            "descriptor reordering preserves the exact paired window fold"
        );
        assert_eq!(
            text(
                joins[pair_id],
                "paired_window_cost_restoration_window_restore_chain_fold_identity"
            ),
            text(
                reordered_joins[pair_id],
                "paired_window_cost_restoration_window_restore_chain_fold_identity"
            ),
            "descriptor reordering preserves the nested window and restore-chain fold"
        );
        assert_eq!(
            text(
                joins[pair_id],
                "paired_window_cost_restoration_pair_envelope_identity"
            ),
            text(
                reordered_joins[pair_id],
                "paired_window_cost_restoration_pair_envelope_identity"
            ),
            "descriptor reordering preserves each nested chain envelope"
        );
        assert_eq!(
            text(
                joins[pair_id],
                "paired_window_cost_restoration_pair_envelope_fold_identity"
            ),
            text(
                reordered_joins[pair_id],
                "paired_window_cost_restoration_pair_envelope_fold_identity"
            ),
            "descriptor reordering preserves the cumulative envelope fold"
        );
    }
    assert_ne!(
        text(child_b, "paired_window_cost_restoration_fold_identity"),
        text(
            changed_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_fold_identity"
        ),
        "changing the real aggregate operation changes the nested restoration identity"
    );
    assert_ne!(
        text(
            child_b,
            "paired_window_cost_restoration_window_fold_identity"
        ),
        text(
            changed_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_window_fold_identity"
        ),
        "the paired window fold binds the changed aggregate operation"
    );
    assert_eq!(
        text(
            changed_joins["pair:table:Tail"],
            "paired_window_cost_restoration_window_fold_identity"
        ),
        text(
            changed_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_window_fold_identity"
        ),
        "the sparse edge carries the changed paired window identity fold"
    );
    assert_ne!(
        text(
            child_b,
            "paired_window_cost_restoration_window_restore_chain_fold_identity"
        ),
        text(
            changed_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_window_restore_chain_fold_identity"
        ),
        "changing a window identity changes the nested restore-chain composite"
    );
    assert_ne!(
        text(
            child_b,
            "paired_window_cost_restoration_restore_chain_fold_identity"
        ),
        text(
            changed_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_restore_chain_fold_identity"
        ),
        "changing a window identity propagates through the restore-chain component"
    );
    assert_eq!(
        text(
            changed_joins["pair:table:Tail"],
            "paired_window_cost_restoration_window_restore_chain_fold_identity"
        ),
        text(
            changed_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_window_restore_chain_fold_identity"
        ),
        "the sparse edge carries the changed composite window restore-chain identity"
    );
    assert_ne!(
        text(child_b, "paired_window_cost_restoration_cost_fold_identity"),
        text(
            changed_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_cost_fold_identity"
        ),
        "the cost fold includes the current window restore pair"
    );
    assert_ne!(
        text(child_b, "join_cost_fold_identity"),
        text(
            changed_joins["pair:table:ChildB"],
            "join_cost_fold_identity"
        )
    );
    assert_ne!(
        text(
            child_b,
            "paired_window_cost_restoration_pair_envelope_identity"
        ),
        text(
            changed_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_pair_envelope_identity"
        ),
        "the changed window is bound into its exact pair envelope"
    );
    assert_ne!(
        text(
            child_b,
            "paired_window_cost_restoration_pair_envelope_fold_identity"
        ),
        text(
            changed_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_pair_envelope_fold_identity"
        ),
        "the changed nested chain advances the cumulative envelope fold"
    );
    assert_ne!(
        text(
            tail,
            "paired_window_cost_restoration_pair_envelope_identity"
        ),
        text(
            changed_joins["pair:table:Tail"],
            "paired_window_cost_restoration_pair_envelope_identity"
        ),
        "the sparse envelope carries the changed nested chain"
    );
    assert_ne!(
        text(
            tail,
            "paired_window_cost_restoration_pair_envelope_fold_identity"
        ),
        text(
            changed_joins["pair:table:Tail"],
            "paired_window_cost_restoration_pair_envelope_fold_identity"
        ),
        "the sparse envelope fold retains the changed nested chain"
    );
    assert_eq!(
        text(child_b, "paired_window_cost_restoration_pairing"),
        "exact_window_chains_across_paired_cost_restore_folds"
    );

    for pair_id in ["pair:table:ChildA", "pair:table:ChildB", "pair:table:Tail"] {
        assert_eq!(
            text(
                joins[pair_id],
                "paired_window_cost_restoration_cost_fold_identity"
            ),
            text(
                reordered_joins[pair_id],
                "paired_window_cost_restoration_cost_fold_identity"
            ),
            "descriptor reordering preserves exact nested cost ancestry"
        );
    }
    for changed in [
        &changed_root_cost_joins,
        &changed_left_pair_cost_joins,
        &changed_right_pair_cost_joins,
        &changed_restore_joins,
    ] {
        for pair_id in ["pair:table:ChildA", "pair:table:ChildB", "pair:table:Tail"] {
            assert_eq!(
                text(
                    joins[pair_id],
                    "paired_window_cost_restoration_window_fold_identity"
                ),
                text(
                    changed[pair_id],
                    "paired_window_cost_restoration_window_fold_identity"
                ),
                "cost and restore changes leave the exact window identity fold unchanged"
            );
        }
        assert_ne!(
            text(
                child_b,
                "paired_window_cost_restoration_restore_chain_fold_identity"
            ),
            text(
                changed["pair:table:ChildB"],
                "paired_window_cost_restoration_restore_chain_fold_identity"
            ),
            "cost and restore changes advance the exact restore-chain component"
        );
        assert_ne!(
            text(
                child_b,
                "paired_window_cost_restoration_window_restore_chain_fold_identity"
            ),
            text(
                changed["pair:table:ChildB"],
                "paired_window_cost_restoration_window_restore_chain_fold_identity"
            ),
            "cost and restore changes advance the nested composite fold"
        );
        assert_ne!(
            text(
                child_b,
                "paired_window_cost_restoration_pair_envelope_identity"
            ),
            text(
                changed["pair:table:ChildB"],
                "paired_window_cost_restoration_pair_envelope_identity"
            ),
            "cost and restore changes advance the exact nested chain envelope"
        );
        assert_ne!(
            text(
                child_b,
                "paired_window_cost_restoration_pair_envelope_fold_identity"
            ),
            text(
                changed["pair:table:ChildB"],
                "paired_window_cost_restoration_pair_envelope_fold_identity"
            ),
            "cost and restore changes advance the cumulative envelope fold"
        );
        assert_ne!(
            text(
                tail,
                "paired_window_cost_restoration_pair_envelope_identity"
            ),
            text(
                changed["pair:table:Tail"],
                "paired_window_cost_restoration_pair_envelope_identity"
            ),
            "a sparse envelope retains each changed nested chain identity"
        );
        assert_ne!(
            text(
                tail,
                "paired_window_cost_restoration_pair_envelope_fold_identity"
            ),
            text(
                changed["pair:table:Tail"],
                "paired_window_cost_restoration_pair_envelope_fold_identity"
            ),
            "the sparse envelope fold retains each changed nested chain identity"
        );
        assert_eq!(
            text(
                changed["pair:table:Tail"],
                "paired_window_cost_restoration_window_restore_chain_fold_identity"
            ),
            text(
                changed["pair:table:ChildB"],
                "paired_window_cost_restoration_window_restore_chain_fold_identity"
            ),
            "sparse edges carry each changed nested window restore-chain fold"
        );
    }
    assert_ne!(
        text(child_b, "paired_window_cost_restoration_cost_fold_identity"),
        text(
            changed_root_cost_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_cost_fold_identity"
        ),
        "the nested cost fold binds the source cost ancestor"
    );
    assert_ne!(
        text(child_b, "paired_window_cost_restoration_cost_fold_identity"),
        text(
            changed_left_pair_cost_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_cost_fold_identity"
        ),
        "the nested cost fold binds the accumulated left cost ancestor"
    );
    assert_ne!(
        text(child_b, "paired_window_cost_restoration_cost_fold_identity"),
        text(
            changed_right_pair_cost_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_cost_fold_identity"
        ),
        "the nested cost fold binds the current right cost ancestor"
    );
    assert_ne!(
        text(child_b, "paired_window_cost_restoration_cost_fold_identity"),
        text(
            changed_restore_joins["pair:table:ChildB"],
            "paired_window_cost_restoration_cost_fold_identity"
        ),
        "the nested cost fold binds the exact restore identity"
    );
    assert_ne!(
        text(tail, "paired_window_cost_restoration_cost_fold_identity"),
        text(
            changed_restore_joins["pair:table:Tail"],
            "paired_window_cost_restoration_cost_fold_identity"
        ),
        "the sparse edge carries changed restore ancestry through nested costs"
    );
}
