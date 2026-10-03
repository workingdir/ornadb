use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, QueryJoinDescription,
    QueryJoinPairIdentityDescription, QueryPartialIndexDescription, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_join_pair_identities,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_index_refold_chain_hgq2n.orna");

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

fn query(order: [&str; 4]) -> QueryPlanDescription {
    let joins = order
        .into_iter()
        .map(|source| match source {
            "table:Expensive" => join(source, Some(500), Some(64_000), "expr:broad-live"),
            "table:Unknown" => join(source, None, None, "expr:unknown-live"),
            "table:Small" => join(source, Some(8), Some(8_192), "expr:small-live"),
            "table:Medium" => join(source, Some(30), Some(4_096), "expr:medium-live"),
            other => panic!("unexpected source {other}"),
        })
        .collect();
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-index-refold-chain-hgq2n"),
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
        pair("pair:expensive", "table:Expensive", "expr:broad-live"),
        pair("pair:unknown", "table:Unknown", "expr:unknown-live"),
        pair("pair:small", "table:Small", "expr:small-live"),
        pair("pair:medium", "table:Medium", "expr:medium-live"),
    ]
}

fn index(table: &str, identity: &str, predicate: &str) -> QueryPartialIndexDescription {
    QueryPartialIndexDescription {
        table: object(table),
        index: object(identity),
        partial_predicate: expression(predicate),
    }
}

fn indexes(use_small_a: bool) -> Vec<QueryPartialIndexDescription> {
    let mut candidates = vec![
        index("table:Medium", "index:medium-live", "expr:medium-live"),
        index("table:Small", "index:small-live-z", "expr:small-live"),
        index("table:Other", "index:wrong-table", "expr:small-live"),
        index("table:Small", "index:wrong-predicate", "expr:medium-live"),
        index(
            "table:Expensive",
            "index:expensive-decoy",
            "expr:other-live",
        ),
    ];
    if use_small_a {
        candidates.push(index(
            "table:Small",
            "index:small-live-a",
            "expr:small-live",
        ));
    }
    candidates
}

fn explain(
    order: [&str; 4],
    candidates: &[QueryPartialIndexDescription],
    descriptions: &[QueryJoinPairIdentityDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_join_pair_identities(
        &query(order),
        candidates,
        descriptions,
    )
    .expect("paired index identities refold across sparse cost ordering")
}

fn text<'a>(node: &'a PlanNode, key: &str) -> &'a str {
    match node.details().get(key) {
        Some(PlanDetail::Text(value)) => value,
        other => panic!("{key} should be text, got {other:?}"),
    }
}

fn pair_joins(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<&str, &PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| (text(node, "join_pair_identity"), node))
        .collect()
}

fn right_access<'a>(plan: &'a orna_sys_v1::ExplainedPlan, join: &PlanNode) -> &'a PlanNode {
    let reference = join.inputs()[1].as_str();
    plan.nodes()
        .iter()
        .find(|node| node.reference().as_str() == reference)
        .expect("paired join retains its own right access")
}

fn cost(node: &PlanNode) -> (Option<u64>, Option<u64>, Option<u64>) {
    (
        node.estimated_rows(),
        node.estimated_bytes(),
        node.estimated_work(),
    )
}

