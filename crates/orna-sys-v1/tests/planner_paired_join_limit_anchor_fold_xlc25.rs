use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, QueryJoinDescription,
    QueryJoinPairIdentityDescription, QueryLimitPushdownDescription, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef,
    explain_query_with_join_pair_identities_and_limit_pushdowns,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_join_limit_anchor_fold_xlc25.orna");

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
    let statistics = match source {
        "table:Small" => Some(statistics(30, 3_000)),
        "table:Middle" => Some(statistics(50, 5_000)),
        "table:Large" => Some(statistics(1_000, 20_000)),
        "table:Unknown" => None,
        other => panic!("unexpected source {other}"),
    };
    QueryJoinDescription {
        source: object(source),
        statistics,
        predicate: Some(expression(&format!(
            "expr:anchor-{}",
            source.trim_start_matches("table:")
        ))),
    }
}

fn query(anchor: &str, order: [&str; 4]) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-join-limit-anchor-fold-xlc25"),
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

fn limit(source: &str, identity: &str, value: u64) -> QueryLimitPushdownDescription {
    QueryLimitPushdownDescription {
        identity: object(&format!("limit:{identity}")),
        join_pair_identity: object(&format!("pair:{}", source.trim_start_matches("table:"))),
        source: object(source),
        limit: value,
    }
}

fn limits(small_last: u64) -> Vec<QueryLimitPushdownDescription> {
    vec![
        limit("table:Small", "small-first", 15),
        limit("table:Small", "small-last", small_last),
        limit("table:Large", "large-only", 100),
    ]
}

fn plan(
    anchor: &str,
    order: [&str; 4],
    pairs: &[QueryJoinPairIdentityDescription],
    limits: &[QueryLimitPushdownDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities_and_limit_pushdowns(
        &query(anchor, order),
        pairs,
        limits,
    )
    .expect("exact join/limit pairs fold through sparse inputs")
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

fn joins_by_source(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<&str, &PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| (text(node, "logical_right_source_identity"), node))
        .collect()
}

#[test]
fn paired_join_fold_identities_follow_sparse_exact_limit_chains() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let declared = [
        "table:Large",
        "table:Unknown",
        "table:Small",
        "table:Middle",
    ];
    let reordered = [
        "table:Middle",
        "table:Small",
        "table:Unknown",
        "table:Large",
    ];
    let limit_descriptors = limits(10);
    let original = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &limit_descriptors,
    );
    let reordered_plan = plan(
        "table:AnchorLeft",
        reordered,
        &pairs("table:AnchorLeft", reordered)
            .into_iter()
            .rev()
            .collect::<Vec<_>>(),
        &limit_descriptors,
    );
    let other_anchor = plan(
        "table:AnchorRight",
        declared,
        &pairs("table:AnchorRight", declared),
        &limit_descriptors,
    );
    let joins = joins_by_source(&original);
    let reordered_joins = joins_by_source(&reordered_plan);
    let other_joins = joins_by_source(&other_anchor);

    let small_pair_fold = text(
        joins["table:Small"],
        "paired_join_limit_anchor_fold_identity",
    );
    let large_pair_fold = text(
        joins["table:Large"],
        "paired_join_limit_anchor_fold_identity",
    );
    assert_ne!(small_pair_fold, large_pair_fold);
    assert_eq!(
        small_pair_fold,
        text(
            reordered_joins["table:Small"],
            "paired_join_limit_anchor_fold_identity"
        ),
        "reordering declarations preserves the exact pair and its ordered limits"
    );
    assert_ne!(
        small_pair_fold,
        text(
            other_joins["table:Small"],
            "paired_join_limit_anchor_fold_identity"
        ),
        "changing the left join anchor changes the paired fold"
    );

    let small_cascade = text(
        joins["table:Small"],
        "paired_join_limit_anchor_cascade_fold_identity",
    );
    assert_eq!(
        small_cascade,
        text(
            joins["table:Middle"],
            "paired_join_limit_anchor_cascade_fold_identity"
        ),
        "a middle join without limits carries the accumulated sparse fold unchanged"
    );
    assert_eq!(
        text(
            joins["table:Unknown"],
            "paired_join_limit_anchor_cascade_fold_identity"
        ),
        text(
            joins["table:Large"],
            "paired_join_limit_anchor_cascade_fold_identity"
        ),
        "an unknown-cost trailing join also preserves the latest limited pair"
    );
    assert_eq!(
        integer(
            original.root(),
            "paired_join_limit_anchor_cascade_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(
            original.root(),
            "paired_join_limit_anchor_cascade_stage_count"
        ),
        3
    );
    assert!(matches!(
        original
            .root()
            .details()
            .get("paired_join_limit_anchor_cascade_overflowed"),
        Some(PlanDetail::Boolean(false))
    ));
    assert_eq!(
        text(
            original.root(),
            "paired_join_limit_anchor_cascade_fold_identity"
        ),
        text(
            reordered_plan.root(),
            "paired_join_limit_anchor_cascade_fold_identity"
        ),
        "the planned sparse pair sequence is stable under declaration reordering"
    );
    assert_eq!(
        text(original.root(), "join_cost_fold_identity"),
        text(reordered_plan.root(), "join_cost_fold_identity")
    );

    let mut small_stages = original
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Limit
                && text(node, "limit_pushdown_source_identity") == "table:Small"
        })
        .collect::<Vec<_>>();
    small_stages.sort_by_key(|node| integer(node, "limit_pushdown_chain_position"));
    assert_eq!(small_stages.len(), 2);
    assert_eq!(
        (
            integer(small_stages[0], "limit"),
            small_stages[0].estimated_rows(),
            small_stages[0].estimated_bytes(),
            small_stages[0].estimated_work()
        ),
        (15, Some(15), Some(1_500), Some(30)),
        "the first limit outputs 15 rows and 1,500 bytes after examining 30 rows"
    );
    assert_eq!(
        (
            integer(small_stages[1], "limit"),
            small_stages[1].estimated_rows(),
            small_stages[1].estimated_bytes(),
            small_stages[1].estimated_work()
        ),
        (10, Some(10), Some(1_000), Some(15)),
        "the second limit outputs 10 rows and 1,000 bytes after examining 15 rows"
    );
    let large_stage = original
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Limit
                && text(node, "limit_pushdown_source_identity") == "table:Large"
        })
        .expect("large input's exact limit stage");
    assert_eq!(
        (
            integer(large_stage, "limit"),
            large_stage.estimated_rows(),
            large_stage.estimated_bytes(),
            large_stage.estimated_work()
        ),
        (100, Some(100), Some(2_000), Some(1_000))
    );

    let changed = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &limits(9),
    );
    assert_ne!(
        text(
            original.root(),
            "paired_join_limit_anchor_cascade_fold_identity"
        ),
        text(
            changed.root(),
            "paired_join_limit_anchor_cascade_fold_identity"
        ),
        "changing a limit stage changes the paired join cascade identity"
    );
    assert_ne!(
        text(original.root(), "join_cost_fold_identity"),
        text(changed.root(), "join_cost_fold_identity"),
        "the cumulative join cost fold includes the paired limit cascade"
    );
    let mut changed_small_stages = changed
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Limit
                && text(node, "limit_pushdown_source_identity") == "table:Small"
        })
        .collect::<Vec<_>>();
    changed_small_stages.sort_by_key(|node| integer(node, "limit_pushdown_chain_position"));
    assert_eq!(changed_small_stages.len(), 2);
    assert_eq!(changed_small_stages[1].estimated_rows(), Some(9));
    assert_eq!(changed_small_stages[1].estimated_bytes(), Some(900));
}
