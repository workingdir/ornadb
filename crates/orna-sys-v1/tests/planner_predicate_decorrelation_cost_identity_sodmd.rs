use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryPartialIndexDescription,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_decorrelated_subqueries,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_predicate_decorrelation_cost_identity_sodmd.orna");
const CASCADE_FIXTURE: &str =
    include_str!("fixtures/planner_decorrelated_pair_cost_cascade_k1e2d.orna");

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

fn query(order: [&str; 5]) -> QueryPlanDescription {
    let joins = order
        .into_iter()
        .map(|source| match source {
            "table:Expensive" => join(source, Some(500), Some(64_000), "expr:broad-live"),
            "table:Unknown" => join(source, None, None, "expr:unknown-live"),
            "table:Small" => join(source, Some(8), Some(8_192), "expr:small-live"),
            "table:Medium" => join(source, Some(30), Some(4_096), "expr:medium-live"),
            "table:Orders" => join(source, Some(3), Some(4_096), "expr:owner-orders"),
            other => panic!("unexpected join source {other}"),
        })
        .collect();
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:predicate-decorrelation-cost-sodmd"),
        source: object("table:Anchor"),
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

fn indexes() -> Vec<QueryPartialIndexDescription> {
    vec![
        QueryPartialIndexDescription {
            table: object("table:Small"),
            index: object("index:small-active"),
            partial_predicate: expression("expr:small-live"),
        },
        QueryPartialIndexDescription {
            table: object("table:Orders"),
            index: object("index:owner-orders"),
            partial_predicate: expression("expr:owner-orders"),
        },
        QueryPartialIndexDescription {
            table: object("table:Wrong"),
            index: object("index:orders-decoy"),
            partial_predicate: expression("expr:owner-orders"),
        },
    ]
}

fn subquery(identity: &str) -> QueryDecorrelatedSubqueryDescription {
    QueryDecorrelatedSubqueryDescription {
        identity: object(identity),
        source: object("table:Orders"),
        correlation_predicate: expression("expr:owner-orders"),
        statistics: Some(statistics(4, 8_192)),
    }
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
        other => panic!("{key} should be integer, got {other:?}"),
    }
}

fn explain(
    order: [&str; 5],
    indexes: &[QueryPartialIndexDescription],
    subquery: &QueryDecorrelatedSubqueryDescription,
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_decorrelated_subqueries(
        &query(order),
        indexes,
        std::slice::from_ref(subquery),
    )
    .expect("exact predicate and resolver-approved subquery produce a paired plan")
}

fn indexes_with_orders_index(index: &str) -> Vec<QueryPartialIndexDescription> {
    let mut indexes = indexes();
    indexes
        .iter_mut()
        .find(|candidate| candidate.table.as_str() == "table:Orders")
        .expect("the fixture declares the exact Orders index")
        .index = object(index);
    indexes
}

fn cascade_indexes(orders_index: &str) -> Vec<QueryPartialIndexDescription> {
    let mut indexes = indexes_with_orders_index(orders_index);
    indexes.push(QueryPartialIndexDescription {
        table: object("table:Invoices"),
        index: object("index:owner-invoices"),
        partial_predicate: expression("expr:owner-invoices"),
    });
    indexes
}

fn cascade_subqueries() -> Vec<QueryDecorrelatedSubqueryDescription> {
    vec![
        QueryDecorrelatedSubqueryDescription {
            identity: object("subquery:orders-owner"),
            source: object("table:Orders"),
            correlation_predicate: expression("expr:owner-orders"),
            statistics: Some(statistics(4, 8_192)),
        },
        QueryDecorrelatedSubqueryDescription {
            identity: object("subquery:invoices-owner"),
            source: object("table:Invoices"),
            correlation_predicate: expression("expr:owner-invoices"),
            statistics: Some(statistics(2, 2_048)),
        },
    ]
}

fn cascade_query(order: [&str; 5]) -> QueryPlanDescription {
    let mut query = query(order);
    query
        .joins
        .iter_mut()
        .find(|join| join.source.as_str() == "table:Orders")
        .expect("the query includes an ordinary Orders join")
        .predicate = Some(expression("expr:ordinary-orders"));
    query
}

