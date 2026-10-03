use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, QueryWindowSpillDescription,
    SnapshotRef, explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_paired_aggregate_spill_restoration_iyekt.orna");
const SPARSE_IDENTITY_FIXTURE: &str =
    include_str!("fixtures/planner_paired_aggregate_spill_identity_xymnf.orna");

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

fn query(anchor: &str, order: [&str; 4]) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-aggregate-spill-restoration-iyekt"),
        source: object(anchor),
        source_statistics: Some(statistics(40, 40_000)),
        joins: order
            .into_iter()
            .map(|source| QueryJoinDescription {
                source: object(source),
                statistics: match source {
                    "table:Small" => Some(statistics(50, 6_400)),
                    "table:Middle" => Some(statistics(60, 8_192)),
                    "table:Large" => Some(statistics(300, 65_536)),
                    "table:Unknown" => None,
                    other => panic!("unexpected source {other}"),
                },
                predicate: Some(expression(&format!(
                    "expr:{}-{}",
                    anchor.trim_start_matches("table:"),
                    source.trim_start_matches("table:")
                ))),
            })
            .collect(),
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
            "expr:{}-{}",
            anchor.trim_start_matches("table:"),
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

fn aggregate(
    identity: &str,
    source: &str,
    frame: &str,
    start: PlanWindowFrameBound,
) -> QueryWindowAggregatePushdownDescription {
    QueryWindowAggregatePushdownDescription {
        identity: object(identity),
        source: object(source),
        aggregate: expression("expr:sum-value"),
        frame_identity: expression(frame),
        frame_start: start,
        frame_end: PlanWindowFrameBound::CurrentRow,
    }
}

fn aggregates() -> Vec<QueryWindowAggregatePushdownDescription> {
    vec![
        aggregate(
            "window:small-total",
            "table:Small",
            "frame:small-running",
            PlanWindowFrameBound::UnboundedPreceding,
        ),
        aggregate(
            "window:small-prior",
            "table:Small",
            "frame:small-prior-two",
            PlanWindowFrameBound::Preceding(2),
        ),
        aggregate(
            "window:large-total",
            "table:Large",
            "frame:large-running",
            PlanWindowFrameBound::UnboundedPreceding,
        ),
        aggregate(
            "window:unknown-total",
            "table:Unknown",
            "frame:unknown-running",
            PlanWindowFrameBound::UnboundedPreceding,
        ),
    ]
}

fn spill(
    identity: &str,
    pair_identity: &str,
    source: &str,
    aggregate_identity: &str,
    working_set: Option<u64>,
    budget: u64,
) -> QueryWindowSpillDescription {
    QueryWindowSpillDescription {
        identity: object(identity),
        join_pair_identity: object(pair_identity),
        source: object(source),
        window_aggregate_identity: object(aggregate_identity),
        estimated_working_set_bytes: working_set,
        memory_budget_bytes: budget,
    }
}

fn spills() -> Vec<QueryWindowSpillDescription> {
    vec![
        spill(
            "spill:small-total",
            "pair:Small",
            "table:Small",
            "window:small-total",
            Some(16_385),
            8_192,
        ),
        spill(
            "spill:small-prior",
            "pair:Small",
            "table:Small",
            "window:small-prior",
            Some(4_000),
            4_096,
        ),
        spill(
            "spill:unknown-total",
            "pair:Unknown",
            "table:Unknown",
            "window:unknown-total",
            None,
            512,
        ),
    ]
}

