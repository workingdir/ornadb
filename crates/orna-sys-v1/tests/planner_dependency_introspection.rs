use std::collections::BTreeSet;

use orna_sys_v1::{
    DependencyConfidence, DependencyGraph, DependencyGraphError, DependencyInput, DependencyKind,
    ExplainError,
    FileRef, FunctionPlanDescription, FunctionRef, ObjectRef, PlanNodeKind, PlanOrdering,
    PlanSortDirection, PlanNullOrder, QueryPlanDescription, SnapshotRef, MAX_PLAN_EXPRESSIONS,
    SystemEffect, explain_function, explain_query, system_function_descriptor, SourceSpan,
};
use serde_json::Value;

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
        predicate: Some(orna_sys_v1::ExpressionRef::descriptive("expr:active")),
        projections: vec![orna_sys_v1::ExpressionRef::descriptive("expr:name")],
        distinct: true,
        ordering: vec![PlanOrdering {
            expression: orna_sys_v1::ExpressionRef::descriptive("expr:name"),
            direction: PlanSortDirection::Ascending,
            null_order: PlanNullOrder::Last,
        }],
        limit: Some(10),
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
        predicate: Some(orna_sys_v1::ExpressionRef::descriptive("expr:active")),
        projections: (0..MAX_PLAN_EXPRESSIONS)
            .map(|position| {
                orna_sys_v1::ExpressionRef::descriptive(format!("expr:{position}"))
            })
            .collect(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
    };
    assert_eq!(
        explain_query(&over_limit),
        Err(ExplainError::TooManyExpressions)
    );

    let invalid_expression = QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:one"),
        source: obj("table:contacts"),
        predicate: Some(orna_sys_v1::ExpressionRef::descriptive("\n")),
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
    };
    assert_eq!(
        explain_query(&invalid_expression),
        Err(ExplainError::InvalidExpression)
    );
}
