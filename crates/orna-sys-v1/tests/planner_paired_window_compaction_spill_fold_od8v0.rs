use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, QueryWindowSpillDescription,
    SnapshotRef, explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_paired_window_compaction_spill_fold_od8v0.orna");

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
        "table:GapOne" => Some(statistics(15, 4_096)),
        "table:Middle" => Some(statistics(20, 4_096)),
        "table:GapTwo" => Some(statistics(22, 4_096)),
        "table:Large" => Some(statistics(30, 16_384)),
        "table:GapTail" => Some(statistics(100, 53_248)),
        "table:Unknown" => None,
        other => panic!("unexpected source {other}"),
    }
}

fn query(anchor: &str, order: &[&str]) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-window-compaction-spill-od8v0"),
        source: object(anchor),
        source_statistics: Some(statistics(40, 40_000)),
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
            PlanWindowFrameBound::Preceding(3),
        ),
        aggregate(
            "window:middle-running",
            "table:Middle",
            "frame:middle-running",
            PlanWindowFrameBound::UnboundedPreceding,
        ),
        aggregate(
            "window:large-running",
            "table:Large",
            "frame:large-running",
            PlanWindowFrameBound::UnboundedPreceding,
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
    identity: &str,
    source: &str,
    aggregate_identity: &str,
    working_set: Option<u64>,
    budget: u64,
) -> QueryWindowSpillDescription {
    QueryWindowSpillDescription {
        identity: object(identity),
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
            "spill:first-running",
            "table:First",
            "window:first-running",
            Some(16_385),
            8_192,
        ),
        spill(
            "spill:first-recent",
            "table:First",
            "window:first-recent",
            Some(4_000),
            4_096,
        ),
        spill(
            "spill:large-running",
            "table:Large",
            "window:large-running",
            Some(20_000),
            8_000,
        ),
        spill(
            "spill:unknown-running",
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
    aggregates: &[QueryWindowAggregatePushdownDescription],
    spills: &[QueryWindowSpillDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns(
        &query(anchor, order),
        pairs,
        aggregates,
        spills,
    )
    .expect("paired window and spill chains form a sparse plan")
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
fn paired_window_identity_restores_across_sparse_compaction_spill_chains() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let declared = [
        "table:Unknown",
        "table:GapTail",
        "table:Large",
        "table:GapTwo",
        "table:Middle",
        "table:GapOne",
        "table:First",
    ];
    let reordered = [
        "table:First",
        "table:GapOne",
        "table:Middle",
        "table:GapTwo",
        "table:Large",
        "table:GapTail",
        "table:Unknown",
    ];
    let aggregate_descriptors = aggregates();
    let spill_descriptors = spills();
    let original = plan(
        "table:AnchorLeft",
        &declared,
        &pairs("table:AnchorLeft", &declared),
        &aggregate_descriptors,
        &spill_descriptors,
    );
    let reordered_aggregates = vec![
        aggregate_descriptors[4].clone(),
        aggregate_descriptors[2].clone(),
        aggregate_descriptors[0].clone(),
        aggregate_descriptors[1].clone(),
        aggregate_descriptors[3].clone(),
    ];
    let reordered = plan(
        "table:AnchorLeft",
        &reordered,
        &pairs("table:AnchorLeft", &reordered)
            .into_iter()
            .rev()
            .collect::<Vec<_>>(),
        &reordered_aggregates,
        &spill_descriptors.iter().rev().cloned().collect::<Vec<_>>(),
    );
    let other_anchor = plan(
        "table:AnchorRight",
        &declared,
        &pairs("table:AnchorRight", &declared),
        &aggregate_descriptors,
        &spill_descriptors,
    );

    let joins = joins_by_source(&original);
    let reordered_root = reordered.root();
    let other_root = other_anchor.root();
    let first = joins["table:First"];
    let gap_one = joins["table:GapOne"];
    let middle = joins["table:Middle"];
    let gap_two = joins["table:GapTwo"];
    let large = joins["table:Large"];
    let gap_tail = joins["table:GapTail"];
    let unknown = joins["table:Unknown"];
    let fold_key = "paired_window_compaction_spill_fold_identity";

    assert_eq!(integer(first, "planned_input_position"), 1);
    assert_eq!(integer(gap_one, "planned_input_position"), 2);
    assert_eq!(integer(middle, "planned_input_position"), 3);
    assert_eq!(integer(gap_two, "planned_input_position"), 4);
    assert_eq!(integer(large, "planned_input_position"), 5);
    assert_eq!(integer(gap_tail, "planned_input_position"), 6);
    assert_eq!(integer(unknown, "planned_input_position"), 7);

    let first_fold = text(first, fold_key);
    assert_eq!(
        text(first, "paired_window_compaction_spill_fold_transition"),
        "advanced_window_pair"
    );
    assert_eq!(
        integer(first, "paired_window_compaction_spill_window_pair_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_window_compaction_spill_window_stage_count"),
        2
    );
    assert_eq!(
        integer(first, "paired_window_compaction_spill_pair_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_window_compaction_spill_stage_count"),
        2
    );
    assert_eq!(
        integer(first, "paired_window_compaction_spill_estimated_bytes"),
        8_193
    );
    assert_eq!(
        integer(first, "paired_window_compaction_spill_estimated_io_blocks"),
        3
    );
    assert_eq!(
        integer(first, "paired_window_compaction_spill_estimated_io_work"),
        6
    );
    assert_eq!(
        text(first, "paired_window_compaction_spill_estimate_status"),
        "computed"
    );
    assert_eq!(text(gap_one, fold_key), first_fold);
    assert_eq!(
        text(gap_one, "paired_window_compaction_spill_fold_transition"),
        "carried_across_sparse_input"
    );
    assert_eq!(
        integer(gap_one, "paired_window_compaction_spill_window_pair_count"),
        1
    );

    let middle_fold = text(middle, fold_key);
    assert_eq!(
        text(middle, "paired_window_compaction_spill_fold_transition"),
        "advanced_window_pair"
    );
    assert_ne!(
        middle_fold, first_fold,
        "a window-only pair restores the fold"
    );
    assert_eq!(
        integer(middle, "paired_window_compaction_spill_window_pair_count"),
        2
    );
    assert_eq!(
        integer(middle, "paired_window_compaction_spill_window_stage_count"),
        3
    );
    assert_eq!(
        integer(middle, "paired_window_compaction_spill_pair_count"),
        1
    );
    assert_eq!(
        integer(middle, "paired_window_compaction_spill_estimated_bytes"),
        8_193,
        "window restoration carries earlier spill estimates"
    );
    assert_eq!(text(gap_two, fold_key), middle_fold);
    assert_eq!(
        text(gap_two, "paired_window_compaction_spill_fold_transition"),
        "carried_across_sparse_input"
    );

    let large_fold = text(large, fold_key);
    assert_eq!(
        text(large, "paired_window_compaction_spill_fold_transition"),
        "advanced_window_pair"
    );
    assert_ne!(
        large_fold, middle_fold,
        "a later spill pair advances the fold"
    );
    assert_eq!(
        integer(large, "paired_window_compaction_spill_window_pair_count"),
        3
    );
    assert_eq!(
        integer(large, "paired_window_compaction_spill_window_stage_count"),
        4
    );
    assert_eq!(
        integer(large, "paired_window_compaction_spill_pair_count"),
        2
    );
    assert_eq!(
        integer(large, "paired_window_compaction_spill_stage_count"),
        3
    );
    assert_eq!(
        integer(large, "paired_window_compaction_spill_estimated_bytes"),
        20_193
    );
    assert_eq!(
        integer(large, "paired_window_compaction_spill_estimated_io_blocks"),
        6
    );
    assert_eq!(
        integer(large, "paired_window_compaction_spill_estimated_io_work"),
        12
    );
    assert_eq!(text(gap_tail, fold_key), large_fold);
    assert_eq!(
        text(gap_tail, "paired_window_compaction_spill_fold_transition"),
        "carried_across_sparse_input"
    );
    assert!(!text(gap_tail, "join_cost_fold_identity").is_empty());

    let final_fold = text(unknown, fold_key);
    assert_eq!(
        text(unknown, "paired_window_compaction_spill_fold_transition"),
        "advanced_window_pair"
    );
    assert_ne!(final_fold, large_fold);
    assert_eq!(
        integer(unknown, "paired_window_compaction_spill_window_pair_count"),
        4
    );
    assert_eq!(
        integer(unknown, "paired_window_compaction_spill_window_stage_count"),
        5
    );
    assert_eq!(
        integer(unknown, "paired_window_compaction_spill_pair_count"),
        3
    );
    assert_eq!(
        integer(unknown, "paired_window_compaction_spill_stage_count"),
        4
    );
    assert_eq!(
        integer(
            unknown,
            "paired_window_compaction_spill_unknown_working_set_count"
        ),
        1
    );
    assert_eq!(
        text(unknown, "paired_window_compaction_spill_estimate_status"),
        "unknown_working_set"
    );
    assert!(
        unknown
            .details()
            .get("paired_window_compaction_spill_estimated_bytes")
            .is_none(),
        "an unknown working set must not retain a plausible total"
    );
    assert_eq!(text(unknown, fold_key), text(reordered_root, fold_key));
    assert_eq!(
        text(unknown, "join_cost_fold_identity"),
        text(reordered_root, "join_cost_fold_identity")
    );
    assert_ne!(final_fold, text(other_root, fold_key));

    let mut changed_aggregates = aggregate_descriptors.clone();
    changed_aggregates[0].frame_identity = expression("frame:first-running-revised");
    let changed_frame = plan(
        "table:AnchorLeft",
        &declared,
        &pairs("table:AnchorLeft", &declared),
        &changed_aggregates,
        &spill_descriptors,
    );
    assert_ne!(
        text(unknown, fold_key),
        text(changed_frame.root(), fold_key),
        "the window frame participates in the paired restoration identity"
    );

    let mut changed_spills = spill_descriptors.clone();
    changed_spills[0].memory_budget_bytes = 8_193;
    let changed_spill = plan(
        "table:AnchorLeft",
        &declared,
        &pairs("table:AnchorLeft", &declared),
        &aggregate_descriptors,
        &changed_spills,
    );
    let changed_large = joins_by_source(&changed_spill)["table:Large"];
    assert_eq!(
        integer(
            changed_large,
            "paired_window_compaction_spill_estimated_bytes"
        ),
        20_192
    );
    assert_eq!(
        integer(
            changed_large,
            "paired_window_compaction_spill_estimated_io_blocks"
        ),
        5
    );
    assert_ne!(text(large, fold_key), text(changed_large, fold_key));
    assert_ne!(
        text(large, "join_cost_fold_identity"),
        text(changed_large, "join_cost_fold_identity")
    );
}
