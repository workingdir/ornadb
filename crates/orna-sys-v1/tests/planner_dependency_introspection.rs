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
