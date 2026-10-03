use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryLimitPushdownDescription,
    QueryPlanDescription, QuerySourceStatistics, QueryWindowAggregatePushdownDescription,
    SnapshotRef, explain_query_with_join_pair_identities_and_limit_window_aggregate_pushdowns,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_limit_window_anchor_fold_3ikfj.orna");

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

fn join(source: &str) -> QueryJoinDescription {
    let stats = match source {
        "table:Small" => Some(statistics(30, 3_000)),
        "table:Middle" => Some(statistics(50, 5_000)),
        "table:Large" => Some(statistics(1_000, 20_000)),
        "table:Unknown" => None,
        other => panic!("unexpected source {other}"),
    };
    QueryJoinDescription {
        source: object(source),
        statistics: stats,
        predicate: Some(expression(&format!(
            "expr:anchor-{}",
            source.trim_start_matches("table:")
        ))),
    }
}

fn query(anchor: &str, order: [&str; 4]) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-limit-window-anchor-fold-3ikfj"),
        source: object(anchor),
        source_statistics: Some(statistics(100, 4_000)),
        joins: order.into_iter().map(join).collect(),
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
            "expr:anchor-{}",
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

fn limit_stage(source: &str, name: &str, limit: u64) -> QueryLimitPushdownDescription {
    QueryLimitPushdownDescription {
        identity: object(&format!("limit:{name}")),
        join_pair_identity: object(&format!("pair:{}", source.trim_start_matches("table:"))),
        source: object(source),
        limit,
    }
}

fn limits(small_last: u64) -> Vec<QueryLimitPushdownDescription> {
    vec![
        limit_stage("table:Small", "small-first", 15),
        limit_stage("table:Small", "small-last", small_last),
        limit_stage("table:Large", "large-only", 100),
    ]
}

fn aggregate(identity: &str, source: &str, frame: &str) -> QueryWindowAggregatePushdownDescription {
    QueryWindowAggregatePushdownDescription {
        identity: object(identity),
        source: object(source),
        aggregate: expression(&format!("expr:sum-{identity}")),
        frame_identity: expression(frame),
        frame_start: PlanWindowFrameBound::UnboundedPreceding,
        frame_end: PlanWindowFrameBound::CurrentRow,
    }
}

fn aggregates() -> Vec<QueryWindowAggregatePushdownDescription> {
    vec![
        aggregate("window:small-running", "table:Small", "frame:small"),
        aggregate("window:small-recent", "table:Small", "frame:small-2"),
        aggregate("window:middle-running", "table:Middle", "frame:middle"),
        aggregate("window:large-running", "table:Large", "frame:large"),
        aggregate("window:unknown-running", "table:Unknown", "frame:unknown"),
    ]
}

