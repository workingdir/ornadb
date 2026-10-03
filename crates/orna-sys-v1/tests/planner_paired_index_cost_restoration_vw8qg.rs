use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, QueryJoinDescription,
    QueryJoinPairIdentityDescription, QueryPartialIndexDescription, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_join_pair_identities,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_index_cost_restoration_vw8qg.orna");

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
    stats: Option<QuerySourceStatistics>,
    predicate: &str,
) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics: stats,
        predicate: Some(expression(predicate)),
    }
}

fn query(order: [&str; 5]) -> QueryPlanDescription {
    let joins = order
        .into_iter()
        .map(|source| match source {
            "table:IndexA" => join(source, Some(statistics(5, 4_096)), "expr:index-a"),
            "table:Loose" => join(source, Some(statistics(8, 4_096)), "expr:loose"),
            "table:Scan" => join(source, Some(statistics(10, 8_192)), "expr:scan"),
            "table:IndexB" => join(source, Some(statistics(20, 8_192)), "expr:index-b"),
            "table:Unknown" => join(source, None, "expr:unknown"),
            other => panic!("unexpected source {other}"),
        })
        .collect();
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-index-cost-restoration-vw8qg"),
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

fn pair(identity: &str, source: &str, predicate: &str) -> QueryJoinPairIdentityDescription {
    QueryJoinPairIdentityDescription {
        identity: object(identity),
        left_source: object("table:Anchor"),
        right_source: object(source),
        predicate: Some(expression(predicate)),
    }
}

fn pairs() -> Vec<QueryJoinPairIdentityDescription> {
    vec![
        pair("pair:index-a", "table:IndexA", "expr:index-a"),
        pair("pair:scan", "table:Scan", "expr:scan"),
        pair("pair:index-b", "table:IndexB", "expr:index-b"),
        pair("pair:unknown", "table:Unknown", "expr:unknown"),
    ]
}

fn index(table: &str, identity: &str, predicate: &str) -> QueryPartialIndexDescription {
    QueryPartialIndexDescription {
        table: object(table),
        index: object(identity),
        partial_predicate: expression(predicate),
    }
}

fn indexes(index_a: &str, include_scan: bool) -> Vec<QueryPartialIndexDescription> {
    let mut indexes = vec![
        index("table:IndexA", index_a, "expr:index-a"),
        index("table:IndexB", "index:b", "expr:index-b"),
        index("table:Scan", "index:wrong-scan-predicate", "expr:other"),
    ];
    if include_scan {
        indexes.push(index("table:Scan", "index:scan", "expr:scan"));
    }
    indexes
}

fn explain(
    order: [&str; 5],
    candidates: &[QueryPartialIndexDescription],
    descriptions: &[QueryJoinPairIdentityDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_join_pair_identities(
        &query(order),
        candidates,
        descriptions,
    )
    .expect("paired routes explain through sparse cost restoration")
}

fn text<'a>(node: &'a PlanNode, key: &str) -> &'a str {
    match node.details().get(key) {
        Some(PlanDetail::Text(value)) => value,
        other => panic!("{key} should be text, got {other:?}"),
    }
}

fn number(node: &PlanNode, key: &str) -> u64 {
    match node.details().get(key) {
        Some(PlanDetail::Integer(value)) => *value,
        other => panic!("{key} should be an integer, got {other:?}"),
    }
}

fn pair_joins(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<&str, &PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .filter_map(|node| match node.details().get("join_pair_identity") {
            Some(PlanDetail::Text(identity)) => Some((identity.as_str(), node)),
            _ => None,
        })
        .collect()
}

fn right_access<'a>(plan: &'a orna_sys_v1::ExplainedPlan, join: &PlanNode) -> &'a PlanNode {
    let reference = join.inputs()[1].as_str();
    plan.nodes()
        .iter()
        .find(|node| node.reference().as_str() == reference)
        .expect("paired join retains its selected right access")
}

fn cost(node: &PlanNode) -> (Option<u64>, Option<u64>, Option<u64>) {
    (
        node.estimated_rows(),
        node.estimated_bytes(),
        node.estimated_work(),
    )
}

