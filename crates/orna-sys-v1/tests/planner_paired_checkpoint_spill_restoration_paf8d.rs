use std::collections::BTreeMap;

use orna_sys_v1::{
    ExplainError, ExpressionRef, MutableBranchSnapshot, ObjectRef, PlanDetail, PlanNode,
    PlanNodeKind, QueryCheckpointGenerationDescription, QueryJoinDescription,
    QueryJoinPairIdentityDescription, QueryPairedCheckpointSegmentCompactionChainDescription,
    QueryPairedCheckpointSegmentCompactionStepDescription,
    QueryPairedCheckpointSpillRestoreChainDescription,
    QueryPairedCheckpointSpillRestoreOccurrenceDescription,
    QueryPairedSegmentRotationChainDescription, QueryPairedSegmentRotationDescription,
    QueryPairedStreamCompactionChainDescription, QueryPairedStreamCompactionOccurrenceDescription,
    QueryPartialIndexDescription, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_paired_checkpoint_rotation_stream_and_spill_restore_chains,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_paired_checkpoint_spill_restoration_paf8d.orna");

fn object(reference: &str) -> ObjectRef {
    ObjectRef::descriptive(reference)
}

fn expression(reference: &str) -> ExpressionRef {
    ExpressionRef::descriptive(reference)
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

fn join(
    source: &str,
    estimate: Option<(u64, u64, Option<MutableBranchSnapshot>)>,
) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics: estimate.map(|(rows, bytes, branch)| statistics(rows, bytes, branch)),
        predicate: Some(expression(&format!("expr:join-{source}"))),
    }
}

fn query(generation_a: u64) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-checkpoint-spill-restoration-paf8d"),
        source: object("table:Anchor"),
        source_statistics: Some(statistics(100, 40_000, Some(branch("branch:anchor", 3)))),
        joins: vec![
            join(
                "table:ChildB",
                Some((7, 8_192, Some(branch("branch:beta", 8)))),
            ),
            join("table:Unknown", None),
            join("table:Middle", Some((4, 8_192, None))),
            join(
                "table:ChildA",
                Some((2, 4_096, Some(branch("branch:alpha", generation_a)))),
            ),
        ],
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: Some(20),
        mutations: Vec::new(),
        materialize_into: None,
    }
}

fn pairs() -> Vec<QueryJoinPairIdentityDescription> {
    ["table:ChildA", "table:ChildB", "table:Unknown"]
        .into_iter()
        .map(|source| QueryJoinPairIdentityDescription {
            identity: object(&format!("pair:{source}")),
            left_source: object("table:Anchor"),
            right_source: object(source),
            predicate: Some(expression(&format!("expr:join-{source}"))),
        })
        .collect()
}

fn checkpoint_chains(
    generation_a: u64,
) -> Vec<QueryPairedCheckpointSegmentCompactionChainDescription> {
    [
        ("table:ChildA", "branch:alpha", generation_a),
        ("table:ChildB", "branch:beta", 8),
    ]
    .into_iter()
    .map(|(source, branch_name, generation)| {
        QueryPairedCheckpointSegmentCompactionChainDescription {
            join_pair_identity: object(&format!("pair:{source}")),
            branch: branch(branch_name, generation),
            steps: vec![QueryPairedCheckpointSegmentCompactionStepDescription {
                checkpoint_identity: object(&format!("checkpoint:{branch_name}")),
                left_segment_identity: Some(object(&format!("segment:{branch_name}:left"))),
                right_segment_identity: Some(object(&format!("segment:{branch_name}:right"))),
                left_compaction_identity: Some(object(&format!("compact:{branch_name}:left"))),
                right_compaction_identity: Some(object(&format!("compact:{branch_name}:right"))),
            }],
        }
    })
    .collect()
}

fn rotation_chains(generation_a: u64) -> Vec<QueryPairedSegmentRotationChainDescription> {
    [
        ("table:ChildA", "branch:alpha", generation_a),
        ("table:ChildB", "branch:beta", 8),
    ]
    .into_iter()
    .map(
        |(source, branch_name, generation)| QueryPairedSegmentRotationChainDescription {
            join_pair_identity: object(&format!("pair:{source}")),
            branch: branch(branch_name, generation),
            rotations: vec![QueryPairedSegmentRotationDescription {
                checkpoint_identity: object(&format!("rotation-checkpoint:{branch_name}")),
                source_stream_ordinal: 1,
                fold_ordinal: 2,
                order: 3,
                left_segment_identity: object(&format!("rotation-left:{branch_name}")),
                right_segment_identity: object(&format!("rotation-right:{branch_name}")),
            }],
        },
    )
    .collect()
}

