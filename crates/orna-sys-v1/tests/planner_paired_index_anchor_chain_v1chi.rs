use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, QueryJoinDescription,
    QueryJoinPairIdentityDescription, QueryPartialIndexDescription, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_join_pair_identities,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_index_anchor_chain_v1chi.orna");

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

fn query(order: [&str; 3]) -> QueryPlanDescription {
    let joins = order
        .into_iter()
        .map(|source| match source {
            "table:First" => QueryJoinDescription {
                source: object(source),
                statistics: Some(statistics(10, 1_000)),
                predicate: Some(expression("expr:first-live")),
            },
            "table:Gap" => QueryJoinDescription {
                source: object(source),
                statistics: Some(statistics(20, 2_000)),
                predicate: Some(expression("expr:gap-live")),
            },
            "table:Last" => QueryJoinDescription {
                source: object(source),
                statistics: Some(statistics(40, 4_000)),
                predicate: Some(expression("expr:last-live")),
            },
            other => panic!("unexpected source {other}"),
        })
        .collect();
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-index-anchor-chain-v1chi"),
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
        pair("pair:first", "table:First", "expr:first-live"),
        pair("pair:gap", "table:Gap", "expr:gap-live"),
        pair("pair:last", "table:Last", "expr:last-live"),
    ]
}

fn index(table: &str, name: &str, predicate: &str) -> QueryPartialIndexDescription {
    QueryPartialIndexDescription {
        table: object(table),
        index: object(name),
        partial_predicate: expression(predicate),
    }
}

fn indexes(first: &str) -> Vec<QueryPartialIndexDescription> {
    vec![
        index("table:First", first, "expr:first-live"),
        index("table:Gap", "index:gap-wrong", "expr:first-live"),
        index("table:Last", "index:last-live", "expr:last-live"),
    ]
}

fn explain(
    order: [&str; 3],
    indexes: &[QueryPartialIndexDescription],
    pairs: &[QueryJoinPairIdentityDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_join_pair_identities(&query(order), indexes, pairs)
        .expect("paired index anchor chain is explainable")
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

#[test]
fn paired_index_anchor_chain_survives_sparse_gaps_and_rebinds_descendants() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = explain(
        ["table:First", "table:Gap", "table:Last"],
        &indexes("index:first-a"),
        &pairs(),
    );
    let reordered = explain(
        ["table:Last", "table:Gap", "table:First"],
        &indexes("index:first-a")
            .into_iter()
            .rev()
            .collect::<Vec<_>>(),
        &pairs().into_iter().rev().collect::<Vec<_>>(),
    );
    let rebound = explain(
        ["table:First", "table:Gap", "table:Last"],
        &indexes("index:first-z"),
        &pairs(),
    );
    let baseline_pairs = pair_joins(&baseline);
    let reordered_pairs = pair_joins(&reordered);
    let rebound_pairs = pair_joins(&rebound);

    for pair_id in ["pair:first", "pair:gap", "pair:last"] {
        let before = baseline_pairs[pair_id];
        let after = reordered_pairs[pair_id];
        assert_eq!(
            text(before, "paired_index_anchor_chain_identity"),
            text(after, "paired_index_anchor_chain_identity"),
            "declaration and catalog order do not perturb {pair_id}'s accumulated anchor"
        );
        assert_eq!(
            text(before, "paired_index_anchor_chain_identity"),
            text(
                right_access(&baseline, before),
                "paired_index_anchor_chain_identity"
            )
        );
    }

    let first = baseline_pairs["pair:first"];
    let gap = baseline_pairs["pair:gap"];
    let last = baseline_pairs["pair:last"];
    assert_eq!(
        right_access(&baseline, first).kind(),
        PlanNodeKind::IndexLookup
    );
    assert_eq!(right_access(&baseline, gap).kind(), PlanNodeKind::Scan);
    assert_eq!(
        right_access(&baseline, last).kind(),
        PlanNodeKind::IndexLookup
    );
    assert_eq!(first.estimated_rows(), Some(100));
    assert_eq!(first.estimated_bytes(), Some(14_000));
    assert_eq!(first.estimated_work(), Some(110));
    assert_eq!(gap.estimated_rows(), Some(200));
    assert_eq!(gap.estimated_bytes(), Some(48_000));
    assert_eq!(gap.estimated_work(), Some(120));
    assert_eq!(last.estimated_rows(), Some(800));
    assert_eq!(last.estimated_bytes(), Some(272_000));
    assert_eq!(last.estimated_work(), Some(240));

    let first_chain = text(first, "paired_index_anchor_chain_identity");
    let gap_chain = text(gap, "paired_index_anchor_chain_identity");
    assert_eq!(
        text(first, "paired_index_anchor_chain_transition"),
        "append_exact_index_refold"
    );
    assert_eq!(
        text(gap, "paired_index_anchor_chain_transition"),
        "carry_through_sparse_cost_fold"
    );
    assert_eq!(
        first_chain, gap_chain,
        "unindexed gap carries the anchor chain"
    );
    assert_eq!(
        text(last, "paired_index_anchor_chain_transition"),
        "append_exact_index_refold"
    );
    assert_eq!(
        text(last, "paired_index_anchor_chain_parent_identity"),
        gap_chain,
        "second exact index appends to the carried first anchor"
    );
    assert_eq!(
        text(last, "paired_index_anchor_chain_step_refold_identity"),
        text(last, "paired_index_refold_identity")
    );
    assert_eq!(
        text(last, "paired_index_anchor_chain_step_selection_identity"),
        text(last, "paired_index_selection_identity")
    );

    let changed_first = rebound_pairs["pair:first"];
    let changed_gap = rebound_pairs["pair:gap"];
    let changed_last = rebound_pairs["pair:last"];
    assert_ne!(
        text(first, "paired_index_anchor_chain_identity"),
        text(changed_first, "paired_index_anchor_chain_identity"),
        "changing the first exact index rekeys the first anchor"
    );
    assert_eq!(
        text(changed_first, "paired_index_anchor_chain_identity"),
        text(changed_gap, "paired_index_anchor_chain_identity"),
        "the sparse scan carries the changed anchor without advancing it"
    );
    assert_ne!(
        text(last, "paired_index_anchor_chain_identity"),
        text(changed_last, "paired_index_anchor_chain_identity"),
        "the later exact index refolds under the changed anchor"
    );
    assert_eq!(
        text(last, "paired_index_selection_identity"),
        text(changed_last, "paired_index_selection_identity"),
        "the later pair's local selected route stays unchanged"
    );
    assert_eq!(last.estimated_rows(), changed_last.estimated_rows());
    assert_eq!(last.estimated_bytes(), changed_last.estimated_bytes());
    assert_eq!(last.estimated_work(), changed_last.estimated_work());
}