fn explain_cascade(
    query: &QueryPlanDescription,
    indexes: &[QueryPartialIndexDescription],
    subqueries: &[QueryDecorrelatedSubqueryDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_decorrelated_subqueries(query, indexes, subqueries)
        .expect("each exact resolver pair selects its own partial-index input")
}

fn joins_by_label<'a>(plan: &'a orna_sys_v1::ExplainedPlan) -> BTreeMap<&'a str, &'a PlanNode> {
    let nodes = plan
        .nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect::<BTreeMap<_, _>>();
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| {
            let label = node
                .details()
                .get("subquery_identity")
                .map(|_| text(node, "subquery_identity"))
                .unwrap_or_else(|| {
                    let access = nodes[node.inputs()[1].as_str()];
                    if access.kind() == PlanNodeKind::IndexLookup {
                        text(access, "table")
                    } else {
                        access.object().expect("scan source identity").as_str()
                    }
                });
            (label, node)
        })
        .collect()
}

#[test]
fn predicate_and_decorrelation_identities_follow_sparse_cost_ordering() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let subquery = subquery("subquery:orders-for-owner");
    let baseline = explain(
        [
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
            "table:Orders",
        ],
        &indexes(),
        &subquery,
    );
    let reordered_indexes = indexes().into_iter().rev().collect::<Vec<_>>();
    let reordered = explain(
        [
            "table:Medium",
            "table:Small",
            "table:Orders",
            "table:Unknown",
            "table:Expensive",
        ],
        &reordered_indexes,
        &subquery,
    );
    let baseline_joins = joins_by_label(&baseline);
    let reordered_joins = joins_by_label(&reordered);

    let expected_order = [
        "table:Orders",
        "subquery:orders-for-owner",
        "table:Small",
        "table:Medium",
        "table:Expensive",
        "table:Unknown",
    ];
    let mut planned = baseline_joins
        .iter()
        .map(|(label, node)| (*label, *node))
        .collect::<Vec<_>>();
    planned.sort_by_key(|(_, node)| integer(node, "planned_input_position"));
    let observed_order = planned.iter().map(|(label, _)| *label).collect::<Vec<_>>();
    assert_eq!(observed_order, expected_order);

    for label in [
        "table:Orders",
        "subquery:orders-for-owner",
        "table:Small",
        "table:Medium",
        "table:Expensive",
        "table:Unknown",
    ] {
        let before = baseline_joins[label];
        let after = reordered_joins[label];
        assert_eq!(
            text(before, "join_cost_fold_right_identity"),
            text(after, "join_cost_fold_right_identity"),
            "right-input identity remains bound to {label} across declaration order"
        );
        assert_eq!(
            text(before, "join_cost_fold_identity"),
            text(after, "join_cost_fold_identity"),
            "cost-ordered fold identity remains stable for {label}"
        );
        assert_eq!(
            (
                before.estimated_rows(),
                before.estimated_bytes(),
                before.estimated_work(),
            ),
            (
                after.estimated_rows(),
                after.estimated_bytes(),
                after.estimated_work(),
            )
        );
    }

    let ordinary = baseline_joins["table:Orders"];
    let decorrelated = baseline_joins["subquery:orders-for-owner"];
    let nodes = baseline
        .nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect::<BTreeMap<_, _>>();
    let ordinary_access = nodes[ordinary.inputs()[1].as_str()];
    let decorrelated_access = nodes[decorrelated.inputs()[1].as_str()];
    assert_eq!(ordinary_access.kind(), PlanNodeKind::IndexLookup);
    assert_eq!(decorrelated_access.kind(), PlanNodeKind::IndexLookup);
    assert_eq!(
        ordinary_access.object().map(ObjectRef::as_str),
        Some("index:owner-orders")
    );
    assert_eq!(
        decorrelated_access.object().map(ObjectRef::as_str),
        Some("index:owner-orders")
    );
    assert_eq!(
        text(ordinary, "predicate_pushdown_identity"),
        text(decorrelated, "predicate_pushdown_identity"),
        "the exact same selected table/index/predicate tuple keeps one index identity"
    );
    assert!(
        ordinary
            .details()
            .get("decorrelated_predicate_pushdown_identity")
            .is_none()
    );
    let paired_identity = text(decorrelated, "decorrelated_predicate_pushdown_identity");
    assert!(paired_identity.starts_with("decorrelated-pushdown:"));
    assert_eq!(
        text(
            decorrelated_access,
            "decorrelated_predicate_pushdown_identity"
        ),
        paired_identity,
        "the lookup and decorrelated fold retain one composite identity"
    );
    assert_eq!(
        text(decorrelated, "decorrelated_predicate_pushdown_pairing"),
        "resolver_subquery_and_exact_index_pair"
    );
    assert_eq!(
        text(decorrelated, "subquery_identity"),
        "subquery:orders-for-owner"
    );
    assert_eq!(
        text(decorrelated, "correlation_predicate_identity"),
        "expr:owner-orders"
    );
    assert_eq!(
        (
            decorrelated.estimated_rows(),
            decorrelated.estimated_bytes(),
            decorrelated.estimated_work(),
        ),
        (Some(12), Some(41_448), Some(34))
    );
    assert_eq!(
        (
            decorrelated_access.estimated_rows(),
            decorrelated_access.estimated_bytes(),
            decorrelated_access.estimated_work(),
        ),
        (Some(4), Some(8_192), Some(6))
    );
    assert_eq!(
        (
            baseline.root().estimated_rows(),
            baseline.root().estimated_bytes()
        ),
        (None, None),
        "the unknown-cost tail preserves unknown estimates"
    );
}

