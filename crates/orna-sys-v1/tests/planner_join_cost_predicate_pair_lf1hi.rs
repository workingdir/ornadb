use std::collections::BTreeMap;

use orna_sys_v1::{
    ExpressionRef, ObjectRef, PlanDetail, PlanNode, PlanNodeKind, QueryJoinDescription,
    QueryJoinPairIdentityDescription, QueryPartialIndexDescription, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef,
    explain_query_with_partial_indexes_and_join_pair_identities,
};

const FIXTURE: &str = include_str!("fixtures/planner_join_cost_predicate_pair_lf1hi.orna");

#[derive(Debug, Eq, PartialEq)]
struct Fold {
    planned_position: u64,
    declared_position: u64,
    pair_identity: String,
    right_identity: String,
    left_identity: String,
    fold_identity: String,
    selected_index: Option<String>,
    predicate_identity: Option<String>,
    paired_predicate_identity: Option<String>,
    rows: Option<u64>,
    bytes: Option<u64>,
    work: Option<u64>,
}

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
    rows: Option<u64>,
    bytes: Option<u64>,
    predicate: &str,
) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics: rows.zip(bytes).map(|(rows, bytes)| statistics(rows, bytes)),
        predicate: Some(expression(predicate)),
    }
}

fn query(order: [&str; 4]) -> QueryPlanDescription {
    let joins = order
        .into_iter()
        .map(|source| match source {
            "table:Expensive" => join(source, Some(500), Some(64_000), "expr:broad-live"),
            "table:Unknown" => join(source, None, None, "expr:unknown-live"),
            "table:Small" => join(source, Some(8), Some(8_192), "expr:small-live"),
            "table:Medium" => join(source, Some(30), Some(4_096), "expr:medium-live"),
            other => panic!("unexpected source {other}"),
        })
        .collect();
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:join-cost-predicate-pair-lf1hi"),
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

fn index(table: &str, identity: &str, predicate: &str) -> QueryPartialIndexDescription {
    QueryPartialIndexDescription {
        table: object(table),
        index: object(identity),
        partial_predicate: expression(predicate),
    }
}

fn pair(identity: &str, source: &str, predicate: &str) -> QueryJoinPairIdentityDescription {
    QueryJoinPairIdentityDescription {
        identity: object(identity),
        left_source: object("table:Anchor"),
        right_source: object(source),
        predicate: Some(expression(predicate)),
    }
}

fn pairs() -> Vec<QueryJoinPairIdentityDescription> {
    vec![
        pair("pair:expensive", "table:Expensive", "expr:broad-live"),
        pair("pair:unknown", "table:Unknown", "expr:unknown-live"),
        pair("pair:small", "table:Small", "expr:small-live"),
        pair("pair:medium", "table:Medium", "expr:medium-live"),
    ]
}

fn explain(
    order: [&str; 4],
    indexes: &[QueryPartialIndexDescription],
    pairs: &[QueryJoinPairIdentityDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_partial_indexes_and_join_pair_identities(&query(order), indexes, pairs)
        .expect("exact pair and index identities resolve through sparse cost planning")
}

fn text(node: &PlanNode, key: &str) -> Option<String> {
    match node.details().get(key) {
        Some(PlanDetail::Text(value)) => Some(value.clone()),
        None => None,
        other => panic!("{key} should be text, got {other:?}"),
    }
}

fn integer(node: &PlanNode, key: &str) -> u64 {
    match node.details().get(key) {
        Some(PlanDetail::Integer(value)) => *value,
        other => panic!("{key} should be integer, got {other:?}"),
    }
}

fn folds(plan: &orna_sys_v1::ExplainedPlan) -> BTreeMap<String, Fold> {
    let nodes = plan
        .nodes()
        .iter()
        .map(|node| (node.reference().as_str().to_owned(), node))
        .collect::<BTreeMap<_, _>>();
    plan.nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| {
            let pair_identity = text(node, "join_pair_identity")
                .expect("every fixture join has a resolver pair identity");
            let access = nodes[node.inputs()[1].as_str()];
            let selected_index = (access.kind() == PlanNodeKind::IndexLookup).then(|| {
                access
                    .object()
                    .expect("index lookup exposes index")
                    .as_str()
                    .to_owned()
            });
            let predicate_identity = text(node, "predicate_pushdown_identity");
            let paired_predicate_identity = text(node, "paired_predicate_pushdown_identity");
            assert_eq!(
                predicate_identity,
                text(access, "predicate_pushdown_identity"),
                "the fold and its right access retain the same index/predicate pair"
            );
            assert_eq!(
                paired_predicate_identity,
                text(access, "paired_predicate_pushdown_identity"),
                "the lookup and join fold retain the same composite identity"
            );
            if paired_predicate_identity.is_some() {
                assert_eq!(
                    text(node, "paired_predicate_pushdown_policy").as_deref(),
                    Some("logical_join_and_exact_table_index_predicate")
                );
            }
            let fold_identity =
                text(node, "join_cost_fold_identity").expect("join has a computed fold identity");
            assert_eq!(
                text(access, "paired_join_cost_fold_identity").as_deref(),
                Some(fold_identity.as_str())
            );
            (
                pair_identity.clone(),
                Fold {
                    planned_position: integer(node, "planned_input_position"),
                    declared_position: integer(node, "declared_input_position"),
                    pair_identity,
                    right_identity: text(node, "join_cost_fold_right_identity")
                        .expect("right input contributes its identity"),
                    left_identity: text(node, "join_cost_fold_left_identity")
                        .expect("left input contributes its identity"),
                    fold_identity,
                    selected_index,
                    predicate_identity,
                    paired_predicate_identity,
                    rows: node.estimated_rows(),
                    bytes: node.estimated_bytes(),
                    work: node.estimated_work(),
                },
            )
        })
        .collect()
}

#[test]
fn paired_predicate_identity_and_join_cost_folds_follow_sparse_reordering() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let descriptors = pairs();
    let first = explain(
        [
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
        ],
        &indexes(),
        &descriptors,
    );
    let reversed = explain(
        [
            "table:Medium",
            "table:Small",
            "table:Unknown",
            "table:Expensive",
        ],
        &indexes().into_iter().rev().collect::<Vec<_>>(),
        &descriptors.iter().rev().cloned().collect::<Vec<_>>(),
    );
    let first_folds = folds(&first);
    let reversed_folds = folds(&reversed);
    let expected = [
        (
            "pair:small",
            Some("index:small-live-a"),
            Some(80),
            Some(85_120),
            Some(108),
        ),
        (
            "pair:medium",
            Some("index:medium-live"),
            Some(240),
            Some(288_240),
            Some(110),
        ),
        (
            "pair:expensive",
            None,
            Some(12_000),
            Some(15_948_000),
            Some(740),
        ),
        ("pair:unknown", None, None, None, None),
    ];
    for (pair_id, index_id, rows, bytes, work) in expected {
        let before = &first_folds[pair_id];
        let after = &reversed_folds[pair_id];
        assert_eq!(before.selected_index.as_deref(), index_id);
        assert_eq!(
            (before.rows, before.bytes, before.work),
            (rows, bytes, work)
        );
        assert_eq!(
            before.predicate_identity.is_some(),
            index_id.is_some(),
            "only exact selected indexes expose a predicate-pushdown identity"
        );
        assert_eq!(
            before.paired_predicate_identity.is_some(),
            index_id.is_some(),
            "only a selected index bound to a logical pair gets a composite identity"
        );
        assert_eq!(before.right_identity, after.right_identity);
        assert_eq!(before.fold_identity, after.fold_identity);
        assert_eq!(
            before.paired_predicate_identity,
            after.paired_predicate_identity
        );
    }

    let mut ordered = first_folds.values().collect::<Vec<_>>();
    ordered.sort_by_key(|fold| fold.planned_position);
    assert_eq!(
        ordered
            .iter()
            .map(|fold| fold.pair_identity.as_str())
            .collect::<Vec<_>>(),
        [
            "pair:small",
            "pair:medium",
            "pair:expensive",
            "pair:unknown"
        ]
    );
    assert!(ordered[0].left_identity.starts_with("join-seed:"));
    for adjacent in ordered.windows(2) {
        assert_eq!(adjacent[1].left_identity, adjacent[0].fold_identity);
    }
    assert_eq!(
        (
            first.root().estimated_rows(),
            first.root().estimated_bytes()
        ),
        (None, None)
    );
}

