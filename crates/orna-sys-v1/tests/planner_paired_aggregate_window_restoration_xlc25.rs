use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, SnapshotRef,
    explain_query_with_join_pair_identities_and_window_aggregate_pushdowns,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_paired_aggregate_window_restoration_xlc25.orna");

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
    let statistics = match source {
        "table:Small" => Some(statistics(5, 4_096)),
        "table:Middle" => Some(statistics(20, 8_192)),
        "table:Large" => Some(statistics(300, 16_384)),
        "table:Unknown" => None,
        other => panic!("unexpected source {other}"),
    };
    QueryJoinDescription {
        source: object(source),
        statistics,
        predicate: Some(expression(&format!(
            "expr:anchor-{}",
            source.trim_start_matches("table:")
        ))),
    }
}

fn query(anchor: &str, order: [&str; 4]) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-aggregate-window-restoration-xlc25"),
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

fn aggregate(
    identity: &str,
    source: &str,
    operation: &str,
    frame: &str,
    start: PlanWindowFrameBound,
    end: PlanWindowFrameBound,
) -> QueryWindowAggregatePushdownDescription {
    QueryWindowAggregatePushdownDescription {
        identity: object(identity),
        source: object(source),
        aggregate: expression(operation),
        frame_identity: expression(frame),
        frame_start: start,
        frame_end: end,
    }
}

fn aggregates() -> Vec<QueryWindowAggregatePushdownDescription> {
    vec![
        aggregate(
            "window:small-running-total",
            "table:Small",
            "expr:sum-small",
            "frame:small-running",
            PlanWindowFrameBound::UnboundedPreceding,
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:small-prior-average",
            "table:Small",
            "expr:mean-small",
            "frame:small-prior-two",
            PlanWindowFrameBound::Preceding(2),
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:large-running-maximum",
            "table:Large",
            "expr:max-large",
            "frame:large-running",
            PlanWindowFrameBound::UnboundedPreceding,
            PlanWindowFrameBound::CurrentRow,
        ),
    ]
}

fn plan(
    anchor: &str,
    order: [&str; 4],
    pairs: &[QueryJoinPairIdentityDescription],
    aggregates: &[QueryWindowAggregatePushdownDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities_and_window_aggregate_pushdowns(
        &query(anchor, order),
        pairs,
        aggregates,
    )
    .expect("paired aggregate chains fold through sparse window restoration")
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
fn paired_aggregate_folds_restore_after_sparse_window_gaps() {
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
    let original = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &aggregate_descriptors,
    );
    let reordered_plan = plan(
        "table:AnchorLeft",
        reordered,
        &pairs("table:AnchorLeft", reordered)
            .into_iter()
            .rev()
            .collect::<Vec<_>>(),
        &aggregate_descriptors,
    );
    let other_anchor = plan(
        "table:AnchorRight",
        declared,
        &pairs("table:AnchorRight", declared),
        &aggregate_descriptors,
    );
    let joins = joins_by_source(&original);
    let reordered_joins = joins_by_source(&reordered_plan);
    let other_joins = joins_by_source(&other_anchor);

    let small_pair_fold = text(
        joins["table:Small"],
        "paired_aggregate_pushdown_anchor_fold_identity",
    );
    let large_pair_fold = text(
        joins["table:Large"],
        "paired_aggregate_pushdown_anchor_fold_identity",
    );
    assert_ne!(small_pair_fold, large_pair_fold);
    assert_eq!(
        small_pair_fold,
        text(
            reordered_joins["table:Small"],
            "paired_aggregate_pushdown_anchor_fold_identity"
        ),
        "cost reordering retains the exact pair's aggregate chain"
    );
    assert_ne!(
        small_pair_fold,
        text(
            other_joins["table:Small"],
            "paired_aggregate_pushdown_anchor_fold_identity"
        ),
        "the anchor participates in each per-pair aggregate fold"
    );

    let small_cascade = text(
        joins["table:Small"],
        "paired_aggregate_anchor_cascade_fold_identity",
    );
    assert_eq!(
        small_cascade,
        text(
            joins["table:Middle"],
            "paired_aggregate_anchor_cascade_fold_identity"
        ),
        "a join with no pushed window chain carries the aggregate identity unchanged"
    );
    let large_cascade = text(
        joins["table:Large"],
        "paired_aggregate_anchor_cascade_fold_identity",
    );
    assert_ne!(
        small_cascade, large_cascade,
        "the next chain restores and advances the fold"
    );
    assert_eq!(
        integer(
            joins["table:Small"],
            "paired_aggregate_anchor_cascade_pair_count"
        ),
        1
    );
    assert_eq!(
        integer(
            joins["table:Small"],
            "paired_aggregate_anchor_cascade_stage_count"
        ),
        2
    );
    assert_eq!(
        integer(
            original.root(),
            "paired_aggregate_anchor_cascade_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(
            original.root(),
            "paired_aggregate_anchor_cascade_stage_count"
        ),
        3
    );
    assert_eq!(
        text(
            joins["table:Unknown"],
            "paired_aggregate_anchor_cascade_fold_identity"
        ),
        large_cascade,
        "an unknown trailing input carries the restored fold"
    );
    assert_eq!(
        text(
            original.root(),
            "paired_aggregate_anchor_cascade_fold_identity"
        ),
        text(
            reordered_plan.root(),
            "paired_aggregate_anchor_cascade_fold_identity"
        ),
        "planned chain order remains stable when declarations and pair descriptors reorder"
    );
    assert_eq!(
        text(original.root(), "join_cost_fold_identity"),
        text(reordered_plan.root(), "join_cost_fold_identity")
    );
    assert_ne!(
        text(
            original.root(),
            "paired_aggregate_anchor_cascade_fold_identity"
        ),
        text(
            other_anchor.root(),
            "paired_aggregate_anchor_cascade_fold_identity"
        )
    );

    for (source, expected_rows, expected_aggregate_count) in
        [("table:Small", 5, 2), ("table:Large", 300, 1)]
    {
        let nodes = original
            .nodes()
            .iter()
            .filter(|node| {
                node.kind() == PlanNodeKind::Aggregate
                    && text(node, "window_source_identity") == source
            })
            .collect::<Vec<_>>();
        assert_eq!(nodes.len(), expected_aggregate_count);
        for node in nodes {
            assert_eq!(node.estimated_rows(), Some(expected_rows));
            assert_eq!(node.estimated_work(), Some(expected_rows));
            assert_eq!(
                text(node, "paired_aggregate_cascade_pair_identity"),
                if source == "table:Small" {
                    small_pair_fold
                } else {
                    large_pair_fold
                }
            );
        }
    }

    let mut changed_aggregates = aggregate_descriptors.clone();
    changed_aggregates[2].frame_identity = expression("frame:large-previous-two");
    changed_aggregates[2].frame_start = PlanWindowFrameBound::Preceding(2);
    let changed = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &changed_aggregates,
    );
    assert_ne!(
        text(
            original.root(),
            "paired_aggregate_anchor_cascade_fold_identity"
        ),
        text(
            changed.root(),
            "paired_aggregate_anchor_cascade_fold_identity"
        ),
        "a restored aggregate frame changes the downstream sparse fold"
    );
    assert_ne!(
        text(original.root(), "join_cost_fold_identity"),
        text(changed.root(), "join_cost_fold_identity")
    );
}
