use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, QueryJoinDescription,
    QueryJoinPairIdentityDescription, QueryLimitPushdownDescription, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef,
    explain_query_with_join_pair_identities_and_limit_pushdowns,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_limit_pushdown_anchor_fold_oabpa.orna");

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
        "table:Large" => (Some(1_000), Some(20_000)),
        "table:Unknown" => (None, None),
        "table:Small" => (Some(30), Some(3_000)),
        "table:Middle" => (Some(50), Some(5_000)),
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
        snapshot: SnapshotRef::descriptive("snapshot:paired-limit-anchor-fold-oabpa"),
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

fn limit_stage(
    pair_identity: &str,
    source: &str,
    name: &str,
    limit: u64,
) -> QueryLimitPushdownDescription {
    QueryLimitPushdownDescription {
        identity: object(&format!("limit:{name}")),
        join_pair_identity: object(pair_identity),
        source: object(source),
        limit,
    }
}

fn limit_pushdowns(order: [&str; 4]) -> Vec<QueryLimitPushdownDescription> {
    let mut pushdowns = Vec::new();
    for source in order {
        match source {
            "table:Small" => {
                pushdowns.push(limit_stage("pair:Small", source, "small-first", 15));
                pushdowns.push(limit_stage("pair:Small", source, "small-second", 10));
            }
            "table:Middle" => {
                pushdowns.push(limit_stage("pair:Middle", source, "middle-only", 20));
            }
            "table:Large" => {
                pushdowns.push(limit_stage("pair:Large", source, "large-only", 100));
            }
            "table:Unknown" => {
                pushdowns.push(limit_stage("pair:Unknown", source, "unknown-only", 7));
            }
            other => panic!("unexpected source {other}"),
        }
    }
    pushdowns
}

fn plan_with(
    anchor: &str,
    order: [&str; 4],
    pairs: &[QueryJoinPairIdentityDescription],
    pushdowns: &[QueryLimitPushdownDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities_and_limit_pushdowns(
        &query(anchor, order),
        pairs,
        pushdowns,
    )
    .expect("exact-pair input limits plan through sparse cost reordering")
}

fn plan(anchor: &str, order: [&str; 4]) -> orna_sys_v1::ExplainedPlan {
    plan_with(
        anchor,
        order,
        &pairs(anchor, order),
        &limit_pushdowns(order),
    )
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

fn joins_by_source<'a>(plan: &'a orna_sys_v1::ExplainedPlan) -> BTreeMap<&'a str, &'a PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| (text(node, "logical_right_source_identity"), node))
        .collect()
}

