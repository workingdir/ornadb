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
const SCOPED_LIMIT_FIXTURE: &str =
    include_str!("fixtures/planner_paired_scoped_window_limit_restoration_7lx92.orna");
const LIMIT_SPILL_FIXTURE: &str =
    include_str!("fixtures/planner_paired_limit_aggregate_spill_restoration_wemku.orna");
const SCOPED_LIMIT_WINDOW_FIXTURE: &str =
    include_str!("fixtures/planner_paired_scoped_limit_window_restoration_9p4e2.orna");
const BOUNDED_LIMIT_WINDOW_SPILL_FIXTURE: &str =
    include_str!("fixtures/planner_paired_bounded_limit_window_spill_cr8qd.orna");
const BOUNDED_PROJECTION_SPILL_RESTORE_FIXTURE: &str =
    include_str!("fixtures/planner_bounded_projection_spill_restore_qf5tz.orna");
const BOUNDED_WINDOW_RESTORE_PROJECTION_FIXTURE: &str =
    include_str!("fixtures/planner_bounded_window_restore_projection_ohyuk.orna");

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

#[test]
fn scoped_window_identity_tracks_paired_limit_restoration_folds() {
    let parsed = orna_syntax_v1::parse_module(SCOPED_LIMIT_FIXTURE);
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
    let joins = joins_by_source(&original);
    let first = joins["table:First"];
    let gap = joins["table:Gap"];
    let compact = joins["table:Compact"];
    let restore = joins["table:Restore"];
    let unknown = joins["table:Unknown"];
    let fold_key = "paired_scoped_window_limit_restoration_fold_identity";

    assert_eq!(
        text(first, "paired_scoped_window_limit_restoration_transition"),
        "advanced_window_and_limit_restoration"
    );
    assert_eq!(
        text(
            first,
            "paired_scoped_window_limit_restoration_window_fold_identity"
        ),
        text(first, "paired_scoped_window_compaction_fold_identity"),
        "the scoped window component is the exact cumulative window fold"
    );
    assert_eq!(
        text(
            first,
            "paired_scoped_window_limit_restoration_limit_fold_identity"
        ),
        text(first, "paired_limit_aggregate_restoration_fold_identity"),
        "the limit component is the exact cumulative restoration fold"
    );
    assert_eq!(
        integer(
            first,
            "paired_scoped_window_limit_restoration_window_pair_count"
        ),
        1
    );
    assert_eq!(
        integer(
            first,
            "paired_scoped_window_limit_restoration_window_stage_count"
        ),
        2
    );
    assert_eq!(
        integer(
            first,
            "paired_scoped_window_limit_restoration_limit_pair_count"
        ),
        1
    );
    assert_eq!(
        integer(
            first,
            "paired_scoped_window_limit_restoration_limit_stage_count"
        ),
        2
    );

    assert_eq!(
        text(gap, fold_key),
        text(first, fold_key),
        "a sparse join carries the scoped window and restore identities together"
    );
    assert_eq!(
        text(gap, "paired_scoped_window_limit_restoration_transition"),
        "carried_across_sparse_input"
    );
    assert_eq!(
        text(compact, "paired_scoped_window_limit_restoration_transition"),
        "advanced_window_and_limit_restoration"
    );
    assert_ne!(text(compact, fold_key), text(gap, fold_key));
    assert_eq!(
        integer(
            compact,
            "paired_scoped_window_limit_restoration_window_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(
            compact,
            "paired_scoped_window_limit_restoration_limit_pair_count"
        ),
        1
    );
    assert_ne!(text(restore, fold_key), text(compact, fold_key));
    assert_eq!(
        integer(
            restore,
            "paired_scoped_window_limit_restoration_limit_pair_count"
        ),
        2
    );
    assert_ne!(text(unknown, fold_key), text(restore, fold_key));
    assert_eq!(
        integer(
            unknown,
            "paired_scoped_window_limit_restoration_window_pair_count"
        ),
        4
    );
    assert_eq!(
        integer(
            unknown,
            "paired_scoped_window_limit_restoration_limit_pair_count"
        ),
        3
    );
    let mut changed_limits = limit_descriptors.clone();
    changed_limits[0].limit += 1;
    let changed_limit = plan(
        "table:Anchor",
        &declared,
        &base_pairs,
        &changed_limits,
        &aggregate_descriptors,
        &spill_descriptors,
    );
    let changed_first = joins_by_source(&changed_limit)["table:First"];
    assert_eq!(
        text(
            changed_first,
            "paired_scoped_window_limit_restoration_window_fold_identity"
        ),
        text(
            changed_first,
            "paired_scoped_window_compaction_fold_identity"
        ),
        "the composite carries the exact scoped window component selected for the changed plan"
    );
    assert_ne!(
        text(
            first,
            "paired_scoped_window_limit_restoration_limit_fold_identity"
        ),
        text(
            changed_first,
            "paired_scoped_window_limit_restoration_limit_fold_identity"
        ),
        "changing the paired limit changes the restoration component"
    );
    assert_ne!(
        text(first, fold_key),
        text(changed_first, fold_key),
        "the composite identity binds both independent components"
    );
    assert_ne!(
        text(unknown, "join_cost_fold_identity"),
        text(
            joins_by_source(&changed_limit)["table:Unknown"],
            "join_cost_fold_identity"
        ),
        "the changed scoped-window restoration fold propagates into later join costs"
    );
}