#[test]
fn changing_decorrelation_identity_rekeys_its_predicate_pair_and_dependent_folds() {
    let indexes = indexes();
    let baseline = explain(
        [
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
            "table:Orders",
        ],
        &indexes,
        &subquery("subquery:orders-for-owner"),
    );
    let changed = explain(
        [
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
            "table:Orders",
        ],
        &indexes,
        &subquery("subquery:orders-for-owner-v2"),
    );
    let baseline_joins = joins_by_label(&baseline);
    let changed_joins = joins_by_label(&changed);

    for label in [
        "subquery:orders-for-owner",
        "table:Small",
        "table:Medium",
        "table:Expensive",
        "table:Unknown",
    ] {
        let changed_label = if label == "subquery:orders-for-owner" {
            "subquery:orders-for-owner-v2"
        } else {
            label
        };
        let before = baseline_joins[label];
        let after = changed_joins[changed_label];
        assert_ne!(
            text(before, "join_cost_fold_identity"),
            text(after, "join_cost_fold_identity"),
            "the changed logical input identity rekeys its fold and all later folds"
        );
        assert_eq!(
            (
                before.estimated_rows(),
                before.estimated_bytes(),
                before.estimated_work(),
            ),
            (
                after.estimated_rows(),
                after.estimated_bytes(),
                after.estimated_work(),
            ),
            "identity changes preserve real cardinality and work values"
        );
    }
    assert_eq!(
        text(baseline_joins["table:Orders"], "join_cost_fold_identity"),
        text(changed_joins["table:Orders"], "join_cost_fold_identity"),
        "the ordinary same-predicate join remains outside the changed decorrelation chain"
    );
    assert_ne!(
        text(
            baseline_joins["subquery:orders-for-owner"],
            "decorrelated_predicate_pushdown_identity"
        ),
        text(
            changed_joins["subquery:orders-for-owner-v2"],
            "decorrelated_predicate_pushdown_identity"
        )
    );
}

