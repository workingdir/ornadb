use orna_sys_v1::{
    ExplainError, ExplainedPlan, ExpressionRef, MutableBranchSnapshot, ObjectRef, PlanDetail,
    PlanNode, PlanNodeKind, QueryCheckpointGenerationDescription, QueryJoinDescription,
    QueryJoinPairIdentityDescription, QueryPairedCheckpointSpillRestoreChainDescription,
    QueryPairedCheckpointSpillRestoreOccurrenceDescription,
    QueryPairedStreamRotationChainDescription, QueryPairedStreamRotationOccurrenceDescription,
    QueryPairedWalRotationChainDescription, QueryPairedWalRotationOccurrenceDescription,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_paired_checkpoint_wal_rotation_chains,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_wal_rotation_73xxo.orna");

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
        snapshot: SnapshotRef::descriptive("snapshot:paired-wal-rotation-73xxo"),
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

fn checkpoint_generation(
    generation: u64,
    position: Option<&[u8]>,
) -> QueryCheckpointGenerationDescription {
    QueryCheckpointGenerationDescription {
        generation,
        position: position.map(<[u8]>::to_vec),
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
                left_checkpoint_generation: Some(checkpoint_generation(generation, Some(&[0xa1]))),
                right_checkpoint_generation: None,
                redo_fold_identity: object(&format!("spill-redo:{branch_name}")),
            }],
        },
    )
    .collect()
}

fn stream_rotation_chains() -> Vec<QueryPairedStreamRotationChainDescription> {
    vec![QueryPairedStreamRotationChainDescription {
        join_pair_identity: object("pair:table:Alpha"),
        branch: branch("branch:alpha", 4),
        occurrences: vec![QueryPairedStreamRotationOccurrenceDescription {
            checkpoint_identity: object("checkpoint:alpha"),
            rotation_identity: object("stream-rotation:alpha"),
            source_stream_ordinal: 1,
            source_stream_identity: object("stream:alpha"),
            rotation_ordinal: 2,
            fold_ordinal: 3,
            order: 5,
            left_stream_identity: Some(object("stream:alpha:left")),
            right_stream_identity: Some(object("stream:alpha:right")),
        }],
    }]
}

fn wal_rotation(
    checkpoint: &str,
    restore: u64,
    handoff: u64,
    stream: u64,
    compaction: u64,
    merge: u64,
    fold: u64,
    order: u64,
    left_checkpoint: Option<QueryCheckpointGenerationDescription>,
    right_checkpoint: Option<QueryCheckpointGenerationDescription>,
    prefix: &str,
) -> QueryPairedWalRotationOccurrenceDescription {
    QueryPairedWalRotationOccurrenceDescription {
        checkpoint_identity: object(checkpoint),
        restore_ordinal: restore,
        handoff_ordinal: handoff,
        stream_ordinal: stream,
        compaction_ordinal: compaction,
        merge_ordinal: merge,
        fold_ordinal: fold,
        order,
        left_checkpoint_generation: left_checkpoint,
        right_checkpoint_generation: right_checkpoint,
        left_redo_fold_identity: object(&format!("redo:{prefix}:left")),
        right_redo_fold_identity: object(&format!("redo:{prefix}:right")),
        left_write_ahead_identity: object(&format!("wal:{prefix}:left")),
        right_write_ahead_identity: object(&format!("wal:{prefix}:right")),
        left_segment_identity: object(&format!("segment:{prefix}:left")),
        right_segment_identity: object(&format!("segment:{prefix}:right")),
    }
}

