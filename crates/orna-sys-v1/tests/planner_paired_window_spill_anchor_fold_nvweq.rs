use std::collections::BTreeMap;

use orna_sys_v1::{
    ExplainError, ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind,
    PlanWindowFrameBound, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryPlanDescription, QuerySourceStatistics, QueryWindowAggregatePushdownDescription,
    QueryWindowSpillDescription, SnapshotRef,
    explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_window_spill_anchor_fold_nvweq.orna");

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

fn join(anchor: &str, source: &str) -> QueryJoinDescription {
    let stats = match source {
        "table:Small" => Some(statistics(50, 6_400)),
        "table:Middle" => Some(statistics(20, 16_384)),
        "table:Large" => Some(statistics(300, 65_536)),
        "table:Unknown" => None,
        other => panic!("unexpected source {other}"),
    };
    QueryJoinDescription {
        source: object(source),
        statistics: stats,
        predicate: Some(expression(&format!(
            "expr:{}-{}",
            anchor.trim_start_matches("table:"),
            source.trim_start_matches("table:")
        ))),
    }
}

fn query(anchor: &str, order: [&str; 4]) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:paired-window-spill-nvweq"),
        source: object(anchor),
        source_statistics: Some(statistics(40, 40_000)),
        joins: order
            .into_iter()
            .map(|source| join(anchor, source))
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
    frame: &str,
    start: PlanWindowFrameBound,
    end: PlanWindowFrameBound,
) -> QueryWindowAggregatePushdownDescription {
    QueryWindowAggregatePushdownDescription {
        identity: object(identity),
        source: object(source),
        aggregate: expression(operation),
        frame_identity: expression(frame),
        frame_start: start,
        frame_end: end,
    }
}

fn aggregates(anchor: &str) -> Vec<QueryWindowAggregatePushdownDescription> {
    vec![
        aggregate(
            &format!("window:{}-total", anchor.trim_start_matches("table:")),
            anchor,
            "expr:sum-anchor",
            "frame:anchor-running",
            PlanWindowFrameBound::UnboundedPreceding,
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:small-running-average",
            "table:Small",
            "expr:mean-small",
            "frame:small-previous-three",
            PlanWindowFrameBound::Preceding(2),
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:small-forward-max",
            "table:Small",
            "expr:max-small",
            "frame:small-current-next",
            PlanWindowFrameBound::CurrentRow,
            PlanWindowFrameBound::Following(1),
        ),
        aggregate(
            "window:middle-count",
            "table:Middle",
            "expr:count-middle",
            "frame:middle-whole-partition",
            PlanWindowFrameBound::UnboundedPreceding,
            PlanWindowFrameBound::UnboundedFollowing,
        ),
        aggregate(
            "window:large-total",
            "table:Large",
            "expr:sum-large",
            "frame:large-running",
            PlanWindowFrameBound::UnboundedPreceding,
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:unknown-total",
            "table:Unknown",
            "expr:sum-unknown",
            "frame:unknown-current",
            PlanWindowFrameBound::CurrentRow,
            PlanWindowFrameBound::CurrentRow,
        ),
    ]
}

fn spill(
    identity: &str,
    pair_identity: &str,
    source: &str,
    aggregate_identity: &str,
    working_set_bytes: Option<u64>,
    memory_budget_bytes: u64,
) -> QueryWindowSpillDescription {
    QueryWindowSpillDescription {
        identity: object(identity),
        join_pair_identity: object(pair_identity),
        source: object(source),
        window_aggregate_identity: object(aggregate_identity),
        estimated_working_set_bytes: working_set_bytes,
        memory_budget_bytes,
    }
}

