use orna_sys_v1::{
    ExplainError, ExplainedPlan, ExpressionRef, MutableBranchSnapshot, ObjectRef, PlanDetail,
    PlanNode, PlanNodeKind, QueryCheckpointGenerationDescription, QueryJoinDescription,
    QueryJoinPairIdentityDescription, QueryPairedCheckpointSpillRestoreChainDescription,
    QueryPairedCheckpointSpillRestoreOccurrenceDescription,
    QueryPairedStreamRotationChainDescription, QueryPairedStreamRotationOccurrenceDescription,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_paired_checkpoint_stream_rotation_chains,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_stream_rotation_wjw60.orna");

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

fn query() -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-stream-rotation-wjw60"),
        source: object("table:Anchor"),
        source_statistics: Some(statistics(100, 40_000, None)),
        joins: vec![
            join(
                "table:Alpha",
                Some((2, 4_096, Some(branch("branch:alpha", 4)))),
            ),
            join(
                "table:Beta",
                Some((5, 8_192, Some(branch("branch:beta", 9)))),
            ),
            join("table:Unknown", None),
        ],
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    }
}

fn pairs() -> Vec<QueryJoinPairIdentityDescription> {
    ["table:Alpha", "table:Beta", "table:Unknown"]
        .into_iter()
        .map(|source| QueryJoinPairIdentityDescription {
            identity: object(&format!("pair:{source}")),
            left_source: object("table:Anchor"),
            right_source: object(source),
            predicate: Some(expression(&format!("expr:join-{source}"))),
        })
        .collect()
}

fn checkpoint_generation(generation: u64, position: &[u8]) -> QueryCheckpointGenerationDescription {
    QueryCheckpointGenerationDescription {
        generation,
        position: Some(position.to_vec()),
    }
}

fn spill_chains() -> Vec<QueryPairedCheckpointSpillRestoreChainDescription> {
    [
        ("table:Alpha", "branch:alpha", 4),
        ("table:Beta", "branch:beta", 9),
    ]
    .into_iter()
    .map(
        |(source, branch_name, generation)| QueryPairedCheckpointSpillRestoreChainDescription {
            join_pair_identity: object(&format!("pair:{source}")),
            branch: branch(branch_name, generation),
            occurrences: vec![QueryPairedCheckpointSpillRestoreOccurrenceDescription {
                checkpoint_identity: object(&format!("checkpoint:{branch_name}")),
                restore_ordinal: 0,
                spill_ordinal: 0,
                spill_identity: object(&format!("spill:{branch_name}")),
                source_stream_ordinal: 2,
                source_stream_identity: object(&format!("spill-stream:{branch_name}")),
                compaction_ordinal: 1,
                handoff_ordinal: 3,
                fold_ordinal: 4,
                order: 5,
                left_checkpoint_generation: Some(checkpoint_generation(generation, &[0xa1])),
                right_checkpoint_generation: None,
                redo_fold_identity: object(&format!("redo:{branch_name}")),
            }],
        },
    )
    .collect()
}

fn rotation(
    checkpoint: &str,
    identity: &str,
    source_stream_ordinal: u64,
    source_stream: &str,
    rotation_ordinal: u64,
    fold_ordinal: u64,
    order: u64,
    left: Option<&str>,
    right: Option<&str>,
) -> QueryPairedStreamRotationOccurrenceDescription {
    QueryPairedStreamRotationOccurrenceDescription {
        checkpoint_identity: object(checkpoint),
        rotation_identity: object(identity),
        source_stream_ordinal,
        source_stream_identity: object(source_stream),
        rotation_ordinal,
        fold_ordinal,
        order,
        left_stream_identity: left.map(object),
        right_stream_identity: right.map(object),
    }
}

fn rotation_chains() -> Vec<QueryPairedStreamRotationChainDescription> {
    vec![
        QueryPairedStreamRotationChainDescription {
            join_pair_identity: object("pair:table:Alpha"),
            branch: branch("branch:alpha", 4),
            occurrences: vec![
                rotation(
                    "checkpoint:alpha",
                    "rotation:alpha:0",
                    0,
                    "stream:alpha:0",
                    0,
                    1,
                    10,
                    Some("stream:alpha:left:0"),
                    Some("stream:alpha:right:0"),
                ),
                rotation(
                    "checkpoint:alpha",
                    "rotation:alpha:0",
                    0,
                    "stream:alpha:0",
                    0,
                    1,
                    11,
                    Some("stream:alpha:left:1"),
                    None,
                ),
                rotation(
                    "checkpoint:alpha",
                    "rotation:alpha:retry",
                    1,
                    "stream:alpha:replay",
                    1,
                    2,
                    12,
                    None,
                    None,
                ),
            ],
        },
        QueryPairedStreamRotationChainDescription {
            join_pair_identity: object("pair:table:Beta"),
            branch: branch("branch:beta", 9),
            occurrences: vec![rotation(
                "checkpoint:beta",
                "rotation:beta:0",
                0,
                "stream:beta:0",
                0,
                3,
                20,
                Some("stream:beta:left"),
                Some("stream:beta:right"),
            )],
        },
    ]
}

fn plan(
    chains: &[QueryPairedStreamRotationChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_with_partial_indexes_and_paired_checkpoint_stream_rotation_chains(
        &query(),
        &[],
        &pairs(),
        &[],
        &[],
        &[],
        &spill_chains(),
        chains,
    )
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

fn join_by_pair<'a>(plan: &'a ExplainedPlan, pair: &str) -> &'a PlanNode {
    plan.nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Join
                && node.details().get("join_pair_identity")
                    == Some(&PlanDetail::Text(pair.to_owned()))
        })
        .expect("join is emitted for the exact input pair")
}

