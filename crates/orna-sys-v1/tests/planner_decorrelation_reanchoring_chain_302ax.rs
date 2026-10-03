use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryPartialIndexDescription,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_decorrelated_subqueries,
};

const FIXTURE: &str = include_str!("fixtures/planner_decorrelation_reanchoring_chain_302ax.orna");

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

fn join(source: &str, estimate: Option<(u64, u64)>) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics: estimate.map(|(rows, bytes)| statistics(rows, bytes)),
        predicate: None,
    }
}

fn query(anchor: &str, reverse_declarations: bool) -> QueryPlanDescription {
    let joins = if reverse_declarations {
        vec![
            join("table:Large", Some((50, 8_192))),
            join("table:Unknown", None),
            join("table:Small", Some((2, 4_096))),
        ]
    } else {
        vec![
            join("table:Small", Some((2, 4_096))),
            join("table:Unknown", None),
            join("table:Large", Some((50, 8_192))),
        ]
    };
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:decorrelation-reanchoring-chain-302ax"),
        source: object(anchor),
        source_statistics: Some(statistics(100, 4_000)),
        joins,
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    }
}

fn subqueries(reverse_declarations: bool) -> Vec<QueryDecorrelatedSubqueryDescription> {
    let orders = QueryDecorrelatedSubqueryDescription {
        identity: object("subquery:orders-for-parent"),
        source: object("table:Orders"),
        correlation_predicate: expression("expr:parent-orders"),
        statistics: Some(statistics(4, 8_192)),
    };
    let totals = QueryDecorrelatedSubqueryDescription {
        identity: object("subquery:totals-for-parent"),
        source: object("table:Totals"),
        correlation_predicate: expression("expr:parent-totals"),
        statistics: Some(statistics(6, 4_096)),
    };
    if reverse_declarations {
        vec![totals, orders]
    } else {
        vec![orders, totals]
    }
}

fn index(table: &str, identity: &str, predicate: &str) -> QueryPartialIndexDescription {
    QueryPartialIndexDescription {
        table: object(table),
        index: object(identity),
        partial_predicate: expression(predicate),
    }
}

fn indexes(reverse: bool) -> Vec<QueryPartialIndexDescription> {
    let mut indexes = vec![
        index(
            "table:Orders",
            "index:orders-by-parent",
            "expr:parent-orders",
        ),
        index(
            "table:Totals",
            "index:totals-by-parent",
            "expr:parent-totals",
        ),
        index(
            "table:Orders",
            "index:orders-for-totals-decoy",
            "expr:parent-totals",
        ),
        index(
            "table:Totals",
            "index:totals-for-orders-decoy",
            "expr:parent-orders",
        ),
    ];
    if reverse {
        indexes.reverse();
    }
    indexes
}

fn explain(
    anchor: &str,
    reverse_joins: bool,
    reverse_indexes: bool,
    reverse_subqueries: bool,
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_decorrelated_subqueries(
        &query(anchor, reverse_joins),
        &indexes(reverse_indexes),
        &subqueries(reverse_subqueries),
    )
    .expect("paired decorrelation chains explain from exact index tuples")
}

fn text<'a>(node: &'a PlanNode, name: &str) -> &'a str {
    match node.details().get(name) {
        Some(PlanDetail::Text(value)) => value,
        other => panic!("{name} should be text, got {other:?}"),
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

fn right_access<'a>(plan: &'a orna_sys_v1::ExplainedPlan, join: &PlanNode) -> &'a PlanNode {
    let reference = join.inputs()[1].as_str();
    plan.nodes()
        .iter()
        .find(|node| node.reference().as_str() == reference)
        .expect("decorrelated join keeps its selected access input")
}

fn estimates(node: &PlanNode) -> (Option<u64>, Option<u64>, Option<u64>) {
    (
        node.estimated_rows(),
        node.estimated_bytes(),
        node.estimated_work(),
    )
}

