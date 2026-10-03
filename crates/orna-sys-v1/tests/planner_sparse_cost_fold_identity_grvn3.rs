use std::collections::{BTreeMap, BTreeSet};

use orna_sys_v1::{
    explain_query_with_join_pair_identities, ExpressionRef, ObjectRef, PlanDetail, PlanNode,
    PlanNodeKind, QueryJoinDescription, QueryJoinPairIdentityDescription, QueryPlanDescription,
    QuerySourceStatistics, SnapshotRef,
};

const FIXTURE: &str = include_str!("fixtures/planner_sparse_cost_fold_identity_grvn3.orna");

#[derive(Debug, Eq, PartialEq)]
struct Fold {
    planned_position: u64,
    declared_position: u64,
    pair_identity: String,
    right_source: String,
    fold_identity: String,
    left_identity: String,
    right_identity: String,
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

fn join(source: &str, rows: Option<u64>, bytes: Option<u64>) -> QueryJoinDescription {
    QueryJoinDescription {
        source: object(source),
        statistics: (rows.is_some() || bytes.is_some()).then_some(QuerySourceStatistics {
            estimated_rows: rows,
            estimated_bytes: bytes,
            mutable_branch: None,
        }),
        predicate: Some(expression(&format!(
            "expr:anchor-{}",
            source.trim_start_matches("table:")
        ))),
    }
}

fn pair(identity: &str, source: &str) -> QueryJoinPairIdentityDescription {
    QueryJoinPairIdentityDescription {
        identity: object(identity),
        left_source: object("table:Anchor"),
        right_source: object(source),
        predicate: Some(expression(&format!(
            "expr:anchor-{}",
            source.trim_start_matches("table:")
        ))),
    }
}

fn query(joins: Vec<QueryJoinDescription>) -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:sparse-cost-fold-identity-grvn3"),
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

fn text_detail(node: &PlanNode, key: &str) -> String {
    match node.details().get(key) {
        Some(PlanDetail::Text(value)) => value.clone(),
        other => panic!("{key} should be text, got {other:?}"),
    }
}

fn integer_detail(node: &PlanNode, key: &str) -> u64 {
    match node.details().get(key) {
        Some(PlanDetail::Integer(value)) => *value,
        other => panic!("{key} should be an integer, got {other:?}"),
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
                .expect("join fold keeps its paired right input");
            let fold_identity = text_detail(node, "join_cost_fold_identity");
            assert_eq!(
                text_detail(access, "paired_join_cost_fold_identity"),
                fold_identity,
                "the right input and its computed join fold retain one identity"
            );
            assert_eq!(
                text_detail(node, "join_cost_fold_identity_policy"),
                "snapshot_seed_then_planned_left_fold"
            );
            Fold {
                planned_position: integer_detail(node, "planned_input_position"),
                declared_position: integer_detail(node, "declared_input_position"),
                pair_identity: text_detail(node, "join_pair_identity"),
                right_source: text_detail(node, "logical_right_source_identity"),
                fold_identity,
                left_identity: text_detail(node, "join_cost_fold_left_identity"),
                right_identity: text_detail(node, "join_cost_fold_right_identity"),
                rows: node.estimated_rows(),
                bytes: node.estimated_bytes(),
                work: node.estimated_work(),
            }
        })
        .collect::<Vec<_>>();
    folds.sort_by_key(|fold| fold.planned_position);
    folds
}

fn pairs() -> Vec<QueryJoinPairIdentityDescription> {
    vec![
        pair("pair:large", "table:Large"),
        pair("pair:unknown", "table:Unknown"),
        pair("pair:small", "table:Small"),
        pair("pair:middle", "table:Middle"),
    ]
}

fn cascade(small_rows: u64, small_bytes: u64, order: [&str; 4]) -> Vec<QueryJoinDescription> {
    order
        .into_iter()
        .map(|source| match source {
            "table:Large" => join(source, Some(300), Some(16_384)),
            "table:Unknown" => join(source, None, None),
            "table:Small" => join(source, Some(small_rows), Some(small_bytes)),
            "table:Middle" => join(source, Some(20), Some(8_192)),
            other => panic!("unexpected cascade source {other}"),
        })
        .collect()
}

fn plan(
    joins: Vec<QueryJoinDescription>,
    descriptors: &[QueryJoinPairIdentityDescription],
) -> orna_sys_v1::ExplainedPlan {
    explain_query_with_join_pair_identities(&query(joins), descriptors)
        .expect("described pair identities resolve in a sparse cost cascade")
}

