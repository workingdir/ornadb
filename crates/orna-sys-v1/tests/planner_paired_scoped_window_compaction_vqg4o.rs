use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, SnapshotRef,
    explain_query_with_join_pair_identities_and_window_aggregate_pushdowns,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_scoped_window_compaction_vqg4o.orna");

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
        "table:Gap" => Some(statistics(30, 8_192)),
        "table:Second" => Some(statistics(40, 12_288)),
        "table:Tail" => Some(statistics(70, 16_384)),
        "table:Unknown" => None,
        other => panic!("unexpected source {other}"),
    }
}

fn query(anchor: &str, order: &[&str]) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-scoped-window-compaction-vqg4o"),
        source: object(anchor),
        source_statistics: Some(statistics(100, 40_000)),
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
            "window:second-running",
            "table:Second",
            "frame:second-running",
            PlanWindowFrameBound::UnboundedPreceding,
        ),
        aggregate(
            "window:tail-running",
            "table:Tail",
            "frame:tail-running",
            PlanWindowFrameBound::UnboundedPreceding,
        ),
        aggregate(
            "window:tail-recent",
            "table:Tail",
            "frame:tail-recent",
            PlanWindowFrameBound::Preceding(2),
        ),
        aggregate(
            "window:unknown-running",
            "table:Unknown",
            "frame:unknown-running",
            PlanWindowFrameBound::UnboundedPreceding,
        ),
    ]
}

