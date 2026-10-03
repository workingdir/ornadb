use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, SnapshotRef,
    explain_query_with_join_pair_identities_and_window_aggregate_pushdowns,
};

const FIXTURE: &str = include_str!("fixtures/planner_window_anchor_fold_identity_wv99y.orna");

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
        snapshot: SnapshotRef::descriptive("snapshot:paired-window-anchor-fold-wv99y"),
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

fn plan(anchor: &str, order: [&str; 4]) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities_and_window_aggregate_pushdowns(
        &query(anchor, order),
        &pairs(anchor, order),
        &aggregates(anchor),
    )
    .expect("paired anchor and exact-source frame chains produce a plan")
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

fn joins_by_pair<'a>(plan: &'a orna_sys_v1::ExplainedPlan) -> BTreeMap<&'a str, &'a PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| (text(node, "join_pair_identity"), node))
        .collect()
}

#[test]
fn paired_window_folds_keep_sparse_anchor_frame_chains_bound_to_their_inputs() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let left = plan(
        "table:ParentLeft",
        [
            "table:Large",
            "table:Unknown",
            "table:Small",
            "table:Middle",
        ],
    );
    let left_reordered = plan(
        "table:ParentLeft",
        [
            "table:Middle",
            "table:Small",
            "table:Unknown",
            "table:Large",
        ],
    );
    let right = plan(
        "table:ParentRight",
        [
            "table:Large",
            "table:Unknown",
            "table:Small",
            "table:Middle",
        ],
    );
    let left_joins = joins_by_pair(&left);
    let reordered_joins = joins_by_pair(&left_reordered);
    let right_joins = joins_by_pair(&right);
    let left_nodes = left
        .nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect::<BTreeMap<_, _>>();
    let expected = [
        (
            "pair:Small",
            "table:Small",
            Some(50),
            Some(43_000),
            Some(105),
        ),
        (
            "pair:Middle",
            "table:Middle",
            Some(100),
            Some(127_000),
            Some(70),
        ),
        (
            "pair:Large",
            "table:Large",
            Some(3_000),
            Some(3_975_000),
            Some(400),
        ),
        ("pair:Unknown", "table:Unknown", None, None, None),
    ];

    for (pair_id, source, rows, bytes, work) in expected {
        let left_join = left_joins[pair_id];
        let reordered_join = reordered_joins[pair_id];
        let right_join = right_joins[pair_id];
        assert_eq!(text(left_join, "logical_right_source_identity"), source);
        assert_eq!(
            (
                left_join.estimated_rows(),
                left_join.estimated_bytes(),
                left_join.estimated_work()
            ),
            (rows, bytes, work),
            "window identity metadata preserves computed values for {pair_id}"
        );
        assert_eq!(
            text(left_join, "window_anchor_fold_identity"),
            text(reordered_join, "window_anchor_fold_identity"),
            "declaration reordering preserves the paired sparse frame fold"
        );
        assert_eq!(
            text(left_join, "join_cost_fold_identity"),
            text(reordered_join, "join_cost_fold_identity"),
            "declaration reordering preserves the accumulated cost fold"
        );
        assert_ne!(
            text(left_join, "window_anchor_fold_identity"),
            text(right_join, "window_anchor_fold_identity"),
            "different anchors produce distinct frame fold identities"
        );
        assert_ne!(
            text(left_join, "join_cost_fold_identity"),
            text(right_join, "join_cost_fold_identity"),
            "the accumulated cost fold includes its anchor-scoped frame identity"
        );
        assert_eq!(
            text(left_join, "window_anchor_fold_pairing"),
            "sparse_anchor_fold_and_exact_source_frame_chain"
        );
        assert_eq!(
            text(left_join, "window_pushdown_chain_identity"),
            text(right_join, "window_pushdown_chain_identity"),
            "the exact child frame chain remains stable across anchors"
        );
        assert_eq!(
            text(left_join, "join_cost_fold_right_identity"),
            text(right_join, "join_cost_fold_right_identity")
        );
        assert_ne!(
            text(left_join, "join_cost_fold_left_identity"),
            text(right_join, "join_cost_fold_left_identity")
        );

        let left_access = left_nodes[left_join.inputs()[1].as_str()];
        assert_eq!(
            text(left_access, "window_anchor_fold_identity"),
            text(left_join, "window_anchor_fold_identity")
        );
        assert_eq!(
            text(left_access, "paired_join_cost_fold_identity"),
            text(left_join, "join_cost_fold_identity")
        );

        let chain_id = text(left_join, "window_pushdown_chain_identity");
        let chain_aggregates = left
            .nodes()
            .iter()
            .filter(|node| {
                node.kind() == PlanNodeKind::Aggregate
                    && node.details().contains_key("window_aggregate_identity")
                    && text(node, "window_source_identity") == source
                    && text(node, "window_pushdown_chain_identity") == chain_id
            })
            .collect::<Vec<_>>();
        assert!(!chain_aggregates.is_empty());
        for aggregate in chain_aggregates {
            assert_eq!(
                text(aggregate, "window_anchor_fold_identity"),
                text(left_join, "window_anchor_fold_identity"),
                "every frame operator in the source chain stays paired to its anchor"
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
        ["pair:Small", "pair:Middle", "pair:Large", "pair:Unknown"],
        planned
            .iter()
            .map(|node| text(node, "join_pair_identity"))
            .collect::<Vec<_>>()
            .as_slice(),
        "known frames fold in estimated-cost order before the sparse unknown tail"
    );
    assert_eq!(
        (left.root().estimated_rows(), left.root().estimated_bytes()),
        (None, None),
        "the sparse unknown tail keeps output cardinality unknown"
    );
}
