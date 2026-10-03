use std::collections::BTreeMap;

use orna_sys_v1::{
    ExplainError, ExpressionRef, MutableBranchSnapshot, ObjectRef, PlanDetail, PlanNode,
    PlanNodeKind, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryPairedCheckpointSegmentCompactionChainDescription,
    QueryPairedCheckpointSegmentCompactionStepDescription,
    QueryPairedSegmentRotationChainDescription, QueryPairedSegmentRotationDescription,
    QueryPairedStreamCompactionChainDescription, QueryPairedStreamCompactionOccurrenceDescription,
    QueryPartialIndexDescription, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_paired_checkpoint_rotation_and_stream_compaction_chains,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_paired_stream_compaction_restoration_5z9hy.orna");

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
        snapshot: SnapshotRef::descriptive("snapshot:paired-stream-compaction-restoration-5z9hy"),
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

fn occurrence(
    checkpoint: &str,
    restore: u64,
    handoff: u64,
    stream: u64,
    compaction: u64,
    fold: u64,
    order: u64,
    left_checkpoint: Option<&str>,
    right_checkpoint: Option<&str>,
    suffix: &str,
) -> QueryPairedStreamCompactionOccurrenceDescription {
    QueryPairedStreamCompactionOccurrenceDescription {
        checkpoint_identity: object(checkpoint),
        restore_ordinal: restore,
        handoff_ordinal: handoff,
        source_stream_ordinal: stream,
        compaction_ordinal: compaction,
        fold_ordinal: fold,
        order,
        left_checkpoint_identity: left_checkpoint.map(object),
        right_checkpoint_identity: right_checkpoint.map(object),
        redo_fold_identity: object(&format!("redo-fold:{suffix}")),
        left_segment_identity: object(&format!("stream-left:{suffix}")),
        right_segment_identity: object(&format!("stream-right:{suffix}")),
    }
}

fn stream_chains(
    generation_a: u64,
    suffix: &str,
) -> Vec<QueryPairedStreamCompactionChainDescription> {
    vec![
        QueryPairedStreamCompactionChainDescription {
            join_pair_identity: object("pair:table:ChildA"),
            branch: branch("branch:alpha", generation_a),
            occurrences: vec![
                occurrence(
                    "checkpoint:alpha-restore-4",
                    4,
                    2,
                    0,
                    1,
                    6,
                    9,
                    Some("checkpoint:alpha-left"),
                    Some("checkpoint:alpha-right"),
                    &format!("alpha-a-{suffix}"),
                ),
                occurrence(
                    "checkpoint:alpha-restore-4",
                    4,
                    2,
                    1,
                    1,
                    6,
                    9,
                    Some("checkpoint:alpha-left"),
                    None,
                    &format!("alpha-b-{suffix}"),
                ),
                occurrence(
                    "checkpoint:alpha-restore-5",
                    5,
                    3,
                    1,
                    2,
                    7,
                    10,
                    None,
                    None,
                    &format!("alpha-c-{suffix}"),
                ),
            ],
        },
        QueryPairedStreamCompactionChainDescription {
            join_pair_identity: object("pair:table:ChildB"),
            branch: branch("branch:beta", 8),
            occurrences: vec![occurrence(
                "checkpoint:beta-restore-2",
                2,
                4,
                0,
                3,
                8,
                11,
                Some("checkpoint:beta-left"),
                Some("checkpoint:beta-right"),
                &format!("beta-a-{suffix}"),
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
    streams: &[QueryPairedStreamCompactionChainDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_paired_checkpoint_rotation_and_stream_compaction_chains(
        &query(generation_a),
        &indexes(),
        &pairs(),
        &checkpoint_chains(generation_a),
        &rotation_chains(generation_a),
        streams,
    )
    .expect("stream compaction lineage folds into its branch-local plan")
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
fn stream_compaction_fold_preserves_coordinates_nests_and_survives_sparse_joins() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 3);

    let streams = stream_chains(4, "v1");
    let baseline = plan(4, &streams);
    let mut reversed = streams.clone();
    reversed.reverse();
    let reordered = plan(4, &reversed);
    let changed_occurrence = plan(4, &stream_chains(4, "v2"));
    let mut changed_handoff = streams.clone();
    changed_handoff[0].occurrences[0].handoff_ordinal += 1;
    let changed_coordinate = plan(4, &changed_handoff);
    let rebound_branch = plan(5, &stream_chains(5, "v1"));
    let joins = joins_by_pair(&baseline);

    let child_a_access = access_by_object(&baseline, "index:table:ChildA");
    assert_eq!(child_a_access.estimated_rows(), Some(2));
    assert_eq!(child_a_access.estimated_bytes(), Some(4_096));
    assert_eq!(child_a_access.estimated_work(), Some(3));
    let child_b_access = access_by_object(&baseline, "index:table:ChildB");
    assert_eq!(child_b_access.estimated_rows(), Some(7));
    assert_eq!(child_b_access.estimated_bytes(), Some(8_192));
    assert_eq!(child_b_access.estimated_work(), Some(9));

    let child_a = joins["pair:table:ChildA"];
    let after_alpha = text(
        child_a,
        "paired_stream_compaction_cost_restoration_fold_identity",
    );
    assert_eq!(
        text(
            child_a,
            "paired_stream_compaction_cost_restoration_transition"
        ),
        "append_paired_stream_compaction_chain"
    );
    assert_eq!(
        text(
            child_a,
            "paired_stream_compaction_cost_restoration_branch_identity"
        ),
        "branch:alpha"
    );
    assert_eq!(
        number(
            child_a,
            "paired_stream_compaction_cost_restoration_branch_generation"
        ),
        4
    );
    assert_eq!(
        text(
            child_a,
            "paired_stream_compaction_cost_restoration_checkpoint_fold_identity"
        ),
        text(child_a, "paired_checkpoint_cost_restoration_fold_identity")
    );
    assert_eq!(
        text(
            child_a,
            "paired_stream_compaction_cost_restoration_rotation_fold_identity"
        ),
        text(
            child_a,
            "paired_segment_rotation_cost_restoration_fold_identity"
        )
    );
    assert_eq!(
        number(
            child_a,
            "paired_stream_compaction_cost_restoration_occurrence_count"
        ),
        3
    );
    assert_eq!(
        number(
            child_a,
            "paired_stream_compaction_cost_restoration_checkpoint_side_count"
        ),
        3
    );
    assert_eq!(
        number(
            child_a,
            "paired_stream_compaction_cost_restoration_absent_checkpoint_side_count"
        ),
        3
    );
    assert_eq!(
        number(
            child_a,
            "paired_stream_compaction_cost_restoration_source_stream_count"
        ),
        2,
        "same stream coordinates retain distinct occurrences by ordered full tuple",
    );
    assert_eq!(child_a.estimated_rows(), Some(20));

    let middle = join_by_source(&baseline, "table:Middle");
    assert_eq!(
        text(
            middle,
            "paired_stream_compaction_cost_restoration_transition"
        ),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        text(
            middle,
            "paired_stream_compaction_cost_restoration_fold_identity"
        ),
        after_alpha
    );
    assert_eq!(middle.estimated_rows(), Some(8));

    let child_b = joins["pair:table:ChildB"];
    let after_beta = text(
        child_b,
        "paired_stream_compaction_cost_restoration_fold_identity",
    );
    assert_ne!(after_alpha, after_beta);
    assert_eq!(
        text(
            child_b,
            "paired_stream_compaction_cost_restoration_parent_identity"
        ),
        after_alpha
    );
    assert_eq!(
        number(
            child_b,
            "paired_stream_compaction_cost_restoration_chain_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_b,
            "paired_stream_compaction_cost_restoration_occurrence_count"
        ),
        4
    );
    assert_eq!(
        number(
            child_b,
            "paired_stream_compaction_cost_restoration_nested_checkpoint_fold_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_b,
            "paired_stream_compaction_cost_restoration_nested_rotation_fold_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_b,
            "paired_stream_compaction_cost_restoration_branch_scope_count"
        ),
        2
    );
    assert_eq!(child_b.estimated_rows(), Some(6));

    let unknown = joins["pair:table:Unknown"];
    assert_eq!(
        text(
            unknown,
            "paired_stream_compaction_cost_restoration_transition"
        ),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        text(
            unknown,
            "paired_stream_compaction_cost_restoration_fold_identity"
        ),
        after_beta
    );
    assert_eq!(unknown.estimated_rows(), None);
    assert_eq!(
        text(
            baseline.root(),
            "paired_stream_compaction_cost_restoration_output_fold_identity"
        ),
        after_beta
    );
    assert_eq!(
        number(
            baseline.root(),
            "paired_stream_compaction_cost_restoration_output_occurrence_count"
        ),
        4
    );
    assert_eq!(baseline.plan().id(), reordered.plan().id());
    assert_ne!(
        text(
            changed_occurrence.root(),
            "paired_stream_compaction_cost_restoration_output_fold_identity"
        ),
        after_beta
    );
    assert_ne!(
        text(
            changed_coordinate.root(),
            "paired_stream_compaction_cost_restoration_output_fold_identity"
        ),
        after_beta,
        "handoff coordinate is part of occurrence identity",
    );
    assert_ne!(
        text(
            rebound_branch.root(),
            "paired_stream_compaction_cost_restoration_output_fold_identity"
        ),
        after_beta
    );
}

#[test]
fn stream_compaction_chains_reject_mismatched_branch_and_duplicate_pair() {
    let mut mismatched = stream_chains(4, "v1");
    mismatched[0].branch.generation = 44;
    assert_eq!(
        explain_query_with_partial_indexes_and_paired_checkpoint_rotation_and_stream_compaction_chains(
            &query(4),
            &indexes(),
            &pairs(),
            &checkpoint_chains(4),
            &rotation_chains(4),
            &mismatched,
        ),
        Err(ExplainError::InvalidObject),
    );

    let mut duplicate_pair = stream_chains(4, "v1");
    duplicate_pair.push(duplicate_pair[0].clone());
    assert_eq!(
        explain_query_with_partial_indexes_and_paired_checkpoint_rotation_and_stream_compaction_chains(
            &query(4),
            &indexes(),
            &pairs(),
            &checkpoint_chains(4),
            &rotation_chains(4),
            &duplicate_pair,
        ),
        Err(ExplainError::InvalidObject),
    );

    let mut invalid_segment = stream_chains(4, "v1");
    invalid_segment[0].occurrences[0].right_segment_identity = object("");
    assert_eq!(
        explain_query_with_partial_indexes_and_paired_checkpoint_rotation_and_stream_compaction_chains(
            &query(4),
            &indexes(),
            &pairs(),
            &checkpoint_chains(4),
            &rotation_chains(4),
            &invalid_segment,
        ),
        Err(ExplainError::InvalidObject),
    );
}
