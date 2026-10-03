use std::collections::BTreeMap;

use orna_sys_v1::{
    ExplainError, ExpressionRef, MutableBranchSnapshot, ObjectRef, PlanDetail, PlanNode,
    PlanNodeKind, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryPairedCheckpointSegmentCompactionChainDescription,
    QueryPairedCheckpointSegmentCompactionStepDescription, QueryPartialIndexDescription,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_paired_checkpoint_segment_compaction_chains,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_paired_checkpoint_cost_restoration_4u2hl.orna");

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
        snapshot: SnapshotRef::descriptive("snapshot:paired-checkpoint-cost-restoration-4u2hl"),
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

fn step(
    checkpoint: &str,
    left_segment: Option<&str>,
    right_segment: Option<&str>,
    left_compaction: Option<&str>,
    right_compaction: Option<&str>,
) -> QueryPairedCheckpointSegmentCompactionStepDescription {
    QueryPairedCheckpointSegmentCompactionStepDescription {
        checkpoint_identity: object(checkpoint),
        left_segment_identity: left_segment.map(object),
        right_segment_identity: right_segment.map(object),
        left_compaction_identity: left_compaction.map(object),
        right_compaction_identity: right_compaction.map(object),
    }
}

fn chains(
    suffix: &str,
    generation_a: u64,
) -> Vec<QueryPairedCheckpointSegmentCompactionChainDescription> {
    vec![
        QueryPairedCheckpointSegmentCompactionChainDescription {
            join_pair_identity: object("pair:table:ChildA"),
            branch: branch("branch:alpha", generation_a),
            steps: vec![
                step(
                    "checkpoint:a-1",
                    Some("segment:a-left-1"),
                    Some("segment:a-right-1"),
                    Some("compact:a-left-1"),
                    Some("compact:a-right-1"),
                ),
                step(
                    "checkpoint:a-2",
                    Some("segment:a-left-2"),
                    None,
                    Some(&format!("compact:a-left-2-{suffix}")),
                    None,
                ),
            ],
        },
        QueryPairedCheckpointSegmentCompactionChainDescription {
            join_pair_identity: object("pair:table:ChildB"),
            branch: branch("branch:beta", 8),
            steps: vec![
                step(
                    "checkpoint:b-1",
                    None,
                    Some("segment:b-right-1"),
                    None,
                    Some("compact:b-right-1"),
                ),
                step(
                    "checkpoint:b-2",
                    Some("segment:b-left-2"),
                    Some("segment:b-right-2"),
                    Some("compact:b-left-2"),
                    Some("compact:b-right-2"),
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
    chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    pairs: &[QueryJoinPairIdentityDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_paired_checkpoint_segment_compaction_chains(
        &query(generation_a),
        &indexes(),
        pairs,
        chains,
    )
    .expect("checkpoint compaction lineage folds into a valid query plan")
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

fn access_by_object<'a>(plan: &'a orna_sys_v1::ExplainedPlan, object: &str) -> &'a PlanNode {
    plan.nodes()
        .iter()
        .find(|node| {
            matches!(node.kind(), PlanNodeKind::IndexLookup | PlanNodeKind::Scan)
                && node.object().map(ObjectRef::as_str) == Some(object)
        })
        .expect("planned access preserves its selected object")
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
fn paired_checkpoint_compaction_folds_branch_local_costs_across_sparse_joins() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 3);

    let pair_descriptions = pairs();
    let chain_descriptions = chains("v1", 4);
    let baseline = plan(4, &chain_descriptions, &pair_descriptions);
    let mut reordered_chains = chain_descriptions.clone();
    reordered_chains.reverse();
    let reordered = plan(4, &reordered_chains, &pair_descriptions);
    let changed_compaction = plan(4, &chains("v2", 4), &pair_descriptions);
    let rebound_branch = plan(5, &chains("v1", 5), &pair_descriptions);
    let pairs_by_id = joins_by_pair(&baseline);

    let child_a_access = access_by_object(&baseline, "index:table:ChildA");
    assert_eq!(child_a_access.kind(), PlanNodeKind::IndexLookup);
    assert_eq!(child_a_access.estimated_rows(), Some(2));
    assert_eq!(child_a_access.estimated_bytes(), Some(4_096));
    assert_eq!(child_a_access.estimated_work(), Some(3));
    let child_b_access = access_by_object(&baseline, "index:table:ChildB");
    assert_eq!(child_b_access.estimated_rows(), Some(7));
    assert_eq!(child_b_access.estimated_bytes(), Some(8_192));
    assert_eq!(child_b_access.estimated_work(), Some(9));

    let child_a = pairs_by_id["pair:table:ChildA"];
    let fold_after_a = text(child_a, "paired_checkpoint_cost_restoration_fold_identity");
    assert_eq!(
        text(child_a, "paired_checkpoint_cost_restoration_transition"),
        "append_branch_checkpoint_segment_chain"
    );
    assert_eq!(
        text(
            child_a,
            "paired_checkpoint_cost_restoration_branch_identity"
        ),
        "branch:alpha"
    );
    assert_eq!(
        number(
            child_a,
            "paired_checkpoint_cost_restoration_branch_generation"
        ),
        4
    );
    assert_eq!(
        number(child_a, "paired_checkpoint_cost_restoration_chain_count"),
        1
    );
    assert_eq!(
        number(
            child_a,
            "paired_checkpoint_cost_restoration_checkpoint_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_a,
            "paired_checkpoint_cost_restoration_left_segment_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_a,
            "paired_checkpoint_cost_restoration_right_segment_count"
        ),
        1
    );

    let middle = join_by_source(&baseline, "table:Middle");
    assert_eq!(
        text(middle, "paired_checkpoint_cost_restoration_transition"),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        text(middle, "paired_checkpoint_cost_restoration_fold_identity"),
        fold_after_a
    );
    assert_eq!(middle.estimated_rows(), Some(8));

    let child_b = pairs_by_id["pair:table:ChildB"];
    let fold_after_b = text(child_b, "paired_checkpoint_cost_restoration_fold_identity");
    assert_ne!(
        fold_after_a, fold_after_b,
        "the second branch-local chain advances the nested fold"
    );
    assert_eq!(
        number(child_b, "paired_checkpoint_cost_restoration_chain_count"),
        2
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_cost_restoration_branch_scope_count"
        ),
        2
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_cost_restoration_checkpoint_count"
        ),
        4
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_cost_restoration_left_segment_count"
        ),
        3
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_cost_restoration_right_segment_count"
        ),
        3
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_cost_restoration_left_compaction_count"
        ),
        3
    );
    assert_eq!(
        number(
            child_b,
            "paired_checkpoint_cost_restoration_right_compaction_count"
        ),
        3
    );

    let unknown = pairs_by_id["pair:table:Unknown"];
    assert_eq!(
        text(unknown, "paired_checkpoint_cost_restoration_transition"),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        text(unknown, "paired_checkpoint_cost_restoration_fold_identity"),
        fold_after_b
    );
    assert_eq!(unknown.estimated_rows(), None);
    assert_eq!(
        access_by_object(&baseline, "table:Unknown").estimated_work(),
        None
    );

    let output = baseline.root();
    assert_eq!(output.kind(), PlanNodeKind::Limit);
    assert_eq!(
        text(
            output,
            "paired_checkpoint_cost_restoration_output_fold_identity"
        ),
        fold_after_b
    );
    assert_eq!(
        number(
            output,
            "paired_checkpoint_cost_restoration_output_chain_count"
        ),
        2
    );
    assert_eq!(
        number(
            output,
            "paired_checkpoint_cost_restoration_output_branch_scope_count"
        ),
        2
    );
    assert_eq!(
        number(
            output,
            "paired_checkpoint_cost_restoration_output_checkpoint_count"
        ),
        4
    );
    assert!(matches!(
        output
            .details()
            .get("paired_checkpoint_cost_restoration_output_overflowed"),
        Some(PlanDetail::Boolean(false))
    ));
    assert_eq!(
        baseline.plan().id(),
        reordered.plan().id(),
        "descriptor declaration order does not affect the resolved plan"
    );
    assert_ne!(
        text(
            changed_compaction.root(),
            "paired_checkpoint_cost_restoration_output_fold_identity"
        ),
        fold_after_b
    );
    assert_ne!(
        text(
            rebound_branch.root(),
            "paired_checkpoint_cost_restoration_output_fold_identity"
        ),
        fold_after_b
    );
    assert_eq!(
        text(
            rebound_branch.root(),
            "paired_checkpoint_cost_restoration_output_policy"
        ),
        "branch_local_cost_fold_with_ordered_sparse_checkpoint_segments"
    );
}

