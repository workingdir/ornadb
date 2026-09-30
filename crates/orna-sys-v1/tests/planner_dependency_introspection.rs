use std::collections::BTreeSet;

use orna_sys_v1::{
    DependencyConfidence, DependencyGraph, DependencyGraphError, DependencyInput, DependencyKind,
    ExplainError,
    FileRef, FunctionPlanDescription, FunctionRef, ObjectRef, PlanDetail, PlanNodeKind, PlanOrdering,
    PlanSortDirection, PlanNullOrder, QueryJoinDescription, QueryPlanDescription,
    QuerySourceStatistics, QueryMutationDescription, QueryMutationKind,
    MutableBranchSnapshot, SnapshotRef, MAX_PLAN_EXPRESSIONS,
    SystemEffect, explain_function, explain_query, system_function_descriptor, SourceSpan,
};
use serde_json::Value;

const DEPENDENCY_DIAMOND: &str = include_str!("fixtures/dependency_diamond.orna");
const MUTABLE_BRANCH_QUERY: &str = include_str!("fixtures/mutable_branch_query.orna");
const ROUNDING_TAIL_INTERPLAY: &str = include_str!("fixtures/rounding_tail_interplay.orna");
const WRITE_MATERIALIZATION_EDGE_TAIL: &str =
    include_str!("fixtures/write_materialization_edge_tail.orna");
const WRITE_MATERIALIZATION_ROUNDING_TAIL: &str =
    include_str!("fixtures/write_materialization_rounding_tail.orna");
const MAX_SOURCE_MATERIALIZED_WRITE_TAIL: &str =
    include_str!("fixtures/max_source_materialized_write_tail.orna");
const MAX_SOURCE_MATERIALIZATION_CLOSURE_TAIL: &str =
    include_str!("fixtures/max_source_materialization_closure_tail.orna");
const MAX_SOURCE_MATERIALIZATION_OVERFLOW_TAIL: &str =
    include_str!("fixtures/max_source_materialization_overflow_tail.orna");

fn obj(name: &str) -> ObjectRef {
    ObjectRef::descriptive(name)
}

fn graph() -> DependencyGraph {
    let a = obj("module:a");
    let b = obj("function:b");
    let c = obj("table:c");
    let d = obj("function:d");
    DependencyGraph::new(
        SnapshotRef::descriptive("snapshot:one"),
        [a.clone(), b.clone(), c.clone(), d.clone()],
        [
            DependencyInput::new(
                a.clone(),
                b.clone(),
                DependencyKind::Import,
                None,
                None,
                DependencyConfidence::Exact,
                false,
            ),
            DependencyInput::new(
                b.clone(),
                c.clone(),
                DependencyKind::TableRead,
                None,
                None,
                DependencyConfidence::Exact,
                false,
            ),
            DependencyInput::new(
                c.clone(),
                a.clone(),
                DependencyKind::Call,
                None,
                None,
                DependencyConfidence::Possible,
                true,
            ),
            DependencyInput::new(
                d.clone(),
                b.clone(),
                DependencyKind::Call,
                None,
                None,
                DependencyConfidence::Exact,
                false,
            ),
            DependencyInput::new(
                c.clone(),
                d.clone(),
                DependencyKind::TableWrite,
                None,
                None,
                DependencyConfidence::Conservative,
                false,
            ),
        ],
    )
    .expect("valid graph")
}

#[test]
fn planner_and_dependency_functions_are_registered_as_read_only_sys_operations() {
    for name in [
        "sys.explain(Query)",
        "sys.explain(FunctionRef)",
        "sys.dependencies",
        "sys.dependents",
    ] {
        assert_eq!(
            system_function_descriptor(name)
                .expect("registered system operation")
                .effect,
            SystemEffect::Read,
            "{name} must remain read-only"
        );
    }
}

#[test]
fn dependency_traversal_is_typed_cycle_safe_and_breadth_first() {
    let graph = graph();
    let a = obj("module:a");
    let direct = graph.dependencies(&a, false, None).expect("direct edges");
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0].from().as_str(), "module:a");
    assert_eq!(direct[0].to().as_str(), "function:b");
    assert!(direct[0].reference().as_str().starts_with("dependency:"));
    assert_eq!(direct[0].kind(), DependencyKind::Import);
    assert_eq!(direct[0].confidence(), DependencyConfidence::Exact);
    assert!(!direct[0].is_conditional());

    let transitive = graph.dependencies(&a, true, None).expect("transitive edges");
    assert_eq!(
        transitive
            .iter()
            .map(|edge| (edge.from().as_str(), edge.to().as_str()))
            .collect::<Vec<_>>(),
        [
            ("module:a", "function:b"),
            ("function:b", "table:c"),
            ("table:c", "function:d"),
            ("table:c", "module:a"),
            ("function:d", "function:b"),
        ]
    );
    assert_eq!(
        transitive
            .iter()
            .map(|edge| edge.reference().as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        transitive.len(),
        "natural dependency keys produce unique typed references"
    );
}

#[test]
fn dependency_filters_apply_to_traversal_and_dependents_keep_edge_direction() {
    let graph = graph();
    let a = obj("module:a");
    let only_calls = BTreeSet::from([DependencyKind::Call]);
    assert!(graph
        .dependencies(&a, true, Some(&only_calls))
        .expect("filtered outgoing edges")
        .is_empty());

    let b = obj("function:b");
    let incoming = graph
        .dependents(&b, true, None)
        .expect("incoming transitive edges");
    assert_eq!(incoming[0].from().as_str(), "function:d");
    assert_eq!(incoming[0].to().as_str(), "function:b");
    assert_eq!(incoming[0].kind(), DependencyKind::Call);
    assert!(incoming.iter().any(|edge| {
        edge.from().as_str() == "function:d" && edge.to().as_str() == "function:b"
    }));
}

#[test]
fn dependency_fixture_proves_deterministic_diamond_and_cycle_traversal() {
    let parsed = orna_syntax_v1::parse_module(DEPENDENCY_DIAMOND);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 3);
    let dashboard = obj("function:dashboard");
    let render = obj("function:render_contacts");
    let active = obj("function:active_contacts");
    let graph = DependencyGraph::new(
        SnapshotRef::descriptive("snapshot:fixture"),
        [dashboard.clone(), render.clone(), active.clone()],
        [
            DependencyInput::new(
                dashboard.clone(),
                render.clone(),
                DependencyKind::Call,
                None,
                None,
                DependencyConfidence::Exact,
                false,
            ),
            DependencyInput::new(
                dashboard.clone(),
                active.clone(),
                DependencyKind::Call,
                None,
                None,
                DependencyConfidence::Exact,
                false,
            ),
            DependencyInput::new(
                render.clone(),
                active.clone(),
                DependencyKind::Call,
                None,
                None,
                DependencyConfidence::Exact,
                false,
            ),
            // Cycles are legal catalogue facts; visited-object tracking keeps
            // the transitive query finite and preserves this return edge.
            DependencyInput::new(
                active.clone(),
                dashboard.clone(),
                DependencyKind::Call,
                None,
                None,
                DependencyConfidence::Conservative,
                true,
            ),
        ],
    )
    .expect("fixture dependency graph");

    let traversed = graph
        .dependencies(&dashboard, true, None)
        .expect("cycle-safe traversal");
    assert_eq!(
        traversed
            .iter()
            .map(|edge| (edge.from().as_str(), edge.to().as_str()))
            .collect::<Vec<_>>(),
        [
            ("function:dashboard", "function:active_contacts"),
            ("function:dashboard", "function:render_contacts"),
            ("function:active_contacts", "function:dashboard"),
            ("function:render_contacts", "function:active_contacts"),
        ]
    );
    assert_eq!(traversed.len(), 4);

    let reverse = graph
        .dependents(&active, true, None)
        .expect("transitive reverse traversal");
    assert!(reverse.iter().any(|edge| {
        edge.from() == &dashboard && edge.to() == &active
    }));
    assert!(reverse.iter().all(|edge| {
        edge.kind() == DependencyKind::Call
    }));
}

#[test]
fn dependency_graph_rejects_unknown_endpoints_and_query_roots() {
    let a = obj("module:a");
    let missing = obj("missing:object");
    let edge = DependencyInput::new(
        a.clone(),
        missing.clone(),
        DependencyKind::Call,
        None,
        None,
        DependencyConfidence::Exact,
        false,
    );
    assert_eq!(
        DependencyGraph::new(SnapshotRef::descriptive("snapshot"), [a.clone()], [edge]),
        Err(DependencyGraphError::UnknownEndpoint)
    );
    assert_eq!(
        graph().dependencies(&missing, false, None),
        Err(DependencyGraphError::UnknownObject)
    );

    let invalid_span = SourceSpan {
        file: FileRef::descriptive("module.orna"),
        start_byte: 8,
        end_byte: 3,
        start_line: 1,
        start_column: 4,
        end_line: 1,
        end_column: 8,
    };
    assert_eq!(
        DependencyGraph::new(
            SnapshotRef::descriptive("snapshot"),
            [a.clone(), missing.clone()],
            [DependencyInput::new(
                a,
                missing,
                DependencyKind::Call,
                None,
                Some(invalid_span),
                DependencyConfidence::Exact,
                false,
            )],
        ),
        Err(DependencyGraphError::InvalidSpan)
    );
}

#[test]
fn query_explain_returns_a_snapshot_pinned_scan_fallback_without_fabricated_stats() {
    let query = QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:one"),
        source: obj("table:contacts"),
        source_statistics: None,
        joins: Vec::new(),
        predicate: Some(orna_sys_v1::ExpressionRef::descriptive("expr:active")),
        projections: vec![orna_sys_v1::ExpressionRef::descriptive("expr:name")],
        distinct: true,
        ordering: vec![PlanOrdering {
            expression: orna_sys_v1::ExpressionRef::descriptive("expr:name"),
            direction: PlanSortDirection::Ascending,
            null_order: PlanNullOrder::Last,
        }],
        limit: Some(10),
        mutations: Vec::new(),
        materialize_into: None,
    };
    let explained = explain_query(&query).expect("structured query plan");
    assert_eq!(
        explained
            .nodes()
            .iter()
            .map(|node| node.kind())
            .collect::<Vec<_>>(),
        [
            PlanNodeKind::Limit,
            PlanNodeKind::Sort,
            PlanNodeKind::Aggregate,
            PlanNodeKind::Project,
            PlanNodeKind::Filter,
            PlanNodeKind::Scan,
        ]
    );
    assert_eq!(explained.root().reference(), explained.plan().root());
    assert_eq!(explained.nodes()[0].inputs(), &[explained.nodes()[1].reference().clone()]);
    assert_eq!(explained.nodes().last().unwrap().object(), Some(&obj("table:contacts")));
    assert!(explained
        .nodes()
        .iter()
        .all(|node| node.actual_rows().is_none()));
    assert!(!explained.plan().actual_available());
    assert_eq!(explained.plan().estimated_cost(), None);

    let same = explain_query(&query).expect("deterministic explain");
    assert_eq!(explained, same);
    let other_snapshot = explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:two"),
        ..query
    })
    .expect("same query at another snapshot");
    assert_ne!(explained.plan().id(), other_snapshot.plan().id());

    let rendered = serde_json::to_value(explained.nodes()).expect("safe JSON projection");
    let rendered = rendered.as_array().unwrap();
    assert!(rendered.iter().all(|node| node.get("actual_rows").is_none()));
    assert!(rendered.iter().all(|node| node.get("actual_bytes").is_none()));
    assert!(rendered.iter().all(|node| node.get("estimated_rows").is_none()));
    let rendered_plan = serde_json::to_value(explained.plan()).expect("portable plan row");
    assert!(rendered_plan.get("estimated_cost").is_none());
    assert_eq!(rendered_plan["actual_available"], Value::Bool(false));
}

