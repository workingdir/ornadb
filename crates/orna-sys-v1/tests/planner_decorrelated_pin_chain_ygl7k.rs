use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryPartialIndexDescription,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_decorrelated_subqueries,
};

const FIXTURE: &str = include_str!("fixtures/planner_decorrelated_pin_chain_ygl7k.orna");
const SNAPSHOT: &str = "snapshot:decorrelated-pin-chain-ygl7k";
const OTHER_SNAPSHOT: &str = "snapshot:decorrelated-pin-chain-rebound-ygl7k";

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

fn ordinary_join(source: &str) -> QueryJoinDescription {
    let (rows, bytes, predicate) = match source {
        "table:Small" => (Some(2), Some(1_000), "expr:small-filter"),
        "table:Gap" => (Some(5), Some(5_000), "expr:gap-filter"),
        "table:Unknown" => (None, None, "expr:unknown-filter"),
        other => panic!("unexpected ordinary source {other}"),
    };
    QueryJoinDescription {
        source: object(source),
        statistics: rows.zip(bytes).map(|(rows, bytes)| statistics(rows, bytes)),
        predicate: Some(expression(predicate)),
    }
}

fn query(order: [&str; 3], snapshot: &str) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive(snapshot),
        source: object("table:Anchor"),
        source_statistics: Some(statistics(100, 4_000)),
        joins: order.into_iter().map(ordinary_join).collect(),
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    }
}

fn subqueries(reverse: bool) -> Vec<QueryDecorrelatedSubqueryDescription> {
    let orders = QueryDecorrelatedSubqueryDescription {
        identity: object("subquery:orders-for-anchor"),
        source: object("table:Orders"),
        correlation_predicate: expression("expr:anchor-orders"),
        statistics: Some(statistics(6, 4_096)),
    };
    let totals = QueryDecorrelatedSubqueryDescription {
        identity: object("subquery:totals-for-anchor"),
        source: object("table:Totals"),
        correlation_predicate: expression("expr:anchor-totals"),
        statistics: Some(statistics(8, 8_192)),
    };
    if reverse {
        vec![totals, orders]
    } else {
        vec![orders, totals]
    }
}

fn index(table: &str, name: &str, predicate: &str) -> QueryPartialIndexDescription {
    QueryPartialIndexDescription {
        table: object(table),
        index: object(name),
        partial_predicate: expression(predicate),
    }
}

fn indexes(orders_index: &str) -> Vec<QueryPartialIndexDescription> {
    vec![
        index("table:Orders", orders_index, "expr:anchor-orders"),
        index("table:Decoy", "index:wrong-source", "expr:anchor-totals"),
    ]
}

fn explain(
    ordinary_order: [&str; 3],
    indexes: &[QueryPartialIndexDescription],
    reverse_subqueries: bool,
    snapshot: &str,
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_decorrelated_subqueries(
        &query(ordinary_order, snapshot),
        indexes,
        &subqueries(reverse_subqueries),
    )
    .expect("snapshot-pinned decorrelations preserve their sparse chain")
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

fn joins_by_subquery(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<&str, &PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .filter_map(|node| {
            node.details()
                .get("subquery_identity")
                .map(|_| (text(node, "subquery_identity"), node))
        })
        .collect()
}

fn ordered_joins(plan: &orna_sys_v1::ExplainedPlan) -> Vec<&PlanNode> {
    let mut joins = plan
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .collect::<Vec<_>>();
    joins.sort_by_key(|node| integer(node, "planned_input_position"));
    joins
}

fn right_access<'a>(plan: &'a orna_sys_v1::ExplainedPlan, join: &PlanNode) -> &'a PlanNode {
    let reference = join.inputs()[1].as_str();
    plan.nodes()
        .iter()
        .find(|node| node.reference().as_str() == reference)
        .expect("decorrelated join keeps its paired input")
}

fn estimate(node: &PlanNode) -> (Option<u64>, Option<u64>, Option<u64>) {
    (
        node.estimated_rows(),
        node.estimated_bytes(),
        node.estimated_work(),
    )
}

