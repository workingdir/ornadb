use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryPartialIndexDescription,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_decorrelated_subqueries,
};

const FIXTURE: &str = include_str!("fixtures/planner_sparse_index_omission_chain_yc0us.orna");

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

fn query(reverse_declarations: bool) -> QueryPlanDescription {
    let joins = if reverse_declarations {
        vec![
            join("table:Large", Some((100, 4_096))),
            join("table:Unknown", None),
            join("table:Small", Some((1, 4_096))),
        ]
    } else {
        vec![
            join("table:Small", Some((1, 4_096))),
            join("table:Unknown", None),
            join("table:Large", Some((100, 4_096))),
        ]
    };
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:sparse-index-omission-chain-yc0us"),
        source: object("table:AccountRoot"),
        source_statistics: Some(statistics(100, 4_096)),
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
        identity: object("subquery:orders-by-account"),
        source: object("table:Orders"),
        correlation_predicate: expression("expr:account-orders"),
        statistics: Some(statistics(2, 4_096)),
    };
    let invoices = QueryDecorrelatedSubqueryDescription {
        identity: object("subquery:invoices-by-account"),
        source: object("table:Invoices"),
        correlation_predicate: expression("expr:account-invoices"),
        statistics: Some(statistics(4, 8_192)),
    };
    let payments = QueryDecorrelatedSubqueryDescription {
        identity: object("subquery:payments-by-account"),
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

fn indexes(orders: bool, invoices: bool, payments: bool) -> Vec<QueryPartialIndexDescription> {
    let mut candidates = vec![
        // These decoys deliberately cover the omitted Invoices predicate but
        // do not match its exact source and predicate together.
        index(
            "table:Orders",
            "index:orders-for-invoices-decoy",
            "expr:account-invoices",
        ),
        index(
            "table:Archive",
            "index:archive-for-invoices-decoy",
            "expr:account-invoices",
        ),
        index(
            "table:Invoices",
            "index:invoices-for-other-predicate",
            "expr:account-id-invoices",
        ),
    ];
    if orders {
        candidates.push(index(
            "table:Orders",
            "index:orders-by-account",
            "expr:account-orders",
        ));
    }
    if invoices {
        candidates.push(index(
            "table:Invoices",
            "index:invoices-by-account",
            "expr:account-invoices",
        ));
    }
    if payments {
        candidates.push(index(
            "table:Payments",
            "index:payments-by-account",
            "expr:account-payments",
        ));
    }
    candidates
}

fn explain(
    reverse_joins: bool,
    reverse_catalog: bool,
    reverse_subqueries: bool,
    orders: bool,
    invoices: bool,
    payments: bool,
) -> orna_sys_v1::ExplainedPlan {
    let mut candidates = indexes(orders, invoices, payments);
    if reverse_catalog {
        candidates.reverse();
    }
    explain_query_with_partial_indexes_and_decorrelated_subqueries(
        &query(reverse_joins),
        &candidates,
        &subqueries(reverse_subqueries),
    )
    .expect("paired sparse decors explain when some exact index candidates are omitted")
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
        .expect("decorrelated join retains its right access")
}

#[test]
fn exact_index_omission_stays_with_its_subquery_across_sparse_reordering() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = explain(false, false, false, true, false, true);
    let reordered = explain(true, true, true, true, false, true);
    let baseline_joins = joins_by_subquery(&baseline);
    let reordered_joins = joins_by_subquery(&reordered);
    let mut omission_ids = Vec::new();

    for (identity, source, index_id, predicate, estimates) in [
        (
            "subquery:orders-by-account",
            "table:Orders",
            Some("index:orders-by-account"),
            "expr:account-orders",
            (Some(2), Some(4_096), Some(3)),
        ),
        (
            "subquery:invoices-by-account",
            "table:Invoices",
            None,
            "expr:account-invoices",
            (Some(4), Some(8_192), Some(6)),
        ),
        (
            "subquery:payments-by-account",
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
        let expected_kind = if index_id.is_some() {
            PlanNodeKind::IndexLookup
        } else {
            PlanNodeKind::Scan
        };
        for (join, access) in [(before, before_access), (after, after_access)] {
            assert_eq!(access.kind(), expected_kind, "right access for {identity}");
            assert_eq!(
                access.object().map(ObjectRef::as_str),
                index_id.or(Some(source))
            );
            assert_eq!(
                (
                    access.estimated_rows(),
                    access.estimated_bytes(),
                    access.estimated_work(),
                ),
                estimates,
                "source estimate remains the actual selected input value for {identity}"
            );
            if index_id.is_some() {
                assert_eq!(text(access, "table"), source);
            }
            assert_eq!(text(access, "correlation_predicate_identity"), predicate);
            if index_id.is_some() {
                assert!(
                    optional_text(join, "decorrelated_index_selection_omission_identity").is_none()
                );
                assert!(
                    optional_text(access, "decorrelated_index_selection_omission_identity")
                        .is_none()
                );
                assert!(optional_text(join, "decorrelated_predicate_pushdown_identity").is_some());
            } else {
                assert!(optional_text(join, "decorrelated_predicate_pushdown_identity").is_none());
                assert!(
                    optional_text(access, "decorrelated_predicate_pushdown_identity").is_none()
                );
                let join_omission = text(join, "decorrelated_index_selection_omission_identity");
                assert_eq!(
                    text(access, "decorrelated_index_selection_omission_identity"),
                    join_omission
                );
                assert_eq!(
                    text(join, "decorrelated_index_selection_omission_reason"),
                    "no_exact_table_and_predicate_candidate"
                );
            }
        }
        if index_id.is_none() {
            omission_ids
                .push(text(before, "decorrelated_index_selection_omission_identity").to_owned());
        }
        for detail in [
            "decorrelated_index_selection_omission_identity",
            "decorrelated_predicate_pushdown_identity",
            "decorrelated_anchor_fold_identity",
            "join_cost_fold_right_identity",
            "join_cost_fold_identity",
        ] {
            assert_eq!(
                optional_text(before, detail),
                optional_text(after, detail),
                "catalog, declaration and sparse-input reordering preserve {detail} for {identity}"
            );
        }
    }
    assert_eq!(omission_ids.len(), 1);
    assert!(omission_ids[0].starts_with("decorrelated-index-omission:"));
}

#[test]
fn omission_markers_keep_paired_folds_aligned_instead_of_shifting_later_indexes() {
    let one_omission = explain(false, false, false, true, false, true);
    let two_omissions = explain(false, true, false, false, false, true);
    let invoice_indexed = explain(false, false, false, true, true, true);
    let one = joins_by_subquery(&one_omission);
    let two = joins_by_subquery(&two_omissions);
    let indexed = joins_by_subquery(&invoice_indexed);

    let orders_one = one["subquery:orders-by-account"];
    let orders_two = two["subquery:orders-by-account"];
    assert_eq!(
        right_access(&one_omission, orders_one).kind(),
        PlanNodeKind::IndexLookup
    );
    assert_eq!(
        right_access(&two_omissions, orders_two).kind(),
        PlanNodeKind::Scan
    );
    assert!(optional_text(orders_one, "decorrelated_index_selection_omission_identity").is_none());
    assert!(optional_text(orders_two, "decorrelated_index_selection_omission_identity").is_some());

    let invoices_one = one["subquery:invoices-by-account"];
    let invoices_two = two["subquery:invoices-by-account"];
    let invoices_indexed = indexed["subquery:invoices-by-account"];
    assert_eq!(
        text(
            invoices_one,
            "decorrelated_index_selection_omission_identity"
        ),
        text(
            invoices_two,
            "decorrelated_index_selection_omission_identity"
        ),
        "each omitted resolver pair keeps its own stable omission identity"
    );
    assert_eq!(
        text(invoices_one, "join_cost_fold_right_identity"),
        text(invoices_two, "join_cost_fold_right_identity"),
        "earlier index omission does not shift or replace the invoice right input"
    );
    assert_ne!(
        text(invoices_one, "decorrelated_anchor_fold_identity"),
        text(invoices_two, "decorrelated_anchor_fold_identity"),
        "the invoice fold still records the changed earlier sparse ancestry"
    );
    assert_eq!(
        right_access(&invoice_indexed, invoices_indexed).kind(),
        PlanNodeKind::IndexLookup
    );
    assert!(
        optional_text(
            invoices_indexed,
            "decorrelated_index_selection_omission_identity"
        )
        .is_none()
    );
    assert!(optional_text(invoices_indexed, "decorrelated_predicate_pushdown_identity").is_some());

    let payments_one = one["subquery:payments-by-account"];
    let payments_two = two["subquery:payments-by-account"];
    let payments_indexed = indexed["subquery:payments-by-account"];
    for plan in [&one_omission, &two_omissions, &invoice_indexed] {
        let joins = joins_by_subquery(plan);
        let payments = joins["subquery:payments-by-account"];
        assert_eq!(
            right_access(plan, payments).object().map(ObjectRef::as_str),
            Some("index:payments-by-account"),
            "a prior omission never shifts the next exact candidate onto Payments"
        );
    }
    assert_eq!(
        text(payments_one, "decorrelated_predicate_pushdown_identity"),
        text(payments_two, "decorrelated_predicate_pushdown_identity")
    );
    assert_eq!(
        text(payments_one, "decorrelated_predicate_pushdown_identity"),
        text(payments_indexed, "decorrelated_predicate_pushdown_identity")
    );
    assert_ne!(
        text(payments_one, "decorrelated_anchor_fold_identity"),
        text(payments_two, "decorrelated_anchor_fold_identity")
    );
    assert_ne!(
        text(payments_one, "decorrelated_anchor_fold_identity"),
        text(payments_indexed, "decorrelated_anchor_fold_identity")
    );
    assert_eq!(
        (
            invoices_one.estimated_rows(),
            invoices_one.estimated_bytes(),
            invoices_one.estimated_work(),
        ),
        (
            invoices_indexed.estimated_rows(),
            invoices_indexed.estimated_bytes(),
            invoices_indexed.estimated_work(),
        ),
        "index availability changes identity and access kind, not computed join estimates"
    );
}
