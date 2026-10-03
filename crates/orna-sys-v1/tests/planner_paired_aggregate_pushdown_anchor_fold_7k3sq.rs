use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, SnapshotRef,
    explain_query_with_join_pair_identities_and_window_aggregate_pushdowns,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_paired_aggregate_pushdown_anchor_fold_7k3sq.orna");

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
    let (rows, bytes) = match source {
        "table:Large" => (Some(300), Some(16_384)),
        "table:Unknown" => (None, None),
        "table:Small" => (Some(5), Some(4_096)),
        "table:Middle" => (Some(20), Some(8_192)),
        other => panic!("unexpected source {other}"),
    };
    QueryJoinDescription {
        source: object(source),
        statistics: rows.zip(bytes).map(|(rows, bytes)| statistics(rows, bytes)),
        predicate: Some(expression(&format!(
            "expr:anchor-{}",
            source.trim_start_matches("table:")
        ))),
    }
}

fn query(anchor: &str, order: [&str; 4]) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-aggregate-anchor-fold-7k3sq"),
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

fn aggregates(anchor: &str) -> Vec<QueryWindowAggregatePushdownDescription> {
    let anchor_name = anchor.trim_start_matches("table:");
    vec![
        aggregate(
            &format!("window:{anchor_name}-running-total"),
            anchor,
            "expr:sum-anchor",
            &format!("frame:{anchor_name}-running"),
            PlanWindowFrameBound::UnboundedPreceding,
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:small-average",
            "table:Small",
            "expr:mean-small",
            "frame:small-previous-through-current",
            PlanWindowFrameBound::Preceding(2),
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:small-maximum",
            "table:Small",
            "expr:max-small",
            "frame:small-forward",
            PlanWindowFrameBound::CurrentRow,
            PlanWindowFrameBound::Following(1),
        ),
        aggregate(
            "window:middle-count",
            "table:Middle",
            "expr:count-middle",
            "frame:middle-all",
            PlanWindowFrameBound::UnboundedPreceding,
            PlanWindowFrameBound::UnboundedFollowing,
        ),
        aggregate(
            "window:large-maximum",
            "table:Large",
            "expr:max-large",
            "frame:large-running",
            PlanWindowFrameBound::UnboundedPreceding,
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:unknown-total",
            "table:Unknown",
            "expr:sum-unknown",
            "frame:unknown-current",
            PlanWindowFrameBound::CurrentRow,
            PlanWindowFrameBound::CurrentRow,
        ),
    ]
}

fn plan_with(
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
    .expect("paired aggregate pushdowns stay bound to exact sparse join inputs")
}

fn plan(anchor: &str, order: [&str; 4]) -> orna_sys_v1::ExplainedPlan {
    plan_with(anchor, order, &pairs(anchor, order), &aggregates(anchor))
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

fn joins_by_source<'a>(plan: &'a orna_sys_v1::ExplainedPlan) -> BTreeMap<&'a str, &'a PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| (text(node, "logical_right_source_identity"), node))
        .collect()
}

