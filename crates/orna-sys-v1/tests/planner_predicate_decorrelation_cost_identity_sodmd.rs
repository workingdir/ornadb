use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryPartialIndexDescription,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_decorrelated_subqueries,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_predicate_decorrelation_cost_identity_sodmd.orna");

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