#[test]
fn paired_index_routes_round_trip_through_sparse_cost_restoration() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let descriptors = pairs();
    let baseline = explain(
        [
            "table:IndexB",
            "table:Loose",
            "table:Unknown",
            "table:Scan",
            "table:IndexA",
        ],
        &indexes("index:a", false),
        &descriptors,
    );
    let reordered = explain(
        [
            "table:IndexA",
            "table:Scan",
            "table:Unknown",
            "table:Loose",
            "table:IndexB",
        ],
        &indexes("index:a", false)
            .into_iter()
            .rev()
            .collect::<Vec<_>>(),
        &descriptors.iter().rev().cloned().collect::<Vec<_>>(),
    );
    let rebound = explain(
        [
            "table:IndexB",
            "table:Loose",
            "table:Unknown",
            "table:Scan",
            "table:IndexA",
        ],
        &indexes("index:a-v2", false),
        &descriptors,
    );
    let indexed_scan = explain(
        [
            "table:IndexB",
            "table:Loose",
            "table:Unknown",
            "table:Scan",
            "table:IndexA",
        ],
        &indexes("index:a", true),
        &descriptors,
    );
    let baseline_pairs = pair_joins(&baseline);
    let reordered_pairs = pair_joins(&reordered);
    let rebound_pairs = pair_joins(&rebound);
    let indexed_scan_pairs = pair_joins(&indexed_scan);

    for (
        pair_id,
        expected_route,
        expected_cost,
        planned_position,
        baseline_declared_position,
        reordered_declared_position,
    ) in [
        (
            "pair:index-a",
            Some("index:a"),
            (Some(50), Some(43_000), Some(105)),
            1,
            5,
            1,
        ),
        (
            "pair:index-b",
            Some("index:b"),
            (Some(80), Some(208_160), Some(60)),
            4,
            1,
            5,
        ),
        (
            "pair:scan",
            Some("table:Scan"),
            (Some(40), Some(87_680), Some(50)),
            3,
            4,
            2,
        ),
        (
            "pair:unknown",
            Some("table:Unknown"),
            (None, None, None),
            5,
            3,
            3,
        ),
    ] {
        let before = baseline_pairs[pair_id];
        let after = reordered_pairs[pair_id];
        let access = right_access(&baseline, before);
        let reordered_access = right_access(&reordered, after);
        let expected_kind = if expected_route.is_some_and(|route| route.starts_with("index:")) {
            PlanNodeKind::IndexLookup
        } else {
            PlanNodeKind::Scan
        };
        assert_eq!(access.kind(), expected_kind, "planned route for {pair_id}");
        assert_eq!(access.object().map(ObjectRef::as_str), expected_route);
        assert_eq!(cost(before), expected_cost, "computed output for {pair_id}");
        assert_eq!(number(before, "planned_input_position"), planned_position);
        assert_eq!(
            number(before, "declared_input_position"),
            baseline_declared_position
        );
        assert_eq!(
            number(after, "declared_input_position"),
            reordered_declared_position
        );
        assert_eq!(
            number(
                access,
                "paired_index_cost_restoration_declared_input_position"
            ),
            baseline_declared_position
        );
        assert_eq!(
            number(
                access,
                "paired_index_cost_restoration_planned_input_position"
            ),
            planned_position
        );
        assert_eq!(
            number(
                reordered_access,
                "paired_index_cost_restoration_declared_input_position"
            ),
            reordered_declared_position
        );
        assert_eq!(
            number(
                reordered_access,
                "paired_index_cost_restoration_planned_input_position"
            ),
            planned_position
        );
        assert_eq!(
            text(before, "paired_index_cost_restoration_chain_identity"),
            text(access, "paired_index_cost_restoration_chain_identity")
        );
        assert_eq!(
            text(before, "paired_index_cost_restoration_chain_identity"),
            text(after, "paired_index_cost_restoration_chain_identity"),
            "catalog and declaration reorder restore the same paired route for {pair_id}"
        );
        assert_eq!(
            text(before, "paired_index_cost_restoration_chain_identity"),
            text(
                reordered_access,
                "paired_index_cost_restoration_chain_identity"
            )
        );
        assert_eq!(cost(after), expected_cost);
    }

    let index_a = baseline_pairs["pair:index-a"];
    let loose = baseline
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Join && number(node, "planned_input_position") == 2
        })
        .expect("unpaired cost fold appears between paired routes");
    let scan = baseline_pairs["pair:scan"];
    let index_b = baseline_pairs["pair:index-b"];
    let unknown = baseline_pairs["pair:unknown"];
    let first_chain = text(index_a, "paired_index_cost_restoration_chain_identity");
    assert_eq!(
        text(index_a, "paired_index_cost_restoration_transition"),
        "append_exact_index_outcome"
    );
    assert_eq!(
        text(index_a, "paired_index_cost_restoration_outcome"),
        "exact_index"
    );
    assert_eq!(
        text(
            index_a,
            "paired_index_cost_restoration_step_refold_identity"
        ),
        text(index_a, "paired_index_refold_identity")
    );
    assert_eq!(
        text(loose, "paired_index_cost_restoration_transition"),
        "carry_through_unpaired_cost_fold"
    );
    assert_eq!(
        text(loose, "paired_index_cost_restoration_chain_identity"),
        first_chain
    );
    assert_eq!(
        text(scan, "paired_index_cost_restoration_transition"),
        "append_scan_outcome"
    );
    assert_eq!(text(scan, "paired_index_cost_restoration_outcome"), "scan");
    assert_ne!(
        text(scan, "paired_index_cost_restoration_chain_identity"),
        first_chain
    );
    assert_eq!(
        text(index_b, "paired_index_cost_restoration_parent_identity"),
        text(scan, "paired_index_cost_restoration_chain_identity")
    );
    assert_eq!(
        text(unknown, "paired_index_cost_restoration_parent_identity"),
        text(index_b, "paired_index_cost_restoration_chain_identity")
    );
    assert_eq!(
        text(unknown, "paired_index_cost_restoration_outcome"),
        "scan"
    );

    assert_ne!(
        text(index_a, "paired_index_selection_identity"),
        text(
            rebound_pairs["pair:index-a"],
            "paired_index_selection_identity"
        )
    );
    assert_eq!(cost(index_a), cost(rebound_pairs["pair:index-a"]));
    for pair_id in ["pair:scan", "pair:index-b", "pair:unknown"] {
        assert_ne!(
            text(
                baseline_pairs[pair_id],
                "paired_index_cost_restoration_chain_identity"
            ),
            text(
                rebound_pairs[pair_id],
                "paired_index_cost_restoration_chain_identity"
            ),
            "a preceding exact index rebind flows through {pair_id}"
        );
        assert_eq!(cost(baseline_pairs[pair_id]), cost(rebound_pairs[pair_id]));
    }

    assert_eq!(
        text(index_a, "paired_index_cost_restoration_chain_identity"),
        text(
            indexed_scan_pairs["pair:index-a"],
            "paired_index_cost_restoration_chain_identity"
        )
    );
    assert_eq!(
        text(loose, "paired_index_cost_restoration_chain_identity"),
        text(
            indexed_scan
                .nodes()
                .iter()
                .find(|node| {
                    node.kind() == PlanNodeKind::Join && number(node, "planned_input_position") == 2
                })
                .expect("unpaired fold remains before the scan pair"),
            "paired_index_cost_restoration_chain_identity"
        )
    );
    assert_ne!(
        text(scan, "paired_index_cost_restoration_chain_identity"),
        text(
            indexed_scan_pairs["pair:scan"],
            "paired_index_cost_restoration_chain_identity"
        ),
        "a newly available exact route is recorded at its own pair slot"
    );
    for pair_id in ["pair:index-b", "pair:unknown"] {
        assert_ne!(
            text(
                baseline_pairs[pair_id],
                "paired_index_cost_restoration_chain_identity"
            ),
            text(
                indexed_scan_pairs[pair_id],
                "paired_index_cost_restoration_chain_identity"
            ),
            "the newly indexed scan occupies its paired slot before {pair_id}"
        );
    }
    for pair_id in ["pair:scan", "pair:index-b", "pair:unknown"] {
        assert_eq!(
            cost(baseline_pairs[pair_id]),
            cost(indexed_scan_pairs[pair_id])
        );
    }
}
