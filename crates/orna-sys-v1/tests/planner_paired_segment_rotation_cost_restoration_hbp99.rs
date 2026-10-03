use std::collections::BTreeMap;

use orna_sys_v1::{
    ExplainError, ExpressionRef, MutableBranchSnapshot, ObjectRef, PlanDetail, PlanNode,
    PlanNodeKind, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryPairedCheckpointSegmentCompactionChainDescription,
    QueryPairedCheckpointSegmentCompactionStepDescription,
    QueryPairedSegmentRotationChainDescription, QueryPairedSegmentRotationDescription,
    QueryPartialIndexDescription, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_join_pair_identities,
    explain_query_with_partial_indexes_and_paired_checkpoint_compaction_and_segment_rotation_chains,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_paired_segment_rotation_cost_restoration_hbp99.orna");

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
        snapshot: SnapshotRef::descriptive(
            "snapshot:paired-segment-rotation-cost-restoration-hbp99",
        ),
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

fn compaction_step(
    checkpoint: &str,
    left_segment: &str,
    right_segment: Option<&str>,
    left_compaction: &str,
    right_compaction: Option<&str>,
) -> QueryPairedCheckpointSegmentCompactionStepDescription {
    QueryPairedCheckpointSegmentCompactionStepDescription {
        checkpoint_identity: object(checkpoint),
        left_segment_identity: Some(object(left_segment)),
        right_segment_identity: right_segment.map(object),
        left_compaction_identity: Some(object(left_compaction)),
        right_compaction_identity: right_compaction.map(object),
    }
}

fn checkpoint_chains(
    generation_a: u64,
) -> Vec<QueryPairedCheckpointSegmentCompactionChainDescription> {
    vec![
        QueryPairedCheckpointSegmentCompactionChainDescription {
            join_pair_identity: object("pair:table:ChildA"),
            branch: branch("branch:alpha", generation_a),
            steps: vec![compaction_step(
                "checkpoint:alpha-1",
                "segment:alpha-left-1",
                Some("segment:alpha-right-1"),
                "compact:alpha-left-1",
                Some("compact:alpha-right-1"),
            )],
        },
        QueryPairedCheckpointSegmentCompactionChainDescription {
            join_pair_identity: object("pair:table:ChildB"),
            branch: branch("branch:beta", 8),
            steps: vec![compaction_step(
                "checkpoint:beta-1",
                "segment:beta-left-1",
                Some("segment:beta-right-1"),
                "compact:beta-left-1",
                Some("compact:beta-right-1"),
            )],
        },
    ]
}

fn rotation(
    checkpoint: &str,
    stream: u64,
    fold: u64,
    order: u64,
    left: &str,
    right: &str,
) -> QueryPairedSegmentRotationDescription {
    QueryPairedSegmentRotationDescription {
        checkpoint_identity: object(checkpoint),
        source_stream_ordinal: stream,
        fold_ordinal: fold,
        order,
        left_segment_identity: object(left),
        right_segment_identity: object(right),
    }
}

fn rotation_chains(
    generation_a: u64,
    suffix: &str,
) -> Vec<QueryPairedSegmentRotationChainDescription> {
    vec![
        QueryPairedSegmentRotationChainDescription {
            join_pair_identity: object("pair:table:ChildA"),
            branch: branch("branch:alpha", generation_a),
            rotations: vec![
                rotation(
                    "rotation/checkpoint-alpha",
                    0,
                    2,
                    9,
                    &format!("rotation/left-alpha-0-{suffix}"),
                    "rotation/right-alpha-0",
                ),
                rotation(
                    "rotation/checkpoint-alpha",
                    1,
                    2,
                    9,
                    "rotation/left-alpha-1",
                    "rotation/right-alpha-1",
                ),
                rotation(
                    "rotation/checkpoint-alpha-next",
                    0,
                    3,
                    10,
                    "rotation/left-alpha-next",
                    "rotation/right-alpha-next",
                ),
            ],
        },
        QueryPairedSegmentRotationChainDescription {
            join_pair_identity: object("pair:table:ChildB"),
            branch: branch("branch:beta", 8),
            rotations: vec![
                rotation(
                    "rotation/checkpoint-beta",
                    0,
                    4,
                    20,
                    "rotation/left-beta-0",
                    "rotation/right-beta-0",
                ),
                rotation(
                    "rotation/checkpoint-beta-next",
                    0,
                    5,
                    21,
                    "rotation/left-beta-next",
                    "rotation/right-beta-next",
                ),
            ],
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
    checkpoint_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    rotation_chains: &[QueryPairedSegmentRotationChainDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_paired_checkpoint_compaction_and_segment_rotation_chains(
        &query(generation_a),
        &indexes(),
        &pairs(),
        checkpoint_chains,
        rotation_chains,
    )
    .expect("paired segment rotation chains fold into the branch-local plan")
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
        .expect("planned access preserves its selected object")
}

fn join_by_source<'a>(plan: &'a orna_sys_v1::ExplainedPlan, source: &str) -> &'a PlanNode {
    let access = plan
        .nodes()
        .iter()
        .find(|node| {
            matches!(node.kind(), PlanNodeKind::IndexLookup | PlanNodeKind::Scan)
                && node.object().map(ObjectRef::as_str) == Some(source)
        })
        .expect("right input access is present");
    plan.nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Join
                && node.inputs()[1].as_str() == access.reference().as_str()
        })
        .expect("join is present for its resolved right input")
}