#[test]
fn explain_builds_a_deep_join_materialization_plan_from_mutable_branch_stats() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 8);
    let query = QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:workspace-generation-7"),
        source: obj("table:Contact"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: Some(1_000),
            estimated_bytes: Some(64_000),
            mutable_branch: Some(MutableBranchSnapshot {
                name: "branch:working".to_owned(),
                generation: 7,
            }),
        }),
        joins: vec![QueryJoinDescription {
            source: obj("table:ContactTag"),
            statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(100),
                estimated_bytes: Some(8_000),
                mutable_branch: Some(MutableBranchSnapshot {
                    name: "branch:working".to_owned(),
                    generation: 7,
                }),
            }),
            predicate: Some(orna_sys_v1::ExpressionRef::descriptive(
                "expr:contact.id=tag.contact_id",
            )),
        }, QueryJoinDescription {
            source: obj("table:ContactBadge"),
            statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(40),
                estimated_bytes: Some(1_600),
                mutable_branch: Some(MutableBranchSnapshot {
                    name: "branch:working".to_owned(),
                    generation: 7,
                }),
            }),
            predicate: Some(orna_sys_v1::ExpressionRef::descriptive(
                "expr:contact.id=badge.contact_id",
            )),
        }],
        predicate: Some(orna_sys_v1::ExpressionRef::descriptive("expr:contact.active")),
        projections: vec![orna_sys_v1::ExpressionRef::descriptive("expr:contact.name")],
        distinct: true,
        ordering: vec![PlanOrdering {
            expression: orna_sys_v1::ExpressionRef::descriptive("expr:contact.name"),
            direction: PlanSortDirection::Ascending,
            null_order: PlanNullOrder::Last,
        }],
        limit: Some(30),
        mutations: vec![
            QueryMutationDescription {
                table: obj("table:Contact"),
                kind: QueryMutationKind::Insert,
                estimated_affected_rows: Some(5),
                estimated_write_bytes: Some(240),
                estimated_table_rows_before: None,
            },
            QueryMutationDescription {
                table: obj("table:ContactTag"),
                kind: QueryMutationKind::Update,
                estimated_affected_rows: Some(2),
                estimated_write_bytes: Some(96),
                estimated_table_rows_before: None,
            },
            QueryMutationDescription {
                table: obj("table:ContactTag"),
                kind: QueryMutationKind::Rekey,
                estimated_affected_rows: Some(1),
                estimated_write_bytes: Some(80),
                estimated_table_rows_before: None,
            },
            QueryMutationDescription {
                table: obj("table:Contact"),
                kind: QueryMutationKind::Delete,
                estimated_affected_rows: Some(2),
                estimated_write_bytes: Some(64),
                estimated_table_rows_before: None,
            },
        ],
        materialize_into: Some(obj("materialization:active_contact_names")),
    };
    let explained = explain_query(&query).expect("deep query plan");
    assert_eq!(explained.root().kind(), PlanNodeKind::Materialize);
    assert_eq!(explained.root().object(), Some(&obj("materialization:active_contact_names")));
    assert_eq!(explained.root().estimated_work(), Some(3));
    assert_eq!(
        explained.root().details().get("estimated_work"),
        Some(&PlanDetail::Integer(3))
    );
    assert!(explained.nodes().iter().any(|node| node.kind() == PlanNodeKind::Join));
    let join = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Join)
        .expect("join operator");
    assert_eq!(join.inputs().len(), 2);
    assert_eq!(join.estimated_rows(), Some(40_000));
    let join_estimates = explained
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .map(|node| node.estimated_rows())
        .collect::<Vec<_>>();
    assert_eq!(join_estimates, [Some(40_000), Some(10_000)]);
    assert_eq!(
        explained
            .nodes()
            .iter()
            .find(|node| node.kind() == PlanNodeKind::Filter)
            .unwrap()
            .estimated_rows(),
        Some(20_000)
    );
    assert_eq!(
        explained
            .nodes()
            .iter()
            .find(|node| node.kind() == PlanNodeKind::Aggregate)
            .unwrap()
            .estimated_rows(),
        Some(10_000)
    );
    assert_eq!(
        explained
            .nodes()
            .iter()
            .find(|node| node.kind() == PlanNodeKind::Limit)
            .unwrap()
            .estimated_rows(),
        Some(30)
    );
    let contact_mutations = explained
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Invoke
                && node.object() == Some(&obj("table:Contact"))
        })
        .collect::<Vec<_>>();
    assert_eq!(contact_mutations.len(), 2);
    assert_eq!(contact_mutations[0].estimated_rows(), Some(2));
    assert_eq!(contact_mutations[1].estimated_rows(), Some(5));
    let tag_mutations = explained
        .nodes()
        .iter()
        .filter(|node| {
            node.kind() == PlanNodeKind::Invoke
                && node.object() == Some(&obj("table:ContactTag"))
        })
        .collect::<Vec<_>>();
    assert_eq!(tag_mutations.len(), 2);
    assert_eq!(tag_mutations[0].estimated_rows(), Some(1));
    assert_eq!(tag_mutations[1].estimated_rows(), Some(2));
    assert_eq!(explained.nodes()[0].inputs(), &[explained.nodes()[1].reference().clone()]);
    for position in 1..4 {
        assert_eq!(
            explained.nodes()[position].inputs(),
            &[explained.nodes()[position + 1].reference().clone()]
        );
        assert_eq!(
            explained.nodes()[position + 1].parent(),
            Some(explained.nodes()[position].reference())
        );
    }
    assert_eq!(explained.nodes()[5].kind(), PlanNodeKind::Limit);
    assert!(explained.nodes().iter().any(|node| {
        node.kind() == PlanNodeKind::Scan
            && node.object() == Some(&obj("table:Contact"))
            && node.estimated_rows() == Some(1_000)
    }));
    assert!(explained.nodes().iter().any(|node| {
        node.kind() == PlanNodeKind::Scan
            && node.object() == Some(&obj("table:ContactTag"))
            && node.estimated_rows() == Some(100)
    }));
    assert!(explained.plan().estimated_cost().is_some());
    assert!(explained
        .nodes()
        .iter()
        .all(|node| node.actual_rows().is_none()));
    assert!(explained
        .nodes()
        .iter()
        .all(|node| node.estimated_bytes().is_none() || node.estimated_rows().is_some()));
    assert!(!explained.plan().actual_available());

    let rendered = serde_json::to_value(explained.nodes()).expect("structured plan nodes");
    let contact_scan = rendered
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["kind"] == "scan" && node["object"] == "table:Contact")
        .expect("serialized contact scan");
    assert_eq!(contact_scan["details"]["mutable_branch"], "branch:working");
    assert_eq!(contact_scan["details"]["branch_generation"], 7);
    assert_eq!(contact_scan["details"]["statistics_scope"], "overlay_inclusive");
    let delete_node = rendered
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["details"]["mutation"] == "delete")
        .expect("mutation tail");
    assert_eq!(delete_node["details"]["table_rows_before"], 1_005);
    assert_eq!(delete_node["details"]["table_rows_after"], 1_003);
    assert_eq!(delete_node["details"]["estimated_work"], 3);
    assert_eq!(rendered.as_array().unwrap()[0]["details"]["estimated_work"], 3);
    for kind in ["update", "rekey"] {
        let node = rendered
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["details"]["mutation"] == kind)
            .expect("cardinality-preserving mutation tail");
        assert_eq!(node["details"]["table_rows_before"], 100);
        assert_eq!(node["details"]["table_rows_after"], 100);
    }
    let total_work = rendered
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|node| node["details"]["estimated_work"].as_u64())
        .sum::<u64>();
    let expected_cost = total_work.to_string();
    assert_eq!(explained.plan().estimated_cost(), Some(expected_cost.as_str()));
    let sys_surface = serde_json::to_value(&explained).expect("complete sys.explain surface");
    assert_eq!(sys_surface["plan"]["estimated_cost"], expected_cost);
    assert_eq!(
        sys_surface["plan"]["root"],
        sys_surface["nodes"][0]["reference"]
    );
    assert_eq!(
        sys_surface["nodes"][0]["details"]["estimated_work"],
        3
    );

    let mut next_generation = query.clone();
    next_generation.source_statistics.as_mut().unwrap().mutable_branch.as_mut().unwrap().generation = 8;
    next_generation.source_statistics.as_mut().unwrap().estimated_rows = Some(1_050);
    let changed = explain_query(&next_generation).expect("new mutable branch state");
    assert_ne!(explained.plan().id(), changed.plan().id());
    assert_eq!(
        changed
            .nodes()
            .iter()
            .find(|node| node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj("table:Contact")))
            .unwrap()
            .estimated_rows(),
        Some(1_050)
    );

    let fallback = explain_query(&QueryPlanDescription {
        materialize_into: None,
        ..query
    })
    .expect("reference scan/query fallback");
    assert_ne!(fallback.root().kind(), PlanNodeKind::Materialize);
    assert!(fallback.nodes().iter().any(|node| node.kind() == PlanNodeKind::Scan));
}

#[test]
fn explain_estimate_reporting_distinguishes_unknown_zero_and_partial_cost() {
    let zero = explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:zero-estimates"),
        source: obj("table:empty"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: Some(0),
            estimated_bytes: Some(0),
            mutable_branch: None,
        }),
        joins: Vec::new(),
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: Some(0),
        mutations: Vec::new(),
        materialize_into: Some(obj("materialization:empty")),
    })
    .expect("known empty plan");
    assert_eq!(zero.plan().estimated_cost(), Some("0"));
    assert!(zero
        .nodes()
        .iter()
        .all(|node| node.estimated_work() == Some(0)));

    let partial = explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:partial-estimates"),
        source: obj("table:known"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: Some(2),
            estimated_bytes: Some(32),
            mutable_branch: None,
        }),
        joins: vec![QueryJoinDescription {
            source: obj("table:unknown"),
            statistics: None,
            predicate: Some(orna_sys_v1::ExpressionRef::descriptive("expr:join")),
        }],
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    })
    .expect("partially estimated plan");
    assert_eq!(partial.plan().estimated_cost(), None);
    assert_eq!(
        partial.root().details().get("estimated_cost_overflow"),
        None
    );
    let known_scan = partial
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj("table:known")))
        .expect("known input estimate");
    assert_eq!(known_scan.estimated_work(), Some(3));
    let unknown_scan = partial
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj("table:unknown")))
        .expect("unknown input estimate");
    assert_eq!(unknown_scan.estimated_work(), None);
    assert!(partial
        .nodes()
        .iter()
        .all(|node| node.actual_rows().is_none() && node.actual_bytes().is_none()));
}