#[test]
fn changing_the_logical_pair_rekeys_its_paired_predicate_fold_chain() {
    let indexes = indexes();
    let baseline_pairs = pairs();
    let baseline = explain(
        [
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
        ],
        &indexes,
        &baseline_pairs,
    );
    let mut changed_pairs = baseline_pairs;
    let small = changed_pairs
        .iter_mut()
        .find(|pair| pair.identity.as_str() == "pair:small")
        .expect("small logical pair is described");
    small.identity = object("pair:small-rebound");
    let changed = explain(
        [
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
        ],
        &indexes,
        &changed_pairs,
    );
    let baseline_folds = folds(&baseline);
    let changed_folds = folds(&changed);

    for (old_id, new_id) in [
        ("pair:small", "pair:small-rebound"),
        ("pair:medium", "pair:medium"),
        ("pair:expensive", "pair:expensive"),
        ("pair:unknown", "pair:unknown"),
    ] {
        let before = &baseline_folds[old_id];
        let after = &changed_folds[new_id];
        assert_ne!(before.fold_identity, after.fold_identity);
        assert_eq!(before.right_identity, after.right_identity);
        assert_eq!(
            (before.rows, before.bytes, before.work),
            (after.rows, after.bytes, after.work)
        );
    }
    assert_ne!(
        baseline_folds["pair:small"].paired_predicate_identity,
        changed_folds["pair:small-rebound"].paired_predicate_identity
    );
    assert_eq!(
        baseline_folds["pair:small"].predicate_identity,
        changed_folds["pair:small-rebound"].predicate_identity,
        "the selected index tuple is unchanged when only its logical join pair changes"
    );
}