fn joins_by_pair<'a>(plan: &'a orna_sys_v1::ExplainedPlan) -> BTreeMap<&'a str, &'a PlanNode> {
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
fn segment_rotation_cost_fold_nests_checkpoint_lineage_and_survives_sparse_joins() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 3);

    let checkpoint_lineage = checkpoint_chains(4);
    let rotations = rotation_chains(4, "v1");
    let baseline = plan(4, &checkpoint_lineage, &rotations);
    let mut reordered_chains = rotations.clone();
    reordered_chains.reverse();
    let reordered = plan(4, &checkpoint_lineage, &reordered_chains);
    let changed_segment = plan(4, &checkpoint_lineage, &rotation_chains(4, "v2"));
    let rebound_branch = plan(5, &checkpoint_chains(5), &rotation_chains(5, "v1"));
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
        "paired_segment_rotation_cost_restoration_fold_identity",
    );
    assert_eq!(
        text(
            child_a,
            "paired_segment_rotation_cost_restoration_transition"
        ),
        "append_paired_segment_rotation_chain"
    );
    assert_eq!(
        text(
            child_a,
            "paired_segment_rotation_cost_restoration_branch_identity"
        ),
        "branch:alpha"
    );
    assert_eq!(
        number(
            child_a,
            "paired_segment_rotation_cost_restoration_branch_generation"
        ),
        4
    );
    assert_eq!(
        text(
            child_a,
            "paired_segment_rotation_cost_restoration_checkpoint_fold_identity"
        ),
        text(child_a, "paired_checkpoint_cost_restoration_fold_identity"),
        "rotation identity nests on the checkpoint-compaction fold for the same join",
    );
    assert_eq!(
        number(
            child_a,
            "paired_segment_rotation_cost_restoration_rotation_count"
        ),
        3
    );
    assert_eq!(
        number(
            child_a,
            "paired_segment_rotation_cost_restoration_source_stream_count"
        ),
        2,
        "same checkpoint/fold/order coordinates remain distinct by source stream ordinal",
    );
    assert_eq!(child_a.estimated_rows(), Some(20));

    let middle = join_by_source(&baseline, "table:Middle");
    assert_eq!(
        text(
            middle,
            "paired_segment_rotation_cost_restoration_transition"
        ),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        text(
            middle,
            "paired_segment_rotation_cost_restoration_fold_identity"
        ),
        after_alpha
    );
    assert_eq!(middle.estimated_rows(), Some(8));

    let child_b = joins["pair:table:ChildB"];
    let after_beta = text(
        child_b,
        "paired_segment_rotation_cost_restoration_fold_identity",
    );
    assert_ne!(after_alpha, after_beta);
    assert_eq!(
        text(
            child_b,
            "paired_segment_rotation_cost_restoration_parent_identity"
        ),
        after_alpha
    );
    assert_eq!(
        number(
            child_b,
            "paired_segment_rotation_cost_restoration_chain_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_b,
            "paired_segment_rotation_cost_restoration_rotation_count"
        ),
        5
    );
    assert_eq!(
        number(
            child_b,
            "paired_segment_rotation_cost_restoration_nested_checkpoint_fold_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_b,
            "paired_segment_rotation_cost_restoration_branch_scope_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_b,
            "paired_segment_rotation_cost_restoration_source_stream_count"
        ),
        3
    );
    assert_eq!(child_b.estimated_rows(), Some(6));

    let unknown = joins["pair:table:Unknown"];
    assert_eq!(
        text(
            unknown,
            "paired_segment_rotation_cost_restoration_transition"
        ),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        text(
            unknown,
            "paired_segment_rotation_cost_restoration_fold_identity"
        ),
        after_beta
    );
    assert_eq!(unknown.estimated_rows(), None);

    let output = baseline.root();
    assert_eq!(
        text(
            output,
            "paired_segment_rotation_cost_restoration_output_fold_identity"
        ),
        after_beta
    );
    assert_eq!(
        number(
            output,
            "paired_segment_rotation_cost_restoration_output_rotation_count"
        ),
        5
    );
    assert!(matches!(
        output
            .details()
            .get("paired_segment_rotation_cost_restoration_output_overflowed"),
        Some(PlanDetail::Boolean(false))
    ));
    assert_eq!(baseline.plan().id(), reordered.plan().id());
    assert_ne!(
        text(
            changed_segment.root(),
            "paired_segment_rotation_cost_restoration_output_fold_identity"
        ),
        after_beta
    );
    assert_ne!(
        text(
            rebound_branch.root(),
            "paired_segment_rotation_cost_restoration_output_fold_identity"
        ),
        after_beta
    );

    let no_lineage = explain_query_with_partial_indexes_and_join_pair_identities(
        &query(4),
        &indexes(),
        &pairs(),
    )
    .expect("ordinary plan without lineage remains valid");
    assert_ne!(baseline.plan().id(), no_lineage.plan().id());
}