#[test]
fn explain_total_work_overflow_keeps_known_operator_estimates_in_the_tail() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let half = u64::MAX / 2;
    let explained = explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:work-total-overflow"),
        source: obj("table:left"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: Some(half),
            estimated_bytes: Some(0),
            mutable_branch: None,
        }),
        joins: vec![QueryJoinDescription {
            source: obj("table:right"),
            statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(half),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            predicate: None,
        }],
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: vec![QueryMutationDescription {
            table: obj("table:left"),
            kind: QueryMutationKind::Update,
            estimated_affected_rows: Some(1),
            estimated_write_bytes: Some(0),
            estimated_table_rows_before: Some(17),
        }],
        materialize_into: Some(obj("materialization:overflow-proof")),
    })
    .expect("bounded plan with overflowing aggregate work");

    assert_eq!(explained.root().kind(), PlanNodeKind::Materialize);
    assert_eq!(explained.root().estimated_work(), Some(1));
    assert_eq!(explained.plan().estimated_cost(), None);
    assert_eq!(
        explained.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    let join = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Join)
        .expect("join estimate");
    assert_eq!(join.estimated_work(), Some(u64::MAX - 1));
    for (table, work) in [("table:left", half), ("table:right", half)] {
        let scan = explained
            .nodes()
            .iter()
            .find(|node| {
                node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj(table))
            })
            .expect("scan estimate");
        assert_eq!(scan.estimated_work(), Some(work));
    }

    let surface = serde_json::to_value(&explained).expect("structured overflow plan");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_work"], 1);
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    let mutation = surface["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["details"]["mutation"] == "update")
        .expect("mutation tail");
    assert_eq!(mutation["details"]["estimated_work"], 1);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_cost_boundary_includes_final_mutation_and_materialization_work() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_after_update = |affected_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:tail-cost-boundary"),
            source: obj("table:large"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![QueryMutationDescription {
                table: obj("table:large"),
                kind: QueryMutationKind::Update,
                estimated_affected_rows: Some(affected_rows),
                estimated_write_bytes: Some(0),
                estimated_table_rows_before: Some(1),
            }],
            materialize_into: Some(obj("materialization:tail-boundary")),
        })
        .expect("estimate boundary plan")
    };

    let exact = explain_after_update(0);
    let max_cost = u64::MAX.to_string();
    assert_eq!(exact.plan().estimated_cost(), Some(max_cost.as_str()));
    assert_eq!(exact.root().kind(), PlanNodeKind::Materialize);
    assert_eq!(exact.root().estimated_work(), Some(0));
    assert_eq!(
        exact.root().details().get("estimated_cost_overflow"),
        None
    );
    let exact_mutation = exact
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Invoke)
        .expect("update tail");
    assert_eq!(exact_mutation.estimated_work(), Some(0));
    assert!(exact.nodes().iter().any(|node| {
        node.kind() == PlanNodeKind::Scan
            && node.object() == Some(&obj("table:large"))
            && node.estimated_work() == Some(u64::MAX)
    }));

    let overflow = explain_after_update(1);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(overflow.root().estimated_work(), Some(1));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    assert!(overflow.nodes().iter().all(|node| {
        node.details().get("estimated_work_overflow").is_none()
    }));
    let overflow_surface = serde_json::to_value(&overflow).expect("cost overflow tail surface");
    assert_eq!(overflow_surface["nodes"][0]["details"]["estimated_work"], 1);
    assert_eq!(
        overflow_surface["nodes"][0]["details"]["estimated_cost_overflow"],
        true
    );
    assert!(overflow_surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_known_cost_overflow_survives_an_unknown_final_tail() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_after_update = |affected_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:overflow-with-unknown-tail"),
            source: obj("table:large"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: Some(affected_rows),
                    estimated_write_bytes: Some(0),
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Insert,
                    estimated_affected_rows: None,
                    estimated_write_bytes: None,
                    estimated_table_rows_before: None,
                },
            ],
            materialize_into: Some(obj("materialization:unknown-tail")),
        })
        .expect("plan with an incomplete final tail")
    };

    let exact_known_subtotal = explain_after_update(0);
    assert_eq!(exact_known_subtotal.plan().estimated_cost(), None);
    assert_eq!(
        exact_known_subtotal
            .root()
            .details()
            .get("estimated_cost_overflow"),
        None
    );
    assert!(exact_known_subtotal.nodes().iter().any(|node| {
        node.kind() == PlanNodeKind::Scan && node.estimated_work() == Some(u64::MAX)
    }));

    let explained = explain_after_update(1);

    assert_eq!(explained.root().kind(), PlanNodeKind::Materialize);
    assert_eq!(explained.plan().estimated_cost(), None);
    assert_eq!(explained.root().estimated_work(), None);
    assert_eq!(
        explained.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    let unknown_insert = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Invoke && node.estimated_rows().is_none())
        .expect("unknown mutation estimate tail");
    assert_eq!(unknown_insert.estimated_work(), None);
    assert_eq!(unknown_insert.details().get("estimated_work_overflow"), None);
    let surface = serde_json::to_value(&explained).expect("unknown overflow tail surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"][0]["details"].get("estimated_work").is_none());
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_partial_unknown_mutation_tail_retains_cost_boundary() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_with_insert_tail = |affected_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:partial-unknown-mutation-tail"),
            source: obj("table:large"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: Some(0),
                    estimated_write_bytes: Some(0),
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Insert,
                    estimated_affected_rows: Some(affected_rows),
                    estimated_write_bytes: None,
                    estimated_table_rows_before: Some(1),
                },
            ],
            materialize_into: Some(obj("materialization:partial-unknown-tail")),
        })
        .expect("partially known mutation estimate at the final plan tail")
    };

    let exact = explain_with_insert_tail(0);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(
        exact.root().details().get("estimated_cost_overflow"),
        None,
        "known rows contribute zero, leaving the exact scan subtotal at u64::MAX"
    );
    let exact_insert = exact
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("insert".to_owned())))
        .expect("partial final insert");
    assert_eq!(exact_insert.estimated_rows(), Some(0));
    assert_eq!(exact_insert.estimated_bytes(), None);
    assert_eq!(exact_insert.estimated_work(), None);

    let overflow = explain_with_insert_tail(1);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(overflow.root().estimated_work(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "one known affected row pushes the MAX scan subtotal over the boundary"
    );
    let overflow_insert = overflow
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("insert".to_owned())))
        .expect("partial final insert");
    assert_eq!(overflow_insert.estimated_rows(), Some(1));
    assert_eq!(overflow_insert.estimated_bytes(), None);
    assert_eq!(overflow_insert.estimated_work(), None);
    let surface = serde_json::to_value(&overflow).expect("partial-tail overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_partial_unknown_write_tail_retains_cost_boundary() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_with_partial_insert = |write_bytes| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:partial-unknown-write-tail"),
            source: obj("table:large"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: Some(0),
                    estimated_write_bytes: Some(0),
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Insert,
                    estimated_affected_rows: None,
                    estimated_write_bytes: Some(write_bytes),
                    estimated_table_rows_before: Some(1),
                },
            ],
            materialize_into: Some(obj("materialization:partial-unknown-write")),
        })
        .expect("partially known write-byte estimate at the final plan tail")
    };

    let exact = explain_with_partial_insert(0);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(
        exact.root().details().get("estimated_cost_overflow"),
        None,
        "zero known write blocks leave the exact scan subtotal at u64::MAX"
    );
    let exact_insert = exact
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("insert".to_owned())))
        .expect("partial final insert");
    assert_eq!(exact_insert.estimated_rows(), None);
    assert_eq!(exact_insert.estimated_bytes(), Some(0));
    assert_eq!(exact_insert.estimated_work(), None);

    let overflow = explain_with_partial_insert(1);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "one byte rounds to a known write block and pushes the MAX scan subtotal over"
    );
    let overflow_insert = overflow
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("insert".to_owned())))
        .expect("partial final insert");
    assert_eq!(overflow_insert.estimated_rows(), None);
    assert_eq!(overflow_insert.estimated_bytes(), Some(1));
    assert_eq!(overflow_insert.estimated_work(), None);
    let surface = serde_json::to_value(&overflow).expect("partial-write overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_partial_write_lower_bounds_accumulate_through_unknown_tails() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_with_last_write = |last_write_bytes| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:partial-write-lower-bound-tail"),
            source: obj("table:large"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX - 2),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: Some(0),
                    estimated_write_bytes: Some(0),
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Insert,
                    estimated_affected_rows: None,
                    estimated_write_bytes: Some(1),
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: None,
                    estimated_write_bytes: None,
                    estimated_table_rows_before: None,
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Delete,
                    estimated_affected_rows: None,
                    estimated_write_bytes: Some(last_write_bytes),
                    estimated_table_rows_before: Some(1),
                },
            ],
            materialize_into: Some(obj("materialization:partial-write-tail")),
        })
        .expect("partial write bounds separated by an unknown mutation tail")
    };

    let exact = explain_with_last_write(4_096);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(
        exact.root().details().get("estimated_cost_overflow"),
        None,
        "one block from each partial write plus the scan is exactly u64::MAX"
    );
    let partial_insert = exact
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("insert".to_owned())))
        .expect("first partial write tail");
    assert_eq!(partial_insert.estimated_rows(), None);
    assert_eq!(partial_insert.estimated_bytes(), Some(1));
    assert_eq!(partial_insert.estimated_work(), None);
    let unknown_update = exact
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
                && node.estimated_work().is_none()
        })
        .expect("fully unknown mutation between partial writes");
    assert_eq!(unknown_update.estimated_rows(), None);
    assert_eq!(unknown_update.estimated_bytes(), None);
    let exact_delete = exact
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("delete".to_owned())))
        .expect("final partial write tail");
    assert_eq!(exact_delete.estimated_rows(), None);
    assert_eq!(exact_delete.estimated_bytes(), Some(4_096));
    assert_eq!(exact_delete.estimated_work(), None);

    let overflow = explain_with_last_write(4_097);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the second partial write rounds to two blocks and carries overflow across the unknown update"
    );
    let surface = serde_json::to_value(&overflow).expect("accumulated partial-write boundary");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_mixed_partial_write_bounds_accumulate_across_unknown_tail() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_with_final_bytes = |write_bytes| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:mixed-partial-write-bound-tail"),
            source: obj("table:large"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX - 2),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: Some(0),
                    estimated_write_bytes: Some(0),
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Insert,
                    estimated_affected_rows: Some(1),
                    estimated_write_bytes: None,
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: None,
                    estimated_write_bytes: None,
                    estimated_table_rows_before: None,
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Delete,
                    estimated_affected_rows: None,
                    estimated_write_bytes: Some(write_bytes),
                    estimated_table_rows_before: Some(1),
                },
            ],
            materialize_into: Some(obj("materialization:mixed-partial-tail")),
        })
        .expect("mixed partial write bounds around an unknown mutation")
    };

    // The known scan subtotal plus a one-row lower bound and one 4 KiB
    // write-block lower bound reaches MAX exactly; the nullable plan cost
    // remains absent because these mutation estimates are still partial.
    let exact = explain_with_final_bytes(4_096);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);
    let partial_insert = exact
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("insert".to_owned())))
        .expect("row-bounded partial insert");
    assert_eq!(partial_insert.estimated_rows(), Some(1));
    assert_eq!(partial_insert.estimated_bytes(), None);
    assert_eq!(partial_insert.estimated_work(), None);
    let unknown_update = exact
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
                && node.estimated_rows().is_none()
        })
        .expect("unknown estimate between partial writes");
    assert_eq!(unknown_update.estimated_work(), None);
    let partial_delete = exact
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("delete".to_owned())))
        .expect("byte-bounded partial delete");
    assert_eq!(partial_delete.estimated_rows(), None);
    assert_eq!(partial_delete.estimated_bytes(), Some(4_096));
    assert_eq!(partial_delete.estimated_work(), None);

    let overflow = explain_with_final_bytes(4_097);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the rounded byte lower bound combines with the row lower bound across an unknown tail"
    );
    let surface = serde_json::to_value(&overflow).expect("mixed-bound overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_mixed_partial_write_bounds_survive_multiple_unknown_tails() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let mutation = |kind, rows, bytes, before| QueryMutationDescription {
        table: obj("table:large"),
        kind,
        estimated_affected_rows: rows,
        estimated_write_bytes: bytes,
        estimated_table_rows_before: before,
    };
    let explain_with_last_write = |write_bytes| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:mixed-bounds-multiple-unknown-tails"),
            source: obj("table:large"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX - 4),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![
                mutation(QueryMutationKind::Insert, Some(1), None, Some(1)),
                mutation(QueryMutationKind::Update, None, None, None),
                mutation(QueryMutationKind::Delete, None, Some(4_096), Some(1)),
                mutation(QueryMutationKind::Rekey, None, None, None),
                mutation(QueryMutationKind::Insert, Some(1), None, Some(1)),
                mutation(QueryMutationKind::Delete, None, Some(write_bytes), Some(1)),
            ],
            materialize_into: Some(obj("materialization:mixed-multiple-unknown-tail")),
        })
        .expect("mixed partial bounds separated by several unknown mutations")
    };

    // Unknown mutations do not erase earlier lower bounds. Two known row
    // counts and two known write blocks bring the scan subtotal to MAX.
    let exact = explain_with_last_write(4_096);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);
    let partial_inserts = exact
        .nodes()
        .iter()
        .filter(|node| node.details().get("mutation") == Some(&PlanDetail::Text("insert".to_owned())))
        .collect::<Vec<_>>();
    assert_eq!(partial_inserts.len(), 2);
    assert!(partial_inserts.iter().all(|node| {
        node.estimated_rows() == Some(1)
            && node.estimated_bytes().is_none()
            && node.estimated_work().is_none()
    }));
    let partial_deletes = exact
        .nodes()
        .iter()
        .filter(|node| node.details().get("mutation") == Some(&PlanDetail::Text("delete".to_owned())))
        .collect::<Vec<_>>();
    assert_eq!(partial_deletes.len(), 2);
    assert!(partial_deletes.iter().all(|node| {
        node.estimated_rows().is_none()
            && node.estimated_bytes() == Some(4_096)
            && node.estimated_work().is_none()
    }));
    let unknown_count = exact
        .nodes()
        .iter()
        .filter(|node| {
            matches!(
                node.details().get("mutation"),
                Some(PlanDetail::Text(kind)) if kind == "update" || kind == "rekey"
            ) && node.estimated_rows().is_none()
                && node.estimated_bytes().is_none()
        })
        .count();
    assert_eq!(unknown_count, 2);

    let overflow = explain_with_last_write(4_097);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the final rounded byte bound pushes the accumulated total past MAX"
    );
    let surface = serde_json::to_value(&overflow).expect("multiple-tail mixed-bound surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_mixed_bound_overflow_state_survives_unknown_suffix() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_with_final_rows = |final_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:mixed-bound-unknown-suffix"),
            source: obj("table:large"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX - 3),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Insert,
                    estimated_affected_rows: Some(1),
                    estimated_write_bytes: None,
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: None,
                    estimated_write_bytes: None,
                    estimated_table_rows_before: None,
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Delete,
                    estimated_affected_rows: None,
                    estimated_write_bytes: Some(4_096),
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Rekey,
                    estimated_affected_rows: None,
                    estimated_write_bytes: None,
                    estimated_table_rows_before: None,
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Insert,
                    estimated_affected_rows: Some(final_rows),
                    estimated_write_bytes: None,
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: None,
                    estimated_write_bytes: None,
                    estimated_table_rows_before: None,
                },
            ],
            materialize_into: Some(obj("materialization:mixed-bound-suffix")),
        })
        .expect("mixed partial bounds followed by unknown mutations")
    };

    // Treat unknown suffixes as adding no provable work while retaining the
    // lower-bound state already established by known nonnegative components.
    let exact = explain_with_final_rows(1);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);
    let exact_surface = serde_json::to_value(&exact).expect("exact mixed-bound suffix");
    assert!(exact_surface["plan"].get("estimated_cost").is_none());

    let overflow = explain_with_final_rows(2);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the known partial bounds prove overflow before the final unknown update"
    );
    let partial_insert = overflow
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("insert".to_owned()))
                && node.estimated_rows() == Some(2)
        })
        .expect("row-bounded final insert before the unknown suffix");
    assert_eq!(partial_insert.estimated_bytes(), None);
    assert_eq!(partial_insert.estimated_work(), None);
    let partial_delete = overflow
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("delete".to_owned())))
        .expect("byte-bounded delete between unknown tails");
    assert_eq!(partial_delete.estimated_rows(), None);
    assert_eq!(partial_delete.estimated_bytes(), Some(4_096));
    assert_eq!(partial_delete.estimated_work(), None);
    assert_eq!(
        overflow
            .nodes()
            .iter()
            .filter(|node| {
                node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
                    && node.estimated_rows().is_none()
                    && node.estimated_bytes().is_none()
            })
            .count(),
        2,
        "both the middle unknown update and the suffix update remain unknown"
    );
    let surface = serde_json::to_value(&overflow).expect("overflow through unknown suffix");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_mixed_partial_bounds_accumulate_through_unknown_join_tails() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let mutation = |kind, rows, bytes| QueryMutationDescription {
        table: obj("table:large"),
        kind,
        estimated_affected_rows: rows,
        estimated_write_bytes: bytes,
        estimated_table_rows_before: Some(1),
    };
    let explain_with_final_rows = |final_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:mixed-bounds-unknown-joins"),
            source: obj("table:large"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX - 4),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: vec![
                QueryJoinDescription {
                    source: obj("table:unknown-first-join"),
                    statistics: None,
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:unknown-second-join"),
                    statistics: None,
                    predicate: None,
                },
            ],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![
                mutation(QueryMutationKind::Insert, Some(1), None),
                mutation(QueryMutationKind::Update, None, None),
                mutation(QueryMutationKind::Delete, None, Some(4_096)),
                mutation(QueryMutationKind::Insert, Some(final_rows), None),
            ],
            materialize_into: Some(obj("materialization:mixed-unknown-joins")),
        })
        .expect("mixed partial writes over unknown join tails")
    };

    // Join estimates are unknown, but their nullable work must not discard
    // row- and byte-derived mutation lower bounds accumulated at the boundary.
    let exact = explain_with_final_rows(2);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);
    let exact_surface = serde_json::to_value(&exact).expect("exact mixed bounds through joins");
    assert!(exact_surface["plan"].get("estimated_cost").is_none());
    assert!(exact.nodes().iter().filter(|node| node.kind() == PlanNodeKind::Join).all(|node| {
        node.estimated_work().is_none()
    }));

    let overflow = explain_with_final_rows(3);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the mixed lower bounds exceed MAX even though both join tails are unknown"
    );
    let partial_inserts = overflow
        .nodes()
        .iter()
        .filter(|node| node.details().get("mutation") == Some(&PlanDetail::Text("insert".to_owned())))
        .collect::<Vec<_>>();
    assert_eq!(partial_inserts.len(), 2);
    assert!(partial_inserts.iter().all(|node| {
        node.estimated_bytes().is_none() && node.estimated_work().is_none()
    }));
    let partial_delete = overflow
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("delete".to_owned())))
        .expect("byte-bounded partial write");
    assert_eq!(partial_delete.estimated_rows(), None);
    assert_eq!(partial_delete.estimated_bytes(), Some(4_096));
    assert_eq!(partial_delete.estimated_work(), None);
    assert_eq!(
        overflow.nodes().iter().filter(|node| node.kind() == PlanNodeKind::Join).count(),
        2
    );
    let surface = serde_json::to_value(&overflow).expect("mixed-bound overflow through joins");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_partial_scan_bounds_accumulate_across_unknown_join_chain() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_with_last_rows = |last_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:partial-scan-join-chain"),
            source: obj("table:row-bounded"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX - 3),
                estimated_bytes: None,
                mutable_branch: None,
            }),
            joins: vec![
                QueryJoinDescription {
                    source: obj("table:byte-bounded"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: None,
                        estimated_bytes: Some(4_096),
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:tail-row-bounded"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: Some(last_rows),
                        estimated_bytes: None,
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
            ],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: Vec::new(),
            materialize_into: Some(obj("materialization:partial-scan-join-chain")),
        })
        .expect("partial scan statistics through unknown join operators")
    };

    // Each scan has one known nonnegative work component. The joins remain
    // unknown, but the independent scan lower bounds still define the total boundary.
    let exact = explain_with_last_rows(2);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);
    let scans = exact
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Scan)
        .collect::<Vec<_>>();
    assert_eq!(scans.len(), 3);
    for scan in &scans {
        assert_eq!(scan.estimated_work(), None);
    }
    for (table, rows, bytes) in [
        ("table:row-bounded", Some(u64::MAX - 3), None),
        ("table:byte-bounded", None, Some(4_096)),
        ("table:tail-row-bounded", Some(2), None),
    ] {
        let scan = scans
            .iter()
            .find(|node| node.object() == Some(&obj(table)))
            .expect("partial-statistics scan");
        assert_eq!(scan.estimated_rows(), rows);
        assert_eq!(scan.estimated_bytes(), bytes);
    }
    let joins = exact
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .collect::<Vec<_>>();
    assert_eq!(joins.len(), 2);
    assert!(joins.iter().all(|node| node.estimated_work().is_none()));

    let overflow = explain_with_last_rows(3);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the row and byte scan bounds exceed MAX across the unknown join chain"
    );
    let surface = serde_json::to_value(&overflow).expect("partial scans overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_partial_scan_byte_rounding_boundary_through_unknown_tail() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_with_bytes = |partial_bytes| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:partial-byte-rounding-boundary"),
            source: obj("table:row-bound"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX - 1),
                estimated_bytes: None,
                mutable_branch: None,
            }),
            joins: vec![
                QueryJoinDescription {
                    source: obj("table:partial-byte-scan"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: None,
                        estimated_bytes: Some(partial_bytes),
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:unknown-final-scan"),
                    statistics: None,
                    predicate: None,
                },
            ],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: Vec::new(),
            materialize_into: Some(obj("materialization:partial-byte-rounding")),
        })
        .expect("partial byte scan around the 4-KiB boundary")
    };

    // Partial scan bytes round up to work blocks: 1 through 4096 bytes adds
    // one, while 4097 bytes adds two. An unknown final scan does not alter it.
    for bytes in [4_095, 4_096] {
        let exact = explain_with_bytes(bytes);
        assert_eq!(exact.plan().estimated_cost(), None);
        assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);
        let partial_scan = exact
            .nodes()
            .iter()
            .find(|node| node.object() == Some(&obj("table:partial-byte-scan")))
            .expect("partial byte scan at exact cost boundary");
        assert_eq!(partial_scan.estimated_rows(), None);
        assert_eq!(partial_scan.estimated_bytes(), Some(bytes));
        assert_eq!(partial_scan.estimated_work(), None);
    }

    let overflow = explain_with_bytes(4_097);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the second rounded block crosses the exact boundary"
    );
    let nodes = overflow.nodes();
    let partial_scan_position = nodes
        .iter()
        .position(|node| node.object() == Some(&obj("table:partial-byte-scan")))
        .expect("partial byte scan that proves overflow");
    assert_eq!(nodes[partial_scan_position].estimated_bytes(), Some(4_097));
    assert_eq!(nodes[partial_scan_position].estimated_work(), None);
    let unknown_scan_position = nodes
        .iter()
        .position(|node| node.object() == Some(&obj("table:unknown-final-scan")))
        .expect("unknown final scan");
    assert!(unknown_scan_position > partial_scan_position);
    assert_eq!(nodes[unknown_scan_position].estimated_work(), None);
    let surface = serde_json::to_value(&overflow).expect("rounded byte overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_partial_byte_scan_boundary_survives_unknown_final_scan() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_with_tail_bytes = |tail_bytes| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:partial-byte-final-scan"),
            source: obj("table:exact-max-scan"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: vec![
                QueryJoinDescription {
                    source: obj("table:partial-byte-scan"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: None,
                        estimated_bytes: Some(tail_bytes),
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:unknown-final-scan"),
                    statistics: None,
                    predicate: None,
                },
            ],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: Vec::new(),
            materialize_into: Some(obj("materialization:partial-byte-final-scan")),
        })
        .expect("partial byte scan before an unknown final scan")
    };

    // A known zero-byte scan contributes zero, while any positive byte count
    // rounds to one 4-KiB work unit; unknown final scans preserve either state.
    let exact = explain_with_tail_bytes(0);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);
    let overflow = explain_with_tail_bytes(1);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "one byte rounds to a block beyond the exact MAX scan"
    );

    let nodes = overflow.nodes();
    let full_scan = nodes
        .iter()
        .find(|node| node.object() == Some(&obj("table:exact-max-scan")))
        .expect("exact maximum source scan");
    assert_eq!(full_scan.estimated_work(), Some(u64::MAX));
    let partial_scan_position = nodes
        .iter()
        .position(|node| node.object() == Some(&obj("table:partial-byte-scan")))
        .expect("partial byte scan");
    assert_eq!(nodes[partial_scan_position].estimated_rows(), None);
    assert_eq!(nodes[partial_scan_position].estimated_bytes(), Some(1));
    assert_eq!(nodes[partial_scan_position].estimated_work(), None);
    let unknown_scan_position = nodes
        .iter()
        .position(|node| node.object() == Some(&obj("table:unknown-final-scan")))
        .expect("unknown final scan");
    assert!(unknown_scan_position > partial_scan_position);
    assert_eq!(nodes[unknown_scan_position].estimated_work(), None);

    let surface = serde_json::to_value(&overflow).expect("unknown final scan overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_partial_scan_overflow_survives_unknown_scan_suffixes() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_with_last_rows = |last_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:partial-scan-unknown-suffix"),
            source: obj("table:row-bounded"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX - 2),
                estimated_bytes: None,
                mutable_branch: None,
            }),
            joins: vec![
                QueryJoinDescription {
                    source: obj("table:byte-bounded"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: None,
                        estimated_bytes: Some(4_096),
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:tail-row-bounded"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: Some(last_rows),
                        estimated_bytes: None,
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:unknown-tail-first"),
                    statistics: None,
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:unknown-tail-second"),
                    statistics: None,
                    predicate: None,
                },
            ],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: Vec::new(),
            materialize_into: Some(obj("materialization:partial-scan-suffix")),
        })
        .expect("partial scan boundary followed by unknown scan suffixes")
    };

    // Bounds from the leading scans reach the boundary before the unknown
    // right-side scans; those suffixes cannot clear or create overflow state.
    let exact = explain_with_last_rows(1);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);
    let overflow = explain_with_last_rows(2);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "overflow established by partial scan bounds survives unknown scan suffixes"
    );

    let nodes = overflow.nodes();
    let bounded_tail_position = nodes
        .iter()
        .position(|node| node.object() == Some(&obj("table:tail-row-bounded")))
        .expect("last scan with a known row bound");
    for table in ["table:unknown-tail-first", "table:unknown-tail-second"] {
        let unknown_position = nodes
            .iter()
            .position(|node| node.object() == Some(&obj(table)))
            .expect("unknown suffix scan");
        assert!(unknown_position > bounded_tail_position);
        assert_eq!(nodes[unknown_position].estimated_work(), None);
    }
    assert!(nodes.iter().filter(|node| node.kind() == PlanNodeKind::Join).all(|node| {
        node.estimated_work().is_none()
    }));
    let surface = serde_json::to_value(&overflow).expect("partial scan overflow suffix surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_partial_scan_bounds_survive_unknown_scan_chain() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_with_last_rows = |last_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:partial-scan-unknown-chain"),
            source: obj("table:unknown-source"),
            source_statistics: None,
            joins: vec![
                QueryJoinDescription {
                    source: obj("table:row-bound"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: Some(u64::MAX - 3),
                        estimated_bytes: None,
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:unknown-middle"),
                    statistics: None,
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:byte-bound"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: None,
                        estimated_bytes: Some(4_096),
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:tail-row-bound"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: Some(last_rows),
                        estimated_bytes: None,
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
            ],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: Vec::new(),
            materialize_into: Some(obj("materialization:partial-scan-unknown-chain")),
        })
        .expect("partial scan bounds with wholly unknown scans in the join chain")
    };

    // An entirely unknown scan contributes no work and cannot erase the
    // independent row- and byte-derived lower bounds from later scan inputs.
    let exact = explain_with_last_rows(2);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);
    let scans = exact
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Scan)
        .collect::<Vec<_>>();
    assert_eq!(scans.len(), 5);
    assert!(scans.iter().all(|scan| scan.estimated_work().is_none()));
    for (table, rows, bytes) in [
        ("table:unknown-source", None, None),
        ("table:row-bound", Some(u64::MAX - 3), None),
        ("table:unknown-middle", None, None),
        ("table:byte-bound", None, Some(4_096)),
        ("table:tail-row-bound", Some(2), None),
    ] {
        let scan = scans
            .iter()
            .find(|node| node.object() == Some(&obj(table)))
            .expect("scan in unknown join chain");
        assert_eq!(scan.estimated_rows(), rows);
        assert_eq!(scan.estimated_bytes(), bytes);
    }
    let joins = exact
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .collect::<Vec<_>>();
    assert_eq!(joins.len(), 4);
    assert!(joins.iter().all(|join| join.estimated_work().is_none()));

    let overflow = explain_with_last_rows(3);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "later partial scans still push the accumulated lower bound past MAX"
    );
    let surface = serde_json::to_value(&overflow).expect("unknown-scan overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_partial_source_bounds_accumulate_through_unknown_join_and_mutation_tails() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain = |source_rows, source_bytes, mutation_rows, mutation_bytes| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:partial-source-bound-tails"),
            source: obj("table:large"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: source_rows,
                estimated_bytes: source_bytes,
                mutable_branch: None,
            }),
            joins: vec![QueryJoinDescription {
                source: obj("table:unknown-right"),
                statistics: None,
                predicate: None,
            }],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: mutation_rows,
                    estimated_write_bytes: mutation_bytes,
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:large"),
                    kind: QueryMutationKind::Delete,
                    estimated_affected_rows: None,
                    estimated_write_bytes: None,
                    estimated_table_rows_before: None,
                },
            ],
            materialize_into: Some(obj("materialization:partial-source-bound")),
        })
        .expect("partial source bound through unknown plan tails")
    };

    // A one-field scan statistic is not an exact node cost, but rows and
    // 4-KiB blocks are independent nonnegative lower bounds for aggregation.
    let exact_rows = explain(Some(u64::MAX - 1), None, Some(1), None);
    assert_eq!(exact_rows.plan().estimated_cost(), None);
    assert_eq!(exact_rows.root().details().get("estimated_cost_overflow"), None);
    let partial_row_scan = exact_rows
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj("table:large")))
        .expect("scan with a known row lower bound");
    assert_eq!(partial_row_scan.estimated_rows(), Some(u64::MAX - 1));
    assert_eq!(partial_row_scan.estimated_bytes(), None);
    assert_eq!(partial_row_scan.estimated_work(), None);
    let partial_row_update = exact_rows
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned())))
        .expect("mutation with a known row lower bound");
    assert_eq!(partial_row_update.estimated_work(), None);
    let exact_overflow = explain(Some(u64::MAX - 1), None, Some(2), None);
    assert_eq!(
        exact_overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the partial scan and mutation row bounds together exceed MAX"
    );

    let exact_bytes = explain(None, Some(4_096), Some(u64::MAX - 1), Some(0));
    assert_eq!(exact_bytes.plan().estimated_cost(), None);
    assert_eq!(exact_bytes.root().details().get("estimated_cost_overflow"), None);
    let partial_byte_scan = exact_bytes
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj("table:large")))
        .expect("scan with a known byte lower bound");
    assert_eq!(partial_byte_scan.estimated_rows(), None);
    assert_eq!(partial_byte_scan.estimated_bytes(), Some(4_096));
    assert_eq!(partial_byte_scan.estimated_work(), None);
    let exact_update = exact_bytes
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned())))
        .expect("exact mutation contribution after the partial scan");
    assert_eq!(exact_update.estimated_work(), Some(u64::MAX - 1));
    let bytes_overflow = explain(None, Some(4_096), Some(u64::MAX), Some(0));
    assert_eq!(
        bytes_overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "one known scan block overflows the MAX mutation contribution"
    );
    let surface = serde_json::to_value(&bytes_overflow).expect("partial source byte overflow");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_partial_bounds_survive_unknown_source_and_join_tails() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_with_last_bytes = |write_bytes| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:partial-bound-unknown-join-tails"),
            source: obj("table:missing-stats"),
            source_statistics: None,
            joins: vec![QueryJoinDescription {
                source: obj("table:large"),
                statistics: Some(QuerySourceStatistics {
                    estimated_rows: Some(u64::MAX - 2),
                    estimated_bytes: Some(0),
                    mutable_branch: None,
                }),
                predicate: None,
            }],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![
                QueryMutationDescription {
                    table: obj("table:missing-stats"),
                    kind: QueryMutationKind::Insert,
                    estimated_affected_rows: Some(1),
                    estimated_write_bytes: None,
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:missing-stats"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: None,
                    estimated_write_bytes: None,
                    estimated_table_rows_before: None,
                },
                QueryMutationDescription {
                    table: obj("table:missing-stats"),
                    kind: QueryMutationKind::Delete,
                    estimated_affected_rows: None,
                    estimated_write_bytes: Some(write_bytes),
                    estimated_table_rows_before: Some(1),
                },
            ],
            materialize_into: Some(obj("materialization:partial-bound-unknown-join")),
        })
        .expect("partial mutation bounds across unknown input and join estimates")
    };

    // An unknown left input makes the join and total unknown. The known right
    // scan and partial mutation components still establish a useful lower bound.
    let exact = explain_with_last_bytes(4_096);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);
    let known_scan = exact
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj("table:large")))
        .expect("known right scan under unknown join");
    assert_eq!(known_scan.estimated_work(), Some(u64::MAX - 2));
    let unknown_scan = exact
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object() == Some(&obj("table:missing-stats"))
        })
        .expect("unknown left scan");
    assert_eq!(unknown_scan.estimated_work(), None);
    let join = exact
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Join)
        .expect("join with unknown input cardinality");
    assert_eq!(join.estimated_work(), None);
    let partial_insert = exact
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("insert".to_owned())))
        .expect("row-bounded partial insert");
    assert_eq!(partial_insert.estimated_rows(), Some(1));
    assert_eq!(partial_insert.estimated_bytes(), None);
    assert_eq!(partial_insert.estimated_work(), None);
    let partial_delete = exact
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("delete".to_owned())))
        .expect("byte-bounded partial delete");
    assert_eq!(partial_delete.estimated_rows(), None);
    assert_eq!(partial_delete.estimated_bytes(), Some(4_096));
    assert_eq!(partial_delete.estimated_work(), None);

    let overflow = explain_with_last_bytes(8_192);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the rounded byte lower bound crosses MAX despite unknown source and join work"
    );
    let surface = serde_json::to_value(&overflow).expect("partial-bound overflow through join");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_cost_overflow_survives_unknown_intermediate_estimates() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    // A missing left-side statistic makes the join and materialization
    // estimates unknown. The known mutation and later right scan still prove
    // an overflowing lower bound; unknown estimates cannot reduce that cost.
    let explained = explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:overflow-through-unknown-join"),
        source: obj("table:missing-stats"),
        source_statistics: None,
        joins: vec![QueryJoinDescription {
            source: obj("table:large"),
            statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            predicate: None,
        }],
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: vec![QueryMutationDescription {
            table: obj("table:missing-stats"),
            kind: QueryMutationKind::Update,
            estimated_affected_rows: Some(1),
            estimated_write_bytes: Some(0),
            estimated_table_rows_before: Some(1),
        }],
        materialize_into: Some(obj("materialization:unknown-intermediate")),
    })
    .expect("plan with an unknown intermediate and overflowing known tail");

    assert_eq!(explained.plan().estimated_cost(), None);
    assert_eq!(explained.root().kind(), PlanNodeKind::Materialize);
    assert_eq!(
        explained.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    let join = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Join)
        .expect("join with missing input cardinality");
    assert_eq!(join.estimated_work(), None);
    assert_eq!(join.details().get("estimated_work_overflow"), None);
    let unknown_scan = explained
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object() == Some(&obj("table:missing-stats"))
        })
        .expect("unknown left scan");
    assert_eq!(unknown_scan.estimated_work(), None);
    let known_tail = explained
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj("table:large"))
        })
        .expect("known right scan after the unknown join");
    assert_eq!(known_tail.estimated_work(), Some(u64::MAX));

    let surface = serde_json::to_value(&explained).expect("unknown-intermediate plan surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_unknown_intermediate_keeps_exact_maximum_known_subtotal() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    // Unknown join/input work makes total cost unavailable, but the update,
    // its materialization, and the later known scan sum to u64::MAX exactly.
    let explained = explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:unknown-at-exact-cost-boundary"),
        source: obj("table:missing-stats"),
        source_statistics: None,
        joins: vec![QueryJoinDescription {
            source: obj("table:large"),
            statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX - 2),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            predicate: None,
        }],
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: vec![QueryMutationDescription {
            table: obj("table:missing-stats"),
            kind: QueryMutationKind::Update,
            estimated_affected_rows: Some(1),
            estimated_write_bytes: Some(0),
            estimated_table_rows_before: Some(1),
        }],
        materialize_into: Some(obj("materialization:unknown-boundary")),
    })
    .expect("unknown estimates around an exact maximum known subtotal");

    assert_eq!(explained.plan().estimated_cost(), None);
    assert_eq!(explained.root().estimated_work(), Some(1));
    assert_eq!(
        explained.root().details().get("estimated_cost_overflow"),
        None,
        "exactly u64::MAX of known nonnegative work is representable"
    );
    let mutation = explained
        .nodes()
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned())))
        .expect("known mutation work before the unknown join");
    assert_eq!(mutation.estimated_work(), Some(1));
    let join = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Join)
        .expect("unknown join work");
    assert_eq!(join.estimated_work(), None);
    let unknown_scan = explained
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object() == Some(&obj("table:missing-stats"))
        })
        .expect("unknown left scan");
    assert_eq!(unknown_scan.estimated_work(), None);
    let known_tail = explained
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj("table:large"))
        })
        .expect("known right scan after unknown estimates");
    assert_eq!(known_tail.estimated_work(), Some(u64::MAX - 2));

    let surface = serde_json::to_value(&explained).expect("unknown-boundary plan surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert!(surface["nodes"][0]["details"]
        .get("estimated_cost_overflow")
        .is_none());
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_cost_boundary_retains_overflow_before_unknown_join_tails() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_after_updates = |last_update_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:boundary-before-unknown-join-tails"),
            source: obj("table:missing-stats"),
            source_statistics: None,
            joins: vec![
                QueryJoinDescription {
                    source: obj("table:unknown-first-join"),
                    statistics: None,
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:unknown-second-join"),
                    statistics: None,
                    predicate: None,
                },
            ],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![
                QueryMutationDescription {
                    table: obj("table:missing-stats"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: Some(u64::MAX - 2),
                    estimated_write_bytes: Some(0),
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:missing-stats"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: Some(last_update_rows),
                    estimated_write_bytes: Some(0),
                    estimated_table_rows_before: Some(1),
                },
            ],
            materialize_into: Some(obj("materialization:boundary-before-unknown-joins")),
        })
        .expect("known cost boundary followed by unknown join tails")
    };

    let exact = explain_after_updates(1);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().estimated_work(), Some(1));
    assert_eq!(
        exact.root().details().get("estimated_cost_overflow"),
        None,
        "the known prefix is exactly u64::MAX before the unknown scan"
    );
    let exact_known_work = exact
        .nodes()
        .iter()
        .filter_map(|node| node.estimated_work())
        .collect::<Vec<_>>();
    assert_eq!(exact_known_work, [1, 1, u64::MAX - 2]);
    let unknown_tail = &exact.nodes()[3..];
    assert_eq!(
        unknown_tail
            .iter()
            .filter(|node| node.kind() == PlanNodeKind::Join)
            .count(),
        2
    );
    assert_eq!(
        unknown_tail
            .iter()
            .filter(|node| node.kind() == PlanNodeKind::Scan)
            .count(),
        3
    );
    assert!(unknown_tail.iter().all(|node| node.estimated_work().is_none()));

    let overflow = explain_after_updates(2);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(overflow.root().estimated_work(), Some(2));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "overflow proven before the unknown scan remains visible on the root"
    );
    assert!(overflow.nodes()[3..]
        .iter()
        .all(|node| node.estimated_work().is_none()));
    let surface = serde_json::to_value(&overflow).expect("boundary overflow with unknown tails");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_unknown_join_chain_retains_known_scan_cost_boundary() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_with_right_tail = |last_scan_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:unknown-join-known-scan-boundary"),
            source: obj("table:missing-stats"),
            source_statistics: None,
            joins: vec![
                QueryJoinDescription {
                    source: obj("table:known-first-right"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: Some(u64::MAX - 3),
                        estimated_bytes: Some(0),
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:known-second-right"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: Some(last_scan_rows),
                        estimated_bytes: Some(0),
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
            ],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![QueryMutationDescription {
                table: obj("table:missing-stats"),
                kind: QueryMutationKind::Update,
                estimated_affected_rows: Some(1),
                estimated_write_bytes: Some(0),
                estimated_table_rows_before: Some(1),
            }],
            materialize_into: Some(obj("materialization:unknown-join-boundary")),
        })
        .expect("known scan tails behind joins with unknown left cardinality")
    };

    let exact = explain_with_right_tail(1);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().estimated_work(), Some(1));
    assert_eq!(
        exact.root().details().get("estimated_cost_overflow"),
        None,
        "mutation, materialization, and both right scans sum to exactly u64::MAX"
    );
    let joins = exact
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .collect::<Vec<_>>();
    assert_eq!(joins.len(), 2);
    assert!(joins.iter().all(|node| node.estimated_work().is_none()));
    let unknown_left = exact
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object() == Some(&obj("table:missing-stats"))
        })
        .expect("unknown left scan");
    assert_eq!(unknown_left.estimated_work(), None);
    for (table, rows) in [
        ("table:known-first-right", u64::MAX - 3),
        ("table:known-second-right", 1),
    ] {
        let scan = exact
            .nodes()
            .iter()
            .find(|node| node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj(table)))
            .expect("known right scan under unknown join");
        assert_eq!(scan.estimated_work(), Some(rows));
    }

    let overflow = explain_with_right_tail(2);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    let surface = serde_json::to_value(&overflow).expect("unknown-join boundary surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_unknown_final_join_tails_retain_known_left_scan_boundary() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    // Missing right-side cardinalities keep join work unknown: a known zero
    // byte estimate cannot stand in for missing row counts. Independently
    // known left-scan work still counts at the total boundary through tails.
    let explain_with_mutation_tail = |affected_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:unknown-final-join-tail-boundary"),
            source: obj("table:known-left"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX - 2),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: vec![
                QueryJoinDescription {
                    source: obj("table:unknown-right-first"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: None,
                        estimated_bytes: Some(0),
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:unknown-right-second"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: None,
                        estimated_bytes: Some(0),
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
            ],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![QueryMutationDescription {
                table: obj("table:known-left"),
                kind: QueryMutationKind::Update,
                estimated_affected_rows: Some(affected_rows),
                estimated_write_bytes: Some(0),
                estimated_table_rows_before: Some(1),
            }],
            materialize_into: Some(obj("materialization:unknown-final-joins")),
        })
        .expect("known left scan followed by unknown final join tails")
    };

    let exact = explain_with_mutation_tail(1);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().estimated_work(), Some(1));
    assert_eq!(
        exact.root().details().get("estimated_cost_overflow"),
        None,
        "known left scan plus mutation and materialization sum to u64::MAX"
    );
    let left_scan = exact
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object() == Some(&obj("table:known-left"))
        })
        .expect("known left scan under the joins");
    assert_eq!(left_scan.estimated_work(), Some(u64::MAX - 2));
    let join_nodes = exact
        .nodes()
        .iter()
        .filter(|node| node.kind() == PlanNodeKind::Join)
        .collect::<Vec<_>>();
    assert_eq!(join_nodes.len(), 2);
    assert!(join_nodes.iter().all(|node| node.estimated_work().is_none()));
    for table in ["table:unknown-right-first", "table:unknown-right-second"] {
        let scan = exact
            .nodes()
            .iter()
            .find(|node| node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj(table)))
            .expect("unknown right scan");
        assert_eq!(scan.estimated_work(), None);
        assert_eq!(scan.estimated_rows(), None);
        assert_eq!(scan.estimated_bytes(), Some(0));
    }

    let overflow = explain_with_mutation_tail(2);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    let surface = serde_json::to_value(&overflow).expect("unknown final join boundary surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_local_work_overflow_is_explicit_on_cost_and_write_tails() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explained = explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:local-work-overflow"),
        source: obj("table:large"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: Some(u64::MAX),
            estimated_bytes: Some(4_096),
            mutable_branch: None,
        }),
        joins: vec![QueryJoinDescription {
            source: obj("table:small"),
            statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(1),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            predicate: None,
        }],
        predicate: None,
        projections: vec![
            orna_sys_v1::ExpressionRef::descriptive("expr:first"),
            orna_sys_v1::ExpressionRef::descriptive("expr:second"),
        ],
        distinct: true,
        ordering: vec![PlanOrdering {
            expression: orna_sys_v1::ExpressionRef::descriptive("expr:ordered"),
            direction: PlanSortDirection::Ascending,
            null_order: PlanNullOrder::Last,
        }],
        limit: None,
        mutations: vec![QueryMutationDescription {
            table: obj("table:large"),
            kind: QueryMutationKind::Update,
            estimated_affected_rows: Some(u64::MAX),
            estimated_write_bytes: Some(4_096),
            estimated_table_rows_before: Some(u64::MAX),
        }],
        materialize_into: Some(obj("materialization:large")),
    })
    .expect("plan with local work overflow");

    assert_eq!(explained.root().kind(), PlanNodeKind::Materialize);
    assert_eq!(explained.plan().estimated_cost(), None);
    assert_eq!(
        explained.root().details().get("estimated_cost_overflow"),
        None
    );
    for kind in [
        PlanNodeKind::Materialize,
        PlanNodeKind::Invoke,
        PlanNodeKind::Sort,
        PlanNodeKind::Aggregate,
        PlanNodeKind::Project,
        PlanNodeKind::Join,
        PlanNodeKind::Scan,
    ] {
        let node = explained
            .nodes()
            .iter()
            .find(|node| node.kind() == kind)
            .expect("plan tail operator");
        assert_eq!(node.estimated_work(), None);
        assert_eq!(
            node.details().get("estimated_work_overflow"),
            Some(&PlanDetail::Boolean(true))
        );
    }
    let small_scan = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj("table:small")))
        .expect("nonoverflowing scan estimate");
    assert_eq!(small_scan.estimated_work(), Some(1));
    assert_eq!(small_scan.details().get("estimated_work_overflow"), None);
    let surface = serde_json::to_value(&explained).expect("structured local overflow plan");
    assert_eq!(surface["nodes"][0]["details"]["estimated_work_overflow"], true);
    let mutation = surface["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["details"]["mutation"] == "update")
        .expect("mutation tail");
    assert_eq!(mutation["details"]["estimated_work_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn mutation_estimate_edges_keep_unknown_overflow_and_underflow_unfabricated() {
    let explained = explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:mutation-estimates"),
        source: obj("table:partial_stats"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: None,
            estimated_bytes: Some(512),
            mutable_branch: None,
        }),
        joins: Vec::new(),
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: vec![
            QueryMutationDescription {
                table: obj("table:overflow"),
                kind: QueryMutationKind::Insert,
                estimated_affected_rows: Some(1),
                estimated_write_bytes: Some(16),
                estimated_table_rows_before: Some(u64::MAX),
            },
            QueryMutationDescription {
                table: obj("table:underflow"),
                kind: QueryMutationKind::Delete,
                estimated_affected_rows: Some(2),
                estimated_write_bytes: Some(16),
                estimated_table_rows_before: Some(1),
            },
            QueryMutationDescription {
                table: obj("table:stable"),
                kind: QueryMutationKind::Update,
                estimated_affected_rows: Some(3),
                estimated_write_bytes: Some(96),
                estimated_table_rows_before: Some(20),
            },
            QueryMutationDescription {
                table: obj("table:stable"),
                kind: QueryMutationKind::Rekey,
                estimated_affected_rows: Some(1),
                estimated_write_bytes: Some(32),
                estimated_table_rows_before: None,
            },
            QueryMutationDescription {
                table: obj("table:unknown"),
                kind: QueryMutationKind::Insert,
                estimated_affected_rows: None,
                estimated_write_bytes: None,
                estimated_table_rows_before: None,
            },
        ],
        materialize_into: None,
    })
    .expect("bounded mutation plan");

    assert_eq!(explained.plan().estimated_cost(), None);
    let nodes = serde_json::to_value(explained.nodes()).expect("portable plan details");
    let nodes = nodes.as_array().unwrap();
    assert_eq!(
        nodes
            .iter()
            .find(|node| node["object"] == "table:partial_stats")
            .unwrap()["estimated_bytes"],
        512
    );
    for table in ["table:overflow", "table:underflow", "table:unknown"] {
        let node = nodes
            .iter()
            .find(|node| node["object"] == table && node["kind"] == "invoke")
            .expect("mutation node");
        assert!(node["details"].get("table_rows_after").is_none());
        assert!(node.get("actual_rows").is_none());
    }
    for kind in ["update", "rekey"] {
        let node = nodes
            .iter()
            .find(|node| node["details"]["mutation"] == kind)
            .expect("known table cardinality");
        assert_eq!(node["details"]["table_rows_after"], 20);
    }
}

