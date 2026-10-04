use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, QueryWindowSpillDescription,
    SnapshotRef, explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_window_spill_cost_identity_eivhv.orna");

fn object(value: &str) -> ObjectRef {
    ObjectRef::descriptive(value)
}

fn expression(value: &str) -> ExpressionRef {
    ExpressionRef::descriptive(value)
}

fn statistics(rows: u64, bytes: u64) -> QuerySourceStatistics {
    QuerySourceStatistics {
        estimated_rows: Some(rows),
        estimated_bytes: Some(bytes),
        mutable_branch: None,
    }
}

fn query() -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-window-spill-cost-eivhv"),
        source: object("table:Anchor"),
        source_statistics: Some(statistics(10, 10_000)),
        joins: ["table:First", "table:Second", "table:Tail"]
            .into_iter()
            .map(|source| QueryJoinDescription {
                source: object(source),
                statistics: Some(match source {
                    "table:First" => statistics(4, 4_000),
                    "table:Second" => statistics(8, 8_000),
                    "table:Tail" => statistics(40, 40_000),
                    _ => unreachable!(),
                }),
                predicate: Some(expression(&format!("expr:Anchor-{source}"))),
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

fn pair(source: &str) -> QueryJoinPairIdentityDescription {
    QueryJoinPairIdentityDescription {
        identity: object(&format!("pair:{}", source.trim_start_matches("table:"))),
        left_source: object("table:Anchor"),
        right_source: object(source),
        predicate: Some(expression(&format!("expr:Anchor-{source}"))),
    }
}

fn aggregate(identity: &str, source: &str) -> QueryWindowAggregatePushdownDescription {
    QueryWindowAggregatePushdownDescription {
        identity: object(identity),
        source: object(source),
        aggregate: expression("expr:sum-value"),
        frame_identity: expression("frame:running"),
        frame_start: PlanWindowFrameBound::UnboundedPreceding,
        frame_end: PlanWindowFrameBound::CurrentRow,
    }
}

fn spill(
    identity: &str,
    pair_identity: &str,
    source: &str,
    aggregate_identity: &str,
    working_set: u64,
    memory_budget: u64,
) -> QueryWindowSpillDescription {
    QueryWindowSpillDescription {
        identity: object(identity),
        join_pair_identity: object(pair_identity),
        source: object(source),
        window_aggregate_identity: object(aggregate_identity),
        estimated_working_set_bytes: Some(working_set),
        memory_budget_bytes: memory_budget,
    }
}

fn plan(first_budget: u64) -> orna_sys_v1::ExplainedPlan {
    let pairs = ["table:First", "table:Second", "table:Tail"]
        .into_iter()
        .map(pair)
        .collect::<Vec<_>>();
    let aggregates = vec![
        aggregate("window:first-sum", "table:First"),
        aggregate("window:second-sum", "table:Second"),
    ];
    let spills = vec![
        spill(
            "spill:first-sum",
            "pair:First",
            "table:First",
            "window:first-sum",
            9_000,
            first_budget,
        ),
        spill(
            "spill:second-sum",
            "pair:Second",
            "table:Second",
            "window:second-sum",
            18_000,
            7_000,
        ),
    ];
    explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns(
        &query(),
        &pairs,
        &aggregates,
        &spills,
    )
    .expect("exact pair and nested rule spill descriptors produce an explain plan")
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

fn paired_cost_nodes_by_pair(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<&str, &PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| {
            node.details()
                .contains_key("paired_join_cost_fold_identity")
        })
        .map(|node| (text(node, "join_pair_identity"), node))
        .collect()
}

#[test]
fn nested_rule_spill_cost_identity_binds_exact_pairs_and_both_cost_ancestors() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let planned = plan(5_000);
    let same_inputs = plan(5_000);
    let changed_rule_cost = plan(5_500);
    let joins = joins_by_pair(&planned);
    let paired_cost_nodes = paired_cost_nodes_by_pair(&planned);
    let stable_joins = joins_by_pair(&same_inputs);
    let changed_joins = joins_by_pair(&changed_rule_cost);
    let first = joins["pair:First"];
    let second = joins["pair:Second"];
    let tail = joins["pair:Tail"];

    assert_eq!(
        integer(first, "paired_window_spill_rule_cost_pair_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_window_spill_rule_cost_rule_count"),
        1
    );
    assert_eq!(
        integer(first, "paired_window_spill_rule_cost_estimated_bytes"),
        4_000
    );
    assert_eq!(
        integer(first, "paired_window_spill_rule_cost_estimated_io_blocks"),
        1
    );
    assert_eq!(
        integer(first, "paired_window_spill_rule_cost_estimated_io_work"),
        2
    );

    assert_eq!(
        integer(second, "paired_window_spill_rule_cost_pair_count"),
        2
    );
    assert_eq!(
        integer(second, "paired_window_spill_rule_cost_rule_count"),
        2
    );
    assert_eq!(
        integer(second, "paired_window_spill_rule_cost_estimated_bytes"),
        15_000
    );
    assert_eq!(
        integer(second, "paired_window_spill_rule_cost_estimated_io_blocks"),
        4
    );
    assert_eq!(
        integer(second, "paired_window_spill_rule_cost_estimated_io_work"),
        8
    );
    assert_eq!(integer(tail, "paired_window_spill_rule_cost_pair_count"), 2);
    assert_eq!(integer(tail, "paired_window_spill_rule_cost_rule_count"), 2);
    assert_eq!(
        integer(tail, "paired_window_spill_rule_cost_estimated_bytes"),
        15_000
    );
    assert_eq!(
        text(tail, "paired_window_spill_rule_cost_transition"),
        "carried_across_sparse_pair"
    );

    for pair_id in ["pair:First", "pair:Second", "pair:Tail"] {
        let join = joins[pair_id];
        let stable = stable_joins[pair_id];
        assert_eq!(
            text(join, "paired_window_spill_rule_cost_pair_identity_policy"),
            "join_ancestors_exact_pair_nested_rule_folds_sha256_v1"
        );
        assert_eq!(
            text(join, "paired_window_spill_rule_cost_pair_identity"),
            text(stable, "paired_window_spill_rule_cost_pair_identity"),
            "the same real plan inputs produce the same paired cost identity"
        );
        assert_eq!(
            text(paired_cost_nodes[pair_id], "paired_join_cost_fold_identity"),
            text(join, "join_cost_fold_identity")
        );
    }

    assert_ne!(
        text(first, "paired_window_spill_rule_cost_pair_identity"),
        text(second, "paired_window_spill_rule_cost_pair_identity"),
        "the nested second pair binds the accumulated first-rule cost and its own rule cost"
    );
    assert_ne!(
        text(second, "paired_window_spill_rule_cost_pair_identity"),
        text(tail, "paired_window_spill_rule_cost_pair_identity"),
        "a sparse tail pair advances the exact-pair identity while carrying the nested totals"
    );
    assert_eq!(
        text(tail, "join_cost_fold_left_identity"),
        text(second, "join_cost_fold_identity"),
        "the tail's left cost ancestor is the prior paired rule-cost join"
    );
    assert_eq!(
        integer(
            changed_joins["pair:Tail"],
            "paired_window_spill_rule_cost_estimated_bytes"
        ),
        14_500
    );
    assert_ne!(
        text(tail, "paired_window_spill_rule_cost_pair_identity"),
        text(
            changed_joins["pair:Tail"],
            "paired_window_spill_rule_cost_pair_identity"
        ),
        "changing a nested rule's budget changes the carried paired cost identity"
    );
    assert_ne!(
        text(tail, "join_cost_fold_identity"),
        text(changed_joins["pair:Tail"], "join_cost_fold_identity")
    );
}