fn stream_chains(generation_a: u64) -> Vec<QueryPairedStreamCompactionChainDescription> {
    [
        ("table:ChildA", "branch:alpha", generation_a),
        ("table:ChildB", "branch:beta", 8),
    ]
    .into_iter()
    .map(
        |(source, branch_name, generation)| QueryPairedStreamCompactionChainDescription {
            join_pair_identity: object(&format!("pair:{source}")),
            branch: branch(branch_name, generation),
            occurrences: vec![QueryPairedStreamCompactionOccurrenceDescription {
                checkpoint_identity: object(&format!("checkpoint:{branch_name}")),
                restore_ordinal: 0,
                handoff_ordinal: 1,
                source_stream_ordinal: 0,
                compaction_ordinal: 0,
                fold_ordinal: 1,
                order: 2,
                left_checkpoint_identity: Some(object(&format!("checkpoint:{branch_name}:left"))),
                right_checkpoint_identity: Some(object(&format!("checkpoint:{branch_name}:right"))),
                redo_fold_identity: object(&format!("redo-fold:{branch_name}")),
                left_segment_identity: object(&format!("stream-left:{branch_name}")),
                right_segment_identity: object(&format!("stream-right:{branch_name}")),
            }],
        },
    )
    .collect()
}

fn generation(value: u64, position: Option<&[u8]>) -> QueryCheckpointGenerationDescription {
    QueryCheckpointGenerationDescription {
        generation: value,
        position: position.map(<[u8]>::to_vec),
    }
}

fn occurrence(
    checkpoint: &str,
    restore: u64,
    spill_ordinal: u64,
    spill: &str,
    stream_ordinal: u64,
    stream: &str,
    compaction: u64,
    handoff: u64,
    fold: u64,
    order: u64,
    left: Option<QueryCheckpointGenerationDescription>,
    right: Option<QueryCheckpointGenerationDescription>,
    redo_fold: &str,
) -> QueryPairedCheckpointSpillRestoreOccurrenceDescription {
    QueryPairedCheckpointSpillRestoreOccurrenceDescription {
        checkpoint_identity: object(checkpoint),
        restore_ordinal: restore,
        spill_ordinal,
        spill_identity: object(spill),
        source_stream_ordinal: stream_ordinal,
        source_stream_identity: object(stream),
        compaction_ordinal: compaction,
        handoff_ordinal: handoff,
        fold_ordinal: fold,
        order,
        left_checkpoint_generation: left,
        right_checkpoint_generation: right,
        redo_fold_identity: object(redo_fold),
    }
}

fn spill_chains(
    generation_a: u64,
    suffix: &str,
) -> Vec<QueryPairedCheckpointSpillRestoreChainDescription> {
    vec![
        QueryPairedCheckpointSpillRestoreChainDescription {
            join_pair_identity: object("pair:table:ChildA"),
            branch: branch("branch:alpha", generation_a),
            occurrences: vec![
                occurrence(
                    "checkpoint:alpha",
                    0,
                    0,
                    &format!("spill:alpha-0-{suffix}"),
                    0,
                    "stream:alpha-0",
                    0,
                    2,
                    0,
                    2,
                    Some(generation(10, Some(&[0xa0]))),
                    Some(generation(10, None)),
                    "redo:alpha-0",
                ),
                occurrence(
                    "checkpoint:alpha",
                    0,
                    0,
                    &format!("spill:alpha-0-{suffix}"),
                    0,
                    "stream:alpha-0",
                    0,
                    2,
                    0,
                    3,
                    Some(generation(10, Some(&[0xa0]))),
                    Some(generation(10, None)),
                    "redo:alpha-0",
                ),
                occurrence(
                    "checkpoint:alpha",
                    0,
                    0,
                    &format!("spill:alpha-0-{suffix}"),
                    0,
                    "stream:alpha-0",
                    1,
                    7,
                    1,
                    3,
                    Some(generation(11, Some(&[]))),
                    None,
                    "redo:alpha-1",
                ),
                occurrence(
                    "checkpoint:alpha",
                    1,
                    0,
                    &format!("spill:alpha-0-{suffix}"),
                    0,
                    "stream:alpha-0",
                    0,
                    9,
                    1,
                    3,
                    None,
                    None,
                    "redo:alpha-later",
                ),
            ],
        },
        QueryPairedCheckpointSpillRestoreChainDescription {
            join_pair_identity: object("pair:table:ChildB"),
            branch: branch("branch:beta", 8),
            occurrences: vec![occurrence(
                "checkpoint:beta",
                0,
                0,
                &format!("spill:beta-{suffix}"),
                0,
                "stream:beta-0",
                0,
                4,
                2,
                5,
                Some(generation(21, None)),
                Some(generation(22, Some(&[0xb2]))),
                "redo:beta-0",
            )],
        },
    ]
}

