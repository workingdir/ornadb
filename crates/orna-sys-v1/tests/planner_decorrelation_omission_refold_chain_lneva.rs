use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryPartialIndexDescription,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_decorrelated_subqueries,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_decorrelation_omission_refold_chain_lneva.orna");

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

fn ordinary_join(source: &str, estimate: Option<(u64, u64)>) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics: estimate.map(|(rows, bytes)| statistics(rows, bytes)),
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
        snapshot: SnapshotRef::descriptive("snapshot:decorrelation-omission-refold-chain-lneva"),
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
        statistics: Some(statistics(2, 4_096)),
    };
    let invoices = QueryDecorrelatedSubqueryDescription {
        identity: object("subquery:invoices-for-account"),
        source: object("table:Invoices"),
        correlation_predicate: expression("expr:account-invoices"),
        statistics: Some(statistics(4, 8_192)),
    };
    let payments = QueryDecorrelatedSubqueryDescription {
        identity: object("subquery:payments-for-account"),
        source: object("table:Payments"),
        correlation_predicate: expression("expr:account-payments"),
        statistics: Some(statistics(8, 4_096)),
    };
    if reverse_declarations {
        vec![payments, invoices, orders]
    } else {
        vec![orders, invoices, payments]
    }
}

fn index(table: &str, identity: &str, predicate: &str) -> QueryPartialIndexDescription {
    QueryPartialIndexDescription {
        table: object(table),
        index: object(identity),
        partial_predicate: expression(predicate),
    }
}

fn indexes(orders_indexed: bool, reverse_catalog: bool) -> Vec<QueryPartialIndexDescription> {
    let mut candidates = vec![
        index(
            "table:Orders",
            "index:orders-for-invoices-decoy",
            "expr:account-invoices",
        ),
        index(
            "table:Invoices",
            "index:invoices-for-other-predicate",
            "expr:account-id-invoices",
        ),
        index(
            "table:Payments",
            "index:payments-by-account",
            "expr:account-payments",
        ),
    ];
    if orders_indexed {
        candidates.push(index(
            "table:Orders",
            "index:orders-by-account",
            "expr:account-orders",
        ));
    }
    if reverse_catalog {
        candidates.reverse();
    }
    candidates
}

fn explain(
    reverse_joins: bool,
    reverse_catalog: bool,
    reverse_subqueries: bool,
    orders_indexed: bool,
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_decorrelated_subqueries(
        &query(reverse_joins),
        &indexes(orders_indexed, reverse_catalog),
        &subqueries(reverse_subqueries),
    )
    .expect("paired decorrelation omissions explain through sparse cost folds")
}

fn text<'a>(node: &'a PlanNode, key: &str) -> &'a str {
    match node.details().get(key) {
        Some(PlanDetail::Text(value)) => value,
        other => panic!("{key} should be text, got {other:?}"),
    }
}

fn optional_text<'a>(node: &'a PlanNode, key: &str) -> Option<&'a str> {
    match node.details().get(key) {
        Some(PlanDetail::Text(value)) => Some(value),
        None => None,
        other => panic!("{key} should be text when present, got {other:?}"),
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
        .expect("decorrelated join retains its right access")
}

fn cost(node: &PlanNode) -> (Option<u64>, Option<u64>, Option<u64>) {
    (
        node.estimated_rows(),
        node.estimated_bytes(),
        node.estimated_work(),
    )
}

