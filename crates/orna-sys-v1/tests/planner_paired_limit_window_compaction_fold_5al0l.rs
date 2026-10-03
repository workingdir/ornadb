use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryLimitPushdownDescription,
    QueryPlanDescription, QuerySourceStatistics, QueryWindowAggregatePushdownDescription,
    QueryWindowSpillDescription, SnapshotRef,
    explain_query_with_join_pair_identities_limit_window_aggregate_and_spill_pushdowns,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_paired_limit_window_compaction_fold_5al0l.orna");

fn object(reference: &str) -> ObjectRef {
    ObjectRef::descriptive(reference)
}

fn expression(reference: &str) -> ExpressionRef {
    ExpressionRef::descriptive(reference)
}

fn statistics(rows: u64, bytes: u64) -> QuerySourceStatistics {
    QuerySourceStatistics {
        estimated_rows: Some(rows),
        estimated_bytes: Some(bytes),
        mutable_branch: None,
    }
}

fn source_statistics(source: &str) -> Option<QuerySourceStatistics> {
    match source {
        "table:First" => Some(statistics(10, 4_096)),
        "table:Gap" => Some(statistics(20, 8_192)),
        "table:Compact" => Some(statistics(30, 12_288)),
        "table:Restore" => Some(statistics(40, 16_384)),
        "table:Unknown" => None,
        other => panic!("unexpected source {other}"),
    }
}

fn query(anchor: &str, order: &[&str]) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-limit-window-compaction-5al0l"),
        source: object(anchor),
        source_statistics: Some(statistics(50, 20_000)),
        joins: order
            .iter()
            .map(|source| QueryJoinDescription {
                source: object(source),
                statistics: source_statistics(source),
                predicate: Some(expression(&format!(
                    "expr:{}-{}",
                    anchor.trim_start_matches("table:"),
                    source.trim_start_matches("table:")
                ))),
            })
            .collect(),
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    }
}

fn pair(anchor: &str, source: &str) -> QueryJoinPairIdentityDescription {
    QueryJoinPairIdentityDescription {
        identity: object(&format!("pair:{}", source.trim_start_matches("table:"))),
        left_source: object(anchor),
        right_source: object(source),
        predicate: Some(expression(&format!(
            "expr:{}-{}",
            anchor.trim_start_matches("table:"),
            source.trim_start_matches("table:")
        ))),
    }
}

fn pairs(anchor: &str, order: &[&str]) -> Vec<QueryJoinPairIdentityDescription> {
    order.iter().map(|source| pair(anchor, source)).collect()
}

fn limit(source: &str, name: &str, value: u64) -> QueryLimitPushdownDescription {
    QueryLimitPushdownDescription {
        identity: object(&format!("limit:{name}")),
        join_pair_identity: object(&format!("pair:{}", source.trim_start_matches("table:"))),
        source: object(source),
        limit: value,
    }
}

fn limits() -> Vec<QueryLimitPushdownDescription> {
    vec![
        limit("table:First", "first-head", 9),
        limit("table:First", "first-tail", 7),
        limit("table:Restore", "restore", 19),
        limit("table:Unknown", "unknown", 11),
    ]
}

fn aggregate(
    identity: &str,
    source: &str,
    frame: &str,
    start: PlanWindowFrameBound,
) -> QueryWindowAggregatePushdownDescription {
    QueryWindowAggregatePushdownDescription {
        identity: object(identity),
        source: object(source),
        aggregate: expression(&format!("expr:sum-{identity}")),
        frame_identity: expression(frame),
        frame_start: start,
        frame_end: PlanWindowFrameBound::CurrentRow,
    }
}

fn aggregates() -> Vec<QueryWindowAggregatePushdownDescription> {
    vec![
        aggregate(
            "window:first-running",
            "table:First",
            "frame:first-running",
            PlanWindowFrameBound::UnboundedPreceding,
        ),
        aggregate(
            "window:first-recent",
            "table:First",
            "frame:first-recent",
            PlanWindowFrameBound::Preceding(2),
        ),
        aggregate(
            "window:compact-running",
            "table:Compact",
            "frame:compact-running",
            PlanWindowFrameBound::UnboundedPreceding,
        ),
        aggregate(
            "window:restore-running",
            "table:Restore",
            "frame:restore-running",
            PlanWindowFrameBound::UnboundedPreceding,
        ),
        aggregate(
            "window:restore-recent",
            "table:Restore",
            "frame:restore-recent",
            PlanWindowFrameBound::Preceding(4),
        ),
        aggregate(
            "window:unknown-running",
            "table:Unknown",
            "frame:unknown-running",
            PlanWindowFrameBound::UnboundedPreceding,
        ),
    ]
}

