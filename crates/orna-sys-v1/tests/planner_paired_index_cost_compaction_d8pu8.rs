use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryPartialIndexDescription, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_decorrelated_subqueries_and_join_pair_identities,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_index_cost_compaction_d8pu8.orna");

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

fn join(source: &str, estimate: Option<(u64, u64)>, predicate: &str) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics: estimate.map(|(rows, bytes)| statistics(rows, bytes)),
        predicate: Some(expression(predicate)),
    }
}

fn query(reverse_known_inputs: bool) -> QueryPlanDescription {
    let fast = join("table:Fast", Some((2, 4_096)), "expr:fast");
    let middle = join("table:Middle", Some((3, 8_192)), "expr:middle");
    let scan_pair = join("table:ScanPair", Some((6, 4_096)), "expr:scan-pair");
    let final_input = join("table:Final", Some((8, 8_192)), "expr:final");
    let sparse = join("table:Sparse", None, "expr:sparse");
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-index-cost-compaction-d8pu8"),
        source: object("table:Anchor"),
        source_statistics: Some(statistics(50, 8_192)),
        joins: if reverse_known_inputs {
            vec![final_input, scan_pair, middle, fast, sparse]
        } else {
            vec![fast, middle, scan_pair, final_input, sparse]
        },
        predicate: None,
        projections: vec![expression("expr:projected-output")],
        distinct: false,
        ordering: Vec::new(),
        limit: Some(25),
        mutations: Vec::new(),
        materialize_into: None,
    }
}

fn subqueries() -> Vec<QueryDecorrelatedSubqueryDescription> {
    vec![
        QueryDecorrelatedSubqueryDescription {
            identity: object("subquery:small-child"),
            source: object("table:Child"),
            correlation_predicate: expression("expr:child-correlation"),
            statistics: Some(statistics(1, 4_096)),
        },
        QueryDecorrelatedSubqueryDescription {
            identity: object("subquery:unknown-child"),
            source: object("table:UnknownChild"),
            correlation_predicate: expression("expr:unknown-correlation"),
            statistics: None,
        },
    ]
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
        pair("pair:fast", "table:Fast", "expr:fast"),
        pair("pair:scan", "table:ScanPair", "expr:scan-pair"),
        pair("pair:final", "table:Final", "expr:final"),
        pair("pair:child", "table:Child", "expr:child-correlation"),
        pair(
            "pair:unknown-child",
            "table:UnknownChild",
            "expr:unknown-correlation",
        ),
    ]
}

fn index(table: &str, identity: &str, predicate: &str) -> QueryPartialIndexDescription {
    QueryPartialIndexDescription {
        table: object(table),
        index: object(identity),
        partial_predicate: expression(predicate),
    }
}

fn indexes(fast_index: &str) -> Vec<QueryPartialIndexDescription> {
    vec![
        index("table:Fast", fast_index, "expr:fast"),
        index("table:Final", "index:final", "expr:final"),
        index("table:Child", "index:child", "expr:child-correlation"),
        index("table:ScanPair", "index:wrong-scan", "expr:other"),
        index(
            "table:UnknownChild",
            "index:wrong-unknown",
            "expr:other-unknown",
        ),
    ]
}

fn explain(
    reverse_known_inputs: bool,
    reverse_metadata: bool,
    fast_index: &str,
) -> orna_sys_v1::ExplainedPlan {
    let mut candidates = indexes(fast_index);
    let mut pair_descriptions = pairs();
    if reverse_metadata {
        candidates.reverse();
        pair_descriptions.reverse();
    }
    explain_query_with_partial_indexes_and_decorrelated_subqueries_and_join_pair_identities(
        &query(reverse_known_inputs),
        &candidates,
        &subqueries(),
        &pair_descriptions,
    )
    .expect("paired routes compact through sparse planned cost order")
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

fn joins_by_pair(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<&str, &PlanNode> {
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

fn join_for_source<'a>(plan: &'a orna_sys_v1::ExplainedPlan, source: &str) -> &'a PlanNode {
    let access = plan
        .nodes()
        .iter()
        .find(|node| {
            matches!(node.kind(), PlanNodeKind::Scan | PlanNodeKind::IndexLookup)
                && node.object().map(ObjectRef::as_str) == Some(source)
        })
        .expect("source access is present in the explained plan");
    plan.nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Join
                && node.inputs()[1].as_str() == access.reference().as_str()
        })
        .expect("source access is restored into a join")
}

