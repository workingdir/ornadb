use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef, explain_query_with_decorrelated_subqueries,
};

const SUBQUERY_FIXTURE: &str =
    include_str!("fixtures/planner_subquery_decorrelation_sparse_cost_l1nbb.orna");

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
    statistics: Option<QuerySourceStatistics>,
    predicate: &str,
) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics,
        predicate: Some(expression(predicate)),
    }
}

fn query() -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:subquery-sparse-boundary"),
        source: object("table:Anchor"),
        source_statistics: Some(statistics(100, 4_000)),
        joins: vec![
            join(
                "table:Expensive",
                Some(statistics(500, 64_000)),
                "expr:anchor-expensive",
            ),
            join("table:Unknown", None, "expr:anchor-unknown"),
            join(
                "table:Medium",
                Some(statistics(30, 4_096)),
                "expr:anchor-medium",
            ),
            // Same source and predicate as the subquery, but this ordinary
            // join has no subquery identity and must stay distinct after sort.
            join(
                "table:Orders",
                Some(statistics(3, 4_096)),
                "expr:owner-orders",
            ),
        ],
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    }
}

fn integer_detail(node: &PlanNode, name: &str) -> u64 {
    match node.details().get(name) {
        Some(PlanDetail::Integer(value)) => *value,
        other => panic!("{name} should be an integer detail, got {other:?}"),
    }
}

fn text_detail(node: &PlanNode, name: &str) -> String {
    match node.details().get(name) {
        Some(PlanDetail::Text(value)) => value.clone(),
        other => panic!("{name} should be a text detail, got {other:?}"),
    }
}

#[test]
fn decorrelated_subquery_identity_survives_sparse_cost_reordering() {
    let parsed = orna_syntax_v1::parse_module(SUBQUERY_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let subquery = QueryDecorrelatedSubqueryDescription {
        identity: object("subquery:orders-for-owner"),
        source: object("table:Orders"),
        correlation_predicate: expression("expr:owner-orders"),
        statistics: Some(statistics(4, 8_192)),
    };
    let explained = explain_query_with_decorrelated_subqueries(&query(), &[subquery])
        .expect("resolved subquery inputs produce a plan");
    let nodes_by_reference = explained
        .nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect::<BTreeMap<_, _>>();

    let mut ordered_joins = explained
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| {
            let right = nodes_by_reference
                .get(node.inputs()[1].as_str())
                .expect("join right input remains attached after ordering");
            (
                integer_detail(node, "planned_input_position"),
                integer_detail(node, "declared_input_position"),
                right
                    .object()
                    .expect("right-side scan retains its source")
                    .as_str()
                    .to_owned(),
                node.details()
                    .get("subquery_identity")
                    .map(|_| text_detail(node, "subquery_identity")),
            )
        })
        .collect::<Vec<_>>();
    ordered_joins.sort_by_key(|entry| entry.0);
    assert_eq!(
        ordered_joins,
        vec![
            (1, 4, "table:Orders".to_owned(), None),
            (
                2,
                5,
                "table:Orders".to_owned(),
                Some("subquery:orders-for-owner".to_owned()),
            ),
            (3, 3, "table:Medium".to_owned(), None),
            (4, 1, "table:Expensive".to_owned(), None),
            (5, 2, "table:Unknown".to_owned(), None),
        ],
        "known-cost decorrelated inputs sort ahead of the unknown-cost boundary without aliasing the matching ordinary join"
    );

    let subquery_join = explained
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Join
                && node.details().get("subquery_identity")
                    == Some(&PlanDetail::Text("subquery:orders-for-owner".to_owned()))
        })
        .expect("decorrelated join exposes the original subquery identity");
    assert_eq!(
        subquery_join
            .details()
            .get("correlation_predicate_identity"),
        Some(&PlanDetail::Text("expr:owner-orders".to_owned()))
    );
    assert_eq!(
        subquery_join.details().get("subquery_decorrelation"),
        Some(&PlanDetail::Text(
            "resolver_approved_predicate_join".to_owned()
        ))
    );
    assert_eq!(subquery_join.estimated_rows(), Some(12));
    assert_eq!(subquery_join.estimated_bytes(), Some(41_448));
    assert_eq!(subquery_join.estimated_work(), Some(34));

    let subquery_input = nodes_by_reference
        .get(subquery_join.inputs()[1].as_str())
        .expect("decorrelated join retains its source input");
    assert_eq!(subquery_input.kind(), PlanNodeKind::Scan);
    assert_eq!(
        subquery_input.object().map(ObjectRef::as_str),
        Some("table:Orders")
    );
    assert_eq!(subquery_input.estimated_rows(), Some(4));
    assert_eq!(subquery_input.estimated_bytes(), Some(8_192));
    assert_eq!(subquery_input.estimated_work(), Some(6));
    assert_eq!(
        subquery_input.details().get("subquery_identity"),
        Some(&PlanDetail::Text("subquery:orders-for-owner".to_owned()))
    );
    assert_eq!(
        subquery_input
            .details()
            .get("correlation_predicate_identity"),
        Some(&PlanDetail::Text("expr:owner-orders".to_owned()))
    );
    assert_eq!(explained.plan().estimated_cost(), None);
    assert_eq!(explained.root().estimated_rows(), None);
}
