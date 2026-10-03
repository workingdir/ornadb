use std::collections::BTreeMap;

use orna_sys_v1::{
    explain_query_with_partial_indexes, ExplainError, ExpressionRef, ObjectRef, PlanDetail,
    PlanNode, PlanNodeKind, QueryJoinDescription, QueryPartialIndexDescription,
    QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_predicate_pushdown_index_pair_sparse_cascades_0u4fb.orna");

#[derive(Debug, Eq, PartialEq)]
struct Fold {
    planned_position: u64,
    declared_position: u64,
    table: String,
    access_kind: PlanNodeKind,
    selected_index: Option<String>,
    predicate: Option<String>,
    pushdown_identity: Option<String>,
    rows: Option<u64>,
    bytes: Option<u64>,
    work: Option<u64>,
}

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
        predicate: predicate.map(expression),
    }
}

fn index(table: &str, identity: &str, predicate: &str) -> QueryPartialIndexDescription {
    QueryPartialIndexDescription {
        table: object(table),
        index: object(identity),
        partial_predicate: expression(predicate),
    }
}

fn query(joins: Vec<QueryJoinDescription>) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:predicate-index-pair-sparse-cascade"),
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

fn integer_detail(node: &PlanNode, key: &str) -> u64 {
    match node.details().get(key) {
        Some(PlanDetail::Integer(value)) => *value,
        other => panic!("{key} should be an integer, got {other:?}"),
    }
}

fn text_detail(node: &PlanNode, key: &str) -> String {
    match node.details().get(key) {
        Some(PlanDetail::Text(value)) => value.clone(),
        other => panic!("{key} should be text, got {other:?}"),
    }
}

fn folds(plan: &orna_sys_v1::ExplainedPlan) -> Vec<Fold> {
    let nodes = plan
        .nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect::<BTreeMap<_, _>>();
    let mut folds = plan
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| {
            let access = nodes
                .get(node.inputs()[1].as_str())
                .expect("join keeps its right access path paired");
            let (table, selected_index, predicate, identity) = match access.kind() {
                PlanNodeKind::IndexLookup => (
                    text_detail(access, "table"),
                    Some(
                        access
                            .object()
                            .expect("lookup exposes selected index")
                            .as_str()
                            .to_owned(),
                    ),
                    Some(text_detail(access, "partial_predicate_identity")),
                    Some(text_detail(access, "predicate_pushdown_identity")),
                ),
                PlanNodeKind::Scan => (
                    access
                        .object()
                        .expect("scan exposes its source table")
                        .as_str()
                        .to_owned(),
                    None,
                    None,
                    None,
                ),
                kind => panic!("unexpected join input access kind: {kind:?}"),
            };
            let join_identity = node.details().get("predicate_pushdown_identity");
            assert_eq!(
                join_identity.map(|_| text_detail(node, "predicate_pushdown_identity")),
                identity,
                "the fold repeats its selected lookup identity and does not borrow a neighbor's label"
            );
            if identity.is_some() {
                assert_eq!(
                    text_detail(node, "predicate_pushdown_pairing"),
                    "exact_table_index_and_predicate"
                );
            }
            Fold {
                planned_position: integer_detail(node, "planned_input_position"),
                declared_position: integer_detail(node, "declared_input_position"),
                table,
                access_kind: access.kind(),
                selected_index,
                predicate,
                pushdown_identity: identity,
                rows: node.estimated_rows(),
                bytes: node.estimated_bytes(),
                work: node.estimated_work(),
            }
        })
        .collect::<Vec<_>>();
    folds.sort_by_key(|fold| fold.planned_position);
    folds
}

fn indexes() -> Vec<QueryPartialIndexDescription> {
    vec![
        index("table:Medium", "index:medium-live", "expr:medium-live"),
        index("table:Small", "index:small-live-z", "expr:small-live"),
        index("table:Other", "index:wrong-table", "expr:small-live"),
        index("table:Small", "index:wrong-predicate", "expr:medium-live"),
        index(
            "table:Expensive",
            "index:expensive-decoy",
            "expr:other-live",
        ),
        index("table:Small", "index:small-live-a", "expr:small-live"),
    ]
}

