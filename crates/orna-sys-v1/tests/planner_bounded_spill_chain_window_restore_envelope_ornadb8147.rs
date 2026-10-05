use orna_sys_v1::{
    ExpressionRef, MutableBranchSnapshot, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    PlanWindowFrameBound, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryPairedCheckpointSegmentCompactionChainDescription,
    QueryPairedCheckpointSegmentCompactionStepDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, QueryWindowSpillDescription,
    SnapshotRef, explain_query_with_paired_cost_restoration_and_window_spill_pushdowns,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_bounded_spill_chain_window_restore_envelope_ornadb8147.orna");

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
    pair_identity: &str,
    branch_name: &str,
    generation: u64,
) -> QueryPairedCheckpointSegmentCompactionChainDescription {
    QueryPairedCheckpointSegmentCompactionChainDescription {
        join_pair_identity: object(pair_identity),
        branch: MutableBranchSnapshot {
            name: branch_name.to_owned(),
            generation,
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
                mutable_branch: matches!(source, "First" | "Restore").then(|| {
                    MutableBranchSnapshot {
                        name: if source == "First" {
                            "branch:first".to_owned()
                        } else {
                            "branch:restore".to_owned()
                        },
                        generation: if source == "First" { 5 } else { 12 },
                    }
                }),
            }),
            predicate: Some(expression(&format!("expr:join-{source}"))),
        })
        .collect::<Vec<_>>();
    if !include_tail {
        joins.pop();
    }
    let query = QueryPlanDescription {
        snapshot: SnapshotRef::descriptive(
            "snapshot:bounded-spill-chain-window-restore-envelope-ornadb8147",
        ),
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
        vec![
            restore_chain(
                restore_checkpoint_identity,
                "pair:Restore",
                "branch:restore",
                12,
            ),
            restore_chain(
                "checkpoint:restore-first-v1",
                "pair:First",
                "branch:first",
                5,
            ),
        ]
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
fn bounded_spill_chain_window_restore_envelope_identity_tracks_paired_components() {
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
    let envelope_identity_key =
        "paired_bounded_window_spill_chain_window_restore_envelope_fold_identity";
    let restore_envelope_key =
        "paired_bounded_window_spill_chain_window_restore_envelope_restore_envelope_fold_identity";

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
    assert!(
        text(baseline, restore_key).starts_with("paired-window-restore-chain-descriptor-fold:")
    );
    assert!(
        text(baseline, window_restore_key)
            .starts_with("paired-window-restore-chain-descriptor-window-fold:")
    );
    assert!(
        text(baseline, envelope_identity_key)
            .starts_with("paired-bounded-window-spill-chain-window-restore-envelope:")
    );
    assert!(
        text(baseline, restore_envelope_key).starts_with("paired-window-restore-envelope-fold:")
    );
    assert_eq!(
        integer(
            baseline,
            "paired_bounded_window_spill_chain_window_restore_envelope_pair_count"
        ),
        2,
        "only pairs with both a window identity and direct restore descriptors enter the envelope"
    );
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
    assert_eq!(
        text(baseline, restore_envelope_key),
        text(project(&reordered), restore_envelope_key),
        "restore descriptors fold in canonical pair order regardless of input order"
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
    assert_eq!(
        text(baseline, restore_envelope_key),
        text(changed_spill, restore_envelope_key),
        "spill settings do not enter the direct per-pair restore envelope"
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
    assert_ne!(
        text(baseline, restore_envelope_key),
        text(changed_restore, restore_envelope_key),
        "changing a paired restore descriptor changes its exact envelope"
    );
}

#[test]
fn bounded_spill_chain_restore_envelope_preserves_unknowns_and_requires_restore_chains() {
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
    assert_eq!(
        text(
            unknown,
            "paired_bounded_window_spill_chain_window_restore_envelope_estimate_status"
        ),
        "unknown_working_set"
    );
    assert!(
        !unknown.details().contains_key(
            "paired_bounded_window_spill_chain_window_restore_envelope_estimated_bytes"
        )
    );
    assert!(
        unknown.details().contains_key(
            "paired_bounded_window_spill_chain_window_restore_envelope_carry_identity"
        )
    );
    assert!(
        integer(
            unknown,
            "paired_bounded_window_spill_chain_window_restore_envelope_carry_state_count"
        ) > 0
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
    assert!(
        !project(&no_restore).details().contains_key(
            "paired_bounded_window_spill_chain_window_restore_envelope_fold_identity"
        )
    );
    assert!(
        !project(&no_restore).details().contains_key(
            "paired_bounded_window_spill_chain_window_restore_envelope_carry_identity"
        )
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
    assert!(
        !project(&unbounded_only).details().contains_key(
            "paired_bounded_window_spill_chain_window_restore_envelope_fold_identity"
        )
    );
    assert!(
        !project(&unbounded_only).details().contains_key(
            "paired_bounded_window_spill_chain_window_restore_envelope_carry_identity"
        )
    );
}

#[test]
fn bounded_spill_restore_envelope_carry_binds_distinct_ancestry_states_8192() {
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
    let carry_identity_key =
        "paired_bounded_window_spill_chain_window_restore_envelope_carry_identity";
    let carry_source_key =
        "paired_bounded_window_spill_chain_window_restore_envelope_carry_source_identity";
    let carry_state_count_key =
        "paired_bounded_window_spill_chain_window_restore_envelope_carry_state_count";
    let direct_restore_key =
        "paired_bounded_window_spill_chain_window_restore_envelope_restore_envelope_fold_identity";
    let spill_window_key =
        "paired_bounded_window_spill_chain_window_restore_envelope_spill_window_fold_identity";
    let bounded_fold_key =
        "paired_bounded_window_spill_chain_window_restore_envelope_fold_identity";

    assert!(
        text(baseline, carry_identity_key)
            .starts_with("paired-bounded-window-spill-chain-window-restore-envelope-carry:")
    );
    assert!(
        text(baseline, carry_source_key)
            .starts_with("paired-window-cost-restoration-cost-window-restore-envelope-carry-fold:")
    );
    assert_eq!(
        text(
            baseline,
            "paired_bounded_window_spill_chain_window_restore_envelope_carry_policy"
        ),
        "bind_bounded_spill_state_to_distinct_cost_restore_envelope_states"
    );

    let no_tail = project(&no_tail);
    assert_eq!(
        text(baseline, bounded_fold_key),
        text(no_tail, bounded_fold_key),
        "family 62's exact spill and direct restore components remain stable across a sparse tail"
    );
    assert_eq!(
        text(baseline, direct_restore_key),
        text(no_tail, direct_restore_key)
    );
    assert_ne!(
        text(baseline, carry_identity_key),
        text(no_tail, carry_identity_key),
        "the new carry identity binds the sparse join's distinct cost/restore state"
    );
    assert_eq!(
        integer(baseline, carry_state_count_key),
        integer(no_tail, carry_state_count_key) + 1
    );

    let reordered = project(&reordered);
    assert_eq!(
        text(baseline, carry_identity_key),
        text(reordered, carry_identity_key),
        "descriptor ordering does not rewrite the carried bounded identity"
    );
    assert_eq!(
        integer(baseline, carry_state_count_key),
        integer(reordered, carry_state_count_key)
    );

    let changed_spill = project(&changed_spill);
    assert_ne!(
        text(baseline, spill_window_key),
        text(changed_spill, spill_window_key)
    );
    assert_eq!(
        text(baseline, direct_restore_key),
        text(changed_spill, direct_restore_key),
        "the direct restore envelope excludes spill budgets"
    );
    assert_ne!(
        text(baseline, carry_identity_key),
        text(changed_spill, carry_identity_key)
    );
    assert_eq!(
        integer(baseline, carry_state_count_key),
        integer(changed_spill, carry_state_count_key)
    );

    let changed_restore = project(&changed_restore);
    assert_eq!(
        text(baseline, spill_window_key),
        text(changed_restore, spill_window_key)
    );
    assert_ne!(
        text(baseline, direct_restore_key),
        text(changed_restore, direct_restore_key)
    );
    assert_ne!(
        text(baseline, carry_identity_key),
        text(changed_restore, carry_identity_key)
    );
    assert_eq!(
        integer(baseline, carry_state_count_key),
        integer(changed_restore, carry_state_count_key)
    );
}
