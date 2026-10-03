use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, SnapshotRef,
    explain_query_with_join_pair_identities_and_window_aggregate_pushdowns,
};

const FIXTURE: &str = include_str!("fixtures/planner_window_identity_cost_fold_m8bnk.orna");

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

fn pair(source: &str) -> QueryJoinPairIdentityDescription {
    QueryJoinPairIdentityDescription {
        identity: object(&format!("pair:{}", source.trim_start_matches("table:"))),
        left_source: object("table:Anchor"),
        right_source: object(source),
        predicate: Some(expression(&format!(
            "expr:anchor-{}",
            source.trim_start_matches("table:")
        ))),
    }
}

fn join(source: &str, rows: Option<u64>, bytes: Option<u64>) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics: rows.zip(bytes).map(|(rows, bytes)| statistics(rows, bytes)),
        predicate: Some(expression(&format!(
            "expr:anchor-{}",
            source.trim_start_matches("table:")
        ))),
    }
}

fn query(order: [&str; 4]) -> QueryPlanDescription {
    let joins = order
        .into_iter()
        .map(|source| match source {
            "table:Large" => join(source, Some(300), Some(16_384)),
            "table:Unknown" => join(source, None, None),
            "table:Small" => join(source, Some(5), Some(4_096)),
            "table:Middle" => join(source, Some(20), Some(8_192)),
            other => panic!("unexpected source {other}"),
        })
        .collect();
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:planner-window-cost-fold-m8bnk"),
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

fn pairs() -> Vec<QueryJoinPairIdentityDescription> {
    [
        "table:Large",
        "table:Unknown",
        "table:Small",
        "table:Middle",
    ]
    .into_iter()
    .map(pair)
    .collect()
}

fn aggregates(small_frame: &str) -> Vec<QueryWindowAggregatePushdownDescription> {
    vec![
        aggregate(
            "window:anchor-total",
            "table:Anchor",
            "expr:sum-anchor",
            "frame:anchor-running",
            PlanWindowFrameBound::UnboundedPreceding,
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:small-average",
            "table:Small",
            "expr:mean-small",
            small_frame,
            PlanWindowFrameBound::Preceding(2),
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:small-maximum",
            "table:Small",
            "expr:max-small",
            "frame:small-forward",
            PlanWindowFrameBound::CurrentRow,
            PlanWindowFrameBound::Following(1),
        ),
        aggregate(
            "window:middle-count",
            "table:Middle",
            "expr:count-middle",
            "frame:middle-all",
            PlanWindowFrameBound::UnboundedPreceding,
            PlanWindowFrameBound::UnboundedFollowing,
        ),
        aggregate(
            "window:large-maximum",
            "table:Large",
            "expr:max-large",
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

fn plan(
    order: [&str; 4],
    pairs: &[QueryJoinPairIdentityDescription],
    aggregates: &[QueryWindowAggregatePushdownDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities_and_window_aggregate_pushdowns(
        &query(order),
        pairs,
        aggregates,
    )
    .expect("join pairs and exact-source window chains plan together")
}

fn joined<'a>(plan: &'a orna_sys_v1::ExplainedPlan) -> BTreeMap<&'a str, &'a PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| (text(node, "join_pair_identity"), node))
        .collect()
}

fn pushed<'a>(plan: &'a orna_sys_v1::ExplainedPlan) -> BTreeMap<&'a str, &'a PlanNode> {
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
fn source_window_chains_stay_paired_to_join_cost_folds_after_sparse_reordering() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let descriptors = pairs();
    let window_descriptors = aggregates("frame:small-previous-through-current");
    let first = plan(
        [
            "table:Large",
            "table:Unknown",
            "table:Small",
            "table:Middle",
        ],
        &descriptors,
        &window_descriptors,
    );
    let reversed_descriptors = descriptors.iter().rev().cloned().collect::<Vec<_>>();
    let second = plan(
        [
            "table:Middle",
            "table:Small",
            "table:Unknown",
            "table:Large",
        ],
        &reversed_descriptors,
        &window_descriptors,
    );
    let first_joins = joined(&first);
    let second_joins = joined(&second);
    let expected = [
        (
            "pair:Small",
            "table:Small",
            Some(50),
            Some(43_000),
            Some(105),
        ),
        (
            "pair:Middle",
            "table:Middle",
            Some(100),
            Some(127_000),
            Some(70),
        ),
        (
            "pair:Large",
            "table:Large",
            Some(3_000),
            Some(3_975_000),
            Some(400),
        ),
        ("pair:Unknown", "table:Unknown", None, None, None),
    ];

    for (pair_id, source, rows, bytes, work) in expected {
        let first_join = first_joins[pair_id];
        let second_join = second_joins[pair_id];
        assert_eq!(text(first_join, "logical_right_source_identity"), source);
        assert_eq!(
            (
                first_join.estimated_rows(),
                first_join.estimated_bytes(),
                first_join.estimated_work()
            ),
            (rows, bytes, work),
            "planner reports the computed real estimate for {pair_id}"
        );
        assert_eq!(
            text(first_join, "join_cost_fold_identity"),
            text(second_join, "join_cost_fold_identity"),
            "changing declarations does not detach an identity from its cost-ordered fold"
        );
        assert_eq!(
            text(first_join, "window_pushdown_chain_identity"),
            text(second_join, "window_pushdown_chain_identity")
        );
        let access = first
            .nodes()
            .iter()
            .find(|node| node.reference() == &first_join.inputs()[1])
            .expect("join right input exists");
        assert_eq!(
            text(access, "window_pushdown_chain_identity"),
            text(first_join, "window_pushdown_chain_identity"),
            "the source access and paired join fold carry the same chain label"
        );
        assert_eq!(
            text(access, "paired_join_cost_fold_identity"),
            text(first_join, "join_cost_fold_identity")
        );
    }
    let mut planned_pair_order = first
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .collect::<Vec<_>>();
    planned_pair_order.sort_by_key(|node| integer(node, "planned_input_position"));
    assert_eq!(
        ["pair:Small", "pair:Middle", "pair:Large", "pair:Unknown"],
        planned_pair_order
            .iter()
            .map(|node| text(node, "join_pair_identity"))
            .collect::<Vec<_>>()
            .as_slice(),
        "known work is cost ordered and the sparse unknown input remains last"
    );

    let aggregate_nodes = pushed(&first);
    assert_eq!(aggregate_nodes.len(), 6);
    assert_eq!(
        text(
            aggregate_nodes["window:small-average"],
            "window_frame_start"
        ),
        "preceding:2"
    );
    assert_eq!(
        text(aggregate_nodes["window:small-average"], "window_frame_end"),
        "current_row"
    );
    assert_eq!(
        integer(
            aggregate_nodes["window:small-average"],
            "window_pushdown_chain_position"
        ),
        1
    );
    assert_eq!(
        integer(
            aggregate_nodes["window:small-maximum"],
            "window_pushdown_chain_position"
        ),
        2
    );
    assert_eq!(
        integer(
            aggregate_nodes["window:small-average"],
            "window_pushdown_chain_length"
        ),
        2
    );
    assert_eq!(
        text(
            aggregate_nodes["window:small-average"],
            "window_pushdown_chain_identity"
        ),
        text(
            aggregate_nodes["window:small-maximum"],
            "window_pushdown_chain_identity"
        )
    );
    assert_eq!(
        text(
            aggregate_nodes["window:unknown-total"],
            "window_pushdown_identity_policy"
        ),
        "source_ordered_sha256_v1"
    );
    assert_eq!(
        aggregate_nodes["window:unknown-total"].estimated_rows(),
        None
    );
    assert_eq!(
        aggregate_nodes["window:unknown-total"].estimated_bytes(),
        None
    );
    assert_eq!(
        aggregate_nodes["window:unknown-total"].estimated_work(),
        None
    );
}

#[test]
fn changing_one_window_frame_reidentifies_only_its_source_and_downstream_folds() {
    let descriptors = pairs();
    let baseline = plan(
        [
            "table:Large",
            "table:Unknown",
            "table:Small",
            "table:Middle",
        ],
        &descriptors,
        &aggregates("frame:small-previous-through-current"),
    );
    let changed = plan(
        [
            "table:Large",
            "table:Unknown",
            "table:Small",
            "table:Middle",
        ],
        &descriptors,
        &aggregates("frame:small-current-through-next"),
    );
    let baseline_joins = joined(&baseline);
    let changed_joins = joined(&changed);
    for pair_id in ["pair:Small", "pair:Middle", "pair:Large", "pair:Unknown"] {
        assert_ne!(
            text(baseline_joins[pair_id], "join_cost_fold_identity"),
            text(changed_joins[pair_id], "join_cost_fold_identity"),
            "the changed early window frame updates its fold and every dependent fold"
        );
        assert_eq!(
            (
                baseline_joins[pair_id].estimated_rows(),
                baseline_joins[pair_id].estimated_bytes(),
                baseline_joins[pair_id].estimated_work(),
            ),
            (
                changed_joins[pair_id].estimated_rows(),
                changed_joins[pair_id].estimated_bytes(),
                changed_joins[pair_id].estimated_work(),
            ),
            "identity inputs do not falsify or erase real cost estimates"
        );
    }
    assert_ne!(
        text(
            pushed(&baseline)["window:small-average"],
            "window_pushdown_chain_identity"
        ),
        text(
            pushed(&changed)["window:small-average"],
            "window_pushdown_chain_identity"
        )
    );
}