#[test]
fn paired_limit_identity_tracks_sparse_aggregate_spill_restoration() {
    let parsed = orna_syntax_v1::parse_module(LIMIT_SPILL_FIXTURE);
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
    let joins = joins_by_source(&original);
    let first = joins["table:First"];
    let gap = joins["table:Gap"];
    let compact = joins["table:Compact"];
    let restore = joins["table:Restore"];
    let unknown = joins["table:Unknown"];
    let key = "paired_limit_aggregate_spill_restoration_fold_identity";
    let limit_component = "paired_limit_aggregate_spill_restoration_limit_fold_identity";
    let aggregate_component =
        "paired_limit_aggregate_spill_restoration_aggregate_fold_identity";

    assert_eq!(
        text(first, "paired_limit_aggregate_spill_restoration_transition"),
        "advanced_limit_and_aggregate_restoration"
    );
    assert_eq!(
        text(first, limit_component),
        text(first, "paired_join_limit_anchor_cascade_fold_identity")
    );
    assert_eq!(
        text(first, aggregate_component),
        text(first, "paired_aggregate_spill_restoration_fold_identity")
    );
    assert_eq!(
        integer(first, "paired_limit_aggregate_spill_restoration_limit_pair_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_limit_aggregate_spill_restoration_limit_stage_count"),
        2
    );
    assert_eq!(
        integer(first, "paired_limit_aggregate_spill_restoration_aggregate_pair_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_limit_aggregate_spill_restoration_spill_pair_count"),
        1
    );

    assert_eq!(text(gap, key), text(first, key));
    assert_eq!(
        text(gap, "paired_limit_aggregate_spill_restoration_transition"),
        "carried_across_sparse_input"
    );
    assert_eq!(
        text(compact, "paired_limit_aggregate_spill_restoration_transition"),
        "advanced_aggregate_restoration"
    );
    assert_eq!(text(compact, limit_component), text(gap, limit_component));
    assert_ne!(text(compact, aggregate_component), text(gap, aggregate_component));
    assert_ne!(text(compact, key), text(gap, key));
    assert_eq!(
        integer(compact, "paired_limit_aggregate_spill_restoration_aggregate_pair_count"),
        2
    );
    assert_eq!(
        integer(compact, "paired_limit_aggregate_spill_restoration_spill_pair_count"),
        2
    );

    assert_eq!(
        text(restore, "paired_limit_aggregate_spill_restoration_transition"),
        "advanced_limit_and_aggregate_restoration"
    );
    assert_ne!(text(restore, key), text(compact, key));
    assert_eq!(
        integer(restore, "paired_limit_aggregate_spill_restoration_limit_pair_count"),
        2
    );
    assert_eq!(
        integer(restore, "paired_limit_aggregate_spill_restoration_aggregate_pair_count"),
        3
    );
    assert_eq!(
        text(unknown, "paired_limit_aggregate_spill_restoration_transition"),
        "advanced_limit_and_aggregate_restoration"
    );
    assert_eq!(
        integer(unknown, "paired_limit_aggregate_spill_restoration_limit_pair_count"),
        3
    );
    assert_eq!(
        integer(unknown, "paired_limit_aggregate_spill_restoration_aggregate_pair_count"),
        4
    );
    assert_eq!(text(original.root(), key), text(unknown, key));
    assert_eq!(
        text(original.root(), "join_cost_fold_identity"),
        text(unknown, "join_cost_fold_identity")
    );

    let mut changed_limits = limit_descriptors.clone();
    changed_limits[0].limit += 1;
    let changed_limit = plan(
        "table:Anchor",
        &declared,
        &base_pairs,
        &changed_limits,
        &aggregate_descriptors,
        &spill_descriptors,
    );
    let changed_first = joins_by_source(&changed_limit)["table:First"];
    assert_ne!(text(first, key), text(changed_first, key));
    assert_ne!(
        text(original.root(), "join_cost_fold_identity"),
        text(changed_limit.root(), "join_cost_fold_identity")
    );
    assert_eq!(
        text(changed_first, limit_component),
        text(changed_first, "paired_join_limit_anchor_cascade_fold_identity")
    );
    assert_eq!(
        text(changed_first, aggregate_component),
        text(changed_first, "paired_aggregate_spill_restoration_fold_identity")
    );

    let mut changed_spills = spill_descriptors.clone();
    changed_spills[0].memory_budget_bytes += 1;
    let changed_spill = plan(
        "table:Anchor",
        &declared,
        &base_pairs,
        &limit_descriptors,
        &aggregate_descriptors,
        &changed_spills,
    );
    let changed_spill_first = joins_by_source(&changed_spill)["table:First"];
    assert_eq!(
        text(changed_spill_first, limit_component),
        text(
            changed_spill_first,
            "paired_join_limit_anchor_cascade_fold_identity"
        ),
        "the composite reflects the limit cascade selected after replanning"
    );
    assert_ne!(
        text(first, aggregate_component),
        text(changed_spill_first, aggregate_component),
        "a changed spill updates the aggregate restoration component"
    );
    assert_ne!(text(first, key), text(changed_spill_first, key));
    assert_ne!(
        text(original.root(), "join_cost_fold_identity"),
        text(changed_spill.root(), "join_cost_fold_identity")
    );

    let without_restore_aggregates = aggregate_descriptors
        .iter()
        .filter(|aggregate| aggregate.source.as_str() != "table:Restore")
        .cloned()
        .collect::<Vec<_>>();
    let without_restore_spills = spill_descriptors
        .iter()
        .filter(|spill| spill.source.as_str() != "table:Restore")
        .cloned()
        .collect::<Vec<_>>();
    let limit_only = plan(
        "table:Anchor",
        &declared,
        &base_pairs,
        &limit_descriptors,
        &without_restore_aggregates,
        &without_restore_spills,
    );
    let limit_only_joins = joins_by_source(&limit_only);
    let limit_only_compact = limit_only_joins["table:Compact"];
    let limit_only_restore = limit_only_joins["table:Restore"];
    assert_eq!(
        text(
            limit_only_restore,
            "paired_limit_aggregate_spill_restoration_transition"
        ),
        "advanced_limit_identity"
    );
    assert_eq!(
        text(limit_only_restore, aggregate_component),
        text(limit_only_compact, aggregate_component)
    );
    assert_ne!(
        text(limit_only_restore, limit_component),
        text(limit_only_compact, limit_component)
    );
    assert_ne!(
        text(limit_only_restore, key),
        text(limit_only_compact, key)
    );
}
#[test]
fn scoped_limit_identity_tracks_paired_window_restoration_folds() {
    let parsed = orna_syntax_v1::parse_module(SCOPED_LIMIT_WINDOW_FIXTURE);
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
    let joins = joins_by_source(&original);
    let first = joins["table:First"];
    let gap = joins["table:Gap"];
    let compact = joins["table:Compact"];
    let restore = joins["table:Restore"];
    let unknown = joins["table:Unknown"];
    let key = "paired_scoped_limit_window_restoration_fold_identity";
    let limit_scope = "paired_scoped_limit_window_restoration_limit_scope_identity";
    let window_fold = "paired_scoped_limit_window_restoration_window_fold_identity";

    assert_eq!(
        text(first, "paired_scoped_limit_window_restoration_transition"),
        "advanced_limit_scope_and_window_restoration"
    );
    assert_eq!(
        text(first, limit_scope),
        text(first, "paired_limit_pushdown_anchor_fold_identity")
    );
    assert_eq!(
        text(first, window_fold),
        text(first, "paired_window_compaction_spill_fold_identity")
    );
    assert_eq!(
        integer(first, "paired_scoped_limit_window_restoration_limit_pair_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_scoped_limit_window_restoration_limit_stage_count"),
        2
    );
    assert_eq!(
        integer(first, "paired_scoped_limit_window_restoration_window_pair_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_scoped_limit_window_restoration_window_stage_count"),
        2
    );

    assert_eq!(text(gap, key), text(first, key));
    assert_eq!(
        text(gap, "paired_scoped_limit_window_restoration_transition"),
        "carried_across_sparse_input"
    );
    assert_eq!(text(gap, limit_scope), text(first, limit_scope));
    assert_eq!(text(gap, window_fold), text(first, window_fold));
    assert_eq!(
        text(compact, "paired_scoped_limit_window_restoration_transition"),
        "advanced_window_restoration"
    );
    assert_eq!(text(compact, limit_scope), text(gap, limit_scope));
    assert_ne!(text(compact, window_fold), text(gap, window_fold));
    assert_ne!(text(compact, key), text(gap, key));
    assert_eq!(
        integer(compact, "paired_scoped_limit_window_restoration_limit_pair_count"),
        1
    );
    assert_eq!(
        integer(compact, "paired_scoped_limit_window_restoration_window_pair_count"),
        2
    );

    assert_eq!(
        text(restore, "paired_scoped_limit_window_restoration_transition"),
        "advanced_limit_scope_and_window_restoration"
    );
    assert_ne!(text(restore, key), text(compact, key));
    assert_eq!(
        integer(restore, "paired_scoped_limit_window_restoration_limit_pair_count"),
        2
    );
    assert_eq!(
        integer(restore, "paired_scoped_limit_window_restoration_window_pair_count"),
        3
    );
    assert_eq!(
        text(unknown, "paired_scoped_limit_window_restoration_transition"),
        "advanced_limit_scope_and_window_restoration"
    );
    assert_eq!(
        integer(unknown, "paired_scoped_limit_window_restoration_limit_pair_count"),
        3
    );
    assert_eq!(
        integer(unknown, "paired_scoped_limit_window_restoration_window_pair_count"),
        4
    );
    assert_eq!(
        integer(
            unknown,
            "paired_scoped_limit_window_restoration_unknown_working_set_count"
        ),
        1
    );
    assert_eq!(text(original.root(), key), text(unknown, key));
    assert_eq!(
        text(original.root(), "join_cost_fold_identity"),
        text(unknown, "join_cost_fold_identity")
    );

    let mut changed_limits = limit_descriptors.clone();
    changed_limits[0].limit += 1;
    let changed_limit = plan(
        "table:Anchor",
        &declared,
        &base_pairs,
        &changed_limits,
        &aggregate_descriptors,
        &spill_descriptors,
    );
    let changed_first = joins_by_source(&changed_limit)["table:First"];
    assert_ne!(text(first, key), text(changed_first, key));
    assert_ne!(
        text(original.root(), "join_cost_fold_identity"),
        text(changed_limit.root(), "join_cost_fold_identity")
    );
    assert_eq!(
        text(changed_first, limit_scope),
        text(changed_first, "paired_limit_pushdown_anchor_fold_identity")
    );
    assert_eq!(
        text(changed_first, window_fold),
        text(changed_first, "paired_window_compaction_spill_fold_identity")
    );

    let mut changed_spills = spill_descriptors.clone();
    changed_spills[0].memory_budget_bytes += 1;
    let changed_spill = plan(
        "table:Anchor",
        &declared,
        &base_pairs,
        &limit_descriptors,
        &aggregate_descriptors,
        &changed_spills,
    );
    let changed_spill_first = joins_by_source(&changed_spill)["table:First"];
    assert_eq!(
        text(changed_spill_first, window_fold),
        text(
            changed_spill_first,
            "paired_window_compaction_spill_fold_identity"
        )
    );
    assert_ne!(text(first, window_fold), text(changed_spill_first, window_fold));
    assert_ne!(text(first, key), text(changed_spill_first, key));
    assert_ne!(
        text(original.root(), "join_cost_fold_identity"),
        text(changed_spill.root(), "join_cost_fold_identity")
    );

    let without_restore_aggregates = aggregate_descriptors
        .iter()
        .filter(|aggregate| aggregate.source.as_str() != "table:Restore")
        .cloned()
        .collect::<Vec<_>>();
    let without_restore_spills = spill_descriptors
        .iter()
        .filter(|spill| spill.source.as_str() != "table:Restore")
        .cloned()
        .collect::<Vec<_>>();
    let limit_only = plan(
        "table:Anchor",
        &declared,
        &base_pairs,
        &limit_descriptors,
        &without_restore_aggregates,
        &without_restore_spills,
    );
    let limit_only_joins = joins_by_source(&limit_only);
    let limit_only_compact = limit_only_joins["table:Compact"];
    let limit_only_restore = limit_only_joins["table:Restore"];
    assert_eq!(
        text(limit_only_restore, "paired_scoped_limit_window_restoration_transition"),
        "advanced_limit_scope"
    );
    assert_ne!(text(limit_only_restore, limit_scope), text(limit_only_compact, limit_scope));
    assert_eq!(text(limit_only_restore, window_fold), text(limit_only_compact, window_fold));
    assert_ne!(text(limit_only_restore, key), text(limit_only_compact, key));
}