fn plan(
    anchor: &str,
    order: [&str; 4],
    pairs: &[QueryJoinPairIdentityDescription],
    limits: &[QueryLimitPushdownDescription],
    aggregates: &[QueryWindowAggregatePushdownDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities_and_limit_window_aggregate_pushdowns(
        &query(anchor, order),
        pairs,
        limits,
        aggregates,
    )
    .expect("paired limit/window fold retains computed estimates and sparse identities")
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
fn paired_limit_folds_follow_sparse_window_anchor_chains_and_compute_outputs() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let declared = [
        "table:Large",
        "table:Unknown",
        "table:Small",
        "table:Middle",
    ];
    let reordered = [
        "table:Middle",
        "table:Small",
        "table:Unknown",
        "table:Large",
    ];
    let aggregate_descriptors = aggregates();
    let limit_descriptors = limits(10);
    let original = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &limit_descriptors,
        &aggregate_descriptors,
    );
    let reordered_plan = plan(
        "table:AnchorLeft",
        reordered,
        &pairs("table:AnchorLeft", reordered)
            .into_iter()
            .rev()
            .collect::<Vec<_>>(),
        &limit_descriptors,
        &aggregate_descriptors,
    );
    let other_anchor = plan(
        "table:AnchorRight",
        declared,
        &pairs("table:AnchorRight", declared),
        &limit_descriptors,
        &aggregate_descriptors,
    );
    let joins = joins_by_source(&original);
    let reordered_joins = joins_by_source(&reordered_plan);
    let other_joins = joins_by_source(&other_anchor);

    let small_fold = text(
        joins["table:Small"],
        "paired_limit_window_anchor_fold_identity",
    );
    let large_fold = text(
        joins["table:Large"],
        "paired_limit_window_anchor_fold_identity",
    );
    assert_eq!(
        small_fold,
        text(
            reordered_joins["table:Small"],
            "paired_limit_window_anchor_fold_identity"
        ),
        "known scan ordering and descriptor list order preserve the exact pair fold"
    );
    assert_ne!(
        small_fold,
        text(
            other_joins["table:Small"],
            "paired_limit_window_anchor_fold_identity"
        ),
        "changing the sparse anchor changes the pair fold"
    );
    assert_ne!(
        small_fold, large_fold,
        "different paired chains remain distinct"
    );
    assert_eq!(
        text(
            joins["table:Middle"],
            "paired_limit_window_cascade_fold_identity"
        ),
        text(
            joins["table:Small"],
            "paired_limit_window_cascade_fold_identity"
        ),
        "window-only middle input carries the fold without adding a limit pair"
    );
    assert_eq!(
        text(
            joins["table:Unknown"],
            "paired_limit_window_cascade_fold_identity"
        ),
        text(original.root(), "paired_limit_window_cascade_fold_identity"),
        "unknown trailing window input carries the completed fold"
    );
    assert_eq!(
        integer(original.root(), "limit_window_cascade_pair_count"),
        2
    );
    assert_eq!(
        integer(original.root(), "limit_window_cascade_stage_count"),
        3
    );
    assert_eq!(
        integer(
            original.root(),
            "limit_window_cascade_estimated_post_limit_rows"
        ),
        110
    );
    assert_eq!(
        integer(
            original.root(),
            "limit_window_cascade_estimated_post_limit_bytes"
        ),
        3_000
    );
    assert_eq!(
        integer(
            original.root(),
            "limit_window_cascade_unknown_estimate_pair_count"
        ),
        0
    );
    assert_eq!(
        text(original.root(), "limit_window_cascade_estimate_status"),
        "computed"
    );
    assert_eq!(
        text(original.root(), "paired_limit_window_cascade_fold_identity"),
        text(
            reordered_plan.root(),
            "paired_limit_window_cascade_fold_identity"
        ),
        "the stable planned pair order yields the same sparse cascade after declaration reorder"
    );
    assert_eq!(
        text(original.root(), "join_cost_fold_identity"),
        text(reordered_plan.root(), "join_cost_fold_identity")
    );

    let mut small_limits = original
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Limit
                && text(node, "limit_pushdown_source_identity") == "table:Small"
        })
        .collect::<Vec<_>>();
    small_limits.sort_by_key(|node| integer(node, "limit_pushdown_chain_position"));
    assert_eq!(small_limits.len(), 2);
    assert_eq!(integer(small_limits[0], "limit"), 15);
    assert_eq!(
        (
            small_limits[0].estimated_rows(),
            small_limits[0].estimated_bytes(),
            small_limits[0].estimated_work()
        ),
        (Some(15), Some(1_500), Some(30)),
        "the first actual limit computes its output and charges all input rows"
    );
    assert_eq!(integer(small_limits[1], "limit"), 10);
    assert_eq!(
        (
            small_limits[1].estimated_rows(),
            small_limits[1].estimated_bytes(),
            small_limits[1].estimated_work()
        ),
        (Some(10), Some(1_000), Some(15)),
        "the second limit computes the final paired cardinality and byte estimate"
    );
    let large_limit = original
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Limit
                && text(node, "limit_pushdown_source_identity") == "table:Large"
        })
        .expect("large input's exact limit stage");
    assert_eq!(
        (
            large_limit.estimated_rows(),
            large_limit.estimated_bytes(),
            large_limit.estimated_work()
        ),
        (Some(100), Some(2_000), Some(1_000))
    );

    let small_windows = original
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Aggregate
                && text(node, "window_source_identity") == "table:Small"
        })
        .collect::<Vec<_>>();
    assert_eq!(small_windows.len(), 2);
    assert!(
        small_windows.iter().all(|node| {
            text(node, "paired_limit_window_anchor_fold_identity") == small_fold
                && node.estimated_rows() == Some(10)
                && node.estimated_work() == Some(10)
        }),
        "both window operators consume the computed post-limit rows"
    );

    let changed = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &limits(9),
        &aggregate_descriptors,
    );
    assert_eq!(
        integer(
            changed.root(),
            "limit_window_cascade_estimated_post_limit_rows"
        ),
        109
    );
    assert_eq!(
        integer(
            changed.root(),
            "limit_window_cascade_estimated_post_limit_bytes"
        ),
        2_900
    );
    assert_ne!(
        text(original.root(), "paired_limit_window_cascade_fold_identity"),
        text(changed.root(), "paired_limit_window_cascade_fold_identity"),
        "changing an actual limit changes the cascade's semantic input and values"
    );
    assert_ne!(
        text(original.root(), "join_cost_fold_identity"),
        text(changed.root(), "join_cost_fold_identity")
    );
}
