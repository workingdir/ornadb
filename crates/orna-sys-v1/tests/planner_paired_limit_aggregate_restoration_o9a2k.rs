use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryLimitPushdownDescription,
    QueryPlanDescription, QuerySourceStatistics, QueryWindowAggregatePushdownDescription,
    SnapshotRef, explain_query_with_join_pair_identities_and_limit_window_aggregate_pushdowns,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_paired_limit_aggregate_restoration_o9a2k.orna");

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
    QueryJoinDescription {
        source: object(source),
        statistics: match source {
            "table:Small" => Some(statistics(30, 3_000)),
            "table:Middle" => Some(statistics(50, 5_000)),
            "table:Large" => Some(statistics(1_000, 20_000)),
            "table:Unknown" => None,
            other => panic!("unexpected source {other}"),
        },
        predicate: Some(expression(&format!(
            "expr:anchor-{}",
            source.trim_start_matches("table:")
        ))),
    }
}

fn query(anchor: &str, order: [&str; 4]) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-limit-aggregate-restoration-o9a2k"),
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

fn limit(source: &str, name: &str, value: u64) -> QueryLimitPushdownDescription {
    QueryLimitPushdownDescription {
        identity: object(&format!("limit:{name}")),
        join_pair_identity: object(&format!("pair:{}", source.trim_start_matches("table:"))),
        source: object(source),
        limit: value,
    }
}

fn limits(small_last: u64) -> Vec<QueryLimitPushdownDescription> {
    vec![
        limit("table:Small", "small-first", 15),
        limit("table:Small", "small-last", small_last),
        limit("table:Unknown", "unknown-only", 7),
    ]
}

fn aggregate(identity: &str, source: &str, frame: &str) -> QueryWindowAggregatePushdownDescription {
    QueryWindowAggregatePushdownDescription {
        identity: object(identity),
        source: object(source),
        aggregate: expression(&format!("expr:sum-{identity}")),
        frame_identity: expression(frame),
        frame_start: PlanWindowFrameBound::UnboundedPreceding,
        frame_end: PlanWindowFrameBound::CurrentRow,
    }
}

fn aggregates() -> Vec<QueryWindowAggregatePushdownDescription> {
    vec![
        aggregate("window:small-running", "table:Small", "frame:small-running"),
        aggregate("window:small-recent", "table:Small", "frame:small-recent"),
        aggregate(
            "window:middle-running",
            "table:Middle",
            "frame:middle-running",
        ),
        aggregate(
            "window:unknown-running",
            "table:Unknown",
            "frame:unknown-running",
        ),
    ]
}