#[test]
fn bounded_limit_identity_tracks_paired_window_spill_folds() {
    let parsed = orna_syntax_v1::parse_module(BOUNDED_LIMIT_WINDOW_SPILL_FIXTURE);
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
    let joins = joins_by_source(&original);
    let first = joins["table:First"];
    let gap = joins["table:Gap"];
    let compact = joins["table:Compact"];
    let restore = joins["table:Restore"];
    let unknown = joins["table:Unknown"];
    let key = "paired_bounded_limit_window_spill_fold_identity";
    let limit_identity = "paired_bounded_limit_window_spill_limit_identity";
    let spill_identity = "paired_bounded_limit_window_spill_window_spill_fold_identity";
    let bound = "paired_bounded_limit_window_spill_bounded_rows";
    assert_eq!(
        text(first, "paired_bounded_limit_window_spill_transition"),
        "advanced_bounded_limit_and_window_spill"
    );
    assert_eq!(integer(first, bound), 7);
    assert_eq!(
        text(first, limit_identity),
        text(first, "paired_limit_pushdown_anchor_fold_identity")
    );
    assert_eq!(
        text(first, spill_identity),
        text(first, "paired_window_spill_cascade_fold_identity")
    );
    assert_eq!(
        integer(first, "paired_bounded_limit_window_spill_limit_pair_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_bounded_limit_window_spill_limit_stage_count"),
        2
    );
    assert_eq!(
        integer(first, "paired_bounded_limit_window_spill_spill_pair_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_bounded_limit_window_spill_spill_stage_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_bounded_limit_window_spill_estimated_bytes"),
        4_000
    );

    assert_eq!(text(gap, key), text(first, key));
    assert_eq!(integer(gap, bound), 7);
    assert_eq!(
        text(gap, "paired_bounded_limit_window_spill_transition"),
        "carried_across_sparse_input"
    );
    assert_eq!(text(gap, limit_identity), text(first, limit_identity));
    assert_eq!(text(gap, spill_identity), text(first, spill_identity));

    assert_eq!(
        text(compact, "paired_bounded_limit_window_spill_transition"),
        "advanced_window_spill"
    );
    assert_eq!(integer(compact, bound), 7);
    assert_eq!(text(compact, limit_identity), text(gap, limit_identity));
    assert_ne!(text(compact, spill_identity), text(gap, spill_identity));
    assert_ne!(text(compact, key), text(gap, key));
    assert_eq!(
        integer(compact, "paired_bounded_limit_window_spill_limit_pair_count"),
        1
    );
    assert_eq!(
        integer(compact, "paired_bounded_limit_window_spill_spill_pair_count"),
        2
    );
    assert_eq!(
        integer(compact, "paired_bounded_limit_window_spill_estimated_bytes"),
        4_001
    );

    assert_eq!(
        text(restore, "paired_bounded_limit_window_spill_transition"),
        "advanced_bounded_limit_and_window_spill"
    );
    assert_eq!(integer(restore, bound), 19);
    assert_ne!(text(restore, key), text(compact, key));
    assert_eq!(
        integer(restore, "paired_bounded_limit_window_spill_limit_pair_count"),
        2
    );
    assert_eq!(
        integer(restore, "paired_bounded_limit_window_spill_spill_pair_count"),
        3
    );
    assert_eq!(
        integer(restore, "paired_bounded_limit_window_spill_estimated_bytes"),
        12_001
    );

    assert_eq!(
        text(unknown, "paired_bounded_limit_window_spill_transition"),
        "advanced_bounded_limit_and_window_spill"
    );
    assert_eq!(integer(unknown, bound), 11);
    assert_eq!(
        integer(unknown, "paired_bounded_limit_window_spill_limit_pair_count"),
        3
    );
    assert_eq!(
        integer(unknown, "paired_bounded_limit_window_spill_spill_pair_count"),
        4
    );
    assert_eq!(
        integer(
            unknown,
            "paired_bounded_limit_window_spill_unknown_working_set_count"
        ),
        1
    );
    assert_eq!(
        text(unknown, "paired_bounded_limit_window_spill_estimate_status"),
        "unknown_working_set"
    );
    assert_eq!(text(original.root(), key), text(unknown, key));
    assert_eq!(
        text(original.root(), "join_cost_fold_identity"),
        text(unknown, "join_cost_fold_identity")
    );

    let mut changed_limits = limit_descriptors.clone();
    changed_limits[0].limit += 1;
    let changed_limit = plan(
        "table:Anchor",
        &declared,
        &base_pairs,
        &changed_limits,
        &aggregate_descriptors,
        &spill_descriptors,
    );
    let changed_first = joins_by_source(&changed_limit)["table:First"];
    assert_eq!(integer(changed_first, bound), 7);
    assert_ne!(text(first, limit_identity), text(changed_first, limit_identity));
    assert_ne!(text(first, key), text(changed_first, key));
    assert_ne!(
        text(original.root(), "join_cost_fold_identity"),
        text(changed_limit.root(), "join_cost_fold_identity")
    );

    let mut changed_spills = spill_descriptors.clone();
    changed_spills[0].memory_budget_bytes += 1;
    let changed_spill = plan(
        "table:Anchor",
        &declared,
        &base_pairs,
        &limit_descriptors,
        &aggregate_descriptors,
        &changed_spills,
    );
    let changed_spill_first = joins_by_source(&changed_spill)["table:First"];
    assert_eq!(integer(changed_spill_first, bound), 7);
    assert_ne!(text(first, spill_identity), text(changed_spill_first, spill_identity));
    assert_ne!(text(first, key), text(changed_spill_first, key));
    assert_ne!(
        text(original.root(), "join_cost_fold_identity"),
        text(changed_spill.root(), "join_cost_fold_identity")
    );

    let no_spills = plan(
        "table:Anchor",
        &declared,
        &base_pairs,
        &limit_descriptors,
        &aggregate_descriptors,
        &[],
    );
    assert!(
        !no_spills.root().details().contains_key(key),
        "the composite requires an actual paired window spill fold"
    );
}

