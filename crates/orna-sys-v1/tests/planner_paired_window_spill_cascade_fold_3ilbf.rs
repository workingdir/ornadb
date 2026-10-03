use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, QueryWindowSpillDescription,
    SnapshotRef, explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_window_spill_cascade_fold_3ilbf.orna");

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

fn query(anchor: &str, order: [&str; 4]) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-window-spill-cascade-3ilbf"),
        source: object(anchor),
        source_statistics: Some(statistics(40, 40_000)),
        joins: order
            .into_iter()
            .map(|source| QueryJoinDescription {
                source: object(source),
                statistics: match source {
                    "table:Small" => Some(statistics(50, 6_400)),
                    "table:Middle" => Some(statistics(20, 16_384)),
                    "table:Large" => Some(statistics(300, 65_536)),
                    "table:Unknown" => None,
                    other => panic!("unexpected source {other}"),
                },
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

fn pairs(anchor: &str, order: [&str; 4]) -> Vec<QueryJoinPairIdentityDescription> {
    order
        .into_iter()
        .map(|source| pair(anchor, source))
        .collect()
}

fn aggregate(identity: &str, source: &str) -> QueryWindowAggregatePushdownDescription {
    QueryWindowAggregatePushdownDescription {
        identity: object(identity),
        source: object(source),
        aggregate: expression("expr:sum-value"),
        frame_identity: expression("frame:running"),
        frame_start: PlanWindowFrameBound::UnboundedPreceding,
        frame_end: PlanWindowFrameBound::CurrentRow,
    }
}

fn aggregates() -> Vec<QueryWindowAggregatePushdownDescription> {
    vec![
        aggregate("window:small-total", "table:Small"),
        aggregate("window:small-count", "table:Small"),
        aggregate("window:large-total", "table:Large"),
        aggregate("window:unknown-total", "table:Unknown"),
    ]
}

fn spill(
    identity: &str,
    pair: &str,
    source: &str,
    aggregate: &str,
    working_set: Option<u64>,
    budget: u64,
) -> QueryWindowSpillDescription {
    QueryWindowSpillDescription {
        identity: object(identity),
        join_pair_identity: object(pair),
        source: object(source),
        window_aggregate_identity: object(aggregate),
        estimated_working_set_bytes: working_set,
        memory_budget_bytes: budget,
    }
}

fn spills() -> Vec<QueryWindowSpillDescription> {
    vec![
        spill(
            "spill:small-total",
            "pair:Small",
            "table:Small",
            "window:small-total",
            Some(16_385),
            8_192,
        ),
        spill(
            "spill:small-count",
            "pair:Small",
            "table:Small",
            "window:small-count",
            Some(4_000),
            4_096,
        ),
        spill(
            "spill:large-total",
            "pair:Large",
            "table:Large",
            "window:large-total",
            Some(20_000),
            8_000,
        ),
    ]
}

fn plan(
    anchor: &str,
    order: [&str; 4],
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
    .expect("resolved pairs and aggregate spill estimates produce a plan")
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

fn joins_by_pair(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<&str, &PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| (text(node, "join_pair_identity"), node))
        .collect()
}

#[test]
fn paired_window_spill_cascade_fold_sums_sparse_pairs_and_survives_reordering() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let declared = [
        "table:Large",
        "table:Unknown",
        "table:Small",
        "table:Middle",
    ];
    let aggregate_descriptors = aggregates();
    let spill_descriptors = spills();
    let original = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &aggregate_descriptors,
        &spill_descriptors,
    );
    let reordered = [
        "table:Middle",
        "table:Small",
        "table:Unknown",
        "table:Large",
    ];
    let reordered_plan = plan(
        "table:AnchorLeft",
        reordered,
        &pairs("table:AnchorLeft", reordered)
            .into_iter()
            .rev()
            .collect::<Vec<_>>(),
        &aggregate_descriptors,
        &spill_descriptors.iter().rev().cloned().collect::<Vec<_>>(),
    );
    let other_anchor = plan(
        "table:AnchorRight",
        declared,
        &pairs("table:AnchorRight", declared),
        &aggregate_descriptors,
        &spill_descriptors,
    );

    let root = original.root();
    let reordered_root = reordered_plan.root();
    let other_root = other_anchor.root();
    assert_eq!(root.kind(), PlanNodeKind::Join);
    assert_eq!(integer(root, "window_spill_cascade_pair_count"), 2);
    assert_eq!(integer(root, "window_spill_cascade_stage_count"), 3);
    assert_eq!(
        integer(root, "window_spill_cascade_estimated_bytes"),
        20_193
    );
    assert_eq!(integer(root, "window_spill_cascade_estimated_io_blocks"), 6);
    assert_eq!(integer(root, "window_spill_cascade_estimated_io_work"), 12);
    assert_eq!(
        text(root, "window_spill_cascade_estimate_status"),
        "computed"
    );
    assert!(matches!(
        root.details().get("window_spill_cascade_overflowed"),
        Some(PlanDetail::Boolean(false))
    ));
    assert_eq!(
        text(root, "paired_window_spill_cascade_fold_pairing"),
        "planned_pair_order_sparse_spill_anchor_chains"
    );
    assert_eq!(
        text(root, "paired_window_spill_cascade_fold_identity"),
        text(reordered_root, "paired_window_spill_cascade_fold_identity"),
        "resolved pair order and totals stay stable when declarations reverse"
    );
    assert_eq!(
        text(root, "join_cost_fold_identity"),
        text(reordered_root, "join_cost_fold_identity")
    );
    assert_ne!(
        text(root, "paired_window_spill_cascade_fold_identity"),
        text(other_root, "paired_window_spill_cascade_fold_identity"),
        "the accumulated anchor is part of the cascade identity"
    );

    let joins = joins_by_pair(&original);
    assert_eq!(
        text(
            joins["pair:Unknown"],
            "paired_window_spill_cascade_fold_identity"
        ),
        text(root, "paired_window_spill_cascade_fold_identity"),
        "a trailing join without spill carries the sparse spill cascade forward"
    );

    let mut changed_spills = spill_descriptors.clone();
    changed_spills[0].memory_budget_bytes = 8_193;
    let changed = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &aggregate_descriptors,
        &changed_spills,
    );
    assert_eq!(
        integer(changed.root(), "window_spill_cascade_estimated_bytes"),
        20_192
    );
    assert_eq!(
        integer(changed.root(), "window_spill_cascade_estimated_io_blocks"),
        5
    );
    assert_eq!(
        integer(changed.root(), "window_spill_cascade_estimated_io_work"),
        10
    );
    assert_ne!(
        text(root, "paired_window_spill_cascade_fold_identity"),
        text(changed.root(), "paired_window_spill_cascade_fold_identity")
    );
    assert_ne!(
        text(root, "join_cost_fold_identity"),
        text(changed.root(), "join_cost_fold_identity")
    );

    let mut unknown_spills = spill_descriptors.clone();
    unknown_spills.push(spill(
        "spill:unknown-total",
        "pair:Unknown",
        "table:Unknown",
        "window:unknown-total",
        None,
        512,
    ));
    let unknown_plan = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &aggregate_descriptors,
        &unknown_spills,
    );
    assert_eq!(
        integer(unknown_plan.root(), "window_spill_cascade_pair_count"),
        3
    );
    assert_eq!(
        integer(
            unknown_plan.root(),
            "window_spill_cascade_unknown_working_set_count"
        ),
        1
    );
    assert_eq!(
        text(unknown_plan.root(), "window_spill_cascade_estimate_status"),
        "unknown_working_set"
    );
    assert!(
        unknown_plan
            .root()
            .details()
            .get("window_spill_cascade_estimated_bytes")
            .is_none()
    );

    let mut overflowing_spills = spill_descriptors.clone();
    for spill in &mut overflowing_spills {
        spill.estimated_working_set_bytes = Some(u64::MAX);
        spill.memory_budget_bytes = 0;
    }
    let overflowing_plan = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &aggregate_descriptors,
        &overflowing_spills,
    );
    assert!(matches!(
        overflowing_plan
            .root()
            .details()
            .get("window_spill_cascade_overflowed"),
        Some(PlanDetail::Boolean(true))
    ));
    assert_eq!(
        text(
            overflowing_plan.root(),
            "window_spill_cascade_estimate_status"
        ),
        "overflow"
    );
    assert!(
        overflowing_plan
            .root()
            .details()
            .get("window_spill_cascade_estimated_bytes")
            .is_none()
    );
}
