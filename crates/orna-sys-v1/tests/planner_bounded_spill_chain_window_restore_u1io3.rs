use orna_sys_v1::{
    ExpressionRef, MutableBranchSnapshot, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    PlanWindowFrameBound, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryPairedCheckpointSegmentCompactionChainDescription,
    QueryPairedCheckpointSegmentCompactionStepDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, QueryWindowSpillDescription,
    SnapshotRef, explain_query_with_paired_cost_restoration_and_window_spill_pushdowns,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_bounded_spill_chain_window_restore_u1io3.orna");

fn object(value: &str) -> ObjectRef {
    ObjectRef::descriptive(value)
}

fn expression(value: &str) -> ExpressionRef {
    ExpressionRef::descriptive(value)
}

fn pair(source: &str) -> QueryJoinPairIdentityDescription {
    QueryJoinPairIdentityDescription {
        identity: object(&format!("pair:{source}")),
        left_source: object("table:Anchor"),
        right_source: object(&format!("table:{source}")),
        predicate: Some(expression(&format!("expr:join-{source}"))),
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

fn restore_chain(
    checkpoint_identity: &str,
) -> QueryPairedCheckpointSegmentCompactionChainDescription {
    QueryPairedCheckpointSegmentCompactionChainDescription {
        join_pair_identity: object("pair:Restore"),
        branch: MutableBranchSnapshot {
            name: "branch:restore".to_owned(),
            generation: 12,
        },
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
    first_budget: u64,
    restore_budget: u64,
    unknown_restore: bool,
    restore_checkpoint_identity: &str,
    include_restore: bool,
    include_tail: bool,
    reverse_descriptors: bool,
    include_bounded_spills: bool,
) -> orna_sys_v1::ExplainedPlan {
    let mut joins = vec!["First", "Gap", "Restore", "Tail"]
        .into_iter()
        .map(|source| QueryJoinDescription {
            source: object(&format!("table:{source}")),
            statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(4),
                estimated_bytes: Some(4_000),
                mutable_branch: (source == "Restore").then(|| MutableBranchSnapshot {
                    name: "branch:restore".to_owned(),
                    generation: 12,
                }),
            }),
            predicate: Some(expression(&format!("expr:join-{source}"))),
        })
        .collect::<Vec<_>>();
    if !include_tail {
        joins.pop();
    }
    let query = QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:bounded-spill-chain-window-restore-u1io3"),
        source: object("table:Anchor"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: Some(100),
            estimated_bytes: Some(40_000),
            mutable_branch: None,
        }),
        joins,
        predicate: None,
        projections: vec![expression("expr:window-output")],
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    };
    let mut pairs = vec![pair("First"), pair("Restore")];
    if include_tail {
        pairs.push(pair("Tail"));
    }
    let aggregates = vec![
        aggregate(
            "window:first-bounded",
            "table:First",
            PlanWindowFrameBound::Preceding(2),
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:restore-bounded",
            "table:Restore",
            PlanWindowFrameBound::Preceding(4),
            PlanWindowFrameBound::Following(1),
        ),
        aggregate(
            "window:restore-unbounded",
            "table:Restore",
            PlanWindowFrameBound::UnboundedPreceding,
            PlanWindowFrameBound::CurrentRow,
        ),
    ];
    let mut spills = vec![
        spill(
            "spill:first-bounded",
            "pair:First",
            "table:First",
            "window:first-bounded",
            Some(9_000),
            first_budget,
        ),
        spill(
            "spill:restore-bounded",
            "pair:Restore",
            "table:Restore",
            "window:restore-bounded",
            (!unknown_restore).then_some(17_000),
            restore_budget,
        ),
        spill(
            "spill:restore-unbounded",
            "pair:Restore",
            "table:Restore",
            "window:restore-unbounded",
            Some(80_000),
            1_000,
        ),
    ];
    let mut restore_chains = if include_restore {
        vec![restore_chain(restore_checkpoint_identity)]
    } else {
        Vec::new()
    };
    if !include_bounded_spills {
        spills.retain(|spill| spill.identity.as_str() == "spill:restore-unbounded");
    }
    if reverse_descriptors {
        pairs.reverse();
        spills.reverse();
        restore_chains.reverse();
    }

    explain_query_with_paired_cost_restoration_and_window_spill_pushdowns(
        &query,
        &pairs,
        &aggregates,
        &spills,
        &restore_chains,
        &[],
        &[],
        &[],
        &[],
        &[],
    )
    .expect("paired chain window spills and restore chains explain together")
}

fn project(plan: &orna_sys_v1::ExplainedPlan) -> &PlanNode {
    plan.nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Project)
        .expect("the output expression creates a Project node")
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