fn spill(
    name: &str,
    source: &str,
    aggregate_identity: &str,
    working_set: Option<u64>,
    budget: u64,
) -> QueryWindowSpillDescription {
    QueryWindowSpillDescription {
        identity: object(&format!("spill:{name}")),
        join_pair_identity: object(&format!("pair:{}", source.trim_start_matches("table:"))),
        source: object(source),
        window_aggregate_identity: object(aggregate_identity),
        estimated_working_set_bytes: working_set,
        memory_budget_bytes: budget,
    }
}

fn spills() -> Vec<QueryWindowSpillDescription> {
    vec![
        spill(
            "first-running",
            "table:First",
            "window:first-running",
            Some(9_000),
            5_000,
        ),
        spill(
            "compact-running",
            "table:Compact",
            "window:compact-running",
            Some(8_193),
            8_192,
        ),
        spill(
            "restore-running",
            "table:Restore",
            "window:restore-running",
            Some(16_000),
            8_000,
        ),
        spill(
            "unknown-running",
            "table:Unknown",
            "window:unknown-running",
            None,
            512,
        ),
    ]
}

fn plan(
    anchor: &str,
    order: &[&str],
    pairs: &[QueryJoinPairIdentityDescription],
    limits: &[QueryLimitPushdownDescription],
    aggregates: &[QueryWindowAggregatePushdownDescription],
    spills: &[QueryWindowSpillDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities_limit_window_aggregate_and_spill_pushdowns(
        &query(anchor, order),
        pairs,
        limits,
        aggregates,
        spills,
    )
    .expect("paired limit restoration and compaction chains form a sparse plan")
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

fn joins_by_source(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<&str, &PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| (text(node, "logical_right_source_identity"), node))
        .collect()
}

#[test]
fn nested_aggregate_compaction_identity_tracks_paired_limit_restores() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let declared = [
        "table:First",
        "table:Gap",
        "table:Compact",
        "table:Restore",
        "table:Unknown",
    ];
    let base_pairs = pairs("table:Anchor", &declared);
    let limit_descriptors = limits();
    let aggregate_descriptors = aggregates();
    let spill_descriptors = spills();
    let original = plan(
        "table:Anchor",
        &declared,
        &base_pairs,
        &limit_descriptors,
        &aggregate_descriptors,
        &spill_descriptors,
    );
    let reordered_pairs = base_pairs.iter().rev().cloned().collect::<Vec<_>>();
    let reordered_aggregates = vec![
        aggregate_descriptors[5].clone(),
        aggregate_descriptors[3].clone(),
        aggregate_descriptors[4].clone(),
        aggregate_descriptors[2].clone(),
        aggregate_descriptors[0].clone(),
        aggregate_descriptors[1].clone(),
    ];
    let reordered_spills = spill_descriptors.iter().rev().cloned().collect::<Vec<_>>();
    let reordered = plan(
        "table:Anchor",
        &declared,
        &reordered_pairs,
        &limit_descriptors,
        &reordered_aggregates,
        &reordered_spills,
    );
    let other_anchor = plan(
        "table:OtherAnchor",
        &declared,
        &pairs("table:OtherAnchor", &declared),
        &limit_descriptors,
        &aggregate_descriptors,
        &spill_descriptors,
    );

    let joins = joins_by_source(&original);
    let first = joins["table:First"];
    let gap = joins["table:Gap"];
    let compact = joins["table:Compact"];
    let restore = joins["table:Restore"];
    let unknown = joins["table:Unknown"];
    let fold_key = "paired_limit_window_compaction_fold_identity";

    assert_eq!(integer(first, "planned_input_position"), 1);
    assert_eq!(integer(gap, "planned_input_position"), 2);
    assert_eq!(integer(compact, "planned_input_position"), 3);
    assert_eq!(integer(restore, "planned_input_position"), 4);
    assert_eq!(integer(unknown, "planned_input_position"), 5);

    let first_fold = text(first, fold_key);
    assert_eq!(
        text(first, "paired_limit_window_compaction_fold_transition"),
        "advanced_restore_or_compaction_pair"
    );
    assert_eq!(
        integer(first, "paired_limit_window_compaction_limit_pair_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_limit_window_compaction_limit_stage_count"),
        2
    );
    assert_eq!(
        integer(first, "paired_limit_window_compaction_aggregate_pair_count"),
        1
    );
    assert_eq!(
        integer(
            first,
            "paired_limit_window_compaction_aggregate_stage_count"
        ),
        2
    );
    assert_eq!(
        integer(first, "paired_limit_window_compaction_window_pair_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_limit_window_compaction_window_stage_count"),
        2
    );
    assert_eq!(
        integer(first, "paired_limit_window_compaction_spill_pair_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_limit_window_compaction_spill_stage_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_limit_window_compaction_estimated_bytes"),
        4_000
    );
    assert_eq!(
        integer(first, "paired_limit_window_compaction_estimated_io_blocks"),
        1
    );
    assert_eq!(
        integer(first, "paired_limit_window_compaction_estimated_io_work"),
        2
    );
    assert_eq!(
        text(first, "paired_limit_window_compaction_estimate_status"),
        "computed"
    );
    assert_eq!(
        text(
            first,
            "paired_limit_window_compaction_limit_aggregate_fold_identity"
        ),
        text(first, "paired_limit_aggregate_restoration_fold_identity")
    );
    assert_eq!(
        text(
            first,
            "paired_limit_window_compaction_window_spill_fold_identity"
        ),
        text(first, "paired_window_compaction_spill_fold_identity")
    );
    assert_eq!(text(gap, fold_key), first_fold);
    assert_eq!(
        text(gap, "paired_limit_window_compaction_fold_transition"),
        "carried_across_sparse_input"
    );

    let compact_fold = text(compact, fold_key);
    assert_ne!(
        compact_fold, first_fold,
        "the nested compaction advances the fold"
    );
    assert_eq!(
        text(compact, "paired_limit_window_compaction_fold_transition"),
        "advanced_restore_or_compaction_pair"
    );
    assert_eq!(
        integer(compact, "paired_limit_window_compaction_limit_pair_count"),
        1
    );
    assert_eq!(
        integer(compact, "paired_limit_window_compaction_limit_stage_count"),
        2
    );
    assert_eq!(
        integer(
            compact,
            "paired_limit_window_compaction_aggregate_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(
            compact,
            "paired_limit_window_compaction_aggregate_stage_count"
        ),
        3
    );
    assert_eq!(
        integer(compact, "paired_limit_window_compaction_window_pair_count"),
        2
    );
    assert_eq!(
        integer(compact, "paired_limit_window_compaction_window_stage_count"),
        3
    );
    assert_eq!(
        integer(compact, "paired_limit_window_compaction_spill_pair_count"),
        2
    );
    assert_eq!(
        integer(compact, "paired_limit_window_compaction_spill_stage_count"),
        2
    );
    assert_eq!(
        integer(compact, "paired_limit_window_compaction_estimated_bytes"),
        4_001
    );
    assert_eq!(
        integer(
            compact,
            "paired_limit_window_compaction_estimated_io_blocks"
        ),
        2
    );
    assert_eq!(
        integer(compact, "paired_limit_window_compaction_estimated_io_work"),
        4
    );
    assert_eq!(
        text(
            compact,
            "paired_limit_window_compaction_limit_aggregate_fold_identity"
        ),
        text(compact, "paired_limit_aggregate_restoration_fold_identity")
    );
    assert_eq!(
        text(
            compact,
            "paired_limit_window_compaction_window_spill_fold_identity"
        ),
        text(compact, "paired_window_compaction_spill_fold_identity")
    );

    let restore_fold = text(restore, fold_key);
    assert_ne!(
        restore_fold, compact_fold,
        "a later paired restore is included"
    );
    assert_eq!(
        integer(restore, "paired_limit_window_compaction_limit_pair_count"),
        2
    );
    assert_eq!(
        integer(restore, "paired_limit_window_compaction_limit_stage_count"),
        3
    );
    assert_eq!(
        integer(
            restore,
            "paired_limit_window_compaction_aggregate_pair_count"
        ),
        3
    );
    assert_eq!(
        integer(
            restore,
            "paired_limit_window_compaction_aggregate_stage_count"
        ),
        5
    );
    assert_eq!(
        integer(restore, "paired_limit_window_compaction_window_pair_count"),
        3
    );
    assert_eq!(
        integer(restore, "paired_limit_window_compaction_window_stage_count"),
        5
    );
    assert_eq!(
        integer(restore, "paired_limit_window_compaction_spill_pair_count"),
        3
    );
    assert_eq!(
        integer(restore, "paired_limit_window_compaction_spill_stage_count"),
        3
    );
    assert_eq!(
        integer(restore, "paired_limit_window_compaction_estimated_bytes"),
        12_001
    );
    assert_eq!(
        integer(
            restore,
            "paired_limit_window_compaction_estimated_io_blocks"
        ),
        4
    );
    assert_eq!(
        integer(restore, "paired_limit_window_compaction_estimated_io_work"),
        8
    );
    assert_eq!(
        text(
            restore,
            "paired_limit_window_compaction_limit_aggregate_fold_identity"
        ),
        text(restore, "paired_limit_aggregate_restoration_fold_identity")
    );
    assert_eq!(
        text(
            restore,
            "paired_limit_window_compaction_window_spill_fold_identity"
        ),
        text(restore, "paired_window_compaction_spill_fold_identity")
    );

    assert_ne!(text(unknown, fold_key), restore_fold);
    assert_eq!(
        integer(unknown, "paired_limit_window_compaction_limit_pair_count"),
        3
    );
    assert_eq!(
        integer(unknown, "paired_limit_window_compaction_limit_stage_count"),
        4
    );
    assert_eq!(
        integer(
            unknown,
            "paired_limit_window_compaction_aggregate_pair_count"
        ),
        4
    );
    assert_eq!(
        integer(
            unknown,
            "paired_limit_window_compaction_aggregate_stage_count"
        ),
        6
    );
    assert_eq!(
        integer(unknown, "paired_limit_window_compaction_window_pair_count"),
        4
    );
    assert_eq!(
        integer(unknown, "paired_limit_window_compaction_window_stage_count"),
        6
    );
    assert_eq!(
        integer(unknown, "paired_limit_window_compaction_spill_pair_count"),
        4
    );
    assert_eq!(
        integer(unknown, "paired_limit_window_compaction_spill_stage_count"),
        4
    );
    assert_eq!(
        integer(
            unknown,
            "paired_limit_window_compaction_spill_unknown_working_set_count"
        ),
        1
    );
    assert_eq!(
        text(unknown, "paired_limit_window_compaction_estimate_status"),
        "unknown_working_set"
    );
    assert!(
        unknown
            .details()
            .get("paired_limit_window_compaction_estimated_bytes")
            .is_none(),
        "an unknown spill estimate must not retain a plausible byte total"
    );
    assert_eq!(text(unknown, fold_key), text(reordered.root(), fold_key));
    assert_ne!(text(unknown, fold_key), text(other_anchor.root(), fold_key));
    assert_eq!(
        text(unknown, "join_cost_fold_identity"),
        text(reordered.root(), "join_cost_fold_identity")
    );

    let mut changed_aggregates = aggregate_descriptors.clone();
    changed_aggregates[0].frame_identity = expression("frame:first-running-revised");
    let changed_frame = plan(
        "table:Anchor",
        &declared,
        &base_pairs,
        &limit_descriptors,
        &changed_aggregates,
        &spill_descriptors,
    );
    assert_ne!(
        text(unknown, fold_key),
        text(changed_frame.root(), fold_key)
    );

    let mut changed_spills = spill_descriptors.clone();
    changed_spills[0].memory_budget_bytes = 4_999;
    let changed_spill = plan(
        "table:Anchor",
        &declared,
        &base_pairs,
        &limit_descriptors,
        &aggregate_descriptors,
        &changed_spills,
    );
    assert_eq!(
        integer(
            &joins_by_source(&changed_spill)["table:Restore"],
            "paired_limit_window_compaction_estimated_bytes"
        ),
        12_002
    );
    assert_ne!(
        text(restore, fold_key),
        text(joins_by_source(&changed_spill)["table:Restore"], fold_key)
    );

    let root = original.root();
    assert_eq!(text(root, fold_key), text(unknown, fold_key));
    assert_eq!(
        root.details()
            .get("paired_limit_window_compaction_limit_pair_count"),
        Some(&PlanDetail::Integer(3))
    );
}
