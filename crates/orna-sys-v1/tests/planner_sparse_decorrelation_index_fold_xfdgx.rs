use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryPartialIndexDescription,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_decorrelated_subqueries,
};

const FIXTURE: &str = include_str!("fixtures/planner_sparse_decorrelation_index_fold_xfdgx.orna");

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
        snapshot: SnapshotRef::descriptive("snapshot:sparse-decorrelation-index-fold-xfdgx"),
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
    .expect("sparse decorrelation folds explain against exact index tuples")
}

fn text<'a>(node: &'a PlanNode, name: &str) -> &'a str {
    match node.details().get(name) {
        Some(PlanDetail::Text(value)) => value,
        other => panic!("{name} should be text, got {other:?}"),
    }
}

fn optional_text<'a>(node: &'a PlanNode, name: &str) -> Option<&'a str> {
    match node.details().get(name) {
        Some(PlanDetail::Text(value)) => Some(value),
        None => None,
        other => panic!("{name} should be text when present, got {other:?}"),
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
        .expect("decorrelated join retains its exact right access")
}

#[test]
fn paired_sparse_index_outcomes_stay_with_each_fold_across_omission_chains() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = explain(false, false, false, true);
    let reordered = explain(true, true, true, true);
    let orders_omitted = explain(false, false, false, false);
    let baseline_joins = joins_by_subquery(&baseline);
    let reordered_joins = joins_by_subquery(&reordered);
    let omitted_joins = joins_by_subquery(&orders_omitted);

    for (identity, source, selected_index, predicate, estimates) in [
        (
            "subquery:orders-for-account",
            "table:Orders",
            Some("index:orders-by-account"),
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
        let access_kind = if selected_index.is_some() {
            PlanNodeKind::IndexLookup
        } else {
            PlanNodeKind::Scan
        };
        for (join, access) in [(before, before_access), (after, after_access)] {
            assert_eq!(access.kind(), access_kind, "access path for {identity}");
            assert_eq!(
                access.object().map(ObjectRef::as_str),
                selected_index.or(Some(source)),
                "exact index outcome remains paired with {identity}"
            );
            assert_eq!(
                (
                    access.estimated_rows(),
                    access.estimated_bytes(),
                    access.estimated_work(),
                ),
                estimates,
                "the selected access or omission scan returns the real source estimates"
            );
            assert_eq!(text(access, "correlation_predicate_identity"), predicate);
            assert_eq!(
                text(join, "decorrelated_anchor_fold_index_selection_identity"),
                text(access, "decorrelated_anchor_fold_index_selection_identity"),
                "the join fold and its access expose the same exact sparse selection"
            );
            assert_eq!(
                text(join, "decorrelated_anchor_fold_parent_identity"),
                text(join, "join_cost_fold_left_identity")
            );
            assert_eq!(
                text(join, "decorrelated_anchor_fold_input_identity"),
                text(join, "join_cost_fold_right_identity")
            );
            assert!(
                text(join, "decorrelated_anchor_fold_index_selection_identity")
                    .starts_with("decorrelated-index-fold:")
            );
        }
        for detail in [
            "decorrelated_anchor_fold_index_selection_identity",
            "decorrelated_index_selection_omission_identity",
            "decorrelated_anchor_fold_parent_identity",
            "decorrelated_anchor_fold_input_identity",
            "decorrelated_anchor_fold_identity",
            "join_cost_fold_right_identity",
            "join_cost_fold_identity",
        ] {
            assert_eq!(
                optional_text(before, detail),
                optional_text(after, detail),
                "catalog, declaration, and sparse join reordering preserve {detail} for {identity}"
            );
        }
    }

    let orders = baseline_joins["subquery:orders-for-account"];
    let orders_without_index = omitted_joins["subquery:orders-for-account"];
    assert_eq!(
        right_access(&orders_omitted, orders_without_index).kind(),
        PlanNodeKind::Scan
    );
    assert!(optional_text(orders, "decorrelated_index_selection_omission_identity").is_none());
    assert!(
        optional_text(
            orders_without_index,
            "decorrelated_index_selection_omission_identity"
        )
        .is_some()
    );
    for detail in [
        "decorrelated_anchor_fold_index_selection_identity",
        "decorrelated_anchor_fold_input_identity",
        "decorrelated_anchor_fold_identity",
        "join_cost_fold_right_identity",
    ] {
        assert_ne!(
            text(orders, detail),
            text(orders_without_index, detail),
            "switching Orders from its exact index to an omission rekeys its own fold ({detail})"
        );
    }
    assert_eq!(
        (
            orders.estimated_rows(),
            orders.estimated_bytes(),
            orders.estimated_work(),
        ),
        (
            orders_without_index.estimated_rows(),
            orders_without_index.estimated_bytes(),
            orders_without_index.estimated_work(),
        ),
        "index omission changes the plan identity while retaining computed estimates"
    );

    for identity in [
        "subquery:invoices-for-account",
        "subquery:payments-for-account",
    ] {
        let before = baseline_joins[identity];
        let after = omitted_joins[identity];
        assert_eq!(
            text(before, "decorrelated_anchor_fold_index_selection_identity"),
            text(after, "decorrelated_anchor_fold_index_selection_identity"),
            "later local selection stays bound to {identity} when Orders becomes omitted"
        );
        assert_eq!(
            text(before, "join_cost_fold_right_identity"),
            text(after, "join_cost_fold_right_identity"),
            "later child input identity stays local to {identity}"
        );
        assert_ne!(
            text(before, "decorrelated_anchor_fold_parent_identity"),
            text(after, "decorrelated_anchor_fold_parent_identity"),
            "later fold ancestry records the changed prior selection before {identity}"
        );
        assert_ne!(
            text(before, "decorrelated_anchor_fold_identity"),
            text(after, "decorrelated_anchor_fold_identity"),
            "later composed fold rekeys on its changed parent ancestry"
        );
    }
}