#[test]
fn join_estimates_handle_empty_cross_partial_and_overflow_cardinalities() {
    let explain = |left: QuerySourceStatistics,
                   right: QuerySourceStatistics,
                   predicate: Option<orna_sys_v1::ExpressionRef>| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:join-estimates"),
            source: obj("table:left"),
            source_statistics: Some(left),
            joins: vec![QueryJoinDescription {
                source: obj("table:right"),
                statistics: Some(right),
                predicate,
            }],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: Vec::new(),
            materialize_into: None,
        })
        .expect("query with a single join")
    };
    let stats = |rows, bytes| QuerySourceStatistics {
        estimated_rows: rows,
        estimated_bytes: bytes,
        mutable_branch: None,
    };

    let empty = explain(
        stats(Some(0), Some(0)),
        stats(Some(7), Some(140)),
        Some(orna_sys_v1::ExpressionRef::descriptive("expr:join")),
    );
    let join = empty
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Join)
        .unwrap();
    assert_eq!(join.estimated_rows(), Some(0));
    assert_eq!(join.estimated_bytes(), Some(0));
    assert!(empty.plan().estimated_cost().is_some());

    let cross = explain(stats(Some(2), Some(32)), stats(Some(3), Some(15)), None);
    let join = cross
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Join)
        .unwrap();
    assert_eq!(join.estimated_rows(), Some(6));
    assert_eq!(join.estimated_bytes(), Some(126));
    let cross_details = serde_json::to_value(join).unwrap();
    assert_eq!(cross_details["details"]["join_type"], "cross");

    let partial = explain(stats(Some(3), Some(30)), stats(None, Some(40)), None);
    let join = partial
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Join)
        .unwrap();
    assert_eq!(join.estimated_rows(), None);
    assert_eq!(join.estimated_bytes(), None);
    assert_eq!(partial.plan().estimated_cost(), None);

    let overflow = explain(
        stats(Some(u64::MAX), Some(0)),
        stats(Some(u64::MAX), Some(0)),
        None,
    );
    let join = overflow
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Join)
        .unwrap();
    assert_eq!(join.estimated_rows(), None);
    assert_eq!(join.estimated_bytes(), None);
    assert_eq!(overflow.plan().estimated_cost(), None);
}