#[test]
fn paired_limit_pushdown_folds_follow_sparse_anchors_and_ordered_chains() {
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
    let left = plan("table:ParentLeft", declared);
    let left_reordered = plan("table:ParentLeft", reordered);
    let right = plan("table:ParentRight", declared);
    let left_joins = joins_by_source(&left);
    let reordered_joins = joins_by_source(&left_reordered);
    let right_joins = joins_by_source(&right);
    let expected = [
        ("table:Small", Some(100), Some(14_000), Some(110)),
        ("table:Middle", Some(200), Some(48_000), Some(120)),
        ("table:Large", Some(2_000), Some(520_000), Some(300)),
        ("table:Unknown", None, None, None),
    ];

    for (source, rows, bytes, work) in expected {
        let left_join = left_joins[source];
        let reordered_join = reordered_joins[source];
        let right_join = right_joins[source];
        let paired_fold = text(left_join, "paired_limit_pushdown_anchor_fold_identity");

        assert_eq!(
            (
                left_join.estimated_rows(),
                left_join.estimated_bytes(),
                left_join.estimated_work()
            ),
            (rows, bytes, work),
            "join estimates reflect each exact input's pushed limits for {source}"
        );
        assert_eq!(
            paired_fold,
            text(reordered_join, "paired_limit_pushdown_anchor_fold_identity"),
            "declaration reordering preserves the pair's anchor-scoped limit fold"
        );
        assert_ne!(
            paired_fold,
            text(right_join, "paired_limit_pushdown_anchor_fold_identity"),
            "the same limited pair under a different sparse anchor has a distinct fold"
        );
        assert_eq!(
            text(left_join, "paired_limit_pushdown_anchor_fold_pairing"),
            "sparse_anchor_fold_resolved_join_pair_and_limit_chain"
        );
        assert_eq!(
            text(left_join, "limit_pushdown_chain_identity"),
            text(right_join, "limit_pushdown_chain_identity"),
            "each right-side limit chain stays stable across anchors"
        );
        assert_eq!(
            text(left_join, "join_cost_fold_identity"),
            text(reordered_join, "join_cost_fold_identity")
        );
        assert_ne!(
            text(left_join, "join_cost_fold_identity"),
            text(right_join, "join_cost_fold_identity")
        );

        let mut stages = left
            .nodes()
            .iter()
            .filter(|node| {
                node.kind() == PlanNodeKind::Limit
                    && text(node, "limit_pushdown_source_identity") == source
            })
            .collect::<Vec<_>>();
        stages.sort_by_key(|node| integer(node, "limit_pushdown_chain_position"));
        let expected_stages: &[(u64, Option<u64>, Option<u64>, Option<u64>)] = match source {
            "table:Small" => &[
                (15, Some(15), Some(1_500), Some(30)),
                (10, Some(10), Some(1_000), Some(15)),
            ],
            "table:Middle" => &[(20, Some(20), Some(2_000), Some(50))],
            "table:Large" => &[(100, Some(100), Some(2_000), Some(1_000))],
            "table:Unknown" => &[(7, None, None, None)],
            _ => unreachable!(),
        };
        assert_eq!(stages.len(), expected_stages.len());
        for (index, (node, (limit, rows, bytes, stage_work))) in
            stages.iter().zip(expected_stages).enumerate()
        {
            assert_eq!(integer(node, "limit"), *limit);
            assert_eq!(
                integer(node, "limit_pushdown_chain_position"),
                (index + 1) as u64
            );
            assert_eq!(
                (
                    node.estimated_rows(),
                    node.estimated_bytes(),
                    node.estimated_work()
                ),
                (*rows, *bytes, *stage_work),
                "each limit stage computes its output and charges its immediate input"
            );
            assert_eq!(
                text(node, "paired_limit_pushdown_anchor_fold_identity"),
                paired_fold
            );
        }
    }

    let mut planned = left
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .collect::<Vec<_>>();
    planned.sort_by_key(|node| integer(node, "planned_input_position"));
    assert_eq!(
        [
            "table:Small",
            "table:Middle",
            "table:Large",
            "table:Unknown"
        ],
        planned
            .iter()
            .map(|node| text(node, "logical_right_source_identity"))
            .collect::<Vec<_>>()
            .as_slice(),
        "known scan work remains before the sparse unknown input"
    );
    assert_eq!(
        (left.root().estimated_rows(), left.root().estimated_bytes()),
        (None, None),
        "the unknown tail keeps final output estimates unknown"
    );

    let mut alternate_pairs = pairs("table:ParentLeft", declared);
    let small_pair = alternate_pairs
        .iter_mut()
        .find(|pair| pair.right_source.as_str() == "table:Small")
        .unwrap();
    small_pair.identity = object("pair:Small:alternate");
    let mut alternate_pair_limits = limit_pushdowns(declared);
    for pushdown in &mut alternate_pair_limits {
        if pushdown.source.as_str() == "table:Small" {
            pushdown.join_pair_identity = object("pair:Small:alternate");
        }
    }
    let alternate_pair_plan = plan_with(
        "table:ParentLeft",
        declared,
        &alternate_pairs,
        &alternate_pair_limits,
    );
    assert_ne!(
        text(
            left_joins["table:Small"],
            "paired_limit_pushdown_anchor_fold_identity"
        ),
        text(
            joins_by_source(&alternate_pair_plan)["table:Small"],
            "paired_limit_pushdown_anchor_fold_identity"
        ),
        "changing the exact resolver pair changes the paired limit fold"
    );

    let mut alternate_limits = limit_pushdowns(declared);
    alternate_limits
        .iter_mut()
        .find(|pushdown| pushdown.identity.as_str() == "limit:small-first")
        .unwrap()
        .limit = 12;
    let alternate_limit_plan = plan_with(
        "table:ParentLeft",
        declared,
        &pairs("table:ParentLeft", declared),
        &alternate_limits,
    );
    assert_ne!(
        text(
            left_joins["table:Small"],
            "paired_limit_pushdown_anchor_fold_identity"
        ),
        text(
            joins_by_source(&alternate_limit_plan)["table:Small"],
            "paired_limit_pushdown_anchor_fold_identity"
        ),
        "changing an ordered limit stage changes the paired fold identity"
    );
    assert_ne!(
        text(
            left_joins["table:Middle"],
            "paired_limit_pushdown_anchor_fold_identity"
        ),
        text(
            joins_by_source(&alternate_limit_plan)["table:Middle"],
            "paired_limit_pushdown_anchor_fold_identity"
        ),
        "an upstream limit change is carried into subsequent sparse anchor folds"
    );
}
