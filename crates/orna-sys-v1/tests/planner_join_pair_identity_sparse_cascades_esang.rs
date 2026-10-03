use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, QueryJoinDescription,
    QueryJoinPairIdentityDescription, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_join_pair_identities,
};

const JOIN_PAIR_FIXTURE: &str =
    include_str!("fixtures/planner_join_pair_identity_sparse_cascades_esang.orna");

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

fn join(
    source: &str,
    stats: Option<QuerySourceStatistics>,
    predicate: &str,
) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics: stats,
        predicate: Some(expression(predicate)),
    }
}

fn pair(identity: &str, right_source: &str, predicate: &str) -> QueryJoinPairIdentityDescription {
    QueryJoinPairIdentityDescription {
        identity: object(identity),
        left_source: object("table:Anchor"),
        right_source: object(right_source),
        predicate: Some(expression(predicate)),
    }
}

fn query(joins: Vec<QueryJoinDescription>) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:join-pair-sparse-cascades"),
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

fn text<'a>(node: &'a PlanNode, name: &str) -> &'a str {
    match node.details().get(name) {
        Some(PlanDetail::Text(value)) => value,
        other => panic!("{name} should be text, got {other:?}"),
    }
}

type JoinObservation = (
    u64,
    u64,
    String,
    String,
    Option<u64>,
    Option<u64>,
    Option<u64>,
);

fn join_observations(plan: &orna_sys_v1::ExplainedPlan) -> Vec<JoinObservation> {
    let nodes = plan
        .nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect::<BTreeMap<_, _>>();
    let mut joins = plan
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| {
            let access = nodes
                .get(node.inputs()[1].as_str())
                .expect("join right input retains its access path");
            assert_eq!(
                text(node, "join_pair_identity"),
                text(access, "join_pair_identity"),
                "the fold and its exact right source share pair identity"
            );
            assert_eq!(
                text(node, "logical_predicate_identity"),
                text(access, "logical_predicate_identity")
            );
            assert_eq!(
                text(node, "logical_right_source_identity"),
                access.object().expect("scan retains right source").as_str()
            );
            assert_eq!(text(node, "logical_left_source_identity"), "table:Anchor");
            assert_eq!(
                text(node, "join_pair_identity_resolution"),
                "exact_right_source_and_predicate"
            );
            (
                match node.details().get("planned_input_position") {
                    Some(PlanDetail::Integer(value)) => *value,
                    other => panic!("planned position should be integer, got {other:?}"),
                },
                match node.details().get("declared_input_position") {
                    Some(PlanDetail::Integer(value)) => *value,
                    other => panic!("declared position should be integer, got {other:?}"),
                },
                text(node, "join_pair_identity").to_owned(),
                text(node, "logical_right_source_identity").to_owned(),
                node.estimated_rows(),
                node.estimated_bytes(),
                node.estimated_work(),
            )
        })
        .collect::<Vec<_>>();
    joins.sort_by_key(|join| join.0);
    joins
}

