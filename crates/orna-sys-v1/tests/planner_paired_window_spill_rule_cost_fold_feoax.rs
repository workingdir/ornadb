use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, QueryWindowSpillDescription,
    SnapshotRef, explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_paired_window_spill_rule_cost_fold_feoax.orna");

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
        snapshot: SnapshotRef::descriptive("snapshot:paired-window-spill-rule-feoax"),
        source: object(anchor),
        source_statistics: Some(statistics(40, 40_000)),
        joins: order
            .into_iter()
            .map(|source| QueryJoinDescription {
                source: object(source),
                statistics: match source {
                    "table:Small" => Some(statistics(50, 6_400)),
                    "table:Middle" => Some(statistics(20, 16_384)),
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
    operation: &str,
) -> QueryWindowAggregatePushdownDescription {
    QueryWindowAggregatePushdownDescription {
        identity: object(identity),
        source: object(source),
        aggregate: expression(operation),
        frame_identity: expression("frame:running"),
        frame_start: PlanWindowFrameBound::UnboundedPreceding,
        frame_end: PlanWindowFrameBound::CurrentRow,
    }
}

fn aggregates() -> Vec<QueryWindowAggregatePushdownDescription> {
    vec![
        aggregate("window:small-total", "table:Small", "expr:sum-value"),
        aggregate("window:small-count", "table:Small", "expr:count-value"),
        aggregate("window:large-total", "table:Large", "expr:sum-value"),
        aggregate("window:unknown-total", "table:Unknown", "expr:sum-value"),
    ]
}

fn spill(
    identity: &str,
    pair: &str,
    source: &str,
    aggregate: &str,
    working_set: Option<u64>,
    budget: u64,
) -> QueryWindowSpillDescription {
    QueryWindowSpillDescription {
        identity: object(identity),
        join_pair_identity: object(pair),
        source: object(source),
        window_aggregate_identity: object(aggregate),
        estimated_working_set_bytes: working_set,
        memory_budget_bytes: budget,
    }
}

fn spills(include_unknown: bool) -> Vec<QueryWindowSpillDescription> {
    let mut spills = vec![
        spill(
            "spill:small-total",
            "pair:Small",
            "table:Small",
            "window:small-total",
            Some(16_385),
            8_192,
        ),
        spill(
            "spill:small-count",
            "pair:Small",
            "table:Small",
            "window:small-count",
            Some(4_000),
            4_096,
        ),
        spill(
            "spill:large-total",
            "pair:Large",
            "table:Large",
            "window:large-total",
            Some(20_000),
            8_000,
        ),
    ];
    if include_unknown {
        spills.push(spill(
            "spill:unknown-total",
            "pair:Unknown",
            "table:Unknown",
            "window:unknown-total",
            None,
            512,
        ));
    }
    spills
}

fn plan(
    anchor: &str,
    order: [&str; 4],
    pair_descriptors: &[QueryJoinPairIdentityDescription],
    aggregate_descriptors: &[QueryWindowAggregatePushdownDescription],
    spill_descriptors: &[QueryWindowSpillDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns(
        &query(anchor, order),
        pair_descriptors,
        aggregate_descriptors,
        spill_descriptors,
    )
    .expect("exact pair, rule, and spill descriptors produce an explain plan")
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

fn aggregates_by_identity(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<&str, &PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Aggregate
                && node.details().contains_key("window_aggregate_identity")
        })
        .map(|node| (text(node, "window_aggregate_identity"), node))
        .collect()
}

#[test]
fn paired_window_spill_rule_folds_keep_nested_values_and_sparse_carry() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let declared = [
        "table:Large",
        "table:Unknown",
        "table:Small",
        "table:Middle",
    ];
    let aggregate_descriptors = aggregates();
    let spill_descriptors = spills(false);
    let original = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &aggregate_descriptors,
        &spill_descriptors,
    );
    let reordered = [
        "table:Middle",
        "table:Small",
        "table:Unknown",
        "table:Large",
    ];
    let reordered_spills = spill_descriptors.iter().rev().cloned().collect::<Vec<_>>();
    let reordered_plan = plan(
        "table:AnchorLeft",
        reordered,
        &pairs("table:AnchorLeft", reordered)
            .into_iter()
            .rev()
            .collect::<Vec<_>>(),
        &aggregate_descriptors,
        &reordered_spills,
    );

    let root = original.root();
    assert_eq!(integer(root, "paired_window_spill_rule_cost_pair_count"), 2);
    assert_eq!(integer(root, "paired_window_spill_rule_cost_rule_count"), 3);
    assert_eq!(
        integer(root, "paired_window_spill_rule_cost_stage_count"),
        3
    );
    assert_eq!(
        integer(root, "paired_window_spill_rule_cost_estimated_bytes"),
        20_193
    );
    assert_eq!(
        integer(root, "paired_window_spill_rule_cost_estimated_io_blocks"),
        6
    );
    assert_eq!(
        integer(root, "paired_window_spill_rule_cost_estimated_io_work"),
        12
    );
    assert_eq!(
        text(root, "paired_window_spill_rule_cost_pairing"),
        "paired_window_spill_chain_with_nested_aggregate_rule_folds"
    );
    assert_eq!(
        text(root, "paired_window_spill_rule_cost_estimate_status"),
        "computed"
    );
    assert_eq!(
        text(root, "paired_window_spill_rule_cost_transition"),
        "carried_across_sparse_pair"
    );
    assert_eq!(
        text(root, "paired_window_spill_rule_cost_fold_identity"),
        text(
            reordered_plan.root(),
            "paired_window_spill_rule_cost_fold_identity"
        ),
        "planned pair and aggregate identities keep the nested fold stable under descriptor reordering"
    );

    let aggregate_nodes = aggregates_by_identity(&original);
    for (aggregate_id, bytes, blocks, work) in [
        ("window:small-total", 8_193, 3, 6),
        ("window:small-count", 0, 0, 0),
        ("window:large-total", 12_000, 3, 6),
    ] {
        let node = aggregate_nodes[aggregate_id];
        assert_eq!(
            text(node, "paired_window_spill_rule_cost_rule_identity"),
            aggregate_id
        );
        assert_eq!(
            integer(node, "paired_window_spill_rule_cost_rule_pair_count"),
            1
        );
        assert_eq!(
            integer(node, "paired_window_spill_rule_cost_rule_stage_count"),
            1
        );
        assert_eq!(
            integer(node, "paired_window_spill_rule_cost_rule_estimated_bytes"),
            bytes
        );
        assert_eq!(
            integer(
                node,
                "paired_window_spill_rule_cost_rule_estimated_io_blocks"
            ),
            blocks
        );
        assert_eq!(
            integer(node, "paired_window_spill_rule_cost_rule_estimated_io_work"),
            work
        );
        assert_eq!(
            text(node, "paired_window_spill_rule_cost_rule_estimate_status"),
            "computed"
        );
    }

    let joins = joins_by_pair(&original);
    assert_eq!(
        text(
            joins["pair:Unknown"],
            "paired_window_spill_rule_cost_fold_identity"
        ),
        text(root, "paired_window_spill_rule_cost_fold_identity")
    );
    assert_eq!(
        text(
            joins["pair:Unknown"],
            "paired_window_spill_rule_cost_transition"
        ),
        "carried_across_sparse_pair",
        "the no-spill unknown pair carries all rule-local cost state"
    );

    let mut unknown_spills = spills(true);
    let unknown_plan = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &aggregate_descriptors,
        &unknown_spills,
    );
    let unknown_root = unknown_plan.root();
    assert_eq!(
        integer(unknown_root, "paired_window_spill_rule_cost_pair_count"),
        3
    );
    assert_eq!(
        integer(unknown_root, "paired_window_spill_rule_cost_rule_count"),
        4
    );
    assert_eq!(
        integer(unknown_root, "paired_window_spill_rule_cost_stage_count"),
        4
    );
    assert_eq!(
        integer(
            unknown_root,
            "paired_window_spill_rule_cost_unknown_working_set_count"
        ),
        1
    );
    assert_eq!(
        text(
            unknown_root,
            "paired_window_spill_rule_cost_estimate_status"
        ),
        "unknown_working_set"
    );
    assert!(
        unknown_root
            .details()
            .get("paired_window_spill_rule_cost_estimated_bytes")
            .is_none()
    );
    let unknown_aggregate = aggregates_by_identity(&unknown_plan)["window:unknown-total"];
    assert_eq!(
        integer(
            unknown_aggregate,
            "paired_window_spill_rule_cost_rule_unknown_working_set_count"
        ),
        1
    );
    assert_eq!(
        text(
            unknown_aggregate,
            "paired_window_spill_rule_cost_rule_estimate_status"
        ),
        "unknown_working_set"
    );
    assert!(
        unknown_aggregate
            .details()
            .get("paired_window_spill_rule_cost_rule_estimated_bytes")
            .is_none()
    );

    unknown_spills[0].memory_budget_bytes = 8_193;
    let changed_budget = plan(
        "table:AnchorLeft",
        declared,
        &pairs("table:AnchorLeft", declared),
        &aggregate_descriptors,
        &unknown_spills,
    );
    let changed_small_total = aggregates_by_identity(&changed_budget)["window:small-total"];
    assert_eq!(
        integer(
            changed_small_total,
            "paired_window_spill_rule_cost_rule_estimated_bytes"
        ),
        8_192
    );
    assert_eq!(
        integer(
            changed_small_total,
            "paired_window_spill_rule_cost_rule_estimated_io_blocks"
        ),
        2
    );
    assert_eq!(
        integer(
            changed_small_total,
            "paired_window_spill_rule_cost_rule_estimated_io_work"
        ),
        4
    );
    assert_ne!(
        text(unknown_root, "paired_window_spill_rule_cost_fold_identity"),
        text(
            changed_budget.root(),
            "paired_window_spill_rule_cost_fold_identity"
        )
    );
    assert_ne!(
        text(unknown_root, "join_cost_fold_identity"),
        text(changed_budget.root(), "join_cost_fold_identity")
    );
}
