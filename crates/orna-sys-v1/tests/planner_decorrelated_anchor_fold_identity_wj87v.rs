use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryPartialIndexDescription,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_decorrelated_subqueries,
};

const FIXTURE: &str = include_str!("fixtures/planner_decorrelated_anchor_fold_identity_wj87v.orna");

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

fn join(
    source: &str,
    rows: Option<u64>,
    bytes: Option<u64>,
    predicate: &str,
) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics: rows.zip(bytes).map(|(rows, bytes)| statistics(rows, bytes)),
        predicate: Some(expression(predicate)),
    }
}

fn query(anchor: &str, reverse_declarations: bool) -> QueryPlanDescription {
    let ordinary = if reverse_declarations {
        vec![
            join("table:Large", Some(20), Some(32_768), "expr:large-filter"),
            join("table:Unknown", None, None, "expr:unknown-filter"),
            join("table:Small", Some(2), Some(4_096), "expr:small-filter"),
        ]
    } else {
        vec![
            join("table:Small", Some(2), Some(4_096), "expr:small-filter"),
            join("table:Unknown", None, None, "expr:unknown-filter"),
            join("table:Large", Some(20), Some(32_768), "expr:large-filter"),
        ]
    };
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-lateral-anchor-folds-wj87v"),
        source: object(anchor),
        source_statistics: Some(statistics(100, 4_000)),
        joins: ordinary,
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
        identity: object("subquery:orders-for-anchor"),
        source: object("table:Orders"),
        correlation_predicate: expression("expr:anchor-orders"),
        statistics: Some(statistics(4, 8_192)),
    };
    let totals = QueryDecorrelatedSubqueryDescription {
        identity: object("subquery:totals-for-anchor"),
        source: object("table:Totals"),
        correlation_predicate: expression("expr:anchor-totals"),
        statistics: Some(statistics(6, 4_096)),
    };
    if reverse_declarations {
        vec![totals, orders]
    } else {
        vec![orders, totals]
    }
}

fn indexes() -> Vec<QueryPartialIndexDescription> {
    vec![
        QueryPartialIndexDescription {
            table: object("table:Orders"),
            index: object("index:orders-by-anchor"),
            partial_predicate: expression("expr:anchor-orders"),
        },
        QueryPartialIndexDescription {
            table: object("table:Totals"),
            index: object("index:totals-by-anchor"),
            partial_predicate: expression("expr:anchor-totals"),
        },
    ]
}

fn explain(anchor: &str, reverse_declarations: bool) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_decorrelated_subqueries(
        &query(anchor, reverse_declarations),
        &indexes(),
        &subqueries(reverse_declarations),
    )
    .expect("resolver-approved lateral subqueries produce an explain plan")
}

fn text<'a>(node: &'a PlanNode, name: &str) -> &'a str {
    match node.details().get(name) {
        Some(PlanDetail::Text(value)) => value,
        other => panic!("{name} should be text, got {other:?}"),
    }
}

fn joins_by_subquery<'a>(plan: &'a orna_sys_v1::ExplainedPlan) -> BTreeMap<&'a str, &'a PlanNode> {
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

fn estimated_values(node: &PlanNode) -> (Option<u64>, Option<u64>, Option<u64>) {
    (
        node.estimated_rows(),
        node.estimated_bytes(),
        node.estimated_work(),
    )
}

#[test]
fn paired_decorrelation_folds_keep_anchor_and_child_identity_through_sparse_reordering() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let left = explain("table:ParentLeft", false);
    let left_reordered = explain("table:ParentLeft", true);
    let right = explain("table:ParentRight", false);
    let left_joins = joins_by_subquery(&left);
    let reordered_joins = joins_by_subquery(&left_reordered);
    let right_joins = joins_by_subquery(&right);

    for (identity, source, index, estimates) in [
        (
            "subquery:orders-for-anchor",
            "table:Orders",
            "index:orders-by-anchor",
            (Some(8), Some(33_088), Some(24)),
        ),
        (
            "subquery:totals-for-anchor",
            "table:Totals",
            "index:totals-by-anchor",
            (Some(5), Some(24_095), Some(14)),
        ),
    ] {
        let left_join = left_joins[identity];
        let reordered_join = reordered_joins[identity];
        let right_join = right_joins[identity];
        let left_nodes = left
            .nodes()
            .iter()
            .map(|node| (node.reference().as_str().to_owned(), node))
            .collect::<BTreeMap<_, _>>();
        let reordered_nodes = left_reordered
            .nodes()
            .iter()
            .map(|node| (node.reference().as_str().to_owned(), node))
            .collect::<BTreeMap<_, _>>();
        let right_nodes = right
            .nodes()
            .iter()
            .map(|node| (node.reference().as_str().to_owned(), node))
            .collect::<BTreeMap<_, _>>();
        let left_access = left_nodes[left_join.inputs()[1].as_str()];
        let reordered_access = reordered_nodes[reordered_join.inputs()[1].as_str()];
        let right_access = right_nodes[right_join.inputs()[1].as_str()];

        assert_eq!(estimated_values(left_join), estimates);
        assert_eq!(estimated_values(reordered_join), estimates);
        assert_eq!(estimated_values(right_join), estimates);
        assert_eq!(left_access.kind(), PlanNodeKind::IndexLookup);
        assert_eq!(left_access.object().map(ObjectRef::as_str), Some(index));
        assert_eq!(text(left_access, "table"), source);
        assert_eq!(
            text(left_join, "decorrelated_anchor_fold_identity"),
            text(left_access, "decorrelated_anchor_fold_identity")
        );
        assert_eq!(
            text(left_join, "decorrelated_anchor_fold_identity"),
            text(reordered_join, "decorrelated_anchor_fold_identity"),
            "declaring distinct-cost joins in a different order preserves the paired fold"
        );
        assert_ne!(
            text(left_join, "decorrelated_anchor_fold_identity"),
            text(right_join, "decorrelated_anchor_fold_identity"),
            "equal child input identities are scoped to distinct sparse anchor folds"
        );
        assert_eq!(
            text(left_join, "decorrelated_anchor_fold_pairing"),
            "sparse_anchor_fold_and_resolved_subquery_input"
        );
        assert_eq!(
            text(left_join, "join_cost_fold_right_identity"),
            text(right_join, "join_cost_fold_right_identity"),
            "the child input identity is independent of its lateral anchor"
        );
        assert_ne!(
            text(left_join, "join_cost_fold_left_identity"),
            text(right_join, "join_cost_fold_left_identity"),
            "each lateral parent contributes its own anchor fold"
        );
        assert_eq!(
            text(reordered_join, "decorrelated_anchor_fold_identity"),
            text(reordered_access, "decorrelated_anchor_fold_identity")
        );
        assert_eq!(
            text(right_join, "decorrelated_anchor_fold_identity"),
            text(right_access, "decorrelated_anchor_fold_identity")
        );
    }

    assert_ne!(
        text(
            left_joins["subquery:orders-for-anchor"],
            "decorrelated_anchor_fold_identity"
        ),
        text(
            left_joins["subquery:totals-for-anchor"],
            "decorrelated_anchor_fold_identity"
        ),
        "paired children retain separate identities within the same sparse anchor"
    );
    assert_eq!(
        (left.root().estimated_rows(), left.root().estimated_bytes()),
        (None, None),
        "the unknown-cost tail preserves unknown output estimates after known folds"
    );
}