fn cost(node: &PlanNode) -> (Option<u64>, Option<u64>, Option<u64>) {
    (
        node.estimated_rows(),
        node.estimated_bytes(),
        node.estimated_work(),
    )
}

#[test]
fn paired_index_compaction_keeps_sparse_slots_and_real_routes() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = explain(false, false, "index:fast");
    let reordered = explain(true, true, "index:fast");
    let rebound = explain(false, false, "index:fast-v2");
    let baseline_pairs = joins_by_pair(&baseline);
    let reordered_pairs = joins_by_pair(&reordered);
    let rebound_pairs = joins_by_pair(&rebound);

    for (pair_id, source, route, expected_cost, pairs, indexes, scans) in [
        (
            "pair:child",
            "table:Child",
            "index:child",
            (Some(1), Some(4_096), Some(2)),
            1,
            1,
            0,
        ),
        (
            "pair:fast",
            "table:Fast",
            "index:fast",
            (Some(2), Some(4_096), Some(3)),
            2,
            2,
            0,
        ),
        (
            "pair:scan",
            "table:ScanPair",
            "table:ScanPair",
            (Some(6), Some(4_096), Some(7)),
            3,
            2,
            1,
        ),
        (
            "pair:final",
            "table:Final",
            "index:final",
            (Some(8), Some(8_192), Some(10)),
            4,
            3,
            1,
        ),
    ] {
        let before = baseline_pairs[pair_id];
        let after = reordered_pairs[pair_id];
        let access = right_access(&baseline, before);
        assert_eq!(access.object().map(ObjectRef::as_str), Some(route));
        assert_eq!(
            access.kind(),
            if route.starts_with("index:") {
                PlanNodeKind::IndexLookup
            } else {
                PlanNodeKind::Scan
            }
        );
        assert_eq!(cost(access), expected_cost, "computed access for {source}");
        assert_eq!(
            number(before, "paired_index_cost_compaction_pair_count"),
            pairs
        );
        assert_eq!(
            number(before, "paired_index_cost_compaction_exact_index_count"),
            indexes
        );
        assert_eq!(
            number(before, "paired_index_cost_compaction_scan_count"),
            scans
        );
        assert_eq!(
            text(
                before,
                "paired_index_cost_compaction_step_selection_identity"
            ),
            text(before, "paired_index_selection_identity")
        );
        assert_eq!(
            text(before, "paired_index_cost_compaction_step_refold_identity"),
            text(before, "paired_index_refold_identity")
        );
        assert_eq!(
            text(before, "paired_index_cost_compaction_chain_identity"),
            text(after, "paired_index_cost_compaction_chain_identity"),
            "input and metadata reorder preserve the compacted route digest for {pair_id}"
        );
    }

    let child = baseline_pairs["pair:child"];
    let child_compaction = text(child, "paired_index_cost_compaction_chain_identity");
    assert_eq!(text(child, "subquery_identity"), "subquery:small-child");
    assert_eq!(
        text(child, "paired_index_cost_compaction_transition"),
        "append_exact_index_outcome"
    );
    assert_eq!(
        text(
            baseline_pairs["pair:scan"],
            "paired_index_cost_compaction_transition"
        ),
        "append_scan_outcome"
    );
    let middle = join_for_source(&baseline, "table:Middle");
    assert_eq!(
        text(middle, "paired_index_cost_compaction_transition"),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        text(middle, "paired_index_cost_compaction_chain_identity"),
        text(
            baseline_pairs["pair:fast"],
            "paired_index_cost_compaction_chain_identity"
        )
    );
    assert_ne!(
        child_compaction,
        text(
            baseline_pairs["pair:fast"],
            "paired_index_cost_compaction_chain_identity"
        )
    );

    let unknown_access = baseline
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object().map(ObjectRef::as_str) == Some("table:Sparse")
        })
        .expect("unknown source remains a scan");
    assert_eq!(cost(unknown_access), (None, None, None));
    let unknown_join = baseline
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Join
                && node.inputs()[1].as_str() == unknown_access.reference().as_str()
        })
        .expect("unknown-cost scan joins through the compacted plan");
    assert_eq!(
        text(unknown_join, "paired_index_cost_compaction_transition"),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        number(unknown_join, "paired_index_cost_compaction_pair_count"),
        4
    );

    let late = baseline_pairs["pair:unknown-child"];
    let late_access = right_access(&baseline, late);
    assert_eq!(late_access.kind(), PlanNodeKind::Scan);
    assert_eq!(
        late_access.object().map(ObjectRef::as_str),
        Some("table:UnknownChild")
    );
    assert_eq!(cost(late_access), (None, None, None));
    assert_eq!(number(late, "paired_index_cost_compaction_pair_count"), 5);
    assert_eq!(
        number(late, "paired_index_cost_compaction_exact_index_count"),
        3
    );
    assert_eq!(number(late, "paired_index_cost_compaction_scan_count"), 2);
    assert_eq!(
        text(late, "paired_index_cost_compaction_chain_identity"),
        text(
            reordered_pairs["pair:unknown-child"],
            "paired_index_cost_compaction_chain_identity"
        )
    );
    let root = baseline.root();
    assert_eq!(root.kind(), PlanNodeKind::Limit);
    assert_eq!(
        text(root, "paired_index_cost_compaction_output_chain_identity"),
        text(late, "paired_index_cost_compaction_chain_identity")
    );
    assert_eq!(
        number(root, "paired_index_cost_compaction_output_pair_count"),
        5
    );
    assert_eq!(
        number(
            root,
            "paired_index_cost_compaction_output_exact_index_count"
        ),
        3
    );
    assert_eq!(
        number(root, "paired_index_cost_compaction_output_scan_count"),
        2
    );
    assert_eq!(
        text(root, "paired_index_cost_compaction_output_policy"),
        "retain_route_digest_and_counts_on_plan_output"
    );

    let rebound_fast = rebound_pairs["pair:fast"];
    assert_eq!(
        right_access(&rebound, rebound_fast)
            .object()
            .map(ObjectRef::as_str),
        Some("index:fast-v2")
    );
    assert_eq!(
        cost(right_access(&rebound, rebound_fast)),
        (Some(2), Some(4_096), Some(3))
    );
    assert_ne!(
        text(
            baseline_pairs["pair:final"],
            "paired_index_cost_compaction_chain_identity"
        ),
        text(
            rebound_pairs["pair:final"],
            "paired_index_cost_compaction_chain_identity"
        ),
        "a rebound exact index changes the later compacted route identity"
    );
    assert_eq!(
        number(
            rebound_pairs["pair:unknown-child"],
            "paired_index_cost_compaction_exact_index_count"
        ),
        3
    );
    assert_ne!(
        text(late, "paired_index_cost_compaction_chain_identity"),
        text(
            rebound_pairs["pair:unknown-child"],
            "paired_index_cost_compaction_chain_identity"
        ),
        "the compact index digest carries an earlier rebind past sparse gaps"
    );
    assert_ne!(
        text(root, "paired_index_cost_compaction_output_chain_identity"),
        text(
            rebound.root(),
            "paired_index_cost_compaction_output_chain_identity"
        )
    );
    assert_eq!(
        cost(late_access),
        cost(right_access(&rebound, rebound_pairs["pair:unknown-child"]))
    );
}