#[test]
fn changing_the_selected_index_rekeys_its_input_and_all_dependent_cost_folds() {
    let baseline_pairs = pairs();
    let baseline = explain(
        [
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
        ],
        &indexes(),
        &baseline_pairs,
    );
    let changed_indexes = indexes()
        .into_iter()
        .filter(|index| index.index.as_str() != "index:small-live-a")
        .collect::<Vec<_>>();
    let changed = explain(
        [
            "table:Expensive",
            "table:Unknown",
            "table:Small",
            "table:Medium",
        ],
        &changed_indexes,
        &baseline_pairs,
    );
    let baseline_folds = folds(&baseline);
    let changed_folds = folds(&changed);

    for pair_id in [
        "pair:small",
        "pair:medium",
        "pair:expensive",
        "pair:unknown",
    ] {
        let before = &baseline_folds[pair_id];
        let after = &changed_folds[pair_id];
        assert_ne!(before.fold_identity, after.fold_identity);
        assert_eq!(
            (before.rows, before.bytes, before.work),
            (after.rows, after.bytes, after.work)
        );
    }
    assert_ne!(
        baseline_folds["pair:small"].right_identity,
        changed_folds["pair:small"].right_identity
    );
    assert_ne!(
        baseline_folds["pair:small"].predicate_identity,
        changed_folds["pair:small"].predicate_identity
    );
    assert_ne!(
        baseline_folds["pair:small"].paired_predicate_identity,
        changed_folds["pair:small"].paired_predicate_identity
    );
    assert_eq!(
        changed_folds["pair:small"].selected_index.as_deref(),
        Some("index:small-live-z")
    );
}