#[test]
fn pair_identities_remain_with_real_cost_cascade_folds_across_sparse_reordering() {
    let parsed = orna_syntax_v1::parse_module(JOIN_PAIR_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let pairs = [
        pair("pair:large", "table:Large", "expr:anchor-large"),
        pair("pair:unknown", "table:Unknown", "expr:anchor-unknown"),
        pair("pair:small", "table:Small", "expr:anchor-small"),
        pair("pair:middle", "table:Middle", "expr:anchor-middle"),
    ];
    let first = explain_query_with_join_pair_identities(
        &query(vec![
            join(
                "table:Large",
                Some(statistics(300, 16_384)),
                "expr:anchor-large",
            ),
            join("table:Unknown", None, "expr:anchor-unknown"),
            join(
                "table:Small",
                Some(statistics(5, 4_096)),
                "expr:anchor-small",
            ),
            join(
                "table:Middle",
                Some(statistics(20, 8_192)),
                "expr:anchor-middle",
            ),
        ]),
        &pairs,
    )
    .expect("exact pair identities resolve across unknown-cost join inputs");
    let reversed_pairs = pairs.iter().rev().cloned().collect::<Vec<_>>();
    let second = explain_query_with_join_pair_identities(
        &query(vec![
            join(
                "table:Middle",
                Some(statistics(20, 8_192)),
                "expr:anchor-middle",
            ),
            join(
                "table:Small",
                Some(statistics(5, 4_096)),
                "expr:anchor-small",
            ),
            join("table:Unknown", None, "expr:anchor-unknown"),
            join(
                "table:Large",
                Some(statistics(300, 16_384)),
                "expr:anchor-large",
            ),
        ]),
        &reversed_pairs,
    )
    .expect("pair identities do not depend on declaration offsets");

    let expected_first = vec![
        (
            1,
            3,
            "pair:small".to_owned(),
            "table:Small".to_owned(),
            Some(50),
            Some(43_000),
            Some(105),
        ),
        (
            2,
            4,
            "pair:middle".to_owned(),
            "table:Middle".to_owned(),
            Some(100),
            Some(127_000),
            Some(70),
        ),
        (
            3,
            1,
            "pair:large".to_owned(),
            "table:Large".to_owned(),
            Some(3_000),
            Some(3_975_000),
            Some(400),
        ),
        (
            4,
            2,
            "pair:unknown".to_owned(),
            "table:Unknown".to_owned(),
            None,
            None,
            None,
        ),
    ];
    let expected_second = vec![
        (
            1,
            2,
            "pair:small".to_owned(),
            "table:Small".to_owned(),
            Some(50),
            Some(43_000),
            Some(105),
        ),
        (
            2,
            1,
            "pair:middle".to_owned(),
            "table:Middle".to_owned(),
            Some(100),
            Some(127_000),
            Some(70),
        ),
        (
            3,
            4,
            "pair:large".to_owned(),
            "table:Large".to_owned(),
            Some(3_000),
            Some(3_975_000),
            Some(400),
        ),
        (
            4,
            3,
            "pair:unknown".to_owned(),
            "table:Unknown".to_owned(),
            None,
            None,
            None,
        ),
    ];
    let first_joins = join_observations(&first);
    let second_joins = join_observations(&second);
    assert_eq!(first_joins, expected_first);
    assert_eq!(second_joins, expected_second);
    assert_eq!(
        first_joins
            .iter()
            .map(|join| (join.0, &join.2, &join.3, join.4, join.5, join.6))
            .collect::<Vec<_>>(),
        second_joins
            .iter()
            .map(|join| (join.0, &join.2, &join.3, join.4, join.5, join.6))
            .collect::<Vec<_>>(),
        "pair identities remain bound to their computed cascade results when declaration offsets change"
    );
    assert_eq!(first.plan().estimated_cost(), None);
    assert_eq!(second.plan().estimated_cost(), None);
}

#[test]
fn join_pair_identity_requires_an_exact_declared_source_and_predicate() {
    let query = query(vec![join(
        "table:Small",
        Some(statistics(5, 4_096)),
        "expr:anchor-small",
    )]);
    let mismatched_predicate = pair("pair:wrong", "table:Small", "expr:other-predicate");
    let missing_left_source = QueryJoinPairIdentityDescription {
        identity: object("pair:missing-left"),
        left_source: object("table:Absent"),
        right_source: object("table:Small"),
        predicate: Some(expression("expr:anchor-small")),
    };

    assert!(matches!(
        explain_query_with_join_pair_identities(&query, &[mismatched_predicate]),
        Err(orna_sys_v1::ExplainError::InvalidObject)
    ));
    assert!(matches!(
        explain_query_with_join_pair_identities(&query, &[missing_left_source]),
        Err(orna_sys_v1::ExplainError::InvalidObject)
    ));
}

#[test]
fn join_pair_identity_contributes_to_the_stable_plan_id() {
    let query = query(vec![join(
        "table:Small",
        Some(statistics(5, 4_096)),
        "expr:anchor-small",
    )]);
    let original = pair("pair:small", "table:Small", "expr:anchor-small");
    let renamed = pair("pair:small-v2", "table:Small", "expr:anchor-small");
    let original_plan = explain_query_with_join_pair_identities(&query, &[original])
        .expect("original pair identity has a concrete plan");
    let renamed_plan = explain_query_with_join_pair_identities(&query, &[renamed])
        .expect("renamed pair identity has a concrete plan");

    assert_ne!(original_plan.plan().id(), renamed_plan.plan().id());
}