#[test]
fn paired_index_refold_identity_tracks_local_choice_and_sparse_parent_chain() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let descriptors = pairs();
    let baseline = explain(
        [
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
        ],
        &indexes(true),
        &descriptors,
    );
    let reordered = explain(
        [
            "table:Medium",
            "table:Small",
            "table:Unknown",
            "table:Expensive",
        ],
        &indexes(true).into_iter().rev().collect::<Vec<_>>(),
        &descriptors.iter().rev().cloned().collect::<Vec<_>>(),
    );
    let rebound = explain(
        [
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
        ],
        &indexes(false),
        &descriptors,
    );
    let baseline_pairs = pair_joins(&baseline);
    let reordered_pairs = pair_joins(&reordered);
    let rebound_pairs = pair_joins(&rebound);

    for (pair_id, table, selected_index, expected_cost) in [
        (
            "pair:small",
            "table:Small",
            Some("index:small-live-a"),
            (Some(80), Some(85_120), Some(108)),
        ),
        (
            "pair:medium",
            "table:Medium",
            Some("index:medium-live"),
            (Some(240), Some(288_240), Some(110)),
        ),
        (
            "pair:expensive",
            "table:Expensive",
            None,
            (Some(12_000), Some(15_948_000), Some(740)),
        ),
        ("pair:unknown", "table:Unknown", None, (None, None, None)),
    ] {
        let before = baseline_pairs[pair_id];
        let after = reordered_pairs[pair_id];
        let before_access = right_access(&baseline, before);
        let after_access = right_access(&reordered, after);
        let expected_kind = if selected_index.is_some() {
            PlanNodeKind::IndexLookup
        } else {
            PlanNodeKind::Scan
        };
        for (fold, access) in [(before, before_access), (after, after_access)] {
            assert_eq!(access.kind(), expected_kind, "access route for {pair_id}");
            assert_eq!(
                access.object().map(ObjectRef::as_str),
                selected_index.or(Some(table)),
                "selection remains paired to its logical source"
            );
            assert_eq!(cost(fold), expected_cost, "real estimate for {pair_id}");
            assert_eq!(
                text(fold, "paired_index_selection_identity"),
                text(access, "paired_index_selection_identity")
            );
            assert_eq!(
                text(fold, "paired_index_refold_identity"),
                text(access, "paired_index_refold_identity")
            );
            assert_eq!(
                text(fold, "paired_index_refold_parent_identity"),
                text(fold, "join_cost_fold_left_identity")
            );
            assert_eq!(
                text(fold, "paired_index_refold_input_identity"),
                text(fold, "join_cost_fold_right_identity")
            );
            assert_eq!(
                text(fold, "paired_index_refold_selection_identity"),
                text(fold, "paired_index_selection_identity")
            );
            assert_eq!(text(fold, "paired_index_refold_pair_identity"), pair_id);
            assert_eq!(
                text(fold, "paired_index_refold_pairing"),
                "sparse_parent_fold_pair_and_exact_index_outcome"
            );
            assert!(text(fold, "paired_index_refold_identity").starts_with("paired-index-refold:"));
            assert!(
                text(fold, "paired_index_selection_identity")
                    .starts_with("paired-index-selection:")
            );
            assert_eq!(
                text(access, "paired_join_cost_fold_identity"),
                text(fold, "join_cost_fold_identity")
            );
        }
        for detail in [
            "paired_index_selection_identity",
            "paired_index_refold_identity",
            "paired_index_refold_parent_identity",
            "paired_index_refold_input_identity",
            "paired_index_refold_pair_identity",
            "paired_index_refold_selection_identity",
            "join_cost_fold_right_identity",
            "join_cost_fold_identity",
        ] {
            assert_eq!(
                text(before, detail),
                text(after, detail),
                "catalog and declaration reordering preserve {detail} for {pair_id}"
            );
        }
    }

    let small_before = baseline_pairs["pair:small"];
    let small_rebound = rebound_pairs["pair:small"];
    assert_eq!(
        right_access(&rebound, small_rebound)
            .object()
            .map(ObjectRef::as_str),
        Some("index:small-live-z")
    );
    for detail in [
        "paired_index_selection_identity",
        "paired_index_refold_identity",
        "join_cost_fold_right_identity",
    ] {
        assert_ne!(
            text(small_before, detail),
            text(small_rebound, detail),
            "reselecting the first exact index rekeys its local paired refold ({detail})"
        );
    }
    for pair_id in ["pair:medium", "pair:expensive", "pair:unknown"] {
        let before = baseline_pairs[pair_id];
        let after = rebound_pairs[pair_id];
        assert_eq!(
            text(before, "paired_index_selection_identity"),
            text(after, "paired_index_selection_identity"),
            "downstream local selection remains attached to {pair_id}"
        );
        assert_eq!(
            text(before, "join_cost_fold_right_identity"),
            text(after, "join_cost_fold_right_identity"),
            "downstream right input remains local to {pair_id}"
        );
        assert_ne!(
            text(before, "paired_index_refold_parent_identity"),
            text(after, "paired_index_refold_parent_identity"),
            "downstream parent carries the changed sparse history at {pair_id}"
        );
        assert_ne!(
            text(before, "paired_index_refold_identity"),
            text(after, "paired_index_refold_identity"),
            "downstream pair refolds against the changed anchor at {pair_id}"
        );
        assert_ne!(
            text(before, "join_cost_fold_identity"),
            text(after, "join_cost_fold_identity"),
            "the total sparse cost cascade incorporates each refold at {pair_id}"
        );
        assert_eq!(
            cost(before),
            cost(after),
            "identity-only index change at {pair_id}"
        );
    }
}