#[test]
fn paired_aggregate_pushdown_folds_follow_sparse_anchor_and_pair_order() {
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
    let left = plan("table:ParentLeft", declared);
    let left_reordered = plan("table:ParentLeft", reordered);
    let right = plan("table:ParentRight", declared);
    let left_joins = joins_by_source(&left);
    let reordered_joins = joins_by_source(&left_reordered);
    let right_joins = joins_by_source(&right);
    let left_nodes = left
        .nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect::<BTreeMap<_, _>>();
    let expected = [
        ("table:Small", Some(50), Some(43_000), Some(105)),
        ("table:Middle", Some(100), Some(127_000), Some(70)),
        ("table:Large", Some(3_000), Some(3_975_000), Some(400)),
        ("table:Unknown", None, None, None),
    ];

    for (source, rows, bytes, work) in expected {
        let left_join = left_joins[source];
        let reordered_join = reordered_joins[source];
        let right_join = right_joins[source];
        let paired_fold = text(left_join, "paired_aggregate_pushdown_anchor_fold_identity");

        assert_eq!(
            (
                left_join.estimated_rows(),
                left_join.estimated_bytes(),
                left_join.estimated_work()
            ),
            (rows, bytes, work),
            "the adapter computes the expected values for {source}"
        );
        assert_eq!(
            paired_fold,
            text(
                reordered_join,
                "paired_aggregate_pushdown_anchor_fold_identity"
            ),
            "the pair and aggregate chain remain stable after sparse cost reordering"
        );
        assert_ne!(
            paired_fold,
            text(right_join, "paired_aggregate_pushdown_anchor_fold_identity"),
            "the same pair and aggregate chain under another anchor has another fold"
        );
        assert_eq!(
            text(left_join, "paired_aggregate_pushdown_anchor_fold_pairing"),
            "sparse_anchor_fold_resolved_join_pair_and_aggregate_chain"
        );
        assert_eq!(
            text(left_join, "window_pushdown_chain_identity"),
            text(right_join, "window_pushdown_chain_identity"),
            "the exact right-side aggregate chain stays stable across anchors"
        );
        assert_eq!(
            text(left_join, "join_cost_fold_identity"),
            text(reordered_join, "join_cost_fold_identity")
        );
        assert_ne!(
            text(left_join, "join_cost_fold_identity"),
            text(right_join, "join_cost_fold_identity")
        );

        let right_input = left_nodes[left_join.inputs()[1].as_str()];
        assert_eq!(
            text(
                right_input,
                "paired_aggregate_pushdown_anchor_fold_identity"
            ),
            paired_fold
        );
        let chain_nodes = left
            .nodes()
            .iter()
            .filter(|node| {
                node.kind() == PlanNodeKind::Aggregate
                    && text(node, "window_source_identity") == source
                    && text(node, "window_pushdown_chain_identity")
                        == text(left_join, "window_pushdown_chain_identity")
            })
            .collect::<Vec<_>>();
        assert!(!chain_nodes.is_empty(), "{source} has pushed aggregates");
        for aggregate in chain_nodes {
            assert_eq!(
                text(aggregate, "paired_aggregate_pushdown_anchor_fold_identity"),
                paired_fold,
                "each aggregate operator carries the pair's anchor fold"
            );
        }
    }

    let mut planned = left
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .collect::<Vec<_>>();
    planned.sort_by_key(|node| integer(node, "planned_input_position"));
    assert_eq!(
        [
            "table:Small",
            "table:Middle",
            "table:Large",
            "table:Unknown"
        ],
        planned
            .iter()
            .map(|node| text(node, "logical_right_source_identity"))
            .collect::<Vec<_>>()
            .as_slice(),
        "known aggregate work is planned before the sparse unknown tail"
    );
    assert_eq!(
        (left.root().estimated_rows(), left.root().estimated_bytes()),
        (None, None)
    );

    let mut alternate_pairs = pairs("table:ParentLeft", declared);
    alternate_pairs
        .iter_mut()
        .find(|pair| pair.right_source.as_str() == "table:Small")
        .unwrap()
        .identity = object("pair:Small:alternate");
    let alternate_pair_plan = plan_with(
        "table:ParentLeft",
        declared,
        &alternate_pairs,
        &aggregates("table:ParentLeft"),
    );
    assert_ne!(
        text(
            left_joins["table:Small"],
            "paired_aggregate_pushdown_anchor_fold_identity"
        ),
        text(
            joins_by_source(&alternate_pair_plan)["table:Small"],
            "paired_aggregate_pushdown_anchor_fold_identity"
        ),
        "changing the resolver pair changes the paired aggregate fold"
    );

    let mut alternate_aggregates = aggregates("table:ParentLeft");
    alternate_aggregates
        .iter_mut()
        .find(|aggregate| aggregate.identity.as_str() == "window:small-average")
        .unwrap()
        .identity = object("window:small-average:alternate");
    let alternate_aggregate_plan = plan_with(
        "table:ParentLeft",
        declared,
        &pairs("table:ParentLeft", declared),
        &alternate_aggregates,
    );
    assert_ne!(
        text(
            left_joins["table:Small"],
            "paired_aggregate_pushdown_anchor_fold_identity"
        ),
        text(
            joins_by_source(&alternate_aggregate_plan)["table:Small"],
            "paired_aggregate_pushdown_anchor_fold_identity"
        ),
        "changing a pushed aggregate's resolver identity changes the paired fold"
    );
}