#[test]
fn decorrelated_omission_refolds_keep_local_pairs_and_follow_changed_cost_ancestry() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = explain(false, false, false, false);
    let reordered = explain(true, true, true, false);
    let orders_indexed = explain(false, false, false, true);
    let baseline_joins = joins_by_subquery(&baseline);
    let reordered_joins = joins_by_subquery(&reordered);
    let indexed_joins = joins_by_subquery(&orders_indexed);

    for (identity, source, selected_index, predicate, input_cost) in [
        (
            "subquery:orders-for-account",
            "table:Orders",
            None,
            "expr:account-orders",
            (Some(2), Some(4_096), Some(3)),
        ),
        (
            "subquery:invoices-for-account",
            "table:Invoices",
            None,
            "expr:account-invoices",
            (Some(4), Some(8_192), Some(6)),
        ),
        (
            "subquery:payments-for-account",
            "table:Payments",
            Some("index:payments-by-account"),
            "expr:account-payments",
            (Some(8), Some(4_096), Some(9)),
        ),
    ] {
        let before = baseline_joins[identity];
        let after = reordered_joins[identity];
        let before_access = right_access(&baseline, before);
        let after_access = right_access(&reordered, after);
        let expected_kind = if selected_index.is_some() {
            PlanNodeKind::IndexLookup
        } else {
            PlanNodeKind::Scan
        };
        for (join, access) in [(before, before_access), (after, after_access)] {
            assert_eq!(access.kind(), expected_kind);
            assert_eq!(
                access.object().map(ObjectRef::as_str),
                selected_index.or(Some(source))
            );
            assert_eq!(
                (
                    access.estimated_rows(),
                    access.estimated_bytes(),
                    access.estimated_work(),
                ),
                input_cost,
                "the input exposes its computed pinned estimates for {identity}"
            );
            assert_eq!(text(access, "correlation_predicate_identity"), predicate);
            assert_eq!(
                text(join, "decorrelated_anchor_fold_parent_identity"),
                text(join, "join_cost_fold_left_identity")
            );
            assert_eq!(
                text(join, "decorrelated_anchor_fold_input_identity"),
                text(join, "join_cost_fold_right_identity")
            );
            assert_eq!(
                text(join, "decorrelated_anchor_fold_index_selection_identity"),
                text(access, "decorrelated_anchor_fold_index_selection_identity")
            );
        }
        for detail in [
            "decorrelated_anchor_fold_parent_identity",
            "decorrelated_anchor_fold_input_identity",
            "decorrelated_anchor_fold_index_selection_identity",
            "decorrelated_anchor_fold_identity",
            "decorrelated_omission_refold_identity",
            "decorrelated_omission_refold_omission_identity",
            "decorrelated_omission_refold_selection_identity",
            "join_cost_fold_right_identity",
            "join_cost_fold_identity",
        ] {
            assert_eq!(
                optional_text(before, detail),
                optional_text(after, detail),
                "catalog, declaration, and sparse join reordering preserve {detail} for {identity}"
            );
        }
        assert_eq!(cost(before), cost(after));
    }

    for identity in [
        "subquery:orders-for-account",
        "subquery:invoices-for-account",
    ] {
        let omitted = baseline_joins[identity];
        let access = right_access(&baseline, omitted);
        assert_eq!(access.kind(), PlanNodeKind::Scan);
        assert_eq!(
            text(omitted, "decorrelated_index_selection_omission_identity"),
            text(access, "decorrelated_index_selection_omission_identity")
        );
        assert!(
            text(omitted, "decorrelated_omission_refold_identity")
                .starts_with("decorrelated-omission-refold:")
        );
        assert_eq!(
            text(omitted, "decorrelated_omission_refold_parent_identity"),
            text(omitted, "join_cost_fold_left_identity")
        );
        assert_eq!(
            text(omitted, "decorrelated_omission_refold_input_identity"),
            text(omitted, "join_cost_fold_right_identity")
        );
        assert_eq!(
            text(omitted, "decorrelated_omission_refold_subquery_identity"),
            identity
        );
        assert_eq!(
            text(omitted, "decorrelated_omission_refold_omission_identity"),
            text(omitted, "decorrelated_index_selection_omission_identity")
        );
        assert_eq!(
            text(omitted, "decorrelated_omission_refold_selection_identity"),
            text(omitted, "decorrelated_anchor_fold_index_selection_identity")
        );
        assert_eq!(
            text(omitted, "decorrelated_omission_refold_pairing"),
            "sparse_parent_fold_and_unindexed_subquery_input"
        );
        assert_eq!(
            text(access, "decorrelated_omission_refold_identity"),
            text(omitted, "decorrelated_omission_refold_identity")
        );
    }
    assert!(
        optional_text(
            baseline_joins["subquery:payments-for-account"],
            "decorrelated_omission_refold_identity"
        )
        .is_none()
    );

    let orders_before = baseline_joins["subquery:orders-for-account"];
    let orders_after = indexed_joins["subquery:orders-for-account"];
    assert_eq!(
        right_access(&orders_indexed, orders_after)
            .object()
            .map(ObjectRef::as_str),
        Some("index:orders-by-account")
    );
    assert!(optional_text(orders_before, "decorrelated_omission_refold_identity").is_some());
    assert!(optional_text(orders_after, "decorrelated_omission_refold_identity").is_none());
    assert_eq!(cost(orders_before), cost(orders_after));

    for identity in [
        "subquery:invoices-for-account",
        "subquery:payments-for-account",
    ] {
        let before = baseline_joins[identity];
        let after = indexed_joins[identity];
        assert_eq!(
            text(before, "decorrelated_anchor_fold_index_selection_identity"),
            text(after, "decorrelated_anchor_fold_index_selection_identity"),
            "local index or omission selection remains attached to {identity}"
        );
        assert_eq!(
            text(before, "join_cost_fold_right_identity"),
            text(after, "join_cost_fold_right_identity"),
            "the child input is local to {identity}"
        );
        assert_ne!(
            text(before, "decorrelated_anchor_fold_parent_identity"),
            text(after, "decorrelated_anchor_fold_parent_identity"),
            "the changed first selection flows into later ancestry at {identity}"
        );
        assert_ne!(
            text(before, "decorrelated_anchor_fold_identity"),
            text(after, "decorrelated_anchor_fold_identity")
        );
        assert_ne!(
            text(before, "join_cost_fold_identity"),
            text(after, "join_cost_fold_identity")
        );
        assert_eq!(
            cost(before),
            cost(after),
            "real estimates remain stable for {identity}"
        );
        if identity == "subquery:invoices-for-account" {
            assert_eq!(
                text(before, "decorrelated_index_selection_omission_identity"),
                text(after, "decorrelated_index_selection_omission_identity"),
                "a later omission keeps its own resolver tuple"
            );
            assert_eq!(
                text(before, "decorrelated_omission_refold_omission_identity"),
                text(after, "decorrelated_omission_refold_omission_identity")
            );
            assert_ne!(
                text(before, "decorrelated_omission_refold_identity"),
                text(after, "decorrelated_omission_refold_identity"),
                "the omission refolds against its changed parent"
            );
        }
    }
}
