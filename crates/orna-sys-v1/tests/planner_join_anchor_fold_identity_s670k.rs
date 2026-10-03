use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, QueryJoinDescription,
    QueryJoinPairIdentityDescription, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_join_pair_identities,
};

const FIXTURE: &str = include_str!("fixtures/planner_join_anchor_fold_identity_s670k.orna");

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

fn join(source: &str) -> QueryJoinDescription {
    let (rows, bytes) = match source {
        "table:Expensive" => (Some(500), Some(64_000)),
        "table:Unknown" => (None, None),
        "table:Small" => (Some(8), Some(8_192)),
        "table:Medium" => (Some(30), Some(4_096)),
        other => panic!("unexpected source {other}"),
    };
    QueryJoinDescription {
        source: object(source),
        statistics: rows.zip(bytes).map(|(rows, bytes)| statistics(rows, bytes)),
        predicate: Some(expression(&format!(
            "expr:anchor-{}",
            source.trim_start_matches("table:")
        ))),
    }
}

fn query(anchor: &str, order: [&str; 4]) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-join-anchor-fold-s670k"),
        source: object(anchor),
        source_statistics: Some(statistics(100, 4_000)),
        joins: order.into_iter().map(join).collect(),
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    }
}

fn pair(anchor: &str, source: &str) -> QueryJoinPairIdentityDescription {
    QueryJoinPairIdentityDescription {
        identity: object(&format!("pair:{}", source.trim_start_matches("table:"))),
        left_source: object(anchor),
        right_source: object(source),
        predicate: Some(expression(&format!(
            "expr:anchor-{}",
            source.trim_start_matches("table:")
        ))),
    }
}

fn pairs(anchor: &str, order: [&str; 4]) -> Vec<QueryJoinPairIdentityDescription> {
    order
        .into_iter()
        .map(|source| pair(anchor, source))
        .collect()
}

fn plan(anchor: &str, order: [&str; 4]) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities(&query(anchor, order), &pairs(anchor, order))
        .expect("exact logical pairs remain resolved through sparse cost planning")
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
        other => panic!("{key} should be an integer, got {other:?}"),
    }
}

fn joins_by_pair(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<&str, &PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| (text(node, "join_pair_identity"), node))
        .collect()
}

fn nodes_by_reference(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<String, &PlanNode> {
    plan.nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect()
}

#[test]
fn paired_join_folds_keep_anchor_identity_through_sparse_reordering() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let left = plan(
        "table:ParentLeft",
        [
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
        ],
    );
    let left_reordered = plan(
        "table:ParentLeft",
        [
            "table:Medium",
            "table:Small",
            "table:Unknown",
            "table:Expensive",
        ],
    );
    let right = plan(
        "table:ParentRight",
        [
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
        ],
    );
    let left_joins = joins_by_pair(&left);
    let reordered_joins = joins_by_pair(&left_reordered);
    let right_joins = joins_by_pair(&right);
    let left_nodes = nodes_by_reference(&left);
    let expected = [
        (
            "pair:Small",
            "table:Small",
            Some(80),
            Some(85_120),
            Some(108),
        ),
        (
            "pair:Medium",
            "table:Medium",
            Some(240),
            Some(288_240),
            Some(110),
        ),
        (
            "pair:Expensive",
            "table:Expensive",
            Some(12_000),
            Some(15_948_000),
            Some(740),
        ),
        ("pair:Unknown", "table:Unknown", None, None, None),
    ];

    for (pair_id, source, rows, bytes, work) in expected {
        let left_join = left_joins[pair_id];
        let reordered_join = reordered_joins[pair_id];
        let right_join = right_joins[pair_id];
        assert_eq!(text(left_join, "logical_right_source_identity"), source);
        assert_eq!(
            (
                left_join.estimated_rows(),
                left_join.estimated_bytes(),
                left_join.estimated_work()
            ),
            (rows, bytes, work),
            "pair identity metadata preserves computed estimates for {pair_id}"
        );
        assert_eq!(
            text(left_join, "join_pair_anchor_fold_identity"),
            text(reordered_join, "join_pair_anchor_fold_identity"),
            "declaration reordering preserves the logical-pair anchor fold"
        );
        assert_eq!(
            text(left_join, "join_cost_fold_identity"),
            text(reordered_join, "join_cost_fold_identity"),
            "the accumulated fold is stable when cost order is unchanged"
        );
        assert_ne!(
            text(left_join, "join_pair_anchor_fold_identity"),
            text(right_join, "join_pair_anchor_fold_identity"),
            "paired inputs under distinct sparse anchors receive distinct identities"
        );
        assert_ne!(
            text(left_join, "join_cost_fold_identity"),
            text(right_join, "join_cost_fold_identity"),
            "the full cost fold includes the selected anchor identity"
        );
        assert_eq!(
            text(left_join, "join_pair_anchor_fold_pairing"),
            "sparse_anchor_fold_and_resolved_join_pair"
        );
        assert_eq!(
            text(left_join, "join_cost_fold_right_identity"),
            text(right_join, "join_cost_fold_right_identity"),
            "identical right inputs retain one child identity across anchors"
        );
        assert_ne!(
            text(left_join, "join_cost_fold_left_identity"),
            text(right_join, "join_cost_fold_left_identity"),
            "each anchor contributes its own accumulated left-side identity"
        );

        let right_input = left_nodes[left_join.inputs()[1].as_str()];
        assert_eq!(
            text(right_input, "join_pair_anchor_fold_identity"),
            text(left_join, "join_pair_anchor_fold_identity")
        );
        assert_eq!(
            text(right_input, "paired_join_cost_fold_identity"),
            text(left_join, "join_cost_fold_identity")
        );
    }

    assert_ne!(
        text(left_joins["pair:Small"], "join_pair_anchor_fold_identity"),
        text(left_joins["pair:Medium"], "join_pair_anchor_fold_identity"),
        "different logical pairs do not share an anchor-fold identity"
    );
    let mut planned = left
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .collect::<Vec<_>>();
    planned.sort_by_key(|node| integer(node, "planned_input_position"));
    assert_eq!(
        [
            "pair:Small",
            "pair:Medium",
            "pair:Expensive",
            "pair:Unknown"
        ],
        planned
            .iter()
            .map(|node| text(node, "join_pair_identity"))
            .collect::<Vec<_>>()
            .as_slice(),
        "known work is ordered ahead of the sparse unknown input"
    );
    assert_eq!(
        (left.root().estimated_rows(), left.root().estimated_bytes()),
        (None, None),
        "unknown-cost tail keeps the final output estimates unknown"
    );
}
