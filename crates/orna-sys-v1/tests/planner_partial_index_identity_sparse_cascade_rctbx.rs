use std::collections::BTreeMap;

use orna_sys_v1::{
    ObjectRef, PlanDetail, PlanNode, PlanNodeKind, QueryJoinDescription,
    QueryPartialIndexDescription, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes,
};

const PARTIAL_INDEX_FIXTURE: &str =
    include_str!("fixtures/planner_partial_index_identity_sparse_cascade_rctbx.orna");

#[derive(Debug, Eq, PartialEq)]
struct JoinObservation {
    planned_position: u64,
    declared_position: u64,
    table: String,
    access_kind: PlanNodeKind,
    index: Option<String>,
    predicate_identity: Option<String>,
    access_rows: Option<u64>,
    access_bytes: Option<u64>,
    access_work: Option<u64>,
    rows: Option<u64>,
    bytes: Option<u64>,
    work: Option<u64>,
}

fn object(reference: &str) -> ObjectRef {
    ObjectRef::descriptive(reference)
}

fn statistics(rows: u64, bytes: u64) -> QuerySourceStatistics {
    QuerySourceStatistics {
        estimated_rows: Some(rows),
        estimated_bytes: Some(bytes),
        mutable_branch: None,
    }
}

fn join(
    table: &str,
    rows: Option<u64>,
    bytes: Option<u64>,
    predicate: Option<&str>,
) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(table),
        statistics: (rows.is_some() || bytes.is_some()).then_some(QuerySourceStatistics {
            estimated_rows: rows,
            estimated_bytes: bytes,
            mutable_branch: None,
        }),
        predicate: predicate.map(orna_sys_v1::ExpressionRef::descriptive),
    }
}

fn index(table: &str, identity: &str, predicate: &str) -> QueryPartialIndexDescription {
    QueryPartialIndexDescription {
        table: object(table),
        index: object(identity),
        partial_predicate: orna_sys_v1::ExpressionRef::descriptive(predicate),
    }
}

fn query(joins: Vec<QueryJoinDescription>) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:partial-index-sparse-cascade"),
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

fn integer_detail(node: &PlanNode, name: &str) -> u64 {
    match node.details().get(name) {
        Some(PlanDetail::Integer(value)) => *value,
        other => panic!("{name} should be an integer, got {other:?}"),
    }
}

fn text_detail(node: &PlanNode, name: &str) -> String {
    match node.details().get(name) {
        Some(PlanDetail::Text(value)) => value.clone(),
        other => panic!("{name} should be text, got {other:?}"),
    }
}

fn join_observations(plan: &orna_sys_v1::ExplainedPlan) -> Vec<JoinObservation> {
    let nodes_by_reference = plan
        .nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect::<BTreeMap<_, _>>();
    let mut observations = plan
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| {
            let access = nodes_by_reference
                .get(node.inputs()[1].as_str())
                .expect("join right input remains attached to its access path");
            let (table, index, predicate_identity) = match access.kind() {
                PlanNodeKind::IndexLookup => (
                    text_detail(access, "table"),
                    Some(
                        access
                            .object()
                            .expect("index lookup exposes the selected index identity")
                            .as_str()
                            .to_owned(),
                    ),
                    Some(text_detail(access, "partial_predicate_identity")),
                ),
                PlanNodeKind::Scan => (
                    access
                        .object()
                        .expect("scan exposes the table identity")
                        .as_str()
                        .to_owned(),
                    None,
                    None,
                ),
                kind => panic!("unexpected right-side access node: {kind:?}"),
            };
            JoinObservation {
                planned_position: integer_detail(node, "planned_input_position"),
                declared_position: integer_detail(node, "declared_input_position"),
                table,
                access_kind: access.kind(),
                index,
                predicate_identity,
                access_rows: access.estimated_rows(),
                access_bytes: access.estimated_bytes(),
                access_work: access.estimated_work(),
                rows: node.estimated_rows(),
                bytes: node.estimated_bytes(),
                work: node.estimated_work(),
            }
        })
        .collect::<Vec<_>>();
    observations.sort_by_key(|observation| observation.planned_position);
    observations
}

#[test]
fn partial_index_identity_follows_sparse_predicate_cascade_after_reordering() {
    let parsed = orna_syntax_v1::parse_module(PARTIAL_INDEX_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let explained = explain_query_with_partial_indexes(
        &query(vec![
            join(
                "table:Expensive",
                Some(500),
                Some(64_000),
                Some("expr:broad-live"),
            ),
            join("table:Unknown", None, None, None),
            join("table:Small", Some(8), Some(8_192), Some("expr:small-live")),
            join(
                "table:Medium",
                Some(30),
                Some(4_096),
                Some("expr:medium-live"),
            ),
        ]),
        &[
            index("table:Medium", "index:medium-live", "expr:medium-live"),
            index("table:Small", "index:small-live-z", "expr:small-live"),
            index(
                "table:Unrelated",
                "index:decoy-small-live",
                "expr:small-live",
            ),
            index(
                "table:Expensive",
                "index:wrong-predicate",
                "expr:small-live",
            ),
            index("table:Small", "index:small-live-a", "expr:small-live"),
        ],
    )
    .expect("partial index candidates produce an explain plan");

    assert_eq!(
        join_observations(&explained),
        vec![
            JoinObservation {
                planned_position: 1,
                declared_position: 3,
                table: "table:Small".to_owned(),
                access_kind: PlanNodeKind::IndexLookup,
                index: Some("index:small-live-a".to_owned()),
                predicate_identity: Some("expr:small-live".to_owned()),
                access_rows: Some(8),
                access_bytes: Some(8_192),
                access_work: Some(10),
                rows: Some(80),
                bytes: Some(85_120),
                work: Some(108),
            },
            JoinObservation {
                planned_position: 2,
                declared_position: 4,
                table: "table:Medium".to_owned(),
                access_kind: PlanNodeKind::IndexLookup,
                index: Some("index:medium-live".to_owned()),
                predicate_identity: Some("expr:medium-live".to_owned()),
                access_rows: Some(30),
                access_bytes: Some(4_096),
                access_work: Some(31),
                rows: Some(240),
                bytes: Some(288_240),
                work: Some(110),
            },
            JoinObservation {
                planned_position: 3,
                declared_position: 1,
                table: "table:Expensive".to_owned(),
                access_kind: PlanNodeKind::Scan,
                index: None,
                predicate_identity: None,
                access_rows: Some(500),
                access_bytes: Some(64_000),
                access_work: Some(516),
                rows: Some(12_000),
                bytes: Some(15_948_000),
                work: Some(740),
            },
            JoinObservation {
                planned_position: 4,
                declared_position: 2,
                table: "table:Unknown".to_owned(),
                access_kind: PlanNodeKind::Scan,
                index: None,
                predicate_identity: None,
                access_rows: None,
                access_bytes: None,
                access_work: None,
                rows: None,
                bytes: None,
                work: None,
            },
        ],
        "indexes stay paired to exact table/predicate identities as known-cost joins reorder around sparse predicate gaps"
    );
    assert_eq!(explained.plan().estimated_cost(), None);
    assert_eq!(explained.root().estimated_rows(), None);
    assert_eq!(explained.root().estimated_bytes(), None);
}