#[test]
fn changing_the_selected_index_rekeys_the_decorrelation_cost_fold_chain() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let mut query = query([
        "table:Expensive",
        "table:Unknown",
        "table:Small",
        "table:Medium",
        "table:Orders",
    ]);
    let ordinary_orders = query
        .joins
        .iter_mut()
        .find(|join| join.source.as_str() == "table:Orders")
        .expect("the query includes an ordinary Orders join");
    ordinary_orders.predicate = Some(expression("expr:ordinary-orders"));

    let subquery = subquery("subquery:orders-for-owner");
    let baseline = explain_query_with_partial_indexes_and_decorrelated_subqueries(
        &query,
        &indexes_with_orders_index("index:owner-orders"),
        std::slice::from_ref(&subquery),
    )
    .expect("the matching resolver predicate selects the exact partial index");
    let changed = explain_query_with_partial_indexes_and_decorrelated_subqueries(
        &query,
        &indexes_with_orders_index("index:owner-orders-v2"),
        std::slice::from_ref(&subquery),
    )
    .expect("a second exact index remains eligible for the same predicate");
    let baseline_joins = joins_by_label(&baseline);
    let changed_joins = joins_by_label(&changed);

    let ordinary = baseline_joins["table:Orders"];
    let ordinary_after = changed_joins["table:Orders"];
    assert_eq!(
        text(ordinary, "join_cost_fold_identity"),
        text(ordinary_after, "join_cost_fold_identity"),
        "an ordinary join with a different predicate is outside the decorrelation/index pair"
    );
    assert!(
        ordinary
            .details()
            .get("decorrelated_predicate_pushdown_identity")
            .is_none()
    );

    let decorrelated_label = "subquery:orders-for-owner";
    let baseline_decorrelated = baseline_joins[decorrelated_label];
    let changed_decorrelated = changed_joins[decorrelated_label];
    assert_eq!(
        text(baseline_decorrelated, "join_cost_fold_identity"),
        "join-fold:72127e74c5531339b74c09ad6ed44ff80d093f94c92e86bf712b5283c301e68e",
        "the paired decorrelation/index and anchor inputs contribute to the fold identity"
    );
    let baseline_nodes = baseline
        .nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect::<BTreeMap<_, _>>();
    let changed_nodes = changed
        .nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect::<BTreeMap<_, _>>();
    let baseline_access = baseline_nodes[baseline_decorrelated.inputs()[1].as_str()];
    let changed_access = changed_nodes[changed_decorrelated.inputs()[1].as_str()];
    assert_eq!(baseline_access.kind(), PlanNodeKind::IndexLookup);
    assert_eq!(changed_access.kind(), PlanNodeKind::IndexLookup);
    assert_eq!(
        baseline_access.object().map(ObjectRef::as_str),
        Some("index:owner-orders")
    );
    assert_eq!(
        changed_access.object().map(ObjectRef::as_str),
        Some("index:owner-orders-v2")
    );
    assert_ne!(
        text(baseline_decorrelated, "predicate_pushdown_identity"),
        text(changed_decorrelated, "predicate_pushdown_identity")
    );
    assert_ne!(
        text(
            baseline_decorrelated,
            "decorrelated_predicate_pushdown_identity"
        ),
        text(
            changed_decorrelated,
            "decorrelated_predicate_pushdown_identity"
        )
    );
    assert_ne!(
        text(baseline_decorrelated, "join_cost_fold_right_identity"),
        text(changed_decorrelated, "join_cost_fold_right_identity")
    );

    for label in [
        decorrelated_label,
        "table:Small",
        "table:Medium",
        "table:Expensive",
        "table:Unknown",
    ] {
        let before = baseline_joins[label];
        let after = changed_joins[label];
        assert_ne!(
            text(before, "join_cost_fold_identity"),
            text(after, "join_cost_fold_identity"),
            "the selected index identity propagates through the fold at {label}"
        );
        assert_eq!(
            (
                before.estimated_rows(),
                before.estimated_bytes(),
                before.estimated_work(),
            ),
            (
                after.estimated_rows(),
                after.estimated_bytes(),
                after.estimated_work(),
            ),
            "changing index identity preserves the actual estimates at {label}"
        );
    }
    assert_eq!(
        (
            baseline_decorrelated.estimated_rows(),
            baseline_decorrelated.estimated_bytes(),
            baseline_decorrelated.estimated_work(),
        ),
        (Some(12), Some(41_448), Some(34))
    );
    assert_eq!(
        (
            baseline_access.estimated_rows(),
            baseline_access.estimated_bytes(),
            baseline_access.estimated_work(),
        ),
        (Some(4), Some(8_192), Some(6))
    );
}