fn wal_rotation_chains() -> Vec<QueryPairedWalRotationChainDescription> {
    vec![
        QueryPairedWalRotationChainDescription {
            join_pair_identity: object("pair:table:Alpha"),
            branch: branch("branch:alpha", 4),
            occurrences: vec![
                wal_rotation(
                    "checkpoint:alpha",
                    0,
                    1,
                    2,
                    0,
                    3,
                    4,
                    5,
                    Some(checkpoint_generation(10, Some(&[0xa0]))),
                    None,
                    "alpha:0",
                ),
                wal_rotation(
                    "checkpoint:alpha",
                    0,
                    1,
                    2,
                    0,
                    3,
                    4,
                    6,
                    Some(checkpoint_generation(10, Some(&[]))),
                    Some(checkpoint_generation(10, None)),
                    "alpha:1",
                ),
                wal_rotation(
                    "checkpoint:alpha",
                    1,
                    4,
                    3,
                    1,
                    7,
                    4,
                    6,
                    None,
                    None,
                    "alpha:replay",
                ),
            ],
        },
        QueryPairedWalRotationChainDescription {
            join_pair_identity: object("pair:table:Beta"),
            branch: branch("branch:beta", 9),
            occurrences: vec![wal_rotation(
                "checkpoint:beta",
                0,
                2,
                0,
                0,
                1,
                8,
                6,
                Some(checkpoint_generation(12, None)),
                Some(checkpoint_generation(13, Some(&[0xb2]))),
                "beta:0",
            )],
        },
    ]
}