fn join_by_source<'a>(plan: &'a ExplainedPlan, source: &str) -> &'a PlanNode {
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
        .expect("join is present for the selected right input")
}

#[test]
fn stream_rotation_fold_nests_after_spills_and_carries_sparse_occurrences() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 3);

    let chains = rotation_chains();
    let folded = plan(&chains).expect("well-scoped stream rotations are folded");
    let baseline = plan(&[]).expect("spill-only plan remains valid");
    let mut changed_coordinate = chains.clone();
    changed_coordinate[0].occurrences[0].order += 1;
    let changed = plan(&changed_coordinate).expect("changed occurrence still folds");

    let alpha = join_by_pair(&folded, "pair:table:Alpha");
    assert_eq!(alpha.estimated_rows(), Some(20));
    assert_eq!(alpha.estimated_bytes(), Some(48_960));
    assert_eq!(
        text(alpha, "paired_stream_rotation_cost_restoration_transition"),
        "append_paired_stream_rotation_chain"
    );
    assert_eq!(
        text(
            alpha,
            "paired_stream_rotation_cost_restoration_parent_identity"
        ),
        text(
            alpha,
            "paired_checkpoint_spill_cost_restoration_fold_identity"
        )
    );
    assert_eq!(
        text(
            alpha,
            "paired_stream_rotation_cost_restoration_spill_fold_identity"
        ),
        text(
            alpha,
            "paired_checkpoint_spill_cost_restoration_fold_identity"
        )
    );
    assert_eq!(
        number(alpha, "paired_stream_rotation_cost_restoration_chain_count"),
        1
    );
    assert_eq!(
        number(
            alpha,
            "paired_stream_rotation_cost_restoration_occurrence_count"
        ),
        3
    );
    assert_eq!(
        number(
            alpha,
            "paired_stream_rotation_cost_restoration_rotation_count"
        ),
        3
    );
    assert_eq!(
        number(
            alpha,
            "paired_stream_rotation_cost_restoration_source_stream_count"
        ),
        2
    );
    assert_eq!(
        number(
            alpha,
            "paired_stream_rotation_cost_restoration_present_stream_side_count"
        ),
        3
    );
    assert_eq!(
        number(
            alpha,
            "paired_stream_rotation_cost_restoration_absent_stream_side_count"
        ),
        3
    );
    assert_eq!(
        number(
            alpha,
            "paired_stream_rotation_cost_restoration_both_stream_sides_absent_occurrence_count"
        ),
        1
    );
    assert_eq!(
        number(
            alpha,
            "paired_stream_rotation_cost_restoration_nested_spill_fold_count"
        ),
        1
    );

    let beta = join_by_pair(&folded, "pair:table:Beta");
    let alpha_fold = text(
        alpha,
        "paired_stream_rotation_cost_restoration_fold_identity",
    );
    assert_eq!(
        text(beta, "paired_stream_rotation_cost_restoration_transition"),
        "append_paired_stream_rotation_chain"
    );
    assert_eq!(
        text(
            beta,
            "paired_stream_rotation_cost_restoration_parent_identity"
        ),
        alpha_fold
    );
    assert_eq!(
        number(beta, "paired_stream_rotation_cost_restoration_chain_count"),
        2
    );
    assert_eq!(
        number(
            beta,
            "paired_stream_rotation_cost_restoration_occurrence_count"
        ),
        4
    );
    assert_eq!(
        number(
            beta,
            "paired_stream_rotation_cost_restoration_source_stream_count"
        ),
        3
    );
    assert_eq!(
        number(
            beta,
            "paired_stream_rotation_cost_restoration_nested_spill_fold_count"
        ),
        2
    );
    assert_eq!(beta.estimated_rows(), Some(10));

    let unknown = join_by_source(&folded, "table:Unknown");
    assert_eq!(
        text(
            unknown,
            "paired_stream_rotation_cost_restoration_transition"
        ),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        text(
            unknown,
            "paired_stream_rotation_cost_restoration_fold_identity"
        ),
        text(
            beta,
            "paired_stream_rotation_cost_restoration_fold_identity"
        )
    );
    assert_eq!(unknown.estimated_rows(), None);
    assert_eq!(
        text(
            folded.root(),
            "paired_stream_rotation_cost_restoration_output_fold_identity"
        ),
        text(
            beta,
            "paired_stream_rotation_cost_restoration_fold_identity"
        )
    );

    for source in ["table:Alpha", "table:Beta", "table:Unknown"] {
        let with_rotation = join_by_source(&folded, source);
        let without_rotation = join_by_source(&baseline, source);
        assert_eq!(
            with_rotation.estimated_rows(),
            without_rotation.estimated_rows()
        );
        assert_eq!(
            with_rotation.estimated_bytes(),
            without_rotation.estimated_bytes()
        );
        assert_eq!(
            with_rotation.estimated_work(),
            without_rotation.estimated_work()
        );
    }
    assert_ne!(
        text(
            folded.root(),
            "paired_stream_rotation_cost_restoration_output_fold_identity"
        ),
        text(
            changed.root(),
            "paired_stream_rotation_cost_restoration_output_fold_identity"
        )
    );

    let mut invalid_branch = chains;
    invalid_branch[0].branch.generation += 1;
    assert!(matches!(
        plan(&invalid_branch),
        Err(ExplainError::InvalidObject)
    ));
}
