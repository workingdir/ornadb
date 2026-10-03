use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryPartialIndexDescription,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_decorrelated_subqueries,
};

const FIXTURE: &str = include_str!("fixtures/planner_sparse_index_decorrelation_pairs_om5bb.orna");

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

fn ordinary_join(source: &str, stats: Option<(u64, u64)>) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics: stats.map(|(rows, bytes)| statistics(rows, bytes)),
        predicate: None,
    }
}

fn query(reverse_declarations: bool) -> QueryPlanDescription {
    let joins = if reverse_declarations {
        vec![
            ordinary_join("table:Large", Some((40, 16_384))),
            ordinary_join("table:Unknown", None),
            ordinary_join("table:Small", Some((2, 4_096))),
        ]
    } else {
        vec![
            ordinary_join("table:Small", Some((2, 4_096))),
            ordinary_join("table:Unknown", None),
            ordinary_join("table:Large", Some((40, 16_384))),
        ]
    };
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:sparse-index-decorrelation-pairs-om5bb"),
        source: object("table:Accounts"),
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
        identity: object("subquery:orders-for-account"),
        source: object("table:Orders"),
        correlation_predicate: expression("expr:account-orders"),
        statistics: Some(statistics(4, 8_192)),
    };
    let invoices = QueryDecorrelatedSubqueryDescription {
        identity: object("subquery:invoices-for-account"),
        source: object("table:Invoices"),
        correlation_predicate: expression("expr:account-invoices"),
        statistics: Some(statistics(6, 4_096)),
    };
    if reverse_declarations {
        vec![invoices, orders]
    } else {
        vec![orders, invoices]
    }
}

fn indexes(add_preferred_invoice_index: bool) -> Vec<QueryPartialIndexDescription> {
    let mut candidates = vec![
        index(
            "table:Orders",
            "index:orders-by-account-z",
            "expr:account-orders",
        ),
        index(
            "table:Orders",
            "index:orders-by-account-a",
            "expr:account-orders",
        ),
        index(
            "table:Invoices",
            "index:invoices-by-account-z",
            "expr:account-invoices",
        ),
        index(
            "table:Invoices",
            "index:invoices-by-account-a",
            "expr:account-invoices",
        ),
        // Same predicate on a different source and same source with a
        // different predicate are not eligible exact pairs.
        index(
            "table:Orders",
            "index:orders-for-invoices-decoy",
            "expr:account-invoices",
        ),
        index(
            "table:Invoices",
            "index:invoices-for-orders-decoy",
            "expr:account-orders",
        ),
        index(
            "table:Orders",
            "index:orders-for-other-predicate",
            "expr:other-account-filter",
        ),
    ];
    if add_preferred_invoice_index {
        candidates.push(index(
            "table:Invoices",
            "index:invoices-by-account-0",
            "expr:account-invoices",
        ));
    }
    candidates
}

fn index(table: &str, identity: &str, predicate: &str) -> QueryPartialIndexDescription {
    QueryPartialIndexDescription {
        table: object(table),
        index: object(identity),
        partial_predicate: expression(predicate),
    }
}

fn explain(
    reverse_joins: bool,
    reverse_catalog: bool,
    reverse_subqueries: bool,
    add_preferred_invoice_index: bool,
) -> orna_sys_v1::ExplainedPlan {
    let mut candidates = indexes(add_preferred_invoice_index);
    if reverse_catalog {
        candidates.reverse();
    }
    explain_query_with_partial_indexes_and_decorrelated_subqueries(
        &query(reverse_joins),
        &candidates,
        &subqueries(reverse_subqueries),
    )
    .expect("sparse paired decorrelations explain with exact index candidates")
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
        .expect("decorrelated join keeps its selected right access input")
}