fn sparse_joins(order: [&str; 4]) -> Vec<QueryJoinDescription> {
    order
        .into_iter()
        .map(|table| match table {
            "table:Expensive" => join(
                "table:Expensive",
                Some(500),
                Some(64_000),
                Some("expr:broad-live"),
            ),
            "table:Unknown" => join("table:Unknown", None, None, None),
            "table:Small" => join("table:Small", Some(8), Some(8_192), Some("expr:small-live")),
            "table:Medium" => join(
                "table:Medium",
                Some(30),
                Some(4_096),
                Some("expr:medium-live"),
            ),
            other => panic!("unexpected fixture table {other}"),
        })
        .collect()
}

#[test]
fn paired_index_and_predicate_identities_follow_sparse_cost_folds() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let explained = explain_query_with_partial_indexes(
        &query(sparse_joins([
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
        ])),
        &indexes(),
    )
    .expect("sparse cost cascade plans its matching partial indexes");

    assert_eq!(
        folds(&explained),
        vec![
            Fold {
                planned_position: 1,
                declared_position: 3,
                table: "table:Small".to_owned(),
                access_kind: PlanNodeKind::IndexLookup,
                selected_index: Some("index:small-live-a".to_owned()),
                predicate: Some("expr:small-live".to_owned()),
                pushdown_identity: Some(
                    "index-pair:99c01ae9e673ea2f88ff87e872eb530dc32af88411ca7f4ef5ffc54b1f8bb9c3"
                        .to_owned()
                ),
                rows: Some(80),
                bytes: Some(85_120),
                work: Some(108),
            },
            Fold {
                planned_position: 2,
                declared_position: 4,
                table: "table:Medium".to_owned(),
                access_kind: PlanNodeKind::IndexLookup,
                selected_index: Some("index:medium-live".to_owned()),
                predicate: Some("expr:medium-live".to_owned()),
                pushdown_identity: Some(
                    "index-pair:fd0d9a889aa7e1d3a8ae4f2e1aeff7ee5ddaba4499c3584255f7a4201fd80709"
                        .to_owned()
                ),
                rows: Some(240),
                bytes: Some(288_240),
                work: Some(110),
            },
            Fold {
                planned_position: 3,
                declared_position: 1,
                table: "table:Expensive".to_owned(),
                access_kind: PlanNodeKind::Scan,
                selected_index: None,
                predicate: None,
                pushdown_identity: None,
                rows: Some(12_000),
                bytes: Some(15_948_000),
                work: Some(740),
            },
            Fold {
                planned_position: 4,
                declared_position: 2,
                table: "table:Unknown".to_owned(),
                access_kind: PlanNodeKind::Scan,
                selected_index: None,
                predicate: None,
                pushdown_identity: None,
                rows: None,
                bytes: None,
                work: None,
            },
        ]
    );
    assert_eq!(explained.plan().estimated_cost(), None);
    assert_eq!(explained.root().estimated_rows(), None);
    assert_eq!(explained.root().estimated_bytes(), None);
}

#[test]
fn paired_pushdown_values_are_stable_when_declared_fold_order_changes() {
    let first = explain_query_with_partial_indexes(
        &query(sparse_joins([
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
        ])),
        &indexes(),
    )
    .expect("first declaration order plans");
    let second = explain_query_with_partial_indexes(
        &query(sparse_joins([
            "table:Medium",
            "table:Small",
            "table:Unknown",
            "table:Expensive",
        ])),
        &indexes().into_iter().rev().collect::<Vec<_>>(),
    )
    .expect("reversed declaration and candidate order plans");
    let identities = |plan: &orna_sys_v1::ExplainedPlan| {
        folds(plan)
            .into_iter()
            .filter_map(|fold| {
                fold.pushdown_identity
                    .map(|identity| (fold.table, identity))
            })
            .collect::<BTreeMap<_, _>>()
    };

    assert_eq!(
        identities(&first),
        BTreeMap::from([
            (
                "table:Small".to_owned(),
                "index-pair:99c01ae9e673ea2f88ff87e872eb530dc32af88411ca7f4ef5ffc54b1f8bb9c3"
                    .to_owned(),
            ),
            (
                "table:Medium".to_owned(),
                "index-pair:fd0d9a889aa7e1d3a8ae4f2e1aeff7ee5ddaba4499c3584255f7a4201fd80709"
                    .to_owned(),
            ),
        ])
    );
    assert_eq!(identities(&second), identities(&first));
    let first_positions = folds(&first)
        .into_iter()
        .map(|fold| (fold.table, fold.declared_position))
        .collect::<BTreeMap<_, _>>();
    let second_positions = folds(&second)
        .into_iter()
        .map(|fold| (fold.table, fold.declared_position))
        .collect::<BTreeMap<_, _>>();
    assert_ne!(first_positions, second_positions);
}