#[test]
fn decorrelated_pin_chain_pairs_index_and_omission_outcomes_across_sparse_costs() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = explain(
        ["table:Small", "table:Gap", "table:Unknown"],
        &indexes("index:orders-anchor-a"),
        false,
        SNAPSHOT,
    );
    let reordered = explain(
        ["table:Unknown", "table:Gap", "table:Small"],
        &indexes("index:orders-anchor-a")
            .into_iter()
            .rev()
            .collect::<Vec<_>>(),
        true,
        SNAPSHOT,
    );
    let rebound = explain(
        ["table:Small", "table:Gap", "table:Unknown"],
        &indexes("index:orders-anchor-z"),
        false,
        SNAPSHOT,
    );
    let repinned = explain(
        ["table:Small", "table:Gap", "table:Unknown"],
        &indexes("index:orders-anchor-a"),
        false,
        OTHER_SNAPSHOT,
    );
    let baseline_subqueries = joins_by_subquery(&baseline);
    let reordered_subqueries = joins_by_subquery(&reordered);
    let rebound_subqueries = joins_by_subquery(&rebound);
    let repinned_subqueries = joins_by_subquery(&repinned);

    let orders = baseline_subqueries["subquery:orders-for-anchor"];
    let totals = baseline_subqueries["subquery:totals-for-anchor"];
    let ordered = ordered_joins(&baseline);
    let gap = ordered[1];
    let unknown = ordered[4];

    assert_eq!(integer(gap, "planned_input_position"), 2);
    assert_eq!(integer(orders, "planned_input_position"), 3);
    assert_eq!(integer(totals, "planned_input_position"), 4);
    assert_eq!(integer(unknown, "planned_input_position"), 5);
    assert!(gap.details().get("subquery_identity").is_none());
    assert!(unknown.details().get("subquery_identity").is_none());

    assert_eq!(
        right_access(&baseline, orders).kind(),
        PlanNodeKind::IndexLookup
    );
    assert_eq!(
        right_access(&baseline, orders)
            .object()
            .map(ObjectRef::as_str),
        Some("index:orders-anchor-a")
    );
    assert_eq!(right_access(&baseline, totals).kind(), PlanNodeKind::Scan);
    assert_eq!(
        right_access(&baseline, totals)
            .object()
            .map(ObjectRef::as_str),
        Some("table:Totals")
    );
    assert_eq!(estimate(orders), (Some(6), Some(13_338), Some(16)));
    assert_eq!(estimate(gap), (Some(10), Some(15_400), Some(25)));
    assert_eq!(estimate(totals), (Some(5), Some(16_235), Some(14)));
    assert_eq!(estimate(unknown), (None, None, None));

    let orders_chain = text(orders, "decorrelated_pin_chain_identity");
    let totals_chain = text(totals, "decorrelated_pin_chain_identity");
    let unknown_chain = text(unknown, "decorrelated_pin_chain_identity");
    assert!(
        gap.details()
            .get("decorrelated_pin_chain_identity")
            .is_none()
    );
    assert_eq!(text(orders, "decorrelated_pin_chain_snapshot"), SNAPSHOT);
    assert_eq!(
        text(orders, "decorrelated_pin_chain_transition"),
        "append_resolved_subquery_pin"
    );
    assert_eq!(
        text(orders, "decorrelated_pin_chain_pairing"),
        "snapshot_pin_subquery_outcome_and_sparse_cost_parent"
    );
    assert_eq!(
        text(orders, "decorrelated_pin_chain_subquery_identity"),
        text(orders, "subquery_identity")
    );
    assert_eq!(
        text(orders, "decorrelated_pin_chain_selection_identity"),
        text(orders, "decorrelated_anchor_fold_index_selection_identity")
    );
    assert_eq!(
        text(orders, "decorrelated_pin_chain_anchor_fold_identity"),
        text(orders, "decorrelated_anchor_fold_identity")
    );
    assert_eq!(
        text(orders, "decorrelated_pin_chain_identity"),
        text(
            right_access(&baseline, orders),
            "decorrelated_pin_chain_identity"
        )
    );
    assert_eq!(
        text(totals, "decorrelated_pin_chain_transition"),
        "append_resolved_subquery_pin"
    );
    assert_eq!(
        text(totals, "decorrelated_pin_chain_parent_identity"),
        orders_chain
    );
    assert_eq!(
        text(totals, "decorrelated_pin_chain_selection_identity"),
        text(totals, "decorrelated_omission_refold_selection_identity")
    );
    assert_eq!(
        text(totals, "decorrelated_pin_chain_omission_refold_identity"),
        text(totals, "decorrelated_omission_refold_identity")
    );
    assert_ne!(
        orders_chain, totals_chain,
        "each resolved subquery appends a pin"
    );
    assert_eq!(
        text(unknown, "decorrelated_pin_chain_transition"),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        totals_chain, unknown_chain,
        "unknown tail carries the final pin"
    );

    for subquery in ["subquery:orders-for-anchor", "subquery:totals-for-anchor"] {
        assert_eq!(
            text(
                baseline_subqueries[subquery],
                "decorrelated_pin_chain_identity"
            ),
            text(
                reordered_subqueries[subquery],
                "decorrelated_pin_chain_identity"
            ),
            "catalog and declaration reordering preserve each pinned subquery chain"
        );
    }

    let changed_orders = rebound_subqueries["subquery:orders-for-anchor"];
    let changed_totals = rebound_subqueries["subquery:totals-for-anchor"];
    assert_ne!(
        text(orders, "decorrelated_pin_chain_identity"),
        text(changed_orders, "decorrelated_pin_chain_identity"),
        "rebinding the selected index changes its pin chain"
    );
    assert_ne!(
        text(totals, "decorrelated_pin_chain_identity"),
        text(changed_totals, "decorrelated_pin_chain_identity"),
        "the later omitted subquery refolds under its changed anchor"
    );
    assert_eq!(
        text(totals, "decorrelated_pin_chain_selection_identity"),
        text(changed_totals, "decorrelated_pin_chain_selection_identity"),
        "the later subquery's local omission remains paired"
    );
    assert_eq!(estimate(totals), estimate(changed_totals));
    for subquery in ["subquery:orders-for-anchor", "subquery:totals-for-anchor"] {
        assert_ne!(
            text(
                baseline_subqueries[subquery],
                "decorrelated_pin_chain_identity"
            ),
            text(
                repinned_subqueries[subquery],
                "decorrelated_pin_chain_identity"
            ),
            "a new snapshot pin rekeys the entire decorrelation chain"
        );
        assert_eq!(
            text(
                repinned_subqueries[subquery],
                "decorrelated_pin_chain_snapshot"
            ),
            OTHER_SNAPSHOT
        );
    }
}