#[test]
fn bounded_spill_chain_window_identity_binds_paired_restore_chains() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = plan(
        5_000,
        8_000,
        false,
        "checkpoint:restore-v1",
        true,
        true,
        false,
        true,
    );
    let no_tail = plan(
        5_000,
        8_000,
        false,
        "checkpoint:restore-v1",
        true,
        false,
        false,
        true,
    );
    let reordered = plan(
        5_000,
        8_000,
        false,
        "checkpoint:restore-v1",
        true,
        true,
        true,
        true,
    );
    let changed_spill = plan(
        5_001,
        8_000,
        false,
        "checkpoint:restore-v1",
        true,
        true,
        false,
        true,
    );
    let changed_restore = plan(
        5_000,
        8_000,
        false,
        "checkpoint:restore-v2",
        true,
        true,
        false,
        true,
    );

    let baseline = project(&baseline);
    let identity_key = "paired_bounded_window_spill_chain_window_restore_chain_fold_identity";
    let chain_window_key =
        "paired_bounded_window_spill_chain_window_restore_chain_window_fold_identity";
    let chain_key = "paired_bounded_window_spill_chain_window_restore_chain_chain_fold_identity";
    let spill_key =
        "paired_bounded_window_spill_chain_window_restore_chain_spill_window_fold_identity";
    let restore_key =
        "paired_bounded_window_spill_chain_window_restore_chain_restore_chain_fold_identity";
    let window_restore_key =
        "paired_bounded_window_spill_chain_window_restore_chain_window_restore_chain_fold_identity";

    assert!(
        text(baseline, identity_key)
            .starts_with("paired-bounded-window-spill-chain-window-restore-chain:")
    );
    assert_eq!(
        text(baseline, chain_window_key),
        text(
            baseline,
            "paired_bounded_window_spill_chain_fold_window_identity"
        )
    );
    assert!(text(baseline, chain_key).starts_with("paired-window-chain-fold:"));
    assert!(
        text(baseline, spill_key).starts_with("paired-bounded-window-spill-chain-window-fold:")
    );
    assert!(text(baseline, restore_key).starts_with("paired-window-restore-chain-descriptor-fold:"));
    assert!(text(baseline, window_restore_key).starts_with("paired-window-restore-chain-descriptor-window-fold:"));
    assert_eq!(
        integer(
            baseline,
            "paired_bounded_window_spill_chain_window_restore_chain_spill_window_count"
        ),
        2
    );
    assert_eq!(
        integer(
            baseline,
            "paired_bounded_window_spill_chain_window_restore_chain_spill_stage_count"
        ),
        2,
        "the unbounded restore window does not enter bounded spill history"
    );
    assert_eq!(
        integer(
            baseline,
            "paired_bounded_window_spill_chain_window_restore_chain_restore_fold_count"
        ),
        1
    );
    assert_eq!(
        integer(
            baseline,
            "paired_bounded_window_spill_chain_window_restore_chain_estimated_bytes"
        ),
        13_000
    );

    assert_eq!(
        text(baseline, identity_key),
        text(project(&no_tail), identity_key),
        "a sparse tail carries the exact spill and restore-chain components"
    );
    assert_eq!(
        text(baseline, identity_key),
        text(project(&reordered), identity_key),
        "pair, spill, and restore descriptor order does not change the identity"
    );

    let changed_spill = project(&changed_spill);
    assert_ne!(
        text(baseline, identity_key),
        text(changed_spill, identity_key)
    );
    assert_ne!(text(baseline, spill_key), text(changed_spill, spill_key));
    assert_eq!(
        text(baseline, restore_key),
        text(changed_spill, restore_key)
    );
    assert_eq!(
        text(baseline, window_restore_key),
        text(changed_spill, window_restore_key)
    );

    let changed_restore = project(&changed_restore);
    assert_ne!(
        text(baseline, identity_key),
        text(changed_restore, identity_key)
    );
    assert_eq!(
        text(baseline, chain_window_key),
        text(changed_restore, chain_window_key)
    );
    assert_eq!(text(baseline, spill_key), text(changed_restore, spill_key));
    assert_ne!(
        text(baseline, restore_key),
        text(changed_restore, restore_key)
    );
    assert_ne!(
        text(baseline, window_restore_key),
        text(changed_restore, window_restore_key)
    );
}

#[test]
fn bounded_spill_chain_restore_fold_preserves_unknowns_and_requires_restore_chains() {
    let unknown_plan = plan(
        5_000,
        8_000,
        true,
        "checkpoint:restore-v1",
        true,
        true,
        false,
        true,
    );
    let unknown = project(&unknown_plan);
    assert_eq!(
        integer(
            unknown,
            "paired_bounded_window_spill_chain_window_restore_chain_unknown_working_set_count"
        ),
        1
    );
    assert_eq!(
        text(
            unknown,
            "paired_bounded_window_spill_chain_window_restore_chain_estimate_status"
        ),
        "unknown_working_set"
    );
    assert!(
        !unknown
            .details()
            .contains_key("paired_bounded_window_spill_chain_window_restore_chain_estimated_bytes")
    );

    let no_restore = plan(
        5_000,
        8_000,
        false,
        "checkpoint:restore-v1",
        false,
        true,
        false,
        true,
    );
    assert!(
        !project(&no_restore)
            .details()
            .contains_key("paired_bounded_window_spill_chain_window_restore_chain_fold_identity")
    );

    let unbounded_only = plan(
        5_000,
        8_000,
        false,
        "checkpoint:restore-v1",
        true,
        true,
        false,
        false,
    );
    assert!(
        !project(&unbounded_only)
            .details()
            .contains_key("paired_bounded_window_spill_chain_window_restore_chain_fold_identity")
    );
}
