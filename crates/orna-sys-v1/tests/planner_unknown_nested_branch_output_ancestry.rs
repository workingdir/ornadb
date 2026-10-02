use orna_sys_v1::{
    DisjunctStormBranchDescription, DisjunctStormCascadeDescription,
    DisjunctStormLimitRebindDescription, ExpressionRef, ObjectRef, PlanByteCapScopeSegment,
    PlanDetail, PlanNodeKind, QueryPlanDescription, SnapshotRef,
    explain_query_with_disjunct_storm_branch_limit_chains,
};

const FIXTURE: &str = include_str!("fixtures/planner_unknown_nested_branch_output_ancestry.orna");

fn branch(
    limits: &[u64],
    conjuncts: u64,
    rebinds: Vec<DisjunctStormLimitRebindDescription>,
) -> DisjunctStormBranchDescription {
    DisjunctStormBranchDescription {
        nested_limits: limits.to_vec(),
        conjunct_count: conjuncts,
        limit_rebinds: rebinds,
        nested_storms: Vec::new(),
    }
}

fn storm(
    predicate: &str,
    branches: Vec<DisjunctStormBranchDescription>,
) -> DisjunctStormCascadeDescription {
    DisjunctStormCascadeDescription {
        predicate: ExpressionRef::descriptive(predicate),
        branches,
    }
}

fn rebind(
    after_limit: usize,
    storms: Vec<DisjunctStormCascadeDescription>,
) -> DisjunctStormLimitRebindDescription {
    DisjunctStormLimitRebindDescription {
        after_limit,
        storms,
    }
}

fn query() -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:planner-unknown-branch-output-ancestry"),
        source: ObjectRef::descriptive("table:PlannerUnknownNestedBranchOutputAncestry"),
        source_statistics: None,
        joins: Vec::new(),
        predicate: None,
        projections: Vec::new(),
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    }
}

#[test]
fn unknown_nested_storm_routes_retain_bounded_branch_output_ancestry() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let first_nested = storm(
        "expr:first-nested-stage",
        vec![branch(
            &[14, 7],
            1,
            vec![rebind(
                2,
                vec![storm(
                    "expr:first-nested-rebind",
                    vec![branch(&[3], 1, vec![])],
                )],
            )],
        )],
    );
    let second_nested = storm(
        "expr:second-nested-stage",
        vec![branch(
            &[12],
            1,
            vec![rebind(
                1,
                vec![storm(
                    "expr:second-nested-rebind",
                    vec![branch(&[5], 1, vec![])],
                )],
            )],
        )],
    );
    let mut parent = branch(
        &[90, 40],
        2,
        vec![
            rebind(
                1,
                vec![storm(
                    "expr:parent-first-rebind",
                    vec![branch(&[60], 1, vec![])],
                )],
            ),
            rebind(
                2,
                vec![storm(
                    "expr:parent-second-rebind",
                    vec![branch(&[30], 1, vec![])],
                )],
            ),
        ],
    );
    parent.nested_storms = vec![first_nested, second_nested];

    // The reference does not specify a separate named scope for this
    // intermediate result. Nested storms consume each parent's bounded
    // branch output after its limit/rebind chain and conjuncts.
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(),
        &[storm("expr:unknown-parent-stage", vec![parent])],
        &[],
    )
    .expect("nested routes retain provenance without row or byte estimates");
    let filter = explained
        .nodes()
        .iter()
        .find(|node| node.kind() == PlanNodeKind::Filter)
        .expect("parent storm is visible");
    assert_eq!(filter.estimated_rows(), None);
    assert_eq!(filter.estimated_bytes(), None);
    assert_eq!(
        filter.details().get("nested_storm_input_scope"),
        Some(&PlanDetail::Text(
            "bounded_branch_output_after_limits_rebinds_and_conjuncts_then_prior_nested_storm_outputs"
                .to_owned()
        ))
    );

    let routes = match filter
        .details()
        .get("limit_chain_rebind_byte_cap_handoff_route_records")
    {
        Some(PlanDetail::ByteCapHandoffRoutes(routes)) => routes,
        other => panic!("expected typed byte-cap handoff routes, got {other:?}"),
    };
    assert_eq!(routes.len(), 4);
    assert!(
        routes
            .iter()
            .all(|route| route.input_bytes.is_none() && route.output_bytes.is_none())
    );
    assert_eq!(routes[0].depth, 1);
    assert_eq!(routes[1].depth, 1);
    assert_eq!(routes[2].depth, 2);
    assert_eq!(routes[3].depth, 2);

    let mut bounded_parent_output = routes[1].output_path.clone();
    bounded_parent_output.extend([
        PlanByteCapScopeSegment::RebindCascadeOutput {
            position: 2,
            index: 1,
        },
        PlanByteCapScopeSegment::BranchOutput { index: 1 },
    ]);
    let mut first_nested_input = bounded_parent_output.clone();
    first_nested_input.extend([
        PlanByteCapScopeSegment::NestedStorm { index: 1 },
        PlanByteCapScopeSegment::Branch { index: 1 },
        PlanByteCapScopeSegment::Limit { position: 1 },
        PlanByteCapScopeSegment::Limit { position: 2 },
    ]);
    assert_eq!(routes[2].input_path, first_nested_input);

    let mut second_nested_input = bounded_parent_output;
    second_nested_input.extend([
        PlanByteCapScopeSegment::NestedStorm { index: 1 },
        PlanByteCapScopeSegment::NestedStormOutput { index: 1 },
        PlanByteCapScopeSegment::NestedStorm { index: 2 },
        PlanByteCapScopeSegment::Branch { index: 1 },
        PlanByteCapScopeSegment::Limit { position: 1 },
    ]);
    assert_eq!(routes[3].input_path, second_nested_input);

    let route_scope = |path: &[PlanByteCapScopeSegment]| {
        std::iter::once("root".to_owned())
            .chain(path.iter().map(PlanByteCapScopeSegment::scope_component_label))
            .collect::<Vec<_>>()
            .join("/")
    };
    for route in routes {
        let input_scope = route_scope(&route.input_path);
        let output_scope = route_scope(&route.output_path);
        assert_eq!(route.input_scope_label(), input_scope);
        assert_eq!(route.output_scope_label(), output_scope);
        assert_eq!(
            route.paired_scope_label(),
            format!("{input_scope}=>{output_scope}")
        );
        let serialized = serde_json::to_value(route).expect("typed ancestry route serializes");
        assert_eq!(serialized["input_scope"], input_scope);
        assert_eq!(serialized["output_scope"], output_scope);
        assert_eq!(
            serialized["paired_scope_label"],
            format!("{input_scope}=>{output_scope}")
        );
    }

    let serialized = serde_json::to_value(&explained).expect("unknown plan serializes");
    let serialized_filter = serialized["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["details"]["disjunct_storm"] == 1)
        .expect("parent storm serializes");
    assert!(serialized_filter.get("estimated_rows").is_none());
    assert!(serialized_filter.get("estimated_bytes").is_none());
    let serialized_routes =
        serialized_filter["details"]["limit_chain_rebind_byte_cap_handoff_route_records"]
            .as_array()
            .expect("route records serialize");
    assert!(
        serialized_routes[2]["input_path"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!({ "kind": "branch_output", "index": 1 }))
    );
    assert!(serialized_routes[2]["input_bytes"].is_null());
    assert!(serialized_routes[2]["output_bytes"].is_null());
}