#[test]
fn cost_fold_identity_follows_pair_results_across_sparse_reordered_chains() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let descriptors = pairs();
    let first = plan(
        cascade(
            5,
            4_096,
            [
                "table:Large",
                "table:Unknown",
                "table:Small",
                "table:Middle",
            ],
        ),
        &descriptors,
    );
    let reversed_descriptors = descriptors.iter().rev().cloned().collect::<Vec<_>>();
    let second = plan(
        cascade(
            5,
            4_096,
            [
                "table:Middle",
                "table:Small",
                "table:Unknown",
                "table:Large",
            ],
        ),
        &reversed_descriptors,
    );

    let expected_results = vec![
        (
            "pair:small",
            "table:Small",
            Some(50),
            Some(43_000),
            Some(105),
        ),
        (
            "pair:middle",
            "table:Middle",
            Some(100),
            Some(127_000),
            Some(70),
        ),
        (
            "pair:large",
            "table:Large",
            Some(3_000),
            Some(3_975_000),
            Some(400),
        ),
        ("pair:unknown", "table:Unknown", None, None, None),
    ];
    for observed in [folds(&first), folds(&second)] {
        assert_eq!(observed.len(), expected_results.len());
        for (fold, (pair_id, source, rows, bytes, work)) in
            observed.iter().zip(expected_results.iter())
        {
            assert_eq!(fold.pair_identity, *pair_id);
            assert_eq!(fold.right_source, *source);
            assert_eq!((fold.rows, fold.bytes, fold.work), (*rows, *bytes, *work));
            assert!(fold.fold_identity.starts_with("join-fold:"));
            assert!(fold.right_identity.starts_with("join-input:"));
        }
        let seed = observed
            .first()
            .expect("nonempty cascade")
            .left_identity
            .clone();
        assert!(seed.starts_with("join-seed:"));
        for pair in observed.windows(2) {
            assert_eq!(pair[1].left_identity, pair[0].fold_identity);
        }
    }

    let by_pair = |plan: &orna_sys_v1::ExplainedPlan| {
        folds(plan)
            .into_iter()
            .map(|fold| (fold.pair_identity, fold.fold_identity))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(by_pair(&first), by_pair(&second));
    assert_eq!(first.plan().estimated_cost(), None);
    assert_eq!(second.plan().estimated_cost(), None);
}

#[test]
fn earlier_cost_change_reidentifies_downstream_fold_chain_and_preserves_real_estimates() {
    let descriptors = pairs();
    let baseline = plan(
        cascade(
            5,
            4_096,
            [
                "table:Large",
                "table:Unknown",
                "table:Small",
                "table:Middle",
            ],
        ),
        &descriptors,
    );
    let changed = plan(
        cascade(
            6,
            4_097,
            [
                "table:Large",
                "table:Unknown",
                "table:Small",
                "table:Middle",
            ],
        ),
        &descriptors,
    );
    let baseline = folds(&baseline);
    let changed = folds(&changed);

    assert_eq!(
        (changed[0].rows, changed[0].bytes, changed[0].work),
        (Some(60), Some(43_380), Some(106))
    );
    assert_eq!(
        (changed[1].rows, changed[1].bytes, changed[1].work),
        (Some(120), Some(135_960), Some(80))
    );
    assert_eq!(
        (changed[2].rows, changed[2].bytes, changed[2].work),
        (Some(3_600), Some(4_276_800), Some(420))
    );
    assert!(changed
        .iter()
        .zip(baseline.iter())
        .all(
            |(changed, baseline)| changed.pair_identity == baseline.pair_identity
                && changed.fold_identity != baseline.fold_identity
        ));
    assert_eq!(
        changed[3].rows, None,
        "unknown tail retains unknown cardinality"
    );
    assert_eq!(changed[3].bytes, None);
    assert_eq!(changed[3].work, None);
}

#[test]
fn each_cost_fold_records_its_left_and_right_inputs() {
    let plan = plan(
        cascade(
            5,
            4_096,
            [
                "table:Large",
                "table:Unknown",
                "table:Small",
                "table:Middle",
            ],
        ),
        &pairs(),
    );
    let observed = folds(&plan);
    let root = plan
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node
                    .object()
                    .is_some_and(|object| object.as_str() == "table:Anchor")
        })
        .expect("root source scan seeds the cost fold");
    assert_eq!(
        text_detail(root, "join_cost_fold_identity"),
        observed[0].left_identity
    );
    assert_eq!(
        observed
            .iter()
            .map(|fold| fold.planned_position)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([1, 2, 3, 4])
    );
    assert_eq!(
        observed
            .iter()
            .map(|fold| fold.right_identity.clone())
            .collect::<BTreeSet<_>>()
            .len(),
        4,
        "each right input contributes its own tuple and pinned estimates"
    );
}

#[test]
fn changing_a_pair_label_changes_its_fold_and_downstream_cascade_identities() {
    let descriptors = pairs();
    let baseline = plan(
        cascade(
            5,
            4_096,
            [
                "table:Large",
                "table:Unknown",
                "table:Small",
                "table:Middle",
            ],
        ),
        &descriptors,
    );
    let mut changed_descriptors = descriptors.clone();
    let small = changed_descriptors
        .iter_mut()
        .find(|pair| pair.identity.as_str() == "pair:small")
        .expect("small logical pair is described");
    small.identity = object("pair:small-rebound");
    let changed = plan(
        cascade(
            5,
            4_096,
            [
                "table:Large",
                "table:Unknown",
                "table:Small",
                "table:Middle",
            ],
        ),
        &changed_descriptors,
    );
    let baseline = folds(&baseline);
    let changed = folds(&changed);

    assert_eq!(
        changed
            .iter()
            .map(|fold| (fold.rows, fold.bytes, fold.work))
            .collect::<Vec<_>>(),
        baseline
            .iter()
            .map(|fold| (fold.rows, fold.bytes, fold.work))
            .collect::<Vec<_>>(),
        "identity-only changes do not alter computed join estimates"
    );
    assert_eq!(changed[0].pair_identity, "pair:small-rebound");
    assert_ne!(changed[0].fold_identity, baseline[0].fold_identity);
    assert_eq!(changed[1].pair_identity, baseline[1].pair_identity);
    assert_ne!(changed[1].fold_identity, baseline[1].fold_identity);
    assert_eq!(changed[2].pair_identity, baseline[2].pair_identity);
    assert_ne!(changed[2].fold_identity, baseline[2].fold_identity);
    assert_eq!(changed[3].pair_identity, baseline[3].pair_identity);
    assert_ne!(changed[3].fold_identity, baseline[3].fold_identity);
}