#[test]
fn bounded_projection_identity_tracks_paired_spill_restore_folds() {
    assert!(BOUNDED_PROJECTION_SPILL_RESTORE_FIXTURE.contains("bounded_projection_spill_restore"));

    let declared = ["table:First", "table:Gap", "table:Compact", "table:Restore"];
    let pair_descriptors = pairs("table:Anchor", &declared);
    let limit_descriptors = limits()
        .into_iter()
        .filter(|limit| limit.source.as_str() != "table:Unknown")
        .collect::<Vec<_>>();
    let aggregate_descriptors = aggregates()
        .into_iter()
        .filter(|aggregate| aggregate.source.as_str() != "table:Unknown")
        .collect::<Vec<_>>();
    let spill_descriptors = spills()
        .into_iter()
        .filter(|spill| spill.source.as_str() != "table:Unknown")
        .collect::<Vec<_>>();
    let mut projected_query = query("table:Anchor", &declared);
    projected_query.projections = vec![
        expression("expr:projection-key"),
        expression("expr:projection-value"),
    ];
    projected_query.limit = Some(6);

    let explain = |query: &QueryPlanDescription,
                   projection_pairs: &[QueryJoinPairIdentityDescription],
                   projection_spills: &[QueryWindowSpillDescription]| {
        explain_query_with_join_pair_identities_limit_window_aggregate_and_spill_pushdowns(
            query,
            projection_pairs,
            &limit_descriptors,
            &aggregate_descriptors,
            projection_spills,
        )
        .expect("bounded projections can be folded over paired sparse spill restores")
    };
    let original = explain(&projected_query, &pair_descriptors, &spill_descriptors);
    let project = original
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Project)
        .expect("the selected expressions create a projection node");
    let restore_join = joins_by_source(&original)["table:Restore"];
    let fold_key = "paired_bounded_projection_spill_restoration_fold_identity";
    let projection_key = "paired_bounded_projection_spill_restoration_projection_identity";
    let restore_key = "paired_bounded_projection_spill_restoration_spill_fold_identity";

    assert_eq!(integer(project, "paired_bounded_projection_spill_restoration_projection_count"), 2);
    assert_eq!(integer(project, "paired_bounded_projection_spill_restoration_input_rows"), 399);
    assert_eq!(
        integer(
            project,
            "paired_bounded_projection_spill_restoration_result_row_upper_bound"
        ),
        6
    );
    assert_eq!(
        integer(
            project,
            "paired_bounded_projection_spill_restoration_estimated_projection_work"
        ),
        798,
        "the final limit does not refund work already done by Project"
    );
    assert_eq!(project.estimated_rows(), Some(399));
    assert_eq!(project.estimated_work(), Some(798));
    assert_eq!(
        integer(project, "paired_bounded_projection_spill_restoration_aggregate_pair_count"),
        3
    );
    assert_eq!(
        integer(project, "paired_bounded_projection_spill_restoration_aggregate_stage_count"),
        5
    );
    assert_eq!(
        integer(project, "paired_bounded_projection_spill_restoration_spill_pair_count"),
        3
    );
    assert_eq!(
        integer(project, "paired_bounded_projection_spill_restoration_spill_stage_count"),
        3
    );
    assert_eq!(
        text(project, restore_key),
        text(restore_join, "paired_aggregate_spill_restoration_fold_identity")
    );
    assert!(text(project, projection_key).starts_with("projection-chain:"));
    assert!(text(project, fold_key).starts_with("paired-bounded-projection-spill-restoration:"));

    let mut reduced_limit_query = projected_query.clone();
    reduced_limit_query.limit = Some(3);
    let reduced_limit = explain(&reduced_limit_query, &pair_descriptors, &spill_descriptors);
    let reduced_project = reduced_limit
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Project)
        .expect("the reduced-limit plan retains Project");
    assert_eq!(integer(reduced_project, "paired_bounded_projection_spill_restoration_result_row_upper_bound"), 3);
    assert_eq!(integer(reduced_project, "paired_bounded_projection_spill_restoration_estimated_projection_work"), 798);
    assert_eq!(text(reduced_project, projection_key), text(project, projection_key));
    assert_eq!(text(reduced_project, restore_key), text(project, restore_key));
    assert_ne!(text(reduced_project, fold_key), text(project, fold_key));

    let mut changed_projection_query = projected_query.clone();
    changed_projection_query.projections[1] = expression("expr:replacement-value");
    let changed_projection = explain(&changed_projection_query, &pair_descriptors, &spill_descriptors);
    let changed_projection_node = changed_projection
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Project)
        .expect("the changed expression still creates Project");
    assert_ne!(text(changed_projection_node, projection_key), text(project, projection_key));
    assert_ne!(text(changed_projection_node, fold_key), text(project, fold_key));
    assert_eq!(text(changed_projection_node, restore_key), text(project, restore_key));

    let mut changed_spills = spill_descriptors.clone();
    changed_spills[0].memory_budget_bytes += 1;
    let changed_restore = explain(&projected_query, &pair_descriptors, &changed_spills);
    let changed_restore_node = changed_restore
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Project)
        .expect("the changed spill still creates Project");
    assert_eq!(text(changed_restore_node, projection_key), text(project, projection_key));
    assert_ne!(text(changed_restore_node, restore_key), text(project, restore_key));
    assert_ne!(text(changed_restore_node, fold_key), text(project, fold_key));
}