#[test]
fn function_explain_keeps_direct_effect_edges_visible() {
    let function = obj("function:main");
    let table = obj("table:contacts");
    let target = obj("function:normalize");
    let edges = vec![
        DependencyInput::new(
            function.clone(),
            table.clone(),
            DependencyKind::TableRead,
            None,
            None,
            DependencyConfidence::Exact,
            false,
        ),
        DependencyInput::new(
            function.clone(),
            target.clone(),
            DependencyKind::Call,
            None,
            None,
            DependencyConfidence::Exact,
            false,
        ),
        DependencyInput::new(
            function.clone(),
            obj("type:contact"),
            DependencyKind::TypeReference,
            None,
            None,
            DependencyConfidence::Exact,
            false,
        ),
    ];
    let explained = explain_function(&FunctionPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:one"),
        function: FunctionRef::descriptive("function:main"),
        dependencies: edges,
    })
    .expect("function plan");
    assert_eq!(explained.root().kind(), PlanNodeKind::Invoke);
    assert_eq!(explained.root().object(), Some(&function));
    assert_eq!(explained.root().inputs().len(), 2);
    assert!(explained.nodes().iter().any(|node| {
        node.kind() == PlanNodeKind::Scan && node.object() == Some(&table)
    }));
    assert!(explained.nodes().iter().any(|node| {
        node.kind() == PlanNodeKind::Invoke && node.object() == Some(&target)
    }));
}

