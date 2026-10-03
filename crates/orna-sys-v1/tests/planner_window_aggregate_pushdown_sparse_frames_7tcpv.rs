use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryPlanDescription, QuerySourceStatistics,
    QueryWindowAggregatePushdownDescription, SnapshotRef,
    explain_query_with_window_aggregate_pushdowns,
};

const WINDOW_FIXTURE: &str =
    include_str!("fixtures/planner_window_aggregate_pushdown_sparse_frames_7tcpv.orna");

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

fn join(source: &str, stats: Option<QuerySourceStatistics>) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics: stats,
        predicate: None,
    }
}

fn query() -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:window-sparse-frame-folds"),
        source: object("table:Anchor"),
        source_statistics: Some(statistics(100, 4_000)),
        joins: vec![
            join("table:Wide", Some(statistics(1_000, 128_000))),
            join("table:Unknown", None),
            join("table:Narrow", Some(statistics(10, 1_024))),
        ],
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

fn text<'a>(node: &'a PlanNode, name: &str) -> &'a str {
    match node.details().get(name) {
        Some(PlanDetail::Text(value)) => value,
        other => panic!("{name} should be text, got {other:?}"),
    }
}

fn pushed_aggregates(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<String, &PlanNode> {
    plan.nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Aggregate
                && node.details().contains_key("window_aggregate_identity")
        })
        .map(|node| (text(node, "window_aggregate_identity").to_owned(), node))
        .collect()
}

fn input_source<'a>(node: &'a PlanNode, nodes: &'a BTreeMap<String, &'a PlanNode>) -> &'a str {
    let input = nodes
        .get(node.inputs()[0].as_str())
        .expect("pushed aggregate has its source input");
    if input.kind() == PlanNodeKind::Aggregate {
        input_source(input, nodes)
    } else {
        input
            .object()
            .expect("scan or index lookup retains its source")
            .as_str()
    }
}

#[test]
fn window_aggregate_and_frame_identities_follow_sparse_reordered_inputs() {
    let parsed = orna_syntax_v1::parse_module(WINDOW_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let aggregates = [
        aggregate(
            "window:wide-total",
            "table:Wide",
            "expr:sum-value",
            "frame:wide-rows-2-through-current",
            PlanWindowFrameBound::Preceding(2),
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:unknown-mean",
            "table:Unknown",
            "expr:mean-value",
            "frame:unknown-current-through-next",
            PlanWindowFrameBound::CurrentRow,
            PlanWindowFrameBound::Following(1),
        ),
        aggregate(
            "window:narrow-forward",
            "table:Narrow",
            "expr:max-value",
            "frame:narrow-current-through-next",
            PlanWindowFrameBound::CurrentRow,
            PlanWindowFrameBound::Following(1),
        ),
        aggregate(
            "window:narrow-trailing",
            "table:Narrow",
            "expr:min-value",
            "frame:narrow-previous-through-current",
            PlanWindowFrameBound::Preceding(1),
            PlanWindowFrameBound::CurrentRow,
        ),
    ];
    let explained = explain_query_with_window_aggregate_pushdowns(&query(), &aggregates)
        .expect("resolved window aggregates attach to their exact sources");
    let aggregates_by_identity = pushed_aggregates(&explained);
    assert_eq!(aggregates_by_identity.len(), 4);

    let nodes_by_reference = explained
        .nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect::<BTreeMap<_, _>>();

    let narrow_trailing = aggregates_by_identity["window:narrow-trailing"];
    let narrow_forward = aggregates_by_identity["window:narrow-forward"];
    assert_eq!(
        input_source(narrow_trailing, &nodes_by_reference),
        "table:Narrow"
    );
    assert_eq!(
        input_source(narrow_forward, &nodes_by_reference),
        "table:Narrow"
    );
    assert_eq!(narrow_trailing.estimated_rows(), Some(10));
    assert_eq!(narrow_trailing.estimated_bytes(), Some(1_024));
    assert_eq!(narrow_trailing.estimated_work(), Some(10));
    assert_eq!(
        text(narrow_trailing, "window_frame_identity"),
        "frame:narrow-previous-through-current"
    );
    assert_eq!(text(narrow_trailing, "window_frame_start"), "preceding:1");
    assert_eq!(text(narrow_trailing, "window_frame_end"), "current_row");
    assert_eq!(
        text(narrow_forward, "window_frame_identity"),
        "frame:narrow-current-through-next"
    );
    assert_eq!(text(narrow_forward, "window_frame_start"), "current_row");
    assert_eq!(text(narrow_forward, "window_frame_end"), "following:1");
    assert_eq!(
        text(narrow_forward, "pushdown_policy"),
        "exact_source_identity_before_join"
    );

    let unknown = aggregates_by_identity["window:unknown-mean"];
    assert_eq!(input_source(unknown, &nodes_by_reference), "table:Unknown");
    assert_eq!(unknown.estimated_rows(), None);
    assert_eq!(unknown.estimated_bytes(), None);
    assert_eq!(unknown.estimated_work(), None);

    let wide = aggregates_by_identity["window:wide-total"];
    assert_eq!(input_source(wide, &nodes_by_reference), "table:Wide");
    assert_eq!(wide.estimated_rows(), Some(1_000));
    assert_eq!(wide.estimated_bytes(), Some(128_000));
    assert_eq!(wide.estimated_work(), Some(1_000));

    assert_eq!(
        explained
            .nodes()
            .iter()
            .filter(|node| node.kind() == PlanNodeKind::Join)
            .map(|node| {
                nodes_by_reference[node.inputs()[1].as_str()]
                    .details()
                    .get("window_aggregate_identity")
                    .map(|detail| match detail {
                        PlanDetail::Text(identity) => identity.as_str(),
                        _ => panic!("window identity should be text"),
                    })
                    .unwrap_or("no_window_aggregate")
            })
            .collect::<Vec<_>>(),
        [
            "window:unknown-mean",
            "window:wide-total",
            "window:narrow-trailing"
        ]
    );
}

#[test]
fn changing_a_window_frame_identity_changes_the_computed_plan_identity() {
    let original = aggregate(
        "window:mean",
        "table:Narrow",
        "expr:mean-value",
        "frame:previous-through-current",
        PlanWindowFrameBound::Preceding(1),
        PlanWindowFrameBound::CurrentRow,
    );
    let changed_frame = aggregate(
        "window:mean",
        "table:Narrow",
        "expr:mean-value",
        "frame:current-through-next",
        PlanWindowFrameBound::CurrentRow,
        PlanWindowFrameBound::Following(1),
    );
    let original_plan = explain_query_with_window_aggregate_pushdowns(&query(), &[original])
        .expect("valid trailing frame plans");
    let changed_plan = explain_query_with_window_aggregate_pushdowns(&query(), &[changed_frame])
        .expect("valid forward frame plans");

    assert_ne!(
        original_plan.plan().id(),
        changed_plan.plan().id(),
        "frame identity and bounds are part of the stable plan digest"
    );
}

#[test]
fn reversed_window_frame_bounds_are_rejected_before_planning() {
    let reversed = aggregate(
        "window:invalid",
        "table:Narrow",
        "expr:sum-value",
        "frame:reversed",
        PlanWindowFrameBound::Following(1),
        PlanWindowFrameBound::Preceding(1),
    );

    assert!(matches!(
        explain_query_with_window_aggregate_pushdowns(&query(), &[reversed]),
        Err(orna_sys_v1::ExplainError::InvalidExpression)
    ));
}
