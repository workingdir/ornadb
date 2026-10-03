use std::collections::BTreeMap;

use orna_sys_v1::{
    ObjectRef, PlanDetail, PlanNode, PlanNodeKind, QueryJoinDescription, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef, explain_query,
};

const SPARSE_JOIN_FIXTURE: &str =
    include_str!("fixtures/planner_sparse_join_reordering_ueml2.orna");

fn object(reference: &str) -> ObjectRef {
    ObjectRef::descriptive(reference)
}

fn statistics(rows: u64, bytes: u64) -> QuerySourceStatistics {
    QuerySourceStatistics {
        estimated_rows: Some(rows),
        estimated_bytes: Some(bytes),
        mutable_branch: None,
    }
}

fn join(source: &str, statistics: Option<QuerySourceStatistics>) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics,
        predicate: None,
    }
}

fn query(snapshot: &str, joins: Vec<QueryJoinDescription>) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive(snapshot),
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

fn integer_detail(node: &PlanNode, name: &str) -> u64 {
    match node.details().get(name) {
        Some(PlanDetail::Integer(value)) => *value,
        other => panic!("{name} should be an integer detail, got {other:?}"),
    }
}

fn join_observations(
    plan: &orna_sys_v1::ExplainedPlan,
) -> Vec<(u64, u64, String, Option<u64>, Option<u64>, Option<u64>)> {
    let scans = plan
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Scan)
        .map(|node| {
            (
                node.reference().as_str().to_owned(),
                node.object()
                    .expect("scan retains its input table")
                    .as_str()
                    .to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut joins = plan
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| {
            let planned_position = integer_detail(node, "planned_input_position");
            let declared_position = integer_detail(node, "declared_input_position");
            let source = scans
                .get(node.inputs()[1].as_str())
                .expect("the right side of each join points to its scan")
                .clone();
            assert_eq!(
                node.details().get("join_order_policy"),
                Some(&PlanDetail::Text(
                    "known_scan_work_then_declared".to_owned()
                ))
            );
            (
                planned_position,
                declared_position,
                source,
                node.estimated_rows(),
                node.estimated_bytes(),
                node.estimated_work(),
            )
        })
        .collect::<Vec<_>>();
    joins.sort_by_key(|join| join.0);
    joins
}

#[test]
fn sparse_join_reordering_pairs_known_cost_cascades_before_unknown_inputs() {
    let parsed = orna_syntax_v1::parse_module(SPARSE_JOIN_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let first = explain_query(&query(
        "snapshot:sparse-join-first-order",
        vec![
            join("table:Expensive", Some(statistics(500, 64_000))),
            join("table:Unknown", None),
            join("table:Small", Some(statistics(8, 8_192))),
            join("table:Medium", Some(statistics(30, 4_096))),
        ],
    ))
    .expect("resolved joins produce a cost-ordered plan");
    let first_joins = join_observations(&first);
    assert_eq!(
        first_joins,
        vec![
            (
                1,
                3,
                "table:Small".to_owned(),
                Some(800),
                Some(851_200),
                Some(108)
            ),
            (
                2,
                4,
                "table:Medium".to_owned(),
                Some(24_000),
                Some(28_824_000),
                Some(830)
            ),
            (
                3,
                1,
                "table:Expensive".to_owned(),
                Some(12_000_000),
                Some(15_948_000_000),
                Some(24_500)
            ),
            (4, 2, "table:Unknown".to_owned(), None, None, None),
        ],
        "known scan costs order small-to-large while the sparse tail retains its unknown estimates"
    );
    assert_eq!(first.plan().estimated_cost(), None);
    assert_eq!(first.root().estimated_rows(), None);
    assert_eq!(first.root().estimated_bytes(), None);

    let paired = explain_query(&query(
        "snapshot:sparse-join-paired-order",
        vec![
            join("table:Medium", Some(statistics(30, 4_096))),
            join("table:Small", Some(statistics(8, 8_192))),
            join("table:Unknown", None),
            join("table:Expensive", Some(statistics(500, 64_000))),
        ],
    ))
    .expect("the paired declaration order also produces a reordered plan");
    let paired_joins = join_observations(&paired);
    assert_eq!(
        paired_joins
            .iter()
            .map(|join| join.2.as_str())
            .collect::<Vec<_>>(),
        vec![
            "table:Small",
            "table:Medium",
            "table:Expensive",
            "table:Unknown",
        ],
        "paired cascades converge on the same deterministic known-cost order"
    );
    assert_eq!(
        paired_joins
            .iter()
            .map(|join| (join.3, join.4, join.5))
            .collect::<Vec<_>>(),
        first_joins
            .iter()
            .map(|join| (join.3, join.4, join.5))
            .collect::<Vec<_>>(),
        "reordering keeps the computed row, byte, and local-work estimates attached to each cascade"
    );
}