#[test]
fn segment_rotation_chains_reject_wrong_branch_pair_and_incomplete_pairs() {
    let checkpoints = checkpoint_chains(4);
    let mut wrong_branch = rotation_chains(4, "v1");
    wrong_branch[0].branch.generation = 44;
    assert_eq!(
        explain_query_with_partial_indexes_and_paired_checkpoint_compaction_and_segment_rotation_chains(
            &query(4),
            &indexes(),
            &pairs(),
            &checkpoints,
            &wrong_branch,
        ),
        Err(ExplainError::InvalidObject),
    );

    let mut wrong_pair = rotation_chains(4, "v1");
    wrong_pair[0].join_pair_identity = object("pair:table:Middle");
    assert_eq!(
        explain_query_with_partial_indexes_and_paired_checkpoint_compaction_and_segment_rotation_chains(
            &query(4),
            &indexes(),
            &pairs(),
            &checkpoints,
            &wrong_pair,
        ),
        Err(ExplainError::InvalidObject),
    );

    let mut invalid_segment = rotation_chains(4, "v1");
    invalid_segment[0].rotations[0].right_segment_identity = object("");
    assert_eq!(
        explain_query_with_partial_indexes_and_paired_checkpoint_compaction_and_segment_rotation_chains(
            &query(4),
            &indexes(),
            &pairs(),
            &checkpoints,
            &invalid_segment,
        ),
        Err(ExplainError::InvalidObject),
    );
}