fn spills() -> Vec<QueryWindowSpillDescription> {
    vec![
        spill(
            "spill:small-average",
            "pair:Small",
            "table:Small",
            "window:small-running-average",
            Some(16_385),
            8_192,
        ),
        spill(
            "spill:small-max",
            "pair:Small",
            "table:Small",
            "window:small-forward-max",
            Some(4_000),
            4_096,
        ),
        spill(
            "spill:middle-count",
            "pair:Middle",
            "table:Middle",
            "window:middle-count",
            Some(8_192),
            8_192,
        ),
        spill(
            "spill:large-total",
            "pair:Large",
            "table:Large",
            "window:large-total",
            Some(20_000),
            8_000,
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
    descriptors: &[QueryJoinPairIdentityDescription],
    aggregate_descriptors: &[QueryWindowAggregatePushdownDescription],
    spill_descriptors: &[QueryWindowSpillDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns(
        &query(anchor, order),
        descriptors,
        aggregate_descriptors,
        spill_descriptors,
    )
    .expect("exact join pairs and source window spill estimates produce a plan")
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

fn boolean(node: &PlanNode, key: &str) -> bool {
    match node.details().get(key) {
        Some(PlanDetail::Boolean(value)) => *value,
        other => panic!("{key} should be a boolean, got {other:?}"),
    }
}

fn joins_by_pair<'a>(plan: &'a orna_sys_v1::ExplainedPlan) -> BTreeMap<&'a str, &'a PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| (text(node, "join_pair_identity"), node))
        .collect()
}

fn aggregates_by_identity<'a>(
    plan: &'a orna_sys_v1::ExplainedPlan,
) -> BTreeMap<&'a str, &'a PlanNode> {
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
fn paired_window_spill_folds_report_computed_io_across_sparse_anchor_reordering() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let declarations = [
        "table:Large",
        "table:Unknown",
        "table:Small",
        "table:Middle",
    ];
    let aggregate_descriptors = aggregates("table:AnchorLeft");
    let spills = spills();
    let first = plan(
        "table:AnchorLeft",
        declarations,
        &pairs("table:AnchorLeft", declarations),
        &aggregate_descriptors,
        &spills,
    );
    let reordered_declarations = [
        "table:Middle",
        "table:Small",
        "table:Unknown",
        "table:Large",
    ];
    let reordered_pairs = pairs("table:AnchorLeft", reordered_declarations)
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    let reordered_spills = spills.iter().rev().cloned().collect::<Vec<_>>();
    let reordered = plan(
        "table:AnchorLeft",
        reordered_declarations,
        &reordered_pairs,
        &aggregate_descriptors,
        &reordered_spills,
    );
    let other_anchor = plan(
        "table:AnchorRight",
        declarations,
        &pairs("table:AnchorRight", declarations),
        &aggregates("table:AnchorRight"),
        &spills,
    );

    let joins = joins_by_pair(&first);
    let reordered_joins = joins_by_pair(&reordered);
    let other_joins = joins_by_pair(&other_anchor);
    let aggregate_nodes = aggregates_by_identity(&first);
    let expected = [
        (
            "pair:Small",
            "window:small-running-average",
            Some(8_193),
            Some(3),
            Some(56),
        ),
        (
            "pair:Small",
            "window:small-forward-max",
            Some(0),
            Some(0),
            Some(50),
        ),
        (
            "pair:Middle",
            "window:middle-count",
            Some(0),
            Some(0),
            Some(20),
        ),
        (
            "pair:Large",
            "window:large-total",
            Some(12_000),
            Some(3),
            Some(306),
        ),
        ("pair:Unknown", "window:unknown-total", None, None, None),
    ];

    for (pair_id, aggregate_id, spill_bytes, io_blocks, work) in expected {
        let join = joins[pair_id];
        let reordered_join = reordered_joins[pair_id];
        let other_join = other_joins[pair_id];
        let aggregate = aggregate_nodes[aggregate_id];
        assert_eq!(
            aggregate.estimated_work(),
            work,
            "window work includes computed spill read/write blocks for {aggregate_id}"
        );
        assert_eq!(
            join.estimated_rows(),
            match pair_id {
                "pair:Small" => Some(400),
                "pair:Middle" => Some(80),
                "pair:Large" => Some(12_000),
                "pair:Unknown" => None,
                _ => unreachable!(),
            }
        );
        assert_eq!(
            join.estimated_bytes(),
            match pair_id {
                "pair:Small" => Some(779_200),
                "pair:Middle" => Some(145_600),
                "pair:Large" => Some(26_004_000),
                "pair:Unknown" => None,
                _ => unreachable!(),
            }
        );
        assert_eq!(
            text(join, "paired_window_spill_anchor_fold_identity"),
            text(reordered_join, "paired_window_spill_anchor_fold_identity"),
            "declaration and descriptor reordering preserve each exact pair's spill fold"
        );
        assert_eq!(
            text(join, "window_spill_chain_identity"),
            text(reordered_join, "window_spill_chain_identity"),
            "spill descriptors follow resolved aggregate order, not descriptor input order"
        );
        assert_eq!(
            text(join, "join_cost_fold_identity"),
            text(reordered_join, "join_cost_fold_identity"),
            "spill metadata participates in a stable sparse join cost fold"
        );
        assert_ne!(
            text(join, "paired_window_spill_anchor_fold_identity"),
            text(other_join, "paired_window_spill_anchor_fold_identity"),
            "the sparse anchor is part of the paired spill fold"
        );
        assert_ne!(
            text(join, "join_cost_fold_identity"),
            text(other_join, "join_cost_fold_identity"),
            "the cost fold preserves distinct paired spill anchors"
        );
        assert_eq!(
            text(aggregate, "paired_window_spill_anchor_fold_identity"),
            text(join, "paired_window_spill_anchor_fold_identity")
        );
        assert_eq!(
            text(join, "window_spill_chain_identity"),
            text(aggregate, "window_spill_chain_identity")
        );
        assert_eq!(
            text(join, "paired_window_spill_anchor_fold_pairing"),
            "sparse_anchor_fold_resolved_join_pair_window_chain_and_spill_chain"
        );
        match spill_bytes {
            Some(expected_bytes) => assert_eq!(
                integer(aggregate, "window_spill_estimated_bytes"),
                expected_bytes
            ),
            None => assert_eq!(
                text(aggregate, "window_spill_estimate_status"),
                "unknown_working_set"
            ),
        }
        match io_blocks {
            Some(expected_blocks) => assert_eq!(
                integer(aggregate, "window_spill_estimated_io_blocks"),
                expected_blocks
            ),
            None => assert!(
                aggregate
                    .details()
                    .get("window_spill_estimated_io_blocks")
                    .is_none()
            ),
        }
    }

    assert!(boolean(
        aggregate_nodes["window:small-running-average"],
        "window_spill_required"
    ));
    assert!(!boolean(
        aggregate_nodes["window:small-forward-max"],
        "window_spill_required"
    ));
    assert_eq!(
        integer(
            aggregate_nodes["window:small-running-average"],
            "window_spill_estimated_io_work"
        ),
        6
    );
    assert_eq!(
        integer(
            aggregate_nodes["window:small-forward-max"],
            "window_spill_estimated_io_work"
        ),
        0
    );
    assert_eq!(
        (
            first.root().estimated_rows(),
            first.root().estimated_bytes()
        ),
        (None, None),
        "the unknown tail keeps final cardinality unknown"
    );

    let mut changed_budget = spills.clone();
    changed_budget[0].memory_budget_bytes = 8_193;
    let changed = plan(
        "table:AnchorLeft",
        declarations,
        &pairs("table:AnchorLeft", declarations),
        &aggregate_descriptors,
        &changed_budget,
    );
    assert_ne!(
        text(
            joins["pair:Small"],
            "paired_window_spill_anchor_fold_identity"
        ),
        text(
            joins_by_pair(&changed)["pair:Small"],
            "paired_window_spill_anchor_fold_identity"
        ),
        "changing the working-set budget changes the computed spill fold"
    );
    assert_ne!(
        text(joins["pair:Small"], "join_cost_fold_identity"),
        text(
            joins_by_pair(&changed)["pair:Small"],
            "join_cost_fold_identity"
        ),
        "spill-cost changes propagate through the accumulated query cost fold"
    );

    let mut mispaired_spills = spills.clone();
    mispaired_spills[0].join_pair_identity = object("pair:Middle");
    assert!(matches!(
        explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns(
            &query("table:AnchorLeft", declarations),
            &pairs("table:AnchorLeft", declarations),
            &aggregate_descriptors,
            &mispaired_spills,
        ),
        Err(ExplainError::InvalidObject)
    ));
}