fn plan(
    anchor: &str,
    order: &[&str],
    pairs: &[QueryJoinPairIdentityDescription],
    aggregates: &[QueryWindowAggregatePushdownDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities_and_window_aggregate_pushdowns(
        &query(anchor, order),
        pairs,
        aggregates,
    )
    .expect("scoped window chains form a paired aggregate-compaction plan")
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
fn scoped_window_identity_tracks_paired_aggregate_compaction_folds() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let declared = [
        "table:First",
        "table:Gap",
        "table:Second",
        "table:Tail",
        "table:Unknown",
    ];
    let pair_descriptors = pairs("table:Anchor", &declared);
    let aggregate_descriptors = aggregates();
    let original = plan(
        "table:Anchor",
        &declared,
        &pair_descriptors,
        &aggregate_descriptors,
    );
    let reordered_aggregates = vec![
        aggregate_descriptors[5].clone(),
        aggregate_descriptors[3].clone(),
        aggregate_descriptors[4].clone(),
        aggregate_descriptors[2].clone(),
        aggregate_descriptors[0].clone(),
        aggregate_descriptors[1].clone(),
    ];
    let reordered = plan(
        "table:Anchor",
        &declared,
        &pair_descriptors.iter().rev().cloned().collect::<Vec<_>>(),
        &reordered_aggregates,
    );
    let other_anchor = plan(
        "table:OtherAnchor",
        &declared,
        &pairs("table:OtherAnchor", &declared),
        &aggregate_descriptors,
    );

    let joins = joins_by_source(&original);
    let first = joins["table:First"];
    let gap = joins["table:Gap"];
    let second = joins["table:Second"];
    let tail = joins["table:Tail"];
    let unknown = joins["table:Unknown"];
    let fold_key = "paired_scoped_window_compaction_fold_identity";

    assert_eq!(integer(first, "planned_input_position"), 1);
    assert_eq!(integer(gap, "planned_input_position"), 2);
    assert_eq!(integer(second, "planned_input_position"), 3);
    assert_eq!(integer(tail, "planned_input_position"), 4);
    assert_eq!(integer(unknown, "planned_input_position"), 5);

    let first_fold = text(first, fold_key);
    assert_eq!(
        text(first, "paired_scoped_window_compaction_fold_transition"),
        "advanced_scoped_window_pair"
    );
    assert_eq!(
        integer(first, "paired_scoped_window_compaction_window_pair_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_scoped_window_compaction_window_stage_count"),
        2
    );
    assert_eq!(
        integer(
            first,
            "paired_scoped_window_compaction_aggregate_pair_count"
        ),
        1
    );
    assert_eq!(
        integer(
            first,
            "paired_scoped_window_compaction_aggregate_stage_count"
        ),
        2
    );
    assert_eq!(
        text(
            first,
            "paired_scoped_window_compaction_window_anchor_fold_identity"
        ),
        text(first, "window_anchor_fold_identity")
    );
    assert_eq!(
        text(
            first,
            "paired_scoped_window_compaction_aggregate_fold_identity"
        ),
        text(first, "paired_aggregate_anchor_cascade_fold_identity")
    );
    assert_eq!(
        text(
            first,
            "paired_scoped_window_compaction_latest_pair_identity"
        ),
        text(first, "paired_scoped_window_compaction_pair_identity")
    );
    assert_eq!(text(gap, fold_key), first_fold);
    assert_eq!(
        text(gap, "paired_scoped_window_compaction_fold_transition"),
        "carried_across_sparse_input"
    );
    assert_eq!(
        text(gap, "paired_scoped_window_compaction_latest_pair_identity"),
        text(first, "paired_scoped_window_compaction_pair_identity")
    );
    assert!(
        gap.details()
            .get("paired_scoped_window_compaction_pair_identity")
            .is_none(),
        "a sparse join carries the last scope but creates no new pair"
    );

    let second_fold = text(second, fold_key);
    assert_ne!(second_fold, first_fold);
    assert_eq!(
        integer(second, "paired_scoped_window_compaction_window_pair_count"),
        2
    );
    assert_eq!(
        integer(second, "paired_scoped_window_compaction_window_stage_count"),
        3
    );
    assert_eq!(
        integer(
            second,
            "paired_scoped_window_compaction_aggregate_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(
            second,
            "paired_scoped_window_compaction_aggregate_stage_count"
        ),
        3
    );
    assert_eq!(
        text(
            second,
            "paired_scoped_window_compaction_window_anchor_fold_identity"
        ),
        text(second, "window_anchor_fold_identity")
    );
    assert_eq!(
        text(
            second,
            "paired_scoped_window_compaction_aggregate_fold_identity"
        ),
        text(second, "paired_aggregate_anchor_cascade_fold_identity")
    );
    assert_eq!(
        text(
            second,
            "paired_scoped_window_compaction_latest_pair_identity"
        ),
        text(second, "paired_scoped_window_compaction_pair_identity")
    );

    let tail_fold = text(tail, fold_key);
    assert_ne!(tail_fold, second_fold);
    assert_eq!(
        integer(tail, "paired_scoped_window_compaction_window_pair_count"),
        3
    );
    assert_eq!(
        integer(tail, "paired_scoped_window_compaction_window_stage_count"),
        5
    );
    assert_eq!(
        integer(tail, "paired_scoped_window_compaction_aggregate_pair_count"),
        3
    );
    assert_eq!(
        integer(
            tail,
            "paired_scoped_window_compaction_aggregate_stage_count"
        ),
        5
    );
    assert_eq!(
        text(
            tail,
            "paired_scoped_window_compaction_window_anchor_fold_identity"
        ),
        text(tail, "window_anchor_fold_identity")
    );

    let final_fold = text(unknown, fold_key);
    assert_ne!(final_fold, tail_fold);
    assert_eq!(
        integer(unknown, "paired_scoped_window_compaction_window_pair_count"),
        4
    );
    assert_eq!(
        integer(
            unknown,
            "paired_scoped_window_compaction_window_stage_count"
        ),
        6
    );
    assert_eq!(
        integer(
            unknown,
            "paired_scoped_window_compaction_aggregate_pair_count"
        ),
        4
    );
    assert_eq!(
        integer(
            unknown,
            "paired_scoped_window_compaction_aggregate_stage_count"
        ),
        6
    );
    assert_eq!(text(unknown, fold_key), text(reordered.root(), fold_key));
    assert_eq!(
        text(unknown, "join_cost_fold_identity"),
        text(reordered.root(), "join_cost_fold_identity")
    );
    assert_ne!(final_fold, text(other_anchor.root(), fold_key));

    let mut changed_aggregates = aggregate_descriptors.clone();
    changed_aggregates[0].frame_identity = expression("frame:first-running-revised");
    let changed_frame = plan(
        "table:Anchor",
        &declared,
        &pair_descriptors,
        &changed_aggregates,
    );
    assert_ne!(final_fold, text(changed_frame.root(), fold_key));
    assert_ne!(
        text(first, "paired_scoped_window_compaction_pair_identity"),
        text(
            joins_by_source(&changed_frame)["table:First"],
            "paired_scoped_window_compaction_pair_identity"
        )
    );
}
