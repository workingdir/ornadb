use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, PlanWindowFrameBound,
    QueryJoinDescription, QueryJoinPairIdentityDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryWindowAggregatePushdownDescription, QueryWindowSpillDescription,
    SnapshotRef, explain_query_with_paired_cost_restoration_and_window_spill_pushdowns,
};

const FIXTURE: &str = include_str!("fixtures/planner_bounded_spill_chain_fold_window_j6won.orna");

fn object(value: &str) -> ObjectRef {
    ObjectRef::descriptive(value)
}

fn expression(value: &str) -> ExpressionRef {
    ExpressionRef::descriptive(value)
}

fn pair(source: &str) -> QueryJoinPairIdentityDescription {
    QueryJoinPairIdentityDescription {
        identity: object(&format!("pair:{source}")),
        left_source: object("table:Anchor"),
        right_source: object(&format!("table:{source}")),
        predicate: Some(expression(&format!("expr:join-{source}"))),
    }
}

fn aggregate(
    identity: &str,
    source: &str,
    frame_identity: &str,
    frame_start: PlanWindowFrameBound,
    frame_end: PlanWindowFrameBound,
) -> QueryWindowAggregatePushdownDescription {
    QueryWindowAggregatePushdownDescription {
        identity: object(identity),
        source: object(source),
        aggregate: expression("expr:sum-value"),
        frame_identity: expression(frame_identity),
        frame_start,
        frame_end,
    }
}

fn spill(
    identity: &str,
    pair_identity: &str,
    source: &str,
    aggregate_identity: &str,
    working_set: Option<u64>,
    memory_budget: u64,
) -> QueryWindowSpillDescription {
    QueryWindowSpillDescription {
        identity: object(identity),
        join_pair_identity: object(pair_identity),
        source: object(source),
        window_aggregate_identity: object(aggregate_identity),
        estimated_working_set_bytes: working_set,
        memory_budget_bytes: memory_budget,
    }
}

fn plan(
    first_budget: u64,
    second_budget: u64,
    second_unknown: bool,
    first_frame_start: PlanWindowFrameBound,
    unbounded_frame_identity: &str,
    include_tail: bool,
    reverse_pair_spill_descriptors: bool,
    include_bounded_spills: bool,
) -> orna_sys_v1::ExplainedPlan {
    let mut joins = vec!["First", "Gap", "Second", "Tail"]
        .into_iter()
        .map(|source| QueryJoinDescription {
            source: object(&format!("table:{source}")),
            statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(4),
                estimated_bytes: Some(4_000),
                mutable_branch: None,
            }),
            predicate: Some(expression(&format!("expr:join-{source}"))),
        })
        .collect::<Vec<_>>();
    if !include_tail {
        joins.pop();
    }
    let query = QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:bounded-spill-chain-fold-window-j6won"),
        source: object("table:Anchor"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: Some(100),
            estimated_bytes: Some(40_000),
            mutable_branch: None,
        }),
        joins,
        predicate: None,
        projections: vec![expression("expr:window-output")],
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    };
    let mut pairs = vec![pair("First"), pair("Second")];
    if include_tail {
        pairs.push(pair("Tail"));
    }
    let aggregates = vec![
        aggregate(
            "window:first-bounded",
            "table:First",
            "frame:first-bounded",
            first_frame_start,
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:first-unbounded",
            "table:First",
            unbounded_frame_identity,
            PlanWindowFrameBound::UnboundedPreceding,
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:second-bounded-a",
            "table:Second",
            "frame:second-bounded-a",
            PlanWindowFrameBound::Preceding(2),
            PlanWindowFrameBound::CurrentRow,
        ),
        aggregate(
            "window:second-bounded-b",
            "table:Second",
            "frame:second-bounded-b",
            PlanWindowFrameBound::Preceding(5),
            PlanWindowFrameBound::Following(1),
        ),
    ];
    let mut spills = vec![
        spill(
            "spill:first-bounded",
            "pair:First",
            "table:First",
            "window:first-bounded",
            Some(9_000),
            first_budget,
        ),
        spill(
            "spill:first-unbounded",
            "pair:First",
            "table:First",
            "window:first-unbounded",
            Some(80_000),
            1_000,
        ),
        spill(
            "spill:second-bounded-a",
            "pair:Second",
            "table:Second",
            "window:second-bounded-a",
            Some(12_000),
            second_budget,
        ),
        spill(
            "spill:second-bounded-b",
            "pair:Second",
            "table:Second",
            "window:second-bounded-b",
            (!second_unknown).then_some(18_000),
            6_000,
        ),
    ];
    if !include_bounded_spills {
        spills.retain(|spill| spill.identity.as_str() == "spill:first-unbounded");
    }
    if reverse_pair_spill_descriptors {
        pairs.reverse();
        spills.reverse();
    }

    explain_query_with_paired_cost_restoration_and_window_spill_pushdowns(
        &query,
        &pairs,
        &aggregates,
        &spills,
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
    )
    .expect("paired chain-fold windows with bounded spill descriptors explain")
}

fn project(plan: &orna_sys_v1::ExplainedPlan) -> &PlanNode {
    plan.nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Project)
        .expect("the output expression creates a Project node")
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