fn indexes() -> Vec<QueryPartialIndexDescription> {
    ["table:ChildA", "table:ChildB"]
        .into_iter()
        .map(|source| QueryPartialIndexDescription {
            table: object(source),
            index: object(&format!("index:{source}")),
            partial_predicate: expression(&format!("expr:join-{source}")),
        })
        .collect()
}

fn plan(
    generation_a: u64,
    spills: &[QueryPairedCheckpointSpillRestoreChainDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_paired_checkpoint_rotation_stream_and_spill_restore_chains(
        &query(generation_a),
        &indexes(),
        &pairs(),
        &checkpoint_chains(generation_a),
        &rotation_chains(generation_a),
        &stream_chains(generation_a),
        spills,
    )
    .expect("paired checkpoint spill restores fold into the branch-local plan")
}

fn text<'a>(node: &'a PlanNode, key: &str) -> &'a str {
    match node.details().get(key) {
        Some(PlanDetail::Text(value)) => value,
        other => panic!("{key} should be text, got {other:?}"),
    }
}

fn number(node: &PlanNode, key: &str) -> u64 {
    match node.details().get(key) {
        Some(PlanDetail::Integer(value)) => *value,
        other => panic!("{key} should be an integer, got {other:?}"),
    }
}

fn access_by_object<'a>(plan: &'a orna_sys_v1::ExplainedPlan, object: &str) -> &'a PlanNode {
    plan.nodes()
        .iter()
        .find(|node| {
            matches!(node.kind(), PlanNodeKind::IndexLookup | PlanNodeKind::Scan)
                && node.object().map(ObjectRef::as_str) == Some(object)
        })
        .expect("planned access preserves the selected object")
}

fn join_by_source<'a>(plan: &'a orna_sys_v1::ExplainedPlan, source: &str) -> &'a PlanNode {
    let access = plan
        .nodes()
        .iter()
        .find(|node| {
            matches!(node.kind(), PlanNodeKind::IndexLookup | PlanNodeKind::Scan)
                && node.object().map(ObjectRef::as_str) == Some(source)
        })
        .expect("right input access exists");
    plan.nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Join
                && node.inputs()[1].as_str() == access.reference().as_str()
        })
        .expect("join exists for its right input")
}

fn joins_by_pair(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<&str, &PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .filter_map(|node| match node.details().get("join_pair_identity") {
            Some(PlanDetail::Text(identity)) => Some((identity.as_str(), node)),
            _ => None,
        })
        .collect()
}