#[test]
fn sparse_index_candidates_without_an_exact_table_predicate_pair_are_rejected_from_pushdown() {
    let explained = explain_query_with_partial_indexes(
        &query(vec![join(
            "table:Facts",
            Some(12),
            Some(12_288),
            Some("expr:active-facts"),
        )]),
        &[
            index("table:Other", "index:other-active", "expr:active-facts"),
            index(
                "table:Facts",
                "index:wrong-predicate",
                "expr:inactive-facts",
            ),
        ],
    )
    .expect("unmatched candidates leave a valid scan plan");

    let access = explained
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node
                    .object()
                    .is_some_and(|table| table.as_str() == "table:Facts")
        })
        .expect("the matching source uses its real scan");
    assert_eq!(access.object().map(ObjectRef::as_str), Some("table:Facts"));
    assert_eq!(access.estimated_rows(), Some(12));
    assert_eq!(access.estimated_bytes(), Some(12_288));
    let join = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Join)
        .expect("source still folds into the input");
    assert!(join.details().get("predicate_pushdown_identity").is_none());
    assert_eq!(join.estimated_rows(), Some(120));
    assert_eq!(join.estimated_bytes(), Some(127_680));
    assert_eq!(join.estimated_work(), Some(112));
}

#[test]
fn selected_pair_identity_changes_when_its_selected_index_changes() {
    let old_selection = explain_query_with_partial_indexes(
        &query(vec![join(
            "table:Facts",
            Some(12),
            Some(12_288),
            Some("expr:active-facts"),
        )]),
        &[index(
            "table:Facts",
            "index:facts-active-a",
            "expr:active-facts",
        )],
    )
    .expect("first exact index selection plans");
    let new_selection = explain_query_with_partial_indexes(
        &query(vec![join(
            "table:Facts",
            Some(12),
            Some(12_288),
            Some("expr:active-facts"),
        )]),
        &[index(
            "table:Facts",
            "index:facts-active-b",
            "expr:active-facts",
        )],
    )
    .expect("second exact index selection plans");
    let identity = |plan: &orna_sys_v1::ExplainedPlan| {
        text_detail(
            plan.nodes()
                .iter()
                .find(|node| node.kind() == PlanNodeKind::IndexLookup)
                .expect("exact partial predicate chooses an index lookup"),
            "predicate_pushdown_identity",
        )
    };

    assert_eq!(
        identity(&old_selection),
        "index-pair:6f541deea5a178bca05a8d113b5c14307058b2a74b3cd6f085617e391b63ab5f"
    );
    assert_eq!(
        identity(&new_selection),
        "index-pair:ebbbe86e5d0b682ebb2ff47f8ae1e2931cb55fdc0bcb979c4403e67752dc0ae4"
    );
    assert_ne!(identity(&old_selection), identity(&new_selection));
}

#[test]
fn unknown_or_invalid_partial_predicates_do_not_gain_a_pair_identity() {
    let invalid_predicate = index("table:Facts", "index:bad", "\n");
    assert_eq!(
        explain_query_with_partial_indexes(
            &query(vec![
                join("table:Facts", Some(1), Some(1), Some("expr:ok"),)
            ]),
            &[invalid_predicate],
        ),
        Err(ExplainError::InvalidExpression)
    );
}