#[test]
fn bounded_spill_identity_binds_exact_members_of_paired_chain_fold_windows() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let baseline = plan(
        5_000,
        8_000,
        false,
        PlanWindowFrameBound::Preceding(3),
        "frame:first-unbounded-v1",
        true,
        false,
        true,
    );
    let no_tail = plan(
        5_000,
        8_000,
        false,
        PlanWindowFrameBound::Preceding(3),
        "frame:first-unbounded-v1",
        false,
        false,
        true,
    );
    let reordered = plan(
        5_000,
        8_000,
        false,
        PlanWindowFrameBound::Preceding(3),
        "frame:first-unbounded-v1",
        true,
        true,
        true,
    );
    let changed_budget = plan(
        5_001,
        8_000,
        false,
        PlanWindowFrameBound::Preceding(3),
        "frame:first-unbounded-v1",
        true,
        false,
        true,
    );
    let changed_bounded_frame = plan(
        5_000,
        8_000,
        false,
        PlanWindowFrameBound::Preceding(4),
        "frame:first-unbounded-v1",
        true,
        false,
        true,
    );
    let changed_unbounded_window = plan(
        5_000,
        8_000,
        false,
        PlanWindowFrameBound::Preceding(3),
        "frame:first-unbounded-v2",
        true,
        false,
        true,
    );

    let baseline = project(&baseline);
    let identity_key = "paired_bounded_window_spill_chain_fold_window_identity";
    let chain_key = "paired_bounded_window_spill_chain_fold_window_chain_fold_identity";
    let spill_key = "paired_bounded_window_spill_chain_fold_window_spill_fold_identity";
    assert!(
        text(baseline, identity_key).starts_with("paired-bounded-window-spill-chain-fold-window:")
    );
    assert!(text(baseline, chain_key).starts_with("paired-window-chain-fold:"));
    assert!(
        text(baseline, spill_key).starts_with("paired-bounded-window-spill-chain-window-fold:")
    );
    assert_eq!(
        integer(
            baseline,
            "paired_bounded_window_spill_chain_fold_window_pair_count"
        ),
        2
    );
    assert_eq!(
        integer(
            baseline,
            "paired_bounded_window_spill_chain_fold_window_spill_window_count"
        ),
        3
    );
    assert_eq!(
        integer(
            baseline,
            "paired_bounded_window_spill_chain_fold_window_spill_stage_count"
        ),
        3
    );
    assert_eq!(
        integer(
            baseline,
            "paired_bounded_window_spill_chain_fold_window_estimated_bytes"
        ),
        20_000
    );
    assert_eq!(
        integer(
            baseline,
            "paired_bounded_window_spill_chain_fold_window_estimated_io_blocks"
        ),
        5
    );
    assert_eq!(
        integer(
            baseline,
            "paired_bounded_window_spill_chain_fold_window_estimated_io_work"
        ),
        10
    );

    assert_eq!(
        text(baseline, identity_key),
        text(project(&no_tail), identity_key),
        "a sparse tail does not advance the exact chain-window spill identity"
    );
    assert_eq!(
        text(baseline, identity_key),
        text(project(&reordered), identity_key),
        "pair and spill descriptor ordering does not change exact membership"
    );

    let changed_budget = project(&changed_budget);
    assert_ne!(
        text(baseline, identity_key),
        text(changed_budget, identity_key)
    );
    assert_ne!(text(baseline, spill_key), text(changed_budget, spill_key));
    assert_eq!(text(baseline, chain_key), text(changed_budget, chain_key));

    let changed_bounded_frame = project(&changed_bounded_frame);
    assert_ne!(
        text(baseline, identity_key),
        text(changed_bounded_frame, identity_key)
    );
    assert_ne!(
        text(baseline, chain_key),
        text(changed_bounded_frame, chain_key)
    );
    assert_ne!(
        text(baseline, spill_key),
        text(changed_bounded_frame, spill_key)
    );

    let changed_unbounded_window = project(&changed_unbounded_window);
    assert_ne!(
        text(baseline, identity_key),
        text(changed_unbounded_window, identity_key)
    );
    assert_ne!(
        text(baseline, chain_key),
        text(changed_unbounded_window, chain_key)
    );
    assert_eq!(
        text(baseline, spill_key),
        text(changed_unbounded_window, spill_key),
        "an unbounded window changes the chain component without becoming a spill stage"
    );
}

#[test]
fn bounded_spill_chain_window_fold_propagates_unknowns_and_requires_exact_bounded_pairs() {
    let unknown_plan = plan(
        5_000,
        8_000,
        true,
        PlanWindowFrameBound::Preceding(3),
        "frame:first-unbounded-v1",
        true,
        false,
        true,
    );
    let unknown = project(&unknown_plan);
    assert_eq!(
        integer(
            unknown,
            "paired_bounded_window_spill_chain_fold_window_unknown_working_set_count"
        ),
        1
    );
    assert_eq!(
        text(
            unknown,
            "paired_bounded_window_spill_chain_fold_window_estimate_status"
        ),
        "unknown_working_set"
    );
    assert!(
        !unknown
            .details()
            .contains_key("paired_bounded_window_spill_chain_fold_window_estimated_bytes")
    );

    let unbounded_only = plan(
        5_000,
        8_000,
        false,
        PlanWindowFrameBound::Preceding(3),
        "frame:first-unbounded-v1",
        true,
        false,
        false,
    );
    assert!(
        !project(&unbounded_only)
            .details()
            .contains_key("paired_bounded_window_spill_chain_fold_window_identity"),
        "an exact unbounded-window spill does not create a bounded-window fold"
    );
}