#[test]
fn checkpoint_spill_fold_keeps_sparse_source_ancestry_over_branch_cost_chains() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 3);

    let spills = spill_chains(4, "v1");
    let baseline = plan(4, &spills);
    let mut reversed = spills.clone();
    reversed.reverse();
    let reordered = plan(4, &reversed);
    let changed_spill = plan(4, &spill_chains(4, "v2"));
    let rebound_branch = plan(5, &spill_chains(5, "v1"));
    let mut changed_order = spills.clone();
    changed_order[0].occurrences[0].order += 1;
    let changed_coordinate = plan(4, &changed_order);
    let mut changed_position_presence = spills.clone();
    changed_position_presence[0].occurrences[0]
        .left_checkpoint_generation
        .as_mut()
        .unwrap()
        .position = None;
    let changed_position = plan(4, &changed_position_presence);
    let joins = joins_by_pair(&baseline);

    let alpha_access = access_by_object(&baseline, "index:table:ChildA");
    assert_eq!(alpha_access.estimated_rows(), Some(2));
    assert_eq!(alpha_access.estimated_bytes(), Some(4_096));
    assert_eq!(alpha_access.estimated_work(), Some(3));
    let beta_access = access_by_object(&baseline, "index:table:ChildB");
    assert_eq!(beta_access.estimated_rows(), Some(7));
    assert_eq!(beta_access.estimated_bytes(), Some(8_192));
    assert_eq!(beta_access.estimated_work(), Some(9));

    let child_a = joins["pair:table:ChildA"];
    let after_alpha = text(
        child_a,
        "paired_checkpoint_spill_cost_restoration_fold_identity",
    );
    assert_eq!(
        text(
            child_a,
            "paired_checkpoint_spill_cost_restoration_transition"
        ),
        "append_paired_checkpoint_spill_restore_chain"
    );
    assert_eq!(
        text(
            child_a,
            "paired_checkpoint_spill_cost_restoration_stream_fold_identity"
        ),
        text(
            child_a,
            "paired_stream_compaction_cost_restoration_fold_identity"
        )
    );
    assert_eq!(
        number(
            child_a,
            "paired_checkpoint_spill_cost_restoration_occurrence_count"
        ),
        4
    );
    assert_eq!(
        number(
            child_a,
            "paired_checkpoint_spill_cost_restoration_restore_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_a,
            "paired_checkpoint_spill_cost_restoration_spill_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_a,
            "paired_checkpoint_spill_cost_restoration_source_stream_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_a,
            "paired_checkpoint_spill_cost_restoration_compaction_count"
        ),
        3
    );
    assert_eq!(
        number(
            child_a,
            "paired_checkpoint_spill_cost_restoration_checkpoint_side_count"
        ),
        5
    );
    assert_eq!(
        number(
            child_a,
            "paired_checkpoint_spill_cost_restoration_absent_checkpoint_side_count"
        ),
        3
    );
    assert_eq!(
        number(
            child_a,
            "paired_checkpoint_spill_cost_restoration_both_sides_absent_occurrence_count"
        ),
        1
    );
    assert_eq!(child_a.estimated_rows(), Some(20));

    let middle = join_by_source(&baseline, "table:Middle");
    assert_eq!(
        text(
            middle,
            "paired_checkpoint_spill_cost_restoration_transition"
        ),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        text(
            middle,
            "paired_checkpoint_spill_cost_restoration_fold_identity"
        ),
        after_alpha
    );
    assert_eq!(middle.estimated_rows(), Some(8));

    let child_b = joins["pair:table:ChildB"];
    let after_beta = text(
        child_b,
        "paired_checkpoint_spill_cost_restoration_fold_identity",
    );
    assert_ne!(after_alpha, after_beta);
    assert_eq!(
        text(
            child_b,
            "paired_checkpoint_spill_cost_restoration_parent_identity"
        ),
        after_alpha
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_spill_cost_restoration_chain_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_spill_cost_restoration_occurrence_count"
        ),
        5
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_spill_cost_restoration_restore_count"
        ),
        3
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_spill_cost_restoration_spill_count"
        ),
        3
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_spill_cost_restoration_source_stream_count"
        ),
        3
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_spill_cost_restoration_compaction_count"
        ),
        4
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_spill_cost_restoration_nested_stream_fold_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_spill_cost_restoration_branch_scope_count"
        ),
        2
    );
    assert_eq!(child_b.estimated_rows(), Some(6));

    let unknown = joins["pair:table:Unknown"];
    assert_eq!(
        text(
            unknown,
            "paired_checkpoint_spill_cost_restoration_transition"
        ),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        text(
            unknown,
            "paired_checkpoint_spill_cost_restoration_fold_identity"
        ),
        after_beta
    );
    assert_eq!(unknown.estimated_rows(), None);
    assert_eq!(
        text(
            baseline.root(),
            "paired_checkpoint_spill_cost_restoration_output_fold_identity"
        ),
        after_beta
    );
    assert_eq!(
        number(
            baseline.root(),
            "paired_checkpoint_spill_cost_restoration_output_occurrence_count"
        ),
        5
    );
    assert_eq!(baseline.plan().id(), reordered.plan().id());
    for variant in [
        &changed_spill,
        &changed_coordinate,
        &changed_position,
        &rebound_branch,
    ] {
        assert_ne!(
            text(
                variant.root(),
                "paired_checkpoint_spill_cost_restoration_output_fold_identity"
            ),
            after_beta,
            "every source coordinate and opaque checkpoint position affects ancestry identity",
        );
    }
}

#[test]
fn checkpoint_spill_chains_reject_wrong_branch_pair_and_empty_identity() {
    let mut wrong_branch = spill_chains(4, "v1");
    wrong_branch[0].branch.generation = 44;
    assert_eq!(
        explain_query_with_partial_indexes_and_paired_checkpoint_rotation_stream_and_spill_restore_chains(
            &query(4), &indexes(), &pairs(), &checkpoint_chains(4), &rotation_chains(4),
            &stream_chains(4), &wrong_branch,
        ),
        Err(ExplainError::InvalidObject),
    );

    let mut wrong_pair = spill_chains(4, "v1");
    wrong_pair[0].join_pair_identity = object("pair:table:Middle");
    assert_eq!(
        explain_query_with_partial_indexes_and_paired_checkpoint_rotation_stream_and_spill_restore_chains(
            &query(4), &indexes(), &pairs(), &checkpoint_chains(4), &rotation_chains(4),
            &stream_chains(4), &wrong_pair,
        ),
        Err(ExplainError::InvalidObject),
    );

    let mut empty_identity = spill_chains(4, "v1");
    empty_identity[0].occurrences[0].source_stream_identity = object("");
    assert_eq!(
        explain_query_with_partial_indexes_and_paired_checkpoint_rotation_stream_and_spill_restore_chains(
            &query(4), &indexes(), &pairs(), &checkpoint_chains(4), &rotation_chains(4),
            &stream_chains(4), &empty_identity,
        ),
        Err(ExplainError::InvalidObject),
    );
}