#[test]
fn checkpoint_compaction_chains_cannot_cross_branch_or_pair_boundaries() {
    let pair_descriptions = pairs();
    let mut wrong_branch = chains("v1", 4);
    wrong_branch[0].branch.generation = 99;
    assert_eq!(
        explain_query_with_partial_indexes_and_paired_checkpoint_segment_compaction_chains(
            &query(4),
            &indexes(),
            &pair_descriptions,
            &wrong_branch,
        ),
        Err(ExplainError::InvalidObject),
        "a chain must belong to the mutable branch generation on its exact right input",
    );

    let mut malformed_step = chains("v1", 4);
    malformed_step[0].steps[1].left_compaction_identity = None;
    assert_eq!(
        explain_query_with_partial_indexes_and_paired_checkpoint_segment_compaction_chains(
            &query(4),
            &indexes(),
            &pair_descriptions,
            &malformed_step,
        ),
        Err(ExplainError::InvalidObject),
        "a segment slot cannot lose its compaction identity",
    );

    let mut wrong_pair = chains("v1", 4);
    wrong_pair[0].join_pair_identity = object("pair:table:ChildB");
    assert_eq!(
        explain_query_with_partial_indexes_and_paired_checkpoint_segment_compaction_chains(
            &query(4),
            &indexes(),
            &pair_descriptions,
            &wrong_pair,
        ),
        Err(ExplainError::InvalidObject),
        "the chain identity resolves by pair and branch, never by nearby planned position",
    );
}