fn plan(
    anchor: &str,
    order: [&str; 4],
    pairs: &[QueryJoinPairIdentityDescription],
    limits: &[QueryLimitPushdownDescription],
    aggregates: &[QueryWindowAggregatePushdownDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities_and_limit_window_aggregate_pushdowns(
        &query(anchor, order),
        pairs,
        limits,
        aggregates,
    )
    .expect("paired limits and aggregate restoration chains plan over sparse joins")
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
fn paired_limit_fold_identity_survives_sparse_aggregate_restoration_chains() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let declared = [
        "table:Unknown",
        "table:Large",
        "table:Middle",
        "table:Small",
    ];
    let reordered = [
        "table:Small",
        "table:Middle",
        "table:Large",
        "table:Unknown",
    ];
    let aggregate_descriptors = aggregates();
    let limit_descriptors = limits(10);
    let original = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &limit_descriptors,
        &aggregate_descriptors,
    );
    let reordered_pairs = pairs("table:AnchorLeft", reordered)
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    let reordered_limits = vec![
        limit_descriptors[2].clone(),
        limit_descriptors[0].clone(),
        limit_descriptors[1].clone(),
    ];
    let reordered_aggregates = vec![
        aggregate_descriptors[3].clone(),
        aggregate_descriptors[0].clone(),
        aggregate_descriptors[1].clone(),
        aggregate_descriptors[2].clone(),
    ];
    let reordered_plan = plan(
        "table:AnchorLeft",
        reordered,
        &reordered_pairs,
        &reordered_limits,
        &reordered_aggregates,
    );
    let other_anchor = plan(
        "table:AnchorRight",
        declared,
        &pairs("table:AnchorRight", declared),
        &limit_descriptors,
        &aggregate_descriptors,
    );
    let joins = joins_by_source(&original);
    let reordered_joins = joins_by_source(&reordered_plan);
    let other_joins = joins_by_source(&other_anchor);

    let small = joins["table:Small"];
    let middle = joins["table:Middle"];
    let large = joins["table:Large"];
    let unknown = joins["table:Unknown"];
    let small_fold = text(small, "paired_limit_aggregate_restoration_fold_identity");
    assert_eq!(
        integer(
            small,
            "paired_limit_aggregate_restoration_aggregate_pair_count"
        ),
        1
    );
    assert_eq!(
        integer(
            small,
            "paired_limit_aggregate_restoration_aggregate_stage_count"
        ),
        2
    );
    assert_eq!(
        integer(small, "paired_limit_aggregate_restoration_limit_pair_count"),
        1
    );
    assert_eq!(
        integer(
            small,
            "paired_limit_aggregate_restoration_limit_stage_count"
        ),
        2
    );
    assert!(matches!(
        small
            .details()
            .get("paired_limit_aggregate_restoration_pair_has_limit"),
        Some(PlanDetail::Boolean(true))
    ));
    assert!(matches!(
        small
            .details()
            .get("paired_limit_aggregate_restoration_pair_has_aggregate"),
        Some(PlanDetail::Boolean(true))
    ));

    let middle_fold = text(middle, "paired_limit_aggregate_restoration_fold_identity");
    assert_ne!(
        small_fold, middle_fold,
        "the aggregate-only pair advances the combined fold"
    );
    assert_eq!(
        integer(
            middle,
            "paired_limit_aggregate_restoration_aggregate_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(
            middle,
            "paired_limit_aggregate_restoration_aggregate_stage_count"
        ),
        3
    );
    assert_eq!(
        integer(
            middle,
            "paired_limit_aggregate_restoration_limit_pair_count"
        ),
        1
    );
    assert_eq!(
        integer(
            middle,
            "paired_limit_aggregate_restoration_limit_stage_count"
        ),
        2
    );
    assert!(matches!(
        middle
            .details()
            .get("paired_limit_aggregate_restoration_pair_has_limit"),
        Some(PlanDetail::Boolean(false))
    ));
    assert_eq!(
        text(large, "paired_limit_aggregate_restoration_fold_identity"),
        middle_fold,
        "a sparse input without either chain carries both identities unchanged"
    );
    assert_eq!(
        integer(large, "paired_limit_aggregate_restoration_limit_pair_count"),
        1
    );
    assert_eq!(
        integer(
            large,
            "paired_limit_aggregate_restoration_aggregate_pair_count"
        ),
        2
    );

    let final_fold = text(unknown, "paired_limit_aggregate_restoration_fold_identity");
    assert_ne!(
        middle_fold, final_fold,
        "the final limit and aggregate pair advances the fold"
    );
    assert_eq!(
        integer(
            unknown,
            "paired_limit_aggregate_restoration_aggregate_pair_count"
        ),
        3
    );
    assert_eq!(
        integer(
            unknown,
            "paired_limit_aggregate_restoration_aggregate_stage_count"
        ),
        4
    );
    assert_eq!(
        integer(
            unknown,
            "paired_limit_aggregate_restoration_limit_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(
            unknown,
            "paired_limit_aggregate_restoration_limit_stage_count"
        ),
        3
    );
    assert!(matches!(
        unknown
            .details()
            .get("paired_limit_aggregate_restoration_pair_has_limit"),
        Some(PlanDetail::Boolean(true))
    ));
    assert_eq!(
        final_fold,
        text(
            reordered_joins["table:Unknown"],
            "paired_limit_aggregate_restoration_fold_identity"
        ),
        "planned order preserves identity when declarations and descriptor groups reorder"
    );
    assert_ne!(
        final_fold,
        text(
            other_joins["table:Unknown"],
            "paired_limit_aggregate_restoration_fold_identity"
        ),
        "the exact left anchor contributes to the paired fold"
    );
    assert_eq!(
        text(unknown, "join_cost_fold_identity"),
        text(reordered_joins["table:Unknown"], "join_cost_fold_identity")
    );

    let mut small_limits = original
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Limit
                && text(node, "limit_pushdown_source_identity") == "table:Small"
        })
        .collect::<Vec<_>>();
    small_limits.sort_by_key(|node| integer(node, "limit_pushdown_chain_position"));
    assert_eq!(small_limits.len(), 2);
    assert_eq!(
        (
            integer(small_limits[0], "limit"),
            small_limits[0].estimated_rows(),
            small_limits[0].estimated_bytes(),
            small_limits[0].estimated_work(),
        ),
        (15, Some(15), Some(1_500), Some(30))
    );
    assert_eq!(
        (
            integer(small_limits[1], "limit"),
            small_limits[1].estimated_rows(),
            small_limits[1].estimated_bytes(),
            small_limits[1].estimated_work(),
        ),
        (10, Some(10), Some(1_000), Some(15))
    );
    let small_aggregates = original
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Aggregate
                && text(node, "window_source_identity") == "table:Small"
        })
        .collect::<Vec<_>>();
    assert_eq!(small_aggregates.len(), 2);
    assert!(
        small_aggregates
            .iter()
            .all(|node| { node.estimated_rows() == Some(10) && node.estimated_work() == Some(10) })
    );
    let middle_aggregate = original
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Aggregate
                && text(node, "window_source_identity") == "table:Middle"
        })
        .expect("aggregate-only restoration stage is present");
    assert_eq!(middle_aggregate.estimated_rows(), Some(50));
    assert_eq!(middle_aggregate.estimated_work(), Some(50));
    let unknown_aggregate = original
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Aggregate
                && text(node, "window_source_identity") == "table:Unknown"
        })
        .expect("unknown aggregate restoration stage is present");
    assert_eq!(unknown_aggregate.estimated_rows(), None);
    assert_eq!(unknown_aggregate.estimated_work(), None);
    let unknown_limit = original
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Limit
                && text(node, "limit_pushdown_source_identity") == "table:Unknown"
        })
        .expect("unknown paired limit stage is present");
    assert_eq!(integer(unknown_limit, "limit"), 7);
    assert_eq!(unknown_limit.estimated_rows(), None);
    assert_eq!(unknown_limit.estimated_bytes(), None);
    assert_eq!(unknown_limit.estimated_work(), None);

    let mut changed_limits = limit_descriptors.clone();
    changed_limits[1].limit = 9;
    let changed_limit = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &changed_limits,
        &aggregate_descriptors,
    );
    let changed_unknown = joins_by_source(&changed_limit)["table:Unknown"];
    assert_ne!(
        final_fold,
        text(
            changed_unknown,
            "paired_limit_aggregate_restoration_fold_identity"
        ),
        "a changed real limit value changes the later restoration fold"
    );
    assert_ne!(
        text(unknown, "join_cost_fold_identity"),
        text(changed_unknown, "join_cost_fold_identity")
    );
    let changed_small_limit = changed_limit
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Limit
                && text(node, "limit_pushdown_source_identity") == "table:Small"
                && integer(node, "limit") == 9
        })
        .expect("changed second small-input limit is present");
    assert_eq!(changed_small_limit.estimated_rows(), Some(9));
    assert_eq!(changed_small_limit.estimated_bytes(), Some(900));
    assert_eq!(changed_small_limit.estimated_work(), Some(15));

    let mut changed_aggregates = aggregate_descriptors.clone();
    changed_aggregates[2].frame_identity = expression("frame:middle-prior-two");
    changed_aggregates[2].frame_start = PlanWindowFrameBound::Preceding(2);
    let changed_frame = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &limit_descriptors,
        &changed_aggregates,
    );
    let changed_frame_unknown = joins_by_source(&changed_frame)["table:Unknown"];
    assert_ne!(
        final_fold,
        text(
            changed_frame_unknown,
            "paired_limit_aggregate_restoration_fold_identity"
        ),
        "aggregate restoration frame identity participates in the paired limit fold"
    );
    assert_ne!(
        text(unknown, "join_cost_fold_identity"),
        text(changed_frame_unknown, "join_cost_fold_identity")
    );
}
