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
