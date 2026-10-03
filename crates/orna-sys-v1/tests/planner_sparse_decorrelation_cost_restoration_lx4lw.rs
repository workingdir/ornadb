use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryPartialIndexDescription, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_decorrelated_subqueries_and_join_pair_identities,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_sparse_decorrelation_cost_restoration_lx4lw.orna");

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

fn join(source: &str, stats: Option<(u64, u64)>, predicate: &str) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics: stats.map(|(rows, bytes)| statistics(rows, bytes)),
        predicate: Some(expression(predicate)),
    }
}

fn query(reverse_known_inputs: bool) -> QueryPlanDescription {
    let primary = join("table:Primary", Some((8, 8_192)), "expr:primary");
    let loose = join("table:Loose", Some((9, 12_288)), "expr:loose");
    let sparse = join("table:Sparse", None, "expr:sparse");
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:sparse-decorrelation-restoration-lx4lw"),
        source: object("table:Anchor"),
        source_statistics: Some(statistics(100, 4_000)),
        joins: if reverse_known_inputs {
            vec![loose, primary, sparse]
        } else {
            vec![primary, loose, sparse]
        },
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    }
}

fn subqueries() -> Vec<QueryDecorrelatedSubqueryDescription> {
    vec![
        QueryDecorrelatedSubqueryDescription {
            identity: object("subquery:child"),
            source: object("table:Child"),
            correlation_predicate: expression("expr:child-correlation"),
            statistics: Some(statistics(4, 4_096)),
        },
        QueryDecorrelatedSubqueryDescription {
            identity: object("subquery:late"),
            source: object("table:Late"),
            correlation_predicate: expression("expr:late-correlation"),
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
        pair("pair:primary", "table:Primary", "expr:primary"),
        pair("pair:child", "table:Child", "expr:child-correlation"),
        pair("pair:late", "table:Late", "expr:late-correlation"),
    ]
}

fn index(table: &str, index: &str, predicate: &str) -> QueryPartialIndexDescription {
    QueryPartialIndexDescription {
        table: object(table),
        index: object(index),
        partial_predicate: expression(predicate),
    }
}

fn indexes(primary: &str) -> Vec<QueryPartialIndexDescription> {
    vec![
        index("table:Primary", primary, "expr:primary"),
        index("table:Child", "index:child", "expr:child-correlation"),
        index(
            "table:Late",
            "index:late-decoy",
            "expr:wrong-late-correlation",
        ),
    ]
}

fn explain(
    reverse_known_inputs: bool,
    reverse_metadata: bool,
    primary_index: &str,
) -> orna_sys_v1::ExplainedPlan {
    let mut candidates = indexes(primary_index);
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
    .expect("paired route and decorrelation events restore through sparse cost folds")
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

fn paired_joins(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<&str, &PlanNode> {
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
fn paired_routes_and_decorrelation_share_sparse_restoration_ancestry() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = explain(false, false, "index:primary");
    let reordered = explain(true, true, "index:primary");
    let rebound = explain(false, false, "index:primary-v2");
    let baseline_pairs = paired_joins(&baseline);
    let reordered_pairs = paired_joins(&reordered);
    let rebound_pairs = paired_joins(&rebound);

    for (pair_id, route, estimate, transition) in [
        (
            "pair:child",
            "index:child",
            (Some(4), Some(4_096), Some(5)),
            "append_paired_route_and_decorrelation",
        ),
        (
            "pair:primary",
            "index:primary",
            (Some(8), Some(8_192), Some(10)),
            "append_paired_route",
        ),
    ] {
        let before = baseline_pairs[pair_id];
        let after = reordered_pairs[pair_id];
        let access = right_access(&baseline, before);
        let reordered_access = right_access(&reordered, after);
        assert_eq!(access.kind(), PlanNodeKind::IndexLookup);
        assert_eq!(access.object().map(ObjectRef::as_str), Some(route));
        assert_eq!(cost(access), estimate, "pinned route values for {pair_id}");
        assert_eq!(
            text(before, "paired_decorrelation_cost_restoration_transition"),
            transition
        );
        assert_eq!(
            text(
                before,
                "paired_decorrelation_cost_restoration_chain_identity"
            ),
            text(
                after,
                "paired_decorrelation_cost_restoration_chain_identity"
            ),
            "declaration/catalog order does not detach {pair_id}"
        );
        assert_eq!(
            text(
                before,
                "paired_decorrelation_cost_restoration_chain_identity"
            ),
            text(
                access,
                "paired_decorrelation_cost_restoration_chain_identity"
            )
        );
        assert_eq!(
            text(
                after,
                "paired_decorrelation_cost_restoration_chain_identity"
            ),
            text(
                reordered_access,
                "paired_decorrelation_cost_restoration_chain_identity"
            )
        );
    }

    let child = baseline_pairs["pair:child"];
    let primary = baseline_pairs["pair:primary"];
    let child_chain = text(
        child,
        "paired_decorrelation_cost_restoration_chain_identity",
    );
    let primary_chain = text(
        primary,
        "paired_decorrelation_cost_restoration_chain_identity",
    );
    assert_eq!(text(child, "subquery_identity"), "subquery:child");
    assert_eq!(
        text(
            child,
            "paired_decorrelation_cost_restoration_subquery_identity"
        ),
        "subquery:child"
    );
    assert_eq!(
        text(child, "paired_decorrelation_cost_restoration_route_outcome"),
        "exact_index"
    );
    assert_ne!(child_chain, primary_chain);
    assert_eq!(
        text(
            primary,
            "paired_decorrelation_cost_restoration_parent_identity"
        ),
        child_chain
    );

    let sparse = baseline
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Join
                && optional_text(node, "join_pair_identity").is_none()
                && optional_text(node, "subquery_identity").is_none()
                && text(node, "paired_decorrelation_cost_restoration_transition")
                    == "carry_through_sparse_cost_fold"
        })
        .expect("unpaired and unknown-cost joins preserve the shared restoration chain");
    let sparse_chain = text(
        sparse,
        "paired_decorrelation_cost_restoration_chain_identity",
    );
    assert_eq!(sparse_chain, primary_chain);
    let unknown_access = baseline
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object().map(ObjectRef::as_str) == Some("table:Sparse")
        })
        .expect("unknown-cost source remains a scan");
    assert_eq!(cost(unknown_access), (None, None, None));
    let unknown_join = baseline
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Join
                && node.inputs()[1].as_str() == unknown_access.reference().as_str()
        })
        .expect("unknown-cost scan is restored into its declared join");
    assert_eq!(
        text(
            unknown_join,
            "paired_decorrelation_cost_restoration_transition"
        ),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        text(
            unknown_join,
            "paired_decorrelation_cost_restoration_chain_identity"
        ),
        primary_chain,
        "an unknown ordinary input leaves both prior paired events intact"
    );

    let late = baseline_pairs["pair:late"];
    let late_access = right_access(&baseline, late);
    assert_eq!(late_access.kind(), PlanNodeKind::Scan);
    assert_eq!(
        late_access.object().map(ObjectRef::as_str),
        Some("table:Late")
    );
    assert_eq!(cost(late_access), (None, None, None));
    assert_eq!(text(late, "subquery_identity"), "subquery:late");
    assert_eq!(
        text(late, "paired_decorrelation_cost_restoration_transition"),
        "append_paired_route_and_decorrelation"
    );
    assert_eq!(
        text(
            late,
            "paired_decorrelation_cost_restoration_parent_identity"
        ),
        sparse_chain
    );
    assert_eq!(
        text(late, "paired_decorrelation_cost_restoration_route_outcome"),
        "scan"
    );
    assert!(
        optional_text(
            late,
            "paired_decorrelation_cost_restoration_omission_refold_identity"
        )
        .is_some()
    );
    assert_eq!(
        text(late, "paired_decorrelation_cost_restoration_chain_identity"),
        text(
            reordered_pairs["pair:late"],
            "paired_decorrelation_cost_restoration_chain_identity"
        )
    );
    let loose_access = baseline
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object().map(ObjectRef::as_str) == Some("table:Loose")
        })
        .expect("known unpaired source remains a scan");
    assert_eq!(cost(loose_access), (Some(9), Some(12_288), Some(12)));

    let rebound_primary = rebound_pairs["pair:primary"];
    assert_eq!(
        right_access(&rebound, rebound_primary)
            .object()
            .map(ObjectRef::as_str),
        Some("index:primary-v2")
    );
    assert_eq!(
        cost(right_access(&rebound, rebound_primary)),
        (Some(8), Some(8_192), Some(10))
    );
    assert_ne!(
        primary_chain,
        text(
            rebound_primary,
            "paired_decorrelation_cost_restoration_chain_identity"
        )
    );
    assert_ne!(
        text(late, "paired_decorrelation_cost_restoration_chain_identity"),
        text(
            rebound_pairs["pair:late"],
            "paired_decorrelation_cost_restoration_chain_identity"
        ),
        "a prior exact-index rebind reaches the later sparse subquery restore"
    );
    assert_eq!(
        cost(late_access),
        cost(right_access(&rebound, rebound_pairs["pair:late"]))
    );
}