fn plan(
    wal_chains: &[QueryPairedWalRotationChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_with_partial_indexes_and_paired_checkpoint_wal_rotation_chains(
        &query(),
        &[],
        &pairs(),
        &[],
        &[],
        &[],
        &spill_chains(),
        &stream_rotation_chains(),
        wal_chains,
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
        .expect("join is emitted for its exact input pair")
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
        .expect("join is present for the right input")
}

#[test]
fn wal_rotation_fold_retains_nested_paired_restore_merge_lineage() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 3);

    let chains = wal_rotation_chains();
    let folded = plan(&chains).expect("scoped WAL rotation chains fold successfully");
    let baseline = plan(&[]).expect("stream-rotation ancestry remains valid without WAL folds");
    let mut changed_merge = chains.clone();
    changed_merge[0].occurrences[0].merge_ordinal += 1;
    let changed = plan(&changed_merge).expect("a distinct merge coordinate remains valid");

    let alpha = join_by_pair(&folded, "pair:table:Alpha");
    assert_eq!(alpha.estimated_rows(), Some(20));
    assert_eq!(alpha.estimated_bytes(), Some(48_960));
    assert_eq!(
        text(alpha, "paired_wal_rotation_cost_restoration_transition"),
        "append_paired_wal_rotation_chain"
    );
    assert_eq!(
        text(
            alpha,
            "paired_wal_rotation_cost_restoration_parent_identity"
        ),
        text(
            alpha,
            "paired_stream_rotation_cost_restoration_fold_identity"
        )
    );
    assert_eq!(
        text(
            alpha,
            "paired_wal_rotation_cost_restoration_stream_rotation_fold_identity"
        ),
        text(
            alpha,
            "paired_stream_rotation_cost_restoration_fold_identity"
        )
    );
    assert_eq!(
        number(alpha, "paired_wal_rotation_cost_restoration_chain_count"),
        1
    );
    assert_eq!(
        number(
            alpha,
            "paired_wal_rotation_cost_restoration_occurrence_count"
        ),
        3
    );
    assert_eq!(
        number(alpha, "paired_wal_rotation_cost_restoration_restore_count"),
        2
    );
    assert_eq!(
        number(alpha, "paired_wal_rotation_cost_restoration_handoff_count"),
        2
    );
    assert_eq!(
        number(alpha, "paired_wal_rotation_cost_restoration_stream_count"),
        2
    );
    assert_eq!(
        number(
            alpha,
            "paired_wal_rotation_cost_restoration_compaction_count"
        ),
        2
    );
    assert_eq!(
        number(alpha, "paired_wal_rotation_cost_restoration_merge_count"),
        2
    );
    assert_eq!(
        number(
            alpha,
            "paired_wal_rotation_cost_restoration_checkpoint_side_count"
        ),
        3
    );
    assert_eq!(
        number(
            alpha,
            "paired_wal_rotation_cost_restoration_absent_checkpoint_side_count"
        ),
        3
    );
    assert_eq!(
        number(
            alpha,
            "paired_wal_rotation_cost_restoration_both_checkpoint_sides_absent_count"
        ),
        1
    );
    assert_eq!(
        number(
            alpha,
            "paired_wal_rotation_cost_restoration_directional_wal_identity_count"
        ),
        6
    );
    assert_eq!(
        number(
            alpha,
            "paired_wal_rotation_cost_restoration_segment_identity_count"
        ),
        6
    );
    assert_eq!(
        number(
            alpha,
            "paired_wal_rotation_cost_restoration_nested_stream_rotation_fold_count"
        ),
        1
    );

    let beta = join_by_pair(&folded, "pair:table:Beta");
    let alpha_fold = text(alpha, "paired_wal_rotation_cost_restoration_fold_identity");
    assert_eq!(
        text(beta, "paired_wal_rotation_cost_restoration_parent_identity"),
        alpha_fold
    );
    assert_eq!(
        number(beta, "paired_wal_rotation_cost_restoration_chain_count"),
        2
    );
    assert_eq!(
        number(
            beta,
            "paired_wal_rotation_cost_restoration_occurrence_count"
        ),
        4
    );
    assert_eq!(
        number(beta, "paired_wal_rotation_cost_restoration_restore_count"),
        3
    );
    assert_eq!(
        number(beta, "paired_wal_rotation_cost_restoration_handoff_count"),
        3
    );
    assert_eq!(
        number(beta, "paired_wal_rotation_cost_restoration_stream_count"),
        3
    );
    assert_eq!(
        number(
            beta,
            "paired_wal_rotation_cost_restoration_compaction_count"
        ),
        3
    );
    assert_eq!(
        number(beta, "paired_wal_rotation_cost_restoration_merge_count"),
        3
    );
    assert_eq!(
        number(
            beta,
            "paired_wal_rotation_cost_restoration_checkpoint_side_count"
        ),
        5
    );
    assert_eq!(
        number(
            beta,
            "paired_wal_rotation_cost_restoration_absent_checkpoint_side_count"
        ),
        3
    );
    assert_eq!(
        number(
            beta,
            "paired_wal_rotation_cost_restoration_directional_wal_identity_count"
        ),
        8
    );
    assert_eq!(
        number(
            beta,
            "paired_wal_rotation_cost_restoration_segment_identity_count"
        ),
        8
    );
    assert_eq!(
        number(
            beta,
            "paired_wal_rotation_cost_restoration_nested_stream_rotation_fold_count"
        ),
        2
    );
    assert_eq!(
        number(
            beta,
            "paired_wal_rotation_cost_restoration_branch_scope_count"
        ),
        2
    );
    assert_eq!(beta.estimated_rows(), Some(10));

    let unknown = join_by_source(&folded, "table:Unknown");
    assert_eq!(
        text(unknown, "paired_wal_rotation_cost_restoration_transition"),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        text(
            unknown,
            "paired_wal_rotation_cost_restoration_fold_identity"
        ),
        text(beta, "paired_wal_rotation_cost_restoration_fold_identity")
    );
    assert_eq!(unknown.estimated_rows(), None);
    assert_eq!(
        text(
            folded.root(),
            "paired_wal_rotation_cost_restoration_output_fold_identity"
        ),
        text(beta, "paired_wal_rotation_cost_restoration_fold_identity")
    );

    for source in ["table:Alpha", "table:Beta", "table:Unknown"] {
        let with_wal = join_by_source(&folded, source);
        let without_wal = join_by_source(&baseline, source);
        assert_eq!(with_wal.estimated_rows(), without_wal.estimated_rows());
        assert_eq!(with_wal.estimated_bytes(), without_wal.estimated_bytes());
        assert_eq!(with_wal.estimated_work(), without_wal.estimated_work());
    }
    assert_ne!(
        text(
            folded.root(),
            "paired_wal_rotation_cost_restoration_output_fold_identity"
        ),
        text(
            changed.root(),
            "paired_wal_rotation_cost_restoration_output_fold_identity"
        )
    );

    let mut invalid_branch = chains;
    invalid_branch[0].branch.generation += 1;
    assert!(matches!(
        plan(&invalid_branch),
        Err(ExplainError::InvalidObject)
    ));
}