fn plan(
    anchor: &str,
    order: [&str; 4],
    pairs: &[QueryJoinPairIdentityDescription],
    aggregates: &[QueryWindowAggregatePushdownDescription],
    spills: &[QueryWindowSpillDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns(
        &query(anchor, order),
        pairs,
        aggregates,
        spills,
    )
    .expect("exact sparse aggregate and spill chains form a plan")
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
fn spill_fold_identity_survives_sparse_aggregate_restoration_chains() {
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
    let aggregates = aggregates();
    let spills = spills();
    let original = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &aggregates,
        &spills,
    );
    let reordered_plan = plan(
        "table:AnchorLeft",
        reordered,
        &pairs("table:AnchorLeft", reordered)
            .into_iter()
            .rev()
            .collect::<Vec<_>>(),
        &aggregates,
        &spills.iter().rev().cloned().collect::<Vec<_>>(),
    );
    let other_anchor = plan(
        "table:AnchorRight",
        declared,
        &pairs("table:AnchorRight", declared),
        &aggregates,
        &spills,
    );
    let joins = joins_by_source(&original);
    let reordered_joins = joins_by_source(&reordered_plan);
    let other_joins = joins_by_source(&other_anchor);

    let small = joins["table:Small"];
    let middle = joins["table:Middle"];
    let large = joins["table:Large"];
    let unknown = joins["table:Unknown"];
    let small_fold = text(small, "paired_aggregate_spill_restoration_fold_identity");
    assert_eq!(
        integer(
            small,
            "paired_aggregate_spill_restoration_aggregate_pair_count"
        ),
        1
    );
    assert_eq!(
        integer(
            small,
            "paired_aggregate_spill_restoration_aggregate_stage_count"
        ),
        2
    );
    assert_eq!(
        integer(small, "paired_aggregate_spill_restoration_spill_pair_count"),
        1
    );
    assert_eq!(
        integer(
            small,
            "paired_aggregate_spill_restoration_spill_stage_count"
        ),
        2
    );
    assert_eq!(
        integer(small, "paired_aggregate_spill_restoration_estimated_bytes"),
        8_193
    );
    assert_eq!(
        integer(
            small,
            "paired_aggregate_spill_restoration_estimated_io_blocks"
        ),
        3
    );
    assert_eq!(
        integer(
            small,
            "paired_aggregate_spill_restoration_estimated_io_work"
        ),
        6
    );
    assert_eq!(
        text(small, "paired_aggregate_spill_restoration_estimate_status"),
        "computed"
    );

    assert_eq!(
        text(small, "paired_aggregate_spill_restoration_fold_pairing"),
        "planned_sparse_aggregate_restoration_chains_with_accumulated_exact_spill_pairs"
    );
    assert!(matches!(
        small
            .details()
            .get("paired_aggregate_spill_restoration_pair_has_spill"),
        Some(PlanDetail::Boolean(true))
    ));
    assert!(matches!(
        large
            .details()
            .get("paired_aggregate_spill_restoration_pair_has_spill"),
        Some(PlanDetail::Boolean(false))
    ));
    assert_eq!(
        text(middle, "paired_aggregate_spill_restoration_fold_identity"),
        small_fold,
        "a nonaggregate join carries both aggregate identity and spill totals"
    );
    assert_eq!(
        integer(
            middle,
            "paired_aggregate_spill_restoration_aggregate_pair_count"
        ),
        1
    );
    assert_eq!(
        integer(
            middle,
            "paired_aggregate_spill_restoration_spill_pair_count"
        ),
        1
    );
    assert_ne!(
        small_fold,
        text(large, "paired_aggregate_spill_restoration_fold_identity"),
        "the next aggregate chain advances identity while retaining prior spills"
    );
    assert_eq!(
        integer(
            large,
            "paired_aggregate_spill_restoration_aggregate_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(
            large,
            "paired_aggregate_spill_restoration_aggregate_stage_count"
        ),
        3
    );
    assert_eq!(
        integer(large, "paired_aggregate_spill_restoration_spill_pair_count"),
        1
    );
    assert_eq!(
        integer(large, "paired_aggregate_spill_restoration_estimated_bytes"),
        8_193
    );
    assert_eq!(
        text(
            unknown,
            "paired_aggregate_spill_restoration_estimate_status"
        ),
        "unknown_working_set"
    );
    assert_eq!(
        integer(
            unknown,
            "paired_aggregate_spill_restoration_aggregate_pair_count"
        ),
        3
    );
    assert_eq!(
        integer(
            unknown,
            "paired_aggregate_spill_restoration_aggregate_stage_count"
        ),
        4
    );
    assert_eq!(
        integer(
            unknown,
            "paired_aggregate_spill_restoration_spill_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(
            unknown,
            "paired_aggregate_spill_restoration_spill_stage_count"
        ),
        3
    );
    assert_eq!(
        integer(
            unknown,
            "paired_aggregate_spill_restoration_unknown_working_set_count"
        ),
        1
    );
    assert!(
        unknown
            .details()
            .get("paired_aggregate_spill_restoration_estimated_bytes")
            .is_none()
    );
    assert!(
        unknown
            .details()
            .get("paired_aggregate_spill_restoration_estimated_io_work")
            .is_none()
    );

    assert_eq!(
        text(small, "paired_aggregate_spill_restoration_fold_identity"),
        text(
            reordered_joins["table:Small"],
            "paired_aggregate_spill_restoration_fold_identity"
        ),
        "cost order and descriptor order preserve the exact combined fold"
    );
    assert_eq!(
        text(unknown, "paired_aggregate_spill_restoration_fold_identity"),
        text(
            reordered_joins["table:Unknown"],
            "paired_aggregate_spill_restoration_fold_identity"
        )
    );
    assert_ne!(
        text(unknown, "paired_aggregate_spill_restoration_fold_identity"),
        text(
            other_joins["table:Unknown"],
            "paired_aggregate_spill_restoration_fold_identity"
        ),
        "anchor identity participates in the restored spill fold"
    );
    assert_eq!(
        text(unknown, "join_cost_fold_identity"),
        text(reordered_joins["table:Unknown"], "join_cost_fold_identity"),
        "the combined fold is represented in the accumulated join-cost identity"
    );

    for (source, expected_rows, expected_aggregates) in
        [("table:Small", 50, 2), ("table:Large", 300, 1)]
    {
        let nodes = original
            .nodes()
            .iter()
            .filter(|node| {
                node.kind() == PlanNodeKind::Aggregate
                    && text(node, "window_source_identity") == source
            })
            .collect::<Vec<_>>();
        assert_eq!(nodes.len(), expected_aggregates);
        for node in nodes {
            assert_eq!(node.estimated_rows(), Some(expected_rows));
            let expected_work = match text(node, "window_aggregate_identity") {
                "window:small-total" => 56,
                "window:small-prior" => 50,
                "window:large-total" => 300,
                other => panic!("unexpected aggregate {other}"),
            };
            assert_eq!(node.estimated_work(), Some(expected_work));
        }
    }
    let unknown_aggregate = original
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Aggregate
                && text(node, "window_source_identity") == "table:Unknown"
        })
        .expect("unknown aggregate stage exists");
    assert_eq!(unknown_aggregate.estimated_rows(), None);
    assert_eq!(unknown_aggregate.estimated_work(), None);

    let mut changed_aggregates = aggregates.clone();
    changed_aggregates[2].frame_identity = expression("frame:large-prior-two");
    changed_aggregates[2].frame_start = PlanWindowFrameBound::Preceding(2);
    let changed_frame = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &changed_aggregates,
        &spills,
    );
    assert_ne!(
        text(unknown, "paired_aggregate_spill_restoration_fold_identity"),
        text(
            joins_by_source(&changed_frame)["table:Unknown"],
            "paired_aggregate_spill_restoration_fold_identity"
        ),
        "restoring a different aggregate frame changes the downstream spill fold"
    );
    assert_ne!(
        text(unknown, "join_cost_fold_identity"),
        text(
            joins_by_source(&changed_frame)["table:Unknown"],
            "join_cost_fold_identity"
        ),
        "the restored aggregate-and-spill identity contributes to join cost folding"
    );

    let mut changed_spills = spills.clone();
    changed_spills[0].memory_budget_bytes = 8_193;
    let changed_budget = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &aggregates,
        &changed_spills,
    );
    assert_ne!(
        text(unknown, "paired_aggregate_spill_restoration_fold_identity"),
        text(
            joins_by_source(&changed_budget)["table:Unknown"],
            "paired_aggregate_spill_restoration_fold_identity"
        ),
        "computed spill-byte changes flow through later sparse restoration chains"
    );
    assert_ne!(
        text(unknown, "join_cost_fold_identity"),
        text(
            joins_by_source(&changed_budget)["table:Unknown"],
            "join_cost_fold_identity"
        ),
        "changed spill cost propagates to the final join-cost identity"
    );
}