#[test]
fn explain_rejects_conflicting_function_edges_and_over_limit_query_shapes() {
    let function = obj("function:main");
    let target = obj("function:normalize");
    let exact = DependencyInput::new(
        function.clone(),
        target.clone(),
        DependencyKind::Call,
        None,
        None,
        DependencyConfidence::Exact,
        false,
    );
    let conflicting = DependencyInput::new(
        function.clone(),
        target.clone(),
        DependencyKind::Call,
        None,
        None,
        DependencyConfidence::Exact,
        true,
    );
    assert_eq!(
        explain_function(&FunctionPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:one"),
            function: FunctionRef::descriptive("function:main"),
            dependencies: vec![exact, conflicting],
        }),
        Err(ExplainError::InvalidDependency)
    );

    let over_limit = QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:one"),
        source: obj("table:contacts"),
        source_statistics: None,
        joins: Vec::new(),
        predicate: Some(orna_sys_v1::ExpressionRef::descriptive("expr:active")),
        projections: (0..MAX_PLAN_EXPRESSIONS)
            .map(|position| {
                orna_sys_v1::ExpressionRef::descriptive(format!("expr:{position}"))
            })
            .collect(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    };
    assert_eq!(
        explain_query(&over_limit),
        Err(ExplainError::TooManyExpressions)
    );

    let invalid_expression = QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:one"),
        source: obj("table:contacts"),
        source_statistics: None,
        joins: Vec::new(),
        predicate: Some(orna_sys_v1::ExpressionRef::descriptive("\n")),
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    };
    assert_eq!(
        explain_query(&invalid_expression),
        Err(ExplainError::InvalidExpression)
    );
}

#[test]
fn explain_rounds_partial_scan_byte_tails_per_scan_before_unknown_suffix() {
    let parsed = orna_syntax_v1::parse_module(MUTABLE_BRANCH_QUERY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explain_with_last_scan_bytes = |last_scan_bytes| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:partial-byte-rounding-tail"),
            source: obj("table:row-boundary"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(u64::MAX - 1),
                estimated_bytes: None,
                mutable_branch: None,
            }),
            joins: vec![
                QueryJoinDescription {
                    source: obj("table:partial-byte-first"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: None,
                        estimated_bytes: Some(4_095),
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:partial-byte-tail"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: None,
                        estimated_bytes: Some(last_scan_bytes),
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:unknown-final-scan"),
                    statistics: None,
                    predicate: None,
                },
            ],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: Vec::new(),
            materialize_into: Some(obj("materialization:partial-byte-rounding-tail")),
        })
        .expect("partial byte scans followed by unknown scan tail")
    };

    // Statistics do not promise bytes can be combined across scans before
    // costing. Round each known scan independently, preserving its lower
    // bound through unknown joins and the final unknown scan.
    let exact = explain_with_last_scan_bytes(0);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);

    // Although these scan byte counts sum to exactly 4 KiB, separate scan
    // blocks cost two units. The second unit exceeds MAX after the row bound.
    let overflow = explain_with_last_scan_bytes(1);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    let nodes = overflow.nodes();
    let row_scan = nodes
        .iter()
        .find(|node| node.object() == Some(&obj("table:row-boundary")))
        .expect("known row-bound source scan");
    assert_eq!(row_scan.estimated_work(), None);
    let first_byte_scan = nodes
        .iter()
        .find(|node| node.object() == Some(&obj("table:partial-byte-first")))
        .expect("first partial byte scan");
    assert_eq!(first_byte_scan.estimated_bytes(), Some(4_095));
    assert_eq!(first_byte_scan.estimated_work(), None);
    let last_byte_scan = nodes
        .iter()
        .find(|node| node.object() == Some(&obj("table:partial-byte-tail")))
        .expect("last partial byte scan");
    assert_eq!(last_byte_scan.estimated_bytes(), Some(1));
    assert_eq!(last_byte_scan.estimated_work(), None);
    let unknown_position = nodes
        .iter()
        .position(|node| node.object() == Some(&obj("table:unknown-final-scan")))
        .expect("unknown final scan");
    let last_byte_scan_position = nodes
        .iter()
        .position(|node| node.object() == Some(&obj("table:partial-byte-tail")))
        .expect("last partial byte scan");
    assert!(unknown_position > last_byte_scan_position);
    assert_eq!(nodes[unknown_position].estimated_work(), None);

    let surface = serde_json::to_value(&overflow).expect("partial rounding overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_keeps_per_scan_rounding_bounds_through_mutation_and_unknown_tails() {
    let parsed = orna_syntax_v1::parse_module(ROUNDING_TAIL_INTERPLAY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 5);

    let explain_with_mutation_rows = |affected_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:rounding-mutation-tail"),
            source: obj("table:RoundingFirst"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: None,
                estimated_bytes: Some(4_095),
                mutable_branch: None,
            }),
            joins: vec![
                QueryJoinDescription {
                    source: obj("table:RoundingTail"),
                    statistics: Some(QuerySourceStatistics {
                        estimated_rows: None,
                        estimated_bytes: Some(1),
                        mutable_branch: None,
                    }),
                    predicate: None,
                },
                QueryJoinDescription {
                    source: obj("table:UnknownTail"),
                    statistics: None,
                    predicate: None,
                },
            ],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![
                QueryMutationDescription {
                    table: obj("table:RoundingWrite"),
                    kind: QueryMutationKind::Update,
                    estimated_affected_rows: Some(affected_rows),
                    estimated_write_bytes: None,
                    estimated_table_rows_before: Some(1),
                },
                QueryMutationDescription {
                    table: obj("table:RoundingWrite"),
                    kind: QueryMutationKind::Delete,
                    estimated_affected_rows: None,
                    estimated_write_bytes: None,
                    estimated_table_rows_before: None,
                },
            ],
            materialize_into: Some(obj("materialization:rounding-mutation-tail")),
        })
        .expect("rounded scans and partial mutation followed by unknown tails")
    };

    // ORNA-PLAN-002/003 require planner introspection and a correct fallback,
    // but leave byte-cost units unspecified. Preserve the existing 4-KiB
    // heuristic: round each scan independently, then carry its nonnegative
    // lower bound into the total even when later estimates are unknown.
    let exact = explain_with_mutation_rows(u64::MAX - 2);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);
    let exact_nodes = exact.nodes();
    for (table, bytes) in [("table:RoundingFirst", 4_095), ("table:RoundingTail", 1)] {
        let scan = exact_nodes
            .iter()
            .find(|node| node.object() == Some(&obj(table)))
            .expect("fixture scan in plan");
        assert_eq!(scan.estimated_bytes(), Some(bytes));
        assert_eq!(scan.estimated_work(), None);
    }
    let update = exact_nodes
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned())))
        .expect("partially estimated update");
    assert_eq!(update.estimated_rows(), Some(u64::MAX - 2));
    assert_eq!(update.estimated_bytes(), None);
    assert_eq!(update.estimated_work(), None);
    let unknown_mutation = exact_nodes
        .iter()
        .find(|node| node.details().get("mutation") == Some(&PlanDetail::Text("delete".to_owned())))
        .expect("unknown mutation tail");
    assert_eq!(unknown_mutation.estimated_work(), None);

    // The two scan tails round to two units although their raw bytes total
    // exactly one 4-KiB block. Adding MAX-1 mutation rows therefore proves
    // overflow; unknown scans/mutations and materialization keep cost null.
    let overflow = explain_with_mutation_rows(u64::MAX - 1);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    let nodes = overflow.nodes();
    let unknown_scan_position = nodes
        .iter()
        .position(|node| node.object() == Some(&obj("table:UnknownTail")))
        .expect("unknown scan tail");
    let update_position = nodes
        .iter()
        .position(|node| node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned())))
        .expect("known partial mutation");
    assert!(unknown_scan_position > update_position);
    assert_eq!(nodes[unknown_scan_position].estimated_work(), None);
    let surface = serde_json::to_value(&overflow).expect("unknown mutation tail surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_rounds_complete_scan_bytes_independently_before_materialization() {
    let parsed = orna_syntax_v1::parse_module(ROUNDING_TAIL_INTERPLAY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 5);

    let explained = explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:complete-scan-rounding-tail"),
        source: obj("table:RoundingFirst"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: Some(1),
            estimated_bytes: Some(2_048),
            mutable_branch: None,
        }),
        joins: vec![QueryJoinDescription {
            source: obj("table:RoundingTail"),
            statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(1),
                estimated_bytes: Some(2_048),
                mutable_branch: None,
            }),
            predicate: None,
        }],
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: Some(obj("materialization:complete-scan-rounding-tail")),
    })
    .expect("complete scan estimates with materialized join output");

    // ORNA-PLAN-002/003 specify planner introspection and a correct fallback,
    // not the byte-cost unit. Keep the current 4-KiB rule explicit: each
    // half-block scan rounds on its own, and materialization prices the joined
    // one-block output separately.
    for table in ["table:RoundingFirst", "table:RoundingTail"] {
        let scan = explained
            .nodes()
            .iter()
            .find(|node| node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj(table)))
            .expect("fixture scan in complete-cost plan");
        assert_eq!(scan.estimated_rows(), Some(1));
        assert_eq!(scan.estimated_bytes(), Some(2_048));
        assert_eq!(scan.estimated_work(), Some(2));
    }
    let join = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Join)
        .expect("join of the two fixture scans");
    assert_eq!(join.estimated_rows(), Some(1));
    assert_eq!(join.estimated_bytes(), Some(4_096));
    assert_eq!(join.estimated_work(), Some(2));
    let materialize = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Materialize)
        .expect("materialized join output");
    assert_eq!(materialize.estimated_rows(), Some(1));
    assert_eq!(materialize.estimated_bytes(), Some(4_096));
    assert_eq!(materialize.estimated_work(), Some(2));
    assert_eq!(explained.plan().estimated_cost(), Some("8"));

    let surface = serde_json::to_value(&explained).expect("complete scan rounding surface");
    assert_eq!(surface["plan"]["estimated_cost"], "8");
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_closes_per_scan_rounding_at_zero_and_one_byte_join_tails() {
    let parsed = orna_syntax_v1::parse_module(ROUNDING_TAIL_INTERPLAY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 5);

    for (tail_bytes, tail_work, joined_bytes, expected_cost) in
        [(0, 1, 4_095, 7), (1, 2, 4_096, 8)]
    {
        let explained = explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:rounding-closure-edge"),
            source: obj("table:RoundingFirst"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(1),
                estimated_bytes: Some(4_095),
                mutable_branch: None,
            }),
            joins: vec![QueryJoinDescription {
                source: obj("table:RoundingTail"),
                statistics: Some(QuerySourceStatistics {
                    estimated_rows: Some(1),
                    estimated_bytes: Some(tail_bytes),
                    mutable_branch: None,
                }),
                predicate: None,
            }],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: Vec::new(),
            materialize_into: Some(obj("materialization:rounding-closure-edge")),
        })
        .expect("complete scan estimates at the positive-byte boundary");

        // ORNA-PLAN leaves byte-cost units unspecified. Under the existing
        // 4-KiB rule, a positive byte adds one block to that scan on its own;
        // 4095+1 bytes across scans therefore charge two blocks. Join output
        // is priced from its own combined cardinality before materialization.
        for (table, bytes, work) in [
            ("table:RoundingFirst", 4_095, 2),
            ("table:RoundingTail", tail_bytes, tail_work),
        ] {
            let scan = explained
                .nodes()
                .iter()
                .find(|node| {
                    node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj(table))
                })
                .expect("fixture scan in closure proof");
            assert_eq!(scan.estimated_rows(), Some(1));
            assert_eq!(scan.estimated_bytes(), Some(bytes));
            assert_eq!(scan.estimated_work(), Some(work));
        }
        let join = explained
            .nodes()
            .iter()
            .find(|node| node.kind() == PlanNodeKind::Join)
            .expect("fixture scan join");
        assert_eq!(join.estimated_rows(), Some(1));
        assert_eq!(join.estimated_bytes(), Some(joined_bytes));
        assert_eq!(join.estimated_work(), Some(2));
        let materialize = explained
            .nodes()
            .iter()
            .find(|node| node.kind() == PlanNodeKind::Materialize)
            .expect("materialization tail");
        assert_eq!(materialize.estimated_rows(), Some(1));
        assert_eq!(materialize.estimated_bytes(), Some(joined_bytes));
        assert_eq!(materialize.estimated_work(), Some(2));
        let expected_cost = expected_cost.to_string();
        assert_eq!(explained.plan().estimated_cost(), Some(expected_cost.as_str()));

        let surface = serde_json::to_value(&explained).expect("rounded closure surface");
        assert_eq!(surface["plan"]["estimated_cost"], expected_cost);
        assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
            node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
        }));
    }
}