#[test]
fn bounded_window_identity_tracks_paired_restore_projection_folds() {
    assert!(BOUNDED_WINDOW_RESTORE_PROJECTION_FIXTURE.contains("bounded_window_restore_projection"));

    let declared = ["table:First", "table:Gap", "table:Compact", "table:Restore"];
    let pair_descriptors = pairs("table:Anchor", &declared);
    let first = aggregate(
        "window:first-bounded",
        "table:First",
        "frame:first-bounded",
        PlanWindowFrameBound::Preceding(2),
    );
    let compact = aggregate(
        "window:compact-bounded",
        "table:Compact",
        "frame:compact-bounded",
        PlanWindowFrameBound::CurrentRow,
    );
    let mut restore = aggregate(
        "window:restore-bounded",
        "table:Restore",
        "frame:restore-bounded",
        PlanWindowFrameBound::Preceding(4),
    );
    restore.frame_end = PlanWindowFrameBound::Following(2);
    let aggregate_descriptors = vec![first, compact, restore];
    let spill_descriptors = vec![
        spill(
            "first-bounded",
            "table:First",
            "window:first-bounded",
            Some(9_000),
            5_000,
        ),
        spill(
            "compact-bounded",
            "table:Compact",
            "window:compact-bounded",
            Some(8_193),
            8_192,
        ),
        spill(
            "restore-bounded",
            "table:Restore",
            "window:restore-bounded",
            Some(16_000),
            8_000,
        ),
    ];
    let mut bounded_query = query("table:Anchor", &declared);
    bounded_query.projections = vec![
        expression("expr:window-key"),
        expression("expr:window-value"),
    ];
    bounded_query.limit = Some(8);

    let explain = |query: &QueryPlanDescription,
                   aggregates: &[QueryWindowAggregatePushdownDescription],
                   spills: &[QueryWindowSpillDescription]| {
        explain_query_with_join_pair_identities_limit_window_aggregate_and_spill_pushdowns(
            query,
            &pair_descriptors,
            &[],
            aggregates,
            spills,
        )
        .expect("bounded windows can be paired with restored projections")
    };
    let original = explain(&bounded_query, &aggregate_descriptors, &spill_descriptors);
    let project = original
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Project)
        .expect("the ordered output expressions create a projection node");
    let restore_join = joins_by_source(&original)["table:Restore"];
    let fold_key = "paired_bounded_window_projection_spill_restoration_fold_identity";
    let window_key = "paired_bounded_window_projection_spill_restoration_window_identity";
    let projection_fold_key =
        "paired_bounded_window_projection_spill_restoration_projection_fold_identity";
    let spill_fold_key = "paired_bounded_window_projection_spill_restoration_spill_fold_identity";

    assert!(text(project, fold_key).starts_with("paired-bounded-window-projection-spill-restoration:"));
    assert!(text(project, window_key).starts_with("bounded-window-chain:"));
    assert_eq!(
        text(project, projection_fold_key),
        text(project, "paired_bounded_projection_spill_restoration_fold_identity")
    );
    assert_eq!(
        text(project, spill_fold_key),
        text(restore_join, "paired_aggregate_spill_restoration_fold_identity")
    );
    assert_eq!(
        integer(
            project,
            "paired_bounded_window_projection_spill_restoration_bounded_window_count"
        ),
        3
    );
    assert_eq!(
        integer(
            project,
            "paired_bounded_window_projection_spill_restoration_frame_rows_upper_bound"
        ),
        7
    );
    assert_eq!(
        integer(
            project,
            "paired_bounded_window_projection_spill_restoration_projection_count"
        ),
        2
    );
    assert_eq!(
        integer(
            project,
            "paired_bounded_window_projection_spill_restoration_result_row_upper_bound"
        ),
        8
    );
    assert_eq!(
        integer(
            project,
            "paired_bounded_window_projection_spill_restoration_aggregate_pair_count"
        ),
        3
    );
    assert_eq!(
        integer(
            project,
            "paired_bounded_window_projection_spill_restoration_spill_pair_count"
        ),
        3
    );

    let mut changed_window = aggregate_descriptors.clone();
    changed_window[2].frame_start = PlanWindowFrameBound::Preceding(3);
    let changed_window_plan = explain(&bounded_query, &changed_window, &spill_descriptors);
    let changed_window_project = changed_window_plan
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Project)
        .expect("the changed finite frame retains Project");
    assert_ne!(text(changed_window_project, window_key), text(project, window_key));
    assert_ne!(text(changed_window_project, fold_key), text(project, fold_key));

    let mut changed_projection_query = bounded_query.clone();
    changed_projection_query.projections[1] = expression("expr:replacement-window-value");
    let changed_projection = explain(
        &changed_projection_query,
        &aggregate_descriptors,
        &spill_descriptors,
    );
    let changed_projection_project = changed_projection
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Project)
        .expect("the changed output expression retains Project");
    assert_eq!(text(changed_projection_project, window_key), text(project, window_key));
    assert_ne!(
        text(changed_projection_project, projection_fold_key),
        text(project, projection_fold_key)
    );
    assert_ne!(text(changed_projection_project, fold_key), text(project, fold_key));

    let mut changed_spills = spill_descriptors.clone();
    changed_spills[0].memory_budget_bytes += 1;
    let changed_restore = explain(&bounded_query, &aggregate_descriptors, &changed_spills);
    let changed_restore_project = changed_restore
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Project)
        .expect("the changed spill still restores a projection fold");
    let changed_restore_join = joins_by_source(&changed_restore)["table:Restore"];
    assert_eq!(text(changed_restore_project, window_key), text(project, window_key));
    assert_eq!(
        text(changed_restore_project, spill_fold_key),
        text(changed_restore_join, "paired_aggregate_spill_restoration_fold_identity")
    );
    assert_ne!(text(changed_restore_project, spill_fold_key), text(project, spill_fold_key));
    assert_ne!(text(changed_restore_project, fold_key), text(project, fold_key));

    let unbounded_aggregates = aggregate_descriptors
        .iter()
        .cloned()
        .map(|mut aggregate| {
            aggregate.frame_start = PlanWindowFrameBound::UnboundedPreceding;
            aggregate.frame_end = PlanWindowFrameBound::CurrentRow;
            aggregate
        })
        .collect::<Vec<_>>();
    let unbounded = explain(&bounded_query, &unbounded_aggregates, &spill_descriptors);
    let unbounded_project = unbounded
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Project)
        .expect("unbounded windows still project output columns");
    assert!(
        !unbounded_project.details().contains_key(fold_key),
        "unbounded frames do not produce a bounded-window identity"
    );

    let no_restore = explain(&bounded_query, &aggregate_descriptors, &[]);
    let no_restore_project = no_restore
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Project)
        .expect("bounded windows still project without spill restores");
    assert!(
        !no_restore_project.details().contains_key(fold_key),
        "the composite requires paired spill-restoration history"
    );
}