#[test]
fn paired_decorrelations_keep_exact_sparse_index_identity_through_catalog_and_join_reordering() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = explain(false, false, false, false);
    let reordered = explain(true, true, true, false);
    let baseline_joins = joins_by_subquery(&baseline);
    let reordered_joins = joins_by_subquery(&reordered);

    for (identity, table, selected_index, predicate, estimates) in [
        (
            "subquery:orders-for-account",
            "table:Orders",
            "index:orders-by-account-a",
            "expr:account-orders",
            (Some(4), Some(8_192), Some(6)),
        ),
        (
            "subquery:invoices-for-account",
            "table:Invoices",
            "index:invoices-by-account-a",
            "expr:account-invoices",
            (Some(6), Some(4_096), Some(7)),
        ),
    ] {
        let before = baseline_joins[identity];
        let after = reordered_joins[identity];
        let before_access = right_access(&baseline, before);
        let after_access = right_access(&reordered, after);
        for join in [before, after] {
            assert_eq!(
                text(join, "index_selection_tie_break"),
                "lexicographically_smallest_matching_index_identity",
                "the paired fold reports how its selected index was resolved"
            );
        }
        for access in [before_access, after_access] {
            assert_eq!(access.kind(), PlanNodeKind::IndexLookup);
            assert_eq!(access.object().map(ObjectRef::as_str), Some(selected_index));
            assert_eq!(text(access, "table"), table);
            assert_eq!(text(access, "partial_predicate_identity"), predicate);
            assert_eq!(
                text(access, "index_selection_tie_break"),
                "lexicographically_smallest_matching_index_identity"
            );
            assert_eq!(
                (
                    access.estimated_rows(),
                    access.estimated_bytes(),
                    access.estimated_work(),
                ),
                estimates,
                "selected index keeps the resolver-provided estimates for {identity}"
            );
        }
        for detail in [
            "predicate_pushdown_identity",
            "decorrelated_predicate_pushdown_identity",
            "decorrelated_anchor_fold_identity",
            "join_cost_fold_right_identity",
            "join_cost_fold_identity",
        ] {
            assert_eq!(
                text(before, detail),
                text(after, detail),
                "catalog, declaration, and sparse-input reordering preserve {detail} for {identity}"
            );
        }
        assert_eq!(
            text(before_access, "decorrelated_predicate_pushdown_identity"),
            text(before, "decorrelated_predicate_pushdown_identity")
        );
    }

    assert_ne!(
        text(
            baseline_joins["subquery:orders-for-account"],
            "decorrelated_predicate_pushdown_identity"
        ),
        text(
            baseline_joins["subquery:invoices-for-account"],
            "decorrelated_predicate_pushdown_identity"
        ),
        "the two exact source/index/predicate tuples remain distinct"
    );
    assert_ne!(
        text(
            baseline_joins["subquery:orders-for-account"],
            "decorrelated_anchor_fold_identity"
        ),
        text(
            baseline_joins["subquery:invoices-for-account"],
            "decorrelated_anchor_fold_identity"
        ),
        "separate decorrelation inputs retain separate sparse fold ancestry"
    );
}

#[test]
fn changing_second_selected_index_rekeys_its_pair_and_fold_without_rekeying_first_pair() {
    let baseline = explain(false, false, false, false);
    let changed = explain(false, true, false, true);
    let baseline_joins = joins_by_subquery(&baseline);
    let changed_joins = joins_by_subquery(&changed);

    let first_before = baseline_joins["subquery:orders-for-account"];
    let first_after = changed_joins["subquery:orders-for-account"];
    for detail in [
        "decorrelated_predicate_pushdown_identity",
        "decorrelated_anchor_fold_identity",
        "join_cost_fold_identity",
    ] {
        assert_eq!(
            text(first_before, detail),
            text(first_after, detail),
            "a later invoice index choice cannot change the earlier Orders pair ({detail})"
        );
    }

    let invoice_before = baseline_joins["subquery:invoices-for-account"];
    let invoice_after = changed_joins["subquery:invoices-for-account"];
    assert_eq!(
        right_access(&changed, invoice_after)
            .object()
            .map(ObjectRef::as_str),
        Some("index:invoices-by-account-0")
    );
    for detail in [
        "decorrelated_predicate_pushdown_identity",
        "decorrelated_anchor_fold_identity",
        "join_cost_fold_right_identity",
        "join_cost_fold_identity",
    ] {
        assert_ne!(
            text(invoice_before, detail),
            text(invoice_after, detail),
            "the changed invoice index rekeys its exact pair and dependent fold ({detail})"
        );
    }
    assert_eq!(
        (
            invoice_before.estimated_rows(),
            invoice_before.estimated_bytes(),
            invoice_before.estimated_work(),
        ),
        (
            invoice_after.estimated_rows(),
            invoice_after.estimated_bytes(),
            invoice_after.estimated_work(),
        ),
        "changing the selected index identity preserves computed join estimates"
    );
}