#[test]
fn aggregate_fold_components_survive_sparse_spill_restoration() {
    let parsed = orna_syntax_v1::parse_module(SPARSE_IDENTITY_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let declared = [
        "table:Unknown",
        "table:Large",
        "table:Middle",
        "table:Small",
    ];
    let aggregates = aggregates();
    let spills = spills();
    let original = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &aggregates,
        &spills,
    );
    let joins = joins_by_source(&original);
    let small = joins["table:Small"];
    let middle = joins["table:Middle"];
    let large = joins["table:Large"];
    let unknown = joins["table:Unknown"];

    assert_eq!(
        text(small, "paired_aggregate_spill_restoration_aggregate_fold_identity"),
        text(small, "paired_aggregate_anchor_cascade_fold_identity"),
        "the paired restore fold exposes its exact aggregate cascade component"
    );
    assert_eq!(
        text(small, "paired_aggregate_spill_restoration_spill_fold_identity"),
        text(small, "paired_window_spill_cascade_fold_identity"),
        "the paired restore fold exposes its exact spill cascade component"
    );
    assert_eq!(
        text(middle, "paired_aggregate_spill_restoration_fold_identity"),
        text(small, "paired_aggregate_spill_restoration_fold_identity"),
        "a sparse join carries the complete paired restore fold"
    );
    assert!(
        middle
            .details()
            .get("paired_aggregate_spill_restoration_previous_fold_identity")
            .is_none(),
        "a sparse join carries the existing fold without inventing a new transition"
    );
    assert_eq!(
        text(large, "paired_aggregate_spill_restoration_previous_fold_identity"),
        text(small, "paired_aggregate_spill_restoration_fold_identity"),
        "the next aggregate event points to the last restoration fold across the gap"
    );
    assert_eq!(
        text(unknown, "paired_aggregate_spill_restoration_previous_fold_identity"),
        text(large, "paired_aggregate_spill_restoration_fold_identity"),
        "the later spill restoration points to the preceding aggregate fold"
    );
    assert_eq!(
        text(middle, "paired_aggregate_spill_restoration_aggregate_fold_identity"),
        text(middle, "paired_aggregate_anchor_cascade_fold_identity"),
        "the aggregate component stays bound to the carried aggregate cascade"
    );
    assert_eq!(
        text(middle, "paired_aggregate_spill_restoration_spill_fold_identity"),
        text(middle, "paired_window_spill_cascade_fold_identity"),
        "the spill component stays bound to accumulated spill history"
    );

    let mut changed_aggregates = aggregates.clone();
    changed_aggregates[2].frame_identity = expression("frame:large-prior-two");
    changed_aggregates[2].frame_start = PlanWindowFrameBound::Preceding(2);
    let changed_aggregate = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &changed_aggregates,
        &spills,
    );
    let changed_large = joins_by_source(&changed_aggregate)["table:Large"];
    assert_ne!(
        text(large, "paired_aggregate_spill_restoration_aggregate_fold_identity"),
        text(
            changed_large,
            "paired_aggregate_spill_restoration_aggregate_fold_identity"
        ),
        "changing the restored frame changes the exposed aggregate component"
    );
    assert_eq!(
        text(large, "paired_aggregate_spill_restoration_spill_fold_identity"),
        text(
            changed_large,
            "paired_aggregate_spill_restoration_spill_fold_identity"
        ),
        "an aggregate-only change preserves the spill component"
    );

    let mut changed_spills = spills.clone();
    changed_spills[0].memory_budget_bytes += 1;
    let changed_spill = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &aggregates,
        &changed_spills,
    );
    let changed_middle = joins_by_source(&changed_spill)["table:Middle"];
    assert_eq!(
        text(
            changed_middle,
            "paired_aggregate_spill_restoration_aggregate_fold_identity"
        ),
        text(
            changed_middle,
            "paired_aggregate_anchor_cascade_fold_identity"
        ),
        "the restored pair exposes the aggregate cascade selected for the changed plan"
    );
    assert_ne!(
        text(middle, "paired_aggregate_spill_restoration_spill_fold_identity"),
        text(
            changed_middle,
            "paired_aggregate_spill_restoration_spill_fold_identity"
        ),
        "the sparse restoration fold exposes the changed spill component"
    );
    assert_ne!(
        text(middle, "paired_aggregate_spill_restoration_fold_identity"),
        text(
            changed_middle,
            "paired_aggregate_spill_restoration_fold_identity"
        ),
        "the combined identity changes when either component changes"
    );
}