#[test]
fn paired_decorrelations_keep_sparse_cost_cascades_bound_through_reordering() {
    let parsed = orna_syntax_v1::parse_module(CASCADE_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline_query = cascade_query([
        "table:Expensive",
        "table:Unknown",
        "table:Small",
        "table:Medium",
        "table:Orders",
    ]);
    let baseline_subqueries = cascade_subqueries();
    let baseline = explain_cascade(
        &baseline_query,
        &cascade_indexes("index:owner-orders"),
        &baseline_subqueries,
    );

    let reordered_query = cascade_query([
        "table:Medium",
        "table:Orders",
        "table:Unknown",
        "table:Expensive",
        "table:Small",
    ]);
    let reordered_subqueries = baseline_subqueries
        .iter()
        .cloned()
        .rev()
        .collect::<Vec<_>>();
    let reordered_indexes = cascade_indexes("index:owner-orders")
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    let reordered = explain_cascade(&reordered_query, &reordered_indexes, &reordered_subqueries);

    let baseline_joins = joins_by_label(&baseline);
    let reordered_joins = joins_by_label(&reordered);
    let expected_order = [
        "subquery:invoices-owner",
        "table:Orders",
        "subquery:orders-owner",
        "table:Small",
        "table:Medium",
        "table:Expensive",
        "table:Unknown",
    ];
    let mut planned = baseline_joins
        .iter()
        .map(|(label, node)| (*label, *node))
        .collect::<Vec<_>>();
    planned.sort_by_key(|(_, node)| integer(node, "planned_input_position"));
    assert_eq!(
        planned.iter().map(|(label, _)| *label).collect::<Vec<_>>(),
        expected_order
    );

    for label in expected_order {
        let before = baseline_joins[label];
        let after = reordered_joins[label];
        assert_eq!(
            text(before, "join_cost_fold_right_identity"),
            text(after, "join_cost_fold_right_identity"),
            "each declared subquery/input stays paired under sparse reordering at {label}"
        );
        assert_eq!(
            text(before, "join_cost_fold_identity"),
            text(after, "join_cost_fold_identity"),
            "the complete ordered fold chain is stable at {label}"
        );
        assert_eq!(
            (
                before.estimated_rows(),
                before.estimated_bytes(),
                before.estimated_work(),
            ),
            (
                after.estimated_rows(),
                after.estimated_bytes(),
                after.estimated_work(),
            ),
            "sparse reordering preserves the computed estimates at {label}"
        );
    }

    let baseline_orders = baseline_joins["subquery:orders-owner"];
    let baseline_invoices = baseline_joins["subquery:invoices-owner"];
    let baseline_nodes = baseline
        .nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect::<BTreeMap<_, _>>();
    let orders_access = baseline_nodes[baseline_orders.inputs()[1].as_str()];
    let invoices_access = baseline_nodes[baseline_invoices.inputs()[1].as_str()];
    assert_eq!(orders_access.kind(), PlanNodeKind::IndexLookup);
    assert_eq!(invoices_access.kind(), PlanNodeKind::IndexLookup);
    assert_eq!(
        orders_access.object().map(ObjectRef::as_str),
        Some("index:owner-orders")
    );
    assert_eq!(
        invoices_access.object().map(ObjectRef::as_str),
        Some("index:owner-invoices")
    );
    let orders_identity = text(baseline_orders, "decorrelated_predicate_pushdown_identity");
    let invoices_identity = text(
        baseline_invoices,
        "decorrelated_predicate_pushdown_identity",
    );
    assert_ne!(orders_identity, invoices_identity);
    assert_eq!(
        text(orders_access, "decorrelated_predicate_pushdown_identity"),
        orders_identity
    );
    assert_eq!(
        text(invoices_access, "decorrelated_predicate_pushdown_identity"),
        invoices_identity
    );
    assert_eq!(
        (
            baseline_invoices.estimated_rows(),
            baseline_invoices.estimated_bytes(),
            baseline_invoices.estimated_work(),
        ),
        (Some(20), Some(21_280), Some(102))
    );
    assert_eq!(
        (
            baseline_orders.estimated_rows(),
            baseline_orders.estimated_bytes(),
            baseline_orders.estimated_work(),
        ),
        (Some(3), Some(13_434), Some(10))
    );

    let changed = explain_cascade(
        &baseline_query,
        &cascade_indexes("index:owner-orders-v2"),
        &baseline_subqueries,
    );
    let changed_joins = joins_by_label(&changed);
    for label in ["subquery:invoices-owner", "table:Orders"] {
        assert_eq!(
            text(baseline_joins[label], "join_cost_fold_identity"),
            text(changed_joins[label], "join_cost_fold_identity"),
            "changing the Orders index leaves earlier unrelated folds stable at {label}"
        );
    }
    for label in [
        "subquery:orders-owner",
        "table:Small",
        "table:Medium",
        "table:Expensive",
        "table:Unknown",
    ] {
        assert_ne!(
            text(baseline_joins[label], "join_cost_fold_identity"),
            text(changed_joins[label], "join_cost_fold_identity"),
            "the Orders pair rekeys its own fold and all subsequent folds at {label}"
        );
    }
    assert_eq!(
        text(
            baseline_joins["subquery:invoices-owner"],
            "decorrelated_predicate_pushdown_identity"
        ),
        text(
            changed_joins["subquery:invoices-owner"],
            "decorrelated_predicate_pushdown_identity"
        )
    );
    assert_ne!(
        text(
            baseline_joins["subquery:orders-owner"],
            "decorrelated_predicate_pushdown_identity"
        ),
        text(
            changed_joins["subquery:orders-owner"],
            "decorrelated_predicate_pushdown_identity"
        )
    );
}