#[test]
fn paired_decorrelation_reanchoring_exposes_parent_and_child_fold_identity() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let left = explain("table:ParentLeft", false, false, false);
    let left_reordered = explain("table:ParentLeft", true, true, true);
    let right = explain("table:ParentRight", false, true, false);
    let left_joins = joins_by_subquery(&left);
    let reordered_joins = joins_by_subquery(&left_reordered);
    let right_joins = joins_by_subquery(&right);

    let orders = left_joins["subquery:orders-for-parent"];
    let totals = left_joins["subquery:totals-for-parent"];
    assert_eq!(
        text(totals, "decorrelated_anchor_fold_parent_identity"),
        text(orders, "join_cost_fold_identity"),
        "the second decorrelation reanchors on the completed first cost fold"
    );

    for (identity, index_id, input_estimates, join_estimates) in [
        (
            "subquery:orders-for-parent",
            "index:orders-by-parent",
            (Some(4), Some(8_192), Some(6)),
            (Some(80), Some(330_880), Some(204)),
        ),
        (
            "subquery:totals-for-parent",
            "index:totals-by-parent",
            (Some(6), Some(4_096), Some(7)),
            (Some(48), Some(231_312), Some(86)),
        ),
    ] {
        let left_join = left_joins[identity];
        let reordered_join = reordered_joins[identity];
        let right_join = right_joins[identity];
        let left_access = right_access(&left, left_join);
        let reordered_access = right_access(&left_reordered, reordered_join);

        assert_eq!(left_access.kind(), PlanNodeKind::IndexLookup);
        assert_eq!(left_access.object().map(ObjectRef::as_str), Some(index_id));
        assert_eq!(estimates(left_access), input_estimates);
        assert_eq!(reordered_access.kind(), PlanNodeKind::IndexLookup);
        assert_eq!(
            reordered_access.object().map(ObjectRef::as_str),
            Some(index_id)
        );
        assert_eq!(estimates(reordered_access), input_estimates);
        assert_eq!(estimates(left_join), join_estimates);
        assert_eq!(estimates(reordered_join), join_estimates);
        assert_eq!(estimates(right_join), join_estimates);
        assert_eq!(
            text(left_join, "decorrelated_anchor_fold_parent_identity"),
            text(left_join, "join_cost_fold_left_identity")
        );
        assert_eq!(
            text(left_join, "decorrelated_anchor_fold_input_identity"),
            text(left_join, "join_cost_fold_right_identity")
        );
        assert_eq!(
            text(left_access, "decorrelated_anchor_fold_parent_identity"),
            text(left_join, "decorrelated_anchor_fold_parent_identity")
        );
        assert_eq!(
            text(left_access, "decorrelated_anchor_fold_input_identity"),
            text(left_join, "decorrelated_anchor_fold_input_identity")
        );
        for detail in [
            "decorrelated_predicate_pushdown_identity",
            "decorrelated_anchor_fold_identity",
            "decorrelated_anchor_fold_parent_identity",
            "decorrelated_anchor_fold_input_identity",
        ] {
            assert_eq!(
                text(reordered_access, detail),
                text(reordered_join, detail),
                "the reordered input carries the same paired component {detail}"
            );
        }

        for detail in [
            "decorrelated_predicate_pushdown_identity",
            "decorrelated_anchor_fold_input_identity",
            "join_cost_fold_right_identity",
        ] {
            assert_eq!(
                text(left_join, detail),
                text(reordered_join, detail),
                "declaration/catalog reorder preserves the child identity at {identity}"
            );
            assert_eq!(
                text(left_join, detail),
                text(right_join, detail),
                "reanchoring changes only the parent side at {identity}"
            );
        }
        assert_eq!(
            text(left_join, "decorrelated_anchor_fold_identity"),
            text(reordered_join, "decorrelated_anchor_fold_identity")
        );
        assert_eq!(
            text(left_join, "decorrelated_anchor_fold_identity"),
            text(left_access, "decorrelated_anchor_fold_identity")
        );
        assert_eq!(
            text(left_join, "decorrelated_anchor_fold_pairing"),
            "sparse_anchor_fold_and_resolved_subquery_input"
        );
        assert_ne!(
            text(left_join, "decorrelated_anchor_fold_parent_identity"),
            text(right_join, "decorrelated_anchor_fold_parent_identity"),
            "distinct root anchors rebind the left side of the pair"
        );
        assert_ne!(
            text(left_join, "decorrelated_anchor_fold_identity"),
            text(right_join, "decorrelated_anchor_fold_identity"),
            "the combined digest changes when the anchor changes"
        );
        assert_eq!(
            text(right_join, "decorrelated_anchor_fold_parent_identity"),
            text(right_join, "join_cost_fold_left_identity")
        );
        assert_eq!(
            text(right_join, "decorrelated_anchor_fold_input_identity"),
            text(right_join, "join_cost_fold_right_identity")
        );
    }

    assert_ne!(
        text(orders, "decorrelated_anchor_fold_identity"),
        text(totals, "decorrelated_anchor_fold_identity"),
        "each point in the paired reanchoring chain retains its own fold identity"
    );
    assert_eq!(
        text(
            right_joins["subquery:totals-for-parent"],
            "decorrelated_anchor_fold_parent_identity"
        ),
        text(
            right_joins["subquery:orders-for-parent"],
            "join_cost_fold_identity"
        )
    );
}