#[test]
fn explain_keeps_per_scan_rounding_representable_at_max_byte_closure() {
    let parsed = orna_syntax_v1::parse_module(ROUNDING_TAIL_INTERPLAY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 5);

    let explain = |materialize| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:max-byte-rounding-closure"),
            source: obj("table:RoundingFirst"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(1),
                estimated_bytes: Some(u64::MAX),
                mutable_branch: None,
            }),
            joins: vec![QueryJoinDescription {
                source: obj("table:RoundingTail"),
                statistics: Some(QuerySourceStatistics {
                    estimated_rows: Some(1),
                    estimated_bytes: Some(1),
                    mutable_branch: None,
                }),
                predicate: None,
            }],
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: Vec::new(),
            materialize_into: materialize,
        })
        .expect("per-scan max-byte plan")
    };

    // ORNA-PLAN leaves byte-cost units unspecified. With the existing
    // 4-KiB-per-scan heuristic, MAX and 1 are rounded independently; their
    // raw byte sum cannot fit u64, so join output bytes become unknown without
    // making either scan's own work overflow.
    let exact = explain(None);
    let source_scan = exact
        .nodes()
        .iter()
        .find(|node| node.object() == Some(&obj("table:RoundingFirst")))
        .expect("MAX-byte source scan");
    assert_eq!(source_scan.estimated_bytes(), Some(u64::MAX));
    assert_eq!(source_scan.estimated_work(), Some(4_503_599_627_370_497));
    let tail_scan = exact
        .nodes()
        .iter()
        .find(|node| node.object() == Some(&obj("table:RoundingTail")))
        .expect("one-byte tail scan");
    assert_eq!(tail_scan.estimated_bytes(), Some(1));
    assert_eq!(tail_scan.estimated_work(), Some(2));
    let join = exact
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Join)
        .expect("join whose output width exceeds u64");
    assert_eq!(join.estimated_rows(), Some(1));
    assert_eq!(join.estimated_bytes(), None);
    assert_eq!(join.estimated_work(), Some(2));
    assert_eq!(exact.plan().estimated_cost(), Some("4503599627370501"));

    // Materialization needs a known output byte count. It makes the total
    // unknown while preserving the known scan subtotal without a false
    // overflow marker.
    let with_materialization = explain(Some(obj("materialization:max-byte-tail")));
    assert_eq!(with_materialization.plan().estimated_cost(), None);
    assert_eq!(
        with_materialization
            .root()
            .details()
            .get("estimated_cost_overflow"),
        None
    );
    assert_eq!(with_materialization.root().estimated_work(), None);
    assert!(with_materialization.nodes().iter().all(|node| {
        node.details().get("estimated_work_overflow").is_none()
    }));
    let surface = serde_json::to_value(&with_materialization)
        .expect("unknown materialization tail surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_closes_independent_half_block_remainders_across_scans() {
    let parsed = orna_syntax_v1::parse_module(ROUNDING_TAIL_INTERPLAY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 5);

    let explained = explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:rounding-remainder-closure"),
        source: obj("table:RoundingFirst"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: Some(1),
            estimated_bytes: Some(2_048),
            mutable_branch: None,
        }),
        joins: vec![QueryJoinDescription {
            source: obj("table:RoundingTail"),
            statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(1),
                estimated_bytes: Some(2_048),
                mutable_branch: None,
            }),
            predicate: None,
        }],
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: Some(obj("materialization:rounding-remainder-closure")),
    })
    .expect("rounded scan remainders close through join materialization");

    // ORNA-PLAN leaves byte-cost units unspecified. With the existing 4-KiB
    // rule, each half-block scan rounds up independently, while their joined
    // 4-KiB output is charged as one block.
    for table in ["table:RoundingFirst", "table:RoundingTail"] {
        let scan = explained
            .nodes()
            .iter()
            .find(|node| node.kind() == PlanNodeKind::Scan && node.object() == Some(&obj(table)))
            .expect("fixture scan in remainder proof");
        assert_eq!(scan.estimated_rows(), Some(1));
        assert_eq!(scan.estimated_bytes(), Some(2_048));
        assert_eq!(scan.estimated_work(), Some(2));
    }
    let join = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Join)
        .expect("joined remainder output");
    assert_eq!(join.estimated_rows(), Some(1));
    assert_eq!(join.estimated_bytes(), Some(4_096));
    assert_eq!(join.estimated_work(), Some(2));
    let materialize = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Materialize)
        .expect("materialization closes the rounded scans");
    assert_eq!(materialize.estimated_bytes(), Some(4_096));
    assert_eq!(materialize.estimated_work(), Some(2));
    assert_eq!(explained.plan().estimated_cost(), Some("8"));

    let surface = serde_json::to_value(&explained).expect("rounded remainder surface");
    assert_eq!(surface["plan"]["estimated_cost"], "8");
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_closes_max_byte_remainder_at_exact_scan_work_boundary() {
    let parsed = orna_syntax_v1::parse_module(ROUNDING_TAIL_INTERPLAY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 5);

    // ORNA-PLAN leaves byte-cost units unspecified. Under the existing 4-KiB
    // rule, MAX bytes have a 4095-byte remainder and charge 2^52 blocks.
    const MAX_BYTE_BLOCKS: u64 = 4_503_599_627_370_496;
    let closing_rows = u64::MAX - MAX_BYTE_BLOCKS;
    let explained = explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:max-byte-remainder-work-boundary"),
        source: obj("table:RoundingFirst"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: Some(closing_rows),
            estimated_bytes: Some(u64::MAX),
            mutable_branch: None,
        }),
        joins: vec![QueryJoinDescription {
            source: obj("table:RoundingTail"),
            statistics: None,
            predicate: None,
        }],
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    })
    .expect("max-byte remainder closes exactly at the scan work boundary");

    let source_scan = explained
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object() == Some(&obj("table:RoundingFirst"))
        })
        .expect("max-byte source scan");
    assert_eq!(source_scan.estimated_rows(), Some(closing_rows));
    assert_eq!(source_scan.estimated_bytes(), Some(u64::MAX));
    assert_eq!(source_scan.estimated_work(), Some(u64::MAX));

    let unknown_scan = explained
        .nodes()
        .iter()
        .find(|node| node.object() == Some(&obj("table:RoundingTail")))
        .expect("unknown fixture tail scan");
    assert_eq!(unknown_scan.estimated_rows(), None);
    assert_eq!(unknown_scan.estimated_bytes(), None);
    assert_eq!(unknown_scan.estimated_work(), None);
    assert_eq!(explained.plan().estimated_cost(), None);
    assert_eq!(explained.root().estimated_work(), None);
    assert_eq!(
        explained.root().details().get("estimated_cost_overflow"),
        None,
        "the unknown tail preserves an exact MAX subtotal without false overflow"
    );

    let surface = serde_json::to_value(&explained).expect("max-byte remainder surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_closes_max_byte_remainder_through_final_write_tail() {
    let parsed = orna_syntax_v1::parse_module(ROUNDING_TAIL_INTERPLAY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 5);

    // ORNA-PLAN leaves byte-cost units unspecified. Under the existing 4-KiB
    // rule, MAX bytes round to 2^52 blocks; these rows close the scan at MAX.
    const MAX_BYTE_BLOCKS: u64 = 4_503_599_627_370_496;
    let closing_rows = u64::MAX - MAX_BYTE_BLOCKS;
    let explain_with_write_bytes = |write_bytes| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:max-byte-remainder-write-tail"),
            source: obj("table:RoundingFirst"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(closing_rows),
                estimated_bytes: Some(u64::MAX),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![QueryMutationDescription {
                table: obj("table:RoundingWrite"),
                kind: QueryMutationKind::Update,
                estimated_affected_rows: Some(0),
                estimated_write_bytes: Some(write_bytes),
                estimated_table_rows_before: Some(1),
            }],
            materialize_into: None,
        })
        .expect("max-byte scan remainder followed by a final write tail")
    };

    let exact = explain_with_write_bytes(0);
    let max_cost = u64::MAX.to_string();
    assert_eq!(exact.plan().estimated_cost(), Some(max_cost.as_str()));
    let source_scan = exact
        .nodes()
        .iter()
        .find(|node| node.object() == Some(&obj("table:RoundingFirst")))
        .expect("max-byte source scan");
    assert_eq!(source_scan.estimated_work(), Some(u64::MAX));
    let exact_write = exact
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
        })
        .expect("zero-byte final write");
    assert_eq!(exact_write.estimated_bytes(), Some(0));
    assert_eq!(exact_write.estimated_work(), Some(0));
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);

    let overflow = explain_with_write_bytes(1);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(overflow.root().estimated_work(), Some(1));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the first rounded write byte exceeds the exact MAX scan subtotal"
    );
    let overflow_write = overflow
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
        })
        .expect("one-byte final write");
    assert_eq!(overflow_write.estimated_bytes(), Some(1));
    assert_eq!(overflow_write.estimated_work(), Some(1));
    let surface = serde_json::to_value(&overflow).expect("rounded final write surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_closes_max_byte_write_remainder_at_final_cost_tail() {
    let parsed = orna_syntax_v1::parse_module(ROUNDING_TAIL_INTERPLAY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 5);

    // ORNA-PLAN leaves byte-cost units unspecified. Under the existing 4-KiB
    // rule, MAX write bytes round to 2^52 blocks; the scan uses the remaining
    // work budget, so zero affected rows close exactly at MAX.
    const MAX_BYTE_BLOCKS: u64 = 4_503_599_627_370_496;
    let scan_rows = u64::MAX - MAX_BYTE_BLOCKS;
    let explain_with_write_rows = |affected_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:max-byte-write-cost-tail"),
            source: obj("table:RoundingFirst"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(scan_rows),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![QueryMutationDescription {
                table: obj("table:RoundingWrite"),
                kind: QueryMutationKind::Update,
                estimated_affected_rows: Some(affected_rows),
                estimated_write_bytes: Some(u64::MAX),
                estimated_table_rows_before: Some(1),
            }],
            materialize_into: None,
        })
        .expect("max-byte write remainder at the final cost boundary")
    };

    let exact = explain_with_write_rows(0);
    let max_cost = u64::MAX.to_string();
    assert_eq!(exact.plan().estimated_cost(), Some(max_cost.as_str()));
    let scan = exact
        .nodes()
        .iter()
        .find(|node| node.object() == Some(&obj("table:RoundingFirst")))
        .expect("fixture source scan");
    assert_eq!(scan.estimated_rows(), Some(scan_rows));
    assert_eq!(scan.estimated_bytes(), Some(0));
    assert_eq!(scan.estimated_work(), Some(scan_rows));
    let write = exact
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
        })
        .expect("max-byte final write");
    assert_eq!(write.estimated_rows(), Some(0));
    assert_eq!(write.estimated_bytes(), Some(u64::MAX));
    assert_eq!(write.estimated_work(), Some(MAX_BYTE_BLOCKS));
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);

    let overflow = explain_with_write_rows(1);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(overflow.root().estimated_work(), Some(MAX_BYTE_BLOCKS + 1));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "one affected row exceeds the exact scan plus rounded write subtotal"
    );
    let overflow_write = overflow
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
        })
        .expect("max-byte final write with one affected row");
    assert_eq!(overflow_write.estimated_rows(), Some(1));
    assert_eq!(overflow_write.estimated_bytes(), Some(u64::MAX));
    assert_eq!(overflow_write.estimated_work(), Some(MAX_BYTE_BLOCKS + 1));
    assert_eq!(overflow_write.details().get("estimated_work_overflow"), None);
    let surface = serde_json::to_value(&overflow).expect("max-byte write remainder surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_closes_max_byte_write_rounding_at_local_work_boundary() {
    let parsed = orna_syntax_v1::parse_module(ROUNDING_TAIL_INTERPLAY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 5);

    // ORNA-PLAN leaves byte-cost units unspecified. Under the existing 4-KiB
    // rule, MAX write bytes round to 2^52 blocks; this row count closes the
    // local write work at MAX, and one more affected row crosses that edge.
    const MAX_BYTE_BLOCKS: u64 = 4_503_599_627_370_496;
    let closing_rows = u64::MAX - MAX_BYTE_BLOCKS;
    let explain_with_affected_rows = |affected_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:max-byte-write-local-boundary"),
            source: obj("table:RoundingFirst"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(0),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![QueryMutationDescription {
                table: obj("table:RoundingWrite"),
                kind: QueryMutationKind::Update,
                estimated_affected_rows: Some(affected_rows),
                estimated_write_bytes: Some(u64::MAX),
                estimated_table_rows_before: Some(1),
            }],
            materialize_into: None,
        })
        .expect("max-byte write rounding at its local work boundary")
    };

    let exact = explain_with_affected_rows(closing_rows);
    let max_cost = u64::MAX.to_string();
    assert_eq!(exact.plan().estimated_cost(), Some(max_cost.as_str()));
    let write = exact
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
        })
        .expect("exact max-byte write");
    assert_eq!(write.estimated_rows(), Some(closing_rows));
    assert_eq!(write.estimated_bytes(), Some(u64::MAX));
    assert_eq!(write.estimated_work(), Some(u64::MAX));
    assert_eq!(write.details().get("estimated_work_overflow"), None);
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);

    let overflow = explain_with_affected_rows(closing_rows + 1);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(overflow.root().estimated_work(), None);
    assert_eq!(
        overflow.root().details().get("estimated_work_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "one more affected row exceeds representable write work"
    );
    assert_eq!(overflow.root().details().get("estimated_cost_overflow"), None);
    let surface = serde_json::to_value(&overflow).expect("max-byte local overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_work_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_closes_aligned_and_remainder_max_byte_write_edges() {
    let parsed = orna_syntax_v1::parse_module(ROUNDING_TAIL_INTERPLAY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 5);

    // ORNA-PLAN leaves byte-cost units unspecified. Under the existing 4-KiB
    // rule, MAX ends with a 4095-byte remainder and rounds to 2^52 blocks;
    // the aligned value just below it rounds to one fewer block.
    const MAX_BYTE_BLOCKS: u64 = 4_503_599_627_370_496;
    let aligned_bytes = u64::MAX - 4_095;
    let aligned_rows = u64::MAX - (MAX_BYTE_BLOCKS - 1);
    let remainder_rows = u64::MAX - MAX_BYTE_BLOCKS;
    assert_eq!(aligned_bytes % 4_096, 0);

    let explain_write = |affected_rows, write_bytes| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:aligned-max-byte-write-edge"),
            source: obj("table:RoundingFirst"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(0),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![QueryMutationDescription {
                table: obj("table:RoundingWrite"),
                kind: QueryMutationKind::Update,
                estimated_affected_rows: Some(affected_rows),
                estimated_write_bytes: Some(write_bytes),
                estimated_table_rows_before: Some(1),
            }],
            materialize_into: None,
        })
        .expect("aligned or remainder write closure edge")
    };

    let aligned = explain_write(aligned_rows, aligned_bytes);
    let max_cost = u64::MAX.to_string();
    assert_eq!(aligned.plan().estimated_cost(), Some(max_cost.as_str()));
    let aligned_write = aligned.root();
    assert_eq!(aligned_write.kind(), PlanNodeKind::Invoke);
    assert_eq!(aligned_write.estimated_rows(), Some(aligned_rows));
    assert_eq!(aligned_write.estimated_bytes(), Some(aligned_bytes));
    assert_eq!(aligned_write.estimated_work(), Some(u64::MAX));

    let remainder = explain_write(remainder_rows, u64::MAX);
    assert_eq!(remainder.plan().estimated_cost(), Some(max_cost.as_str()));
    let remainder_write = remainder.root();
    assert_eq!(remainder_write.estimated_rows(), Some(remainder_rows));
    assert_eq!(remainder_write.estimated_bytes(), Some(u64::MAX));
    assert_eq!(remainder_write.estimated_work(), Some(u64::MAX));

    let overflow = explain_write(remainder_rows + 1, u64::MAX);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(overflow.root().estimated_work(), None);
    assert_eq!(
        overflow.root().details().get("estimated_work_overflow"),
        Some(&PlanDetail::Boolean(true))
    );
    let surface = serde_json::to_value(&overflow).expect("write remainder overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_work_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_closes_max_byte_write_remainder_through_materialization_tail() {
    let parsed = orna_syntax_v1::parse_module(ROUNDING_TAIL_INTERPLAY);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 5);

    // ORNA-PLAN leaves byte-cost units unspecified. Under the existing 4-KiB
    // rule, MAX write bytes round to 2^52 blocks. The scan leaves room for one
    // such write and one matching materialization at the exact MAX boundary.
    const MAX_BYTE_BLOCKS: u64 = 4_503_599_627_370_496;
    let scan_rows = u64::MAX - (2 * MAX_BYTE_BLOCKS);
    let explain_with_affected_rows = |affected_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:max-byte-write-materialize-tail"),
            source: obj("table:RoundingFirst"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(scan_rows),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![QueryMutationDescription {
                table: obj("table:RoundingWrite"),
                kind: QueryMutationKind::Update,
                estimated_affected_rows: Some(affected_rows),
                estimated_write_bytes: Some(u64::MAX),
                estimated_table_rows_before: Some(1),
            }],
            materialize_into: Some(obj("materialization:rounding-write-tail")),
        })
        .expect("max-byte write remainder closes through materialization")
    };

    let exact = explain_with_affected_rows(0);
    let max_cost = u64::MAX.to_string();
    assert_eq!(exact.plan().estimated_cost(), Some(max_cost.as_str()));
    let scan = exact
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object() == Some(&obj("table:RoundingFirst"))
        })
        .expect("fixture source scan");
    assert_eq!(scan.estimated_work(), Some(scan_rows));
    let write = exact
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
        })
        .expect("max-byte write");
    assert_eq!(write.estimated_rows(), Some(0));
    assert_eq!(write.estimated_bytes(), Some(u64::MAX));
    assert_eq!(write.estimated_work(), Some(MAX_BYTE_BLOCKS));
    assert_eq!(exact.root().kind(), PlanNodeKind::Materialize);
    assert_eq!(exact.root().estimated_rows(), Some(0));
    assert_eq!(exact.root().estimated_bytes(), Some(u64::MAX));
    assert_eq!(exact.root().estimated_work(), Some(MAX_BYTE_BLOCKS));

    let overflow = explain_with_affected_rows(1);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(overflow.root().estimated_work(), Some(MAX_BYTE_BLOCKS + 1));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the materialization tail pushes the rounded write total past MAX"
    );
    assert_eq!(overflow.root().details().get("estimated_work_overflow"), None);
    let overflow_write = overflow
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
        })
        .expect("one-row max-byte write");
    assert_eq!(overflow_write.estimated_work(), Some(MAX_BYTE_BLOCKS + 1));
    let surface = serde_json::to_value(&overflow).expect("materialized write overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_closes_one_row_max_byte_write_at_materialization_cost_tail() {
    let parsed = orna_syntax_v1::parse_module(WRITE_MATERIALIZATION_EDGE_TAIL);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 3);

    // ORNA-PLAN leaves byte-cost units unspecified. Under the existing 4-KiB
    // rule, the MAX-byte write and its materialization each cost 2^52 blocks.
    // One affected row adds one unit to each, so reserve both row units in the
    // scan estimate to close the complete plan exactly at u64::MAX.
    const MAX_BYTE_BLOCKS: u64 = 4_503_599_627_370_496;
    let scan_rows = u64::MAX - (2 * MAX_BYTE_BLOCKS) - 2;
    let explain_with_affected_rows = |affected_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:max-byte-write-materialization-edge-tail"),
            source: obj("table:MaterializedWriteSource"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(scan_rows),
                estimated_bytes: Some(0),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![QueryMutationDescription {
                table: obj("table:MaterializedWriteTarget"),
                kind: QueryMutationKind::Update,
                estimated_affected_rows: Some(affected_rows),
                estimated_write_bytes: Some(u64::MAX),
                estimated_table_rows_before: Some(1),
            }],
            materialize_into: Some(obj("materialization:max-byte-write-edge-tail")),
        })
        .expect("max-byte write with a materialization row tail")
    };

    let exact = explain_with_affected_rows(1);
    let max_cost = u64::MAX.to_string();
    assert_eq!(exact.plan().estimated_cost(), Some(max_cost.as_str()));
    let scan = exact
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object() == Some(&obj("table:MaterializedWriteSource"))
        })
        .expect("fixture source scan");
    assert_eq!(scan.estimated_work(), Some(scan_rows));
    let write = exact
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
        })
        .expect("one-row max-byte write");
    assert_eq!(write.estimated_rows(), Some(1));
    assert_eq!(write.estimated_bytes(), Some(u64::MAX));
    assert_eq!(write.estimated_work(), Some(MAX_BYTE_BLOCKS + 1));
    assert_eq!(exact.root().kind(), PlanNodeKind::Materialize);
    assert_eq!(exact.root().estimated_rows(), Some(1));
    assert_eq!(exact.root().estimated_bytes(), Some(u64::MAX));
    assert_eq!(exact.root().estimated_work(), Some(MAX_BYTE_BLOCKS + 1));
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);

    let overflow = explain_with_affected_rows(2);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(overflow.root().estimated_rows(), Some(2));
    assert_eq!(overflow.root().estimated_work(), Some(MAX_BYTE_BLOCKS + 2));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the second affected row crosses the aggregate MAX boundary"
    );
    assert_eq!(overflow.root().details().get("estimated_work_overflow"), None);
    let overflow_write = overflow
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
        })
        .expect("two-row max-byte write");
    assert_eq!(overflow_write.estimated_work(), Some(MAX_BYTE_BLOCKS + 2));
    let surface = serde_json::to_value(&overflow).expect("materialization cost-tail surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_closes_source_byte_remainder_with_max_byte_write_materialization_tail() {
    let parsed = orna_syntax_v1::parse_module(WRITE_MATERIALIZATION_ROUNDING_TAIL);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 3);

    // ORNA-PLAN leaves byte-cost units unspecified. Keep the established
    // 4-KiB heuristic: the 4095-byte scan tail rounds to one work unit, while
    // the MAX-byte write and its materialization each round to 2^52 blocks.
    // Reserve that scan unit and both affected-row units to close the total.
    const MAX_BYTE_BLOCKS: u64 = 4_503_599_627_370_496;
    const SOURCE_TAIL_BYTES: u64 = 4_095;
    let scan_rows = u64::MAX - (2 * MAX_BYTE_BLOCKS) - 3;
    let explain_with_affected_rows = |affected_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive(
                "snapshot:source-byte-max-write-materialization-tail",
            ),
            source: obj("table:MaterializedRoundingSource"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(scan_rows),
                estimated_bytes: Some(SOURCE_TAIL_BYTES),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![QueryMutationDescription {
                table: obj("table:MaterializedRoundingTarget"),
                kind: QueryMutationKind::Update,
                estimated_affected_rows: Some(affected_rows),
                estimated_write_bytes: Some(u64::MAX),
                estimated_table_rows_before: Some(1),
            }],
            materialize_into: Some(obj("materialization:source-byte-max-write-tail")),
        })
        .expect("source scan byte tail plus max-byte write materialization")
    };

    let exact = explain_with_affected_rows(1);
    let max_cost = u64::MAX.to_string();
    assert_eq!(exact.plan().estimated_cost(), Some(max_cost.as_str()));
    let scan = exact
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object() == Some(&obj("table:MaterializedRoundingSource"))
        })
        .expect("fixture source scan");
    assert_eq!(scan.estimated_rows(), Some(scan_rows));
    assert_eq!(scan.estimated_bytes(), Some(SOURCE_TAIL_BYTES));
    assert_eq!(scan.estimated_work(), Some(scan_rows + 1));
    let write = exact
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
        })
        .expect("one-row max-byte write");
    assert_eq!(write.estimated_rows(), Some(1));
    assert_eq!(write.estimated_bytes(), Some(u64::MAX));
    assert_eq!(write.estimated_work(), Some(MAX_BYTE_BLOCKS + 1));
    assert_eq!(exact.root().kind(), PlanNodeKind::Materialize);
    assert_eq!(exact.root().estimated_rows(), Some(1));
    assert_eq!(exact.root().estimated_bytes(), Some(u64::MAX));
    assert_eq!(exact.root().estimated_work(), Some(MAX_BYTE_BLOCKS + 1));
    assert_eq!(exact.root().details().get("estimated_cost_overflow"), None);

    let overflow = explain_with_affected_rows(2);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(overflow.root().estimated_work(), Some(MAX_BYTE_BLOCKS + 2));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "the extra write row and materialization row exceed the scan-adjusted MAX"
    );
    assert_eq!(overflow.root().details().get("estimated_work_overflow"), None);
    let overflow_write = overflow
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
        })
        .expect("two-row max-byte write");
    assert_eq!(overflow_write.estimated_work(), Some(MAX_BYTE_BLOCKS + 2));
    assert_eq!(overflow_write.details().get("estimated_work_overflow"), None);
    let surface = serde_json::to_value(&overflow)
        .expect("source-byte materialized write overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_closes_max_source_byte_remainder_with_materialized_max_byte_write_tail() {
    let parsed = orna_syntax_v1::parse_module(MAX_SOURCE_MATERIALIZED_WRITE_TAIL);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 3);

    // ORNA-PLAN leaves byte-cost units unspecified. Under the established
    // 4-KiB heuristic, MAX source bytes, MAX write bytes, and MAX materialized
    // bytes each charge 2^52 blocks. Reserve all three charges in the scan's
    // row estimate so their combined work closes exactly at u64::MAX.
    const MAX_BYTE_BLOCKS: u64 = 4_503_599_627_370_496;
    let scan_rows = u64::MAX - (3 * MAX_BYTE_BLOCKS);
    let explain_with_affected_rows = |affected_rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive("snapshot:max-source-materialized-max-write-tail"),
            source: obj("table:MaxByteMaterializedSource"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(scan_rows),
                estimated_bytes: Some(u64::MAX),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: vec![QueryMutationDescription {
                table: obj("table:MaxByteMaterializedTarget"),
                kind: QueryMutationKind::Update,
                estimated_affected_rows: Some(affected_rows),
                estimated_write_bytes: Some(u64::MAX),
                estimated_table_rows_before: Some(1),
            }],
            materialize_into: Some(obj("materialization:max-source-max-write-tail")),
        })
        .expect("max source and write byte remainders through materialization")
    };

    let exact = explain_with_affected_rows(0);
    let max_cost = u64::MAX.to_string();
    assert_eq!(exact.plan().estimated_cost(), Some(max_cost.as_str()));
    let scan = exact
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object() == Some(&obj("table:MaxByteMaterializedSource"))
        })
        .expect("fixture MAX-byte source scan");
    assert_eq!(scan.estimated_rows(), Some(scan_rows));
    assert_eq!(scan.estimated_bytes(), Some(u64::MAX));
    assert_eq!(scan.estimated_work(), Some(scan_rows + MAX_BYTE_BLOCKS));
    let write = exact
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
        })
        .expect("zero-row MAX-byte write");
    assert_eq!(write.estimated_rows(), Some(0));
    assert_eq!(write.estimated_bytes(), Some(u64::MAX));
    assert_eq!(write.estimated_work(), Some(MAX_BYTE_BLOCKS));
    assert_eq!(exact.root().kind(), PlanNodeKind::Materialize);
    assert_eq!(exact.root().estimated_rows(), Some(0));
    assert_eq!(exact.root().estimated_bytes(), Some(u64::MAX));
    assert_eq!(exact.root().estimated_work(), Some(MAX_BYTE_BLOCKS));

    let overflow = explain_with_affected_rows(1);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(overflow.root().estimated_work(), Some(MAX_BYTE_BLOCKS + 1));
    assert_eq!(
        overflow.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "one affected row adds work to both the write and materialization tails"
    );
    assert_eq!(overflow.root().details().get("estimated_work_overflow"), None);
    let overflow_write = overflow
        .nodes()
        .iter()
        .find(|node| {
            node.details().get("mutation") == Some(&PlanDetail::Text("update".to_owned()))
        })
        .expect("one-row MAX-byte write");
    assert_eq!(overflow_write.estimated_work(), Some(MAX_BYTE_BLOCKS + 1));
    assert_eq!(overflow_write.details().get("estimated_work_overflow"), None);
    let surface = serde_json::to_value(&overflow).expect("MAX source/write overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_keeps_max_source_materialization_at_local_work_boundary() {
    let parsed = orna_syntax_v1::parse_module(MAX_SOURCE_MATERIALIZATION_CLOSURE_TAIL);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // ORNA-PLAN leaves byte-cost units unspecified. Under the existing 4-KiB
    // heuristic, MAX bytes charge 2^52 blocks. Choosing the remaining rows so
    // both the source scan and materialization each end exactly at MAX proves
    // that their aggregate overflow is reported on the plan, not on either
    // locally representable node.
    const MAX_BYTE_BLOCKS: u64 = 4_503_599_627_370_496;
    let closing_rows = u64::MAX - MAX_BYTE_BLOCKS;
    let explained = explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:max-source-materialization-local-boundary"),
        source: obj("table:MaxSourceMaterialization"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: Some(closing_rows),
            estimated_bytes: Some(u64::MAX),
            mutable_branch: None,
        }),
        joins: Vec::new(),
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: Some(obj("materialization:max-source-local-boundary")),
    })
    .expect("MAX source bytes close through a materialization tail");

    assert_eq!(explained.plan().estimated_cost(), None);
    let scan = explained
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Scan
                && node.object() == Some(&obj("table:MaxSourceMaterialization"))
        })
        .expect("fixture MAX-byte source scan");
    assert_eq!(scan.estimated_rows(), Some(closing_rows));
    assert_eq!(scan.estimated_bytes(), Some(u64::MAX));
    assert_eq!(scan.estimated_work(), Some(u64::MAX));
    assert_eq!(scan.details().get("estimated_work_overflow"), None);

    assert_eq!(explained.root().kind(), PlanNodeKind::Materialize);
    assert_eq!(explained.root().estimated_rows(), Some(closing_rows));
    assert_eq!(explained.root().estimated_bytes(), Some(u64::MAX));
    assert_eq!(explained.root().estimated_work(), Some(u64::MAX));
    assert_eq!(
        explained.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true)),
        "two exact MAX local contributions overflow only at the aggregate"
    );
    assert_eq!(explained.root().details().get("estimated_work_overflow"), None);

    let surface = serde_json::to_value(&explained).expect("max-source materialization surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(
        surface["nodes"][0]["details"]["estimated_work"],
        serde_json::json!(u64::MAX)
    );
    assert_eq!(surface["nodes"][0]["details"]["estimated_cost_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("actual_rows").is_none() && node.get("actual_bytes").is_none()
    }));
}

#[test]
fn explain_marks_max_source_rounding_overflow_on_materialization_closure_tail() {
    let parsed = orna_syntax_v1::parse_module(MAX_SOURCE_MATERIALIZATION_OVERFLOW_TAIL);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    // ORNA-PLAN leaves byte-cost units unspecified. Under the established
    // 4-KiB heuristic, MAX bytes round to 2^52 blocks. The exact row boundary
    // gives both scan and materialization local work of MAX; one more row
    // makes each local sum unrepresentable, which stays distinct from an
    // aggregate overflow computed from exact local contributions.
    const MAX_BYTE_BLOCKS: u64 = 4_503_599_627_370_496;
    let closing_rows = u64::MAX - MAX_BYTE_BLOCKS;
    let explain_with_rows = |rows| {
        explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive(
                "snapshot:max-source-materialization-rounding-overflow-tail",
            ),
            source: obj("table:MaxSourceMaterializationOverflow"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(rows),
                estimated_bytes: Some(u64::MAX),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: None,
            mutations: Vec::new(),
            materialize_into: Some(obj("materialization:max-source-rounding-overflow")),
        })
        .expect("MAX source materialization closure edge")
    };

    let exact = explain_with_rows(closing_rows);
    assert_eq!(exact.plan().estimated_cost(), None);
    assert_eq!(exact.nodes()[0].estimated_work(), Some(u64::MAX));
    assert_eq!(exact.nodes()[1].estimated_work(), Some(u64::MAX));
    assert_eq!(
        exact.root().details().get("estimated_cost_overflow"),
        Some(&PlanDetail::Boolean(true))
    );

    let overflow = explain_with_rows(closing_rows + 1);
    assert_eq!(overflow.plan().estimated_cost(), None);
    assert_eq!(overflow.root().kind(), PlanNodeKind::Materialize);
    for node in overflow.nodes() {
        assert_eq!(node.estimated_work(), None);
        assert_eq!(
            node.details().get("estimated_work_overflow"),
            Some(&PlanDetail::Boolean(true)),
            "the extra row crosses the MAX-plus-rounded-source-bytes boundary"
        );
    }
    assert_eq!(overflow.root().details().get("estimated_cost_overflow"), None);

    let surface = serde_json::to_value(&overflow)
        .expect("max-source materialization local overflow surface");
    assert!(surface["plan"].get("estimated_cost").is_none());
    assert_eq!(surface["nodes"][0]["details"]["estimated_work_overflow"], true);
    assert_eq!(surface["nodes"][1]["details"]["estimated_work_overflow"], true);
    assert!(surface["nodes"].as_array().unwrap().iter().all(|node| {
        node.get("estimated_work").is_none()
            && node.get("actual_rows").is_none()
            && node.get("actual_bytes").is_none()
    }));
}
