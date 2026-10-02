use orna_sys_v1::{
    DisjunctStormBranchDescription, DisjunctStormCascadeDescription,
    DisjunctStormLimitRebindDescription, ExpressionRef, ObjectRef, PlanByteCapScopeSegment,
    PlanDetail, PlanNodeKind, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query_with_disjunct_storm_branch_limit_chains,
};

const FIXTURE: &str = include_str!("fixtures/planner_paired_depth_post_storm_route_stability.orna");

fn branch(
    limits: &[u64],
    rebinds: Vec<DisjunctStormLimitRebindDescription>,
    nested_storms: Vec<DisjunctStormCascadeDescription>,
) -> DisjunctStormBranchDescription {
    DisjunctStormBranchDescription {
        nested_limits: limits.to_vec(),
        conjunct_count: 1,
        limit_rebinds: rebinds,
        nested_storms,
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

fn rebind(after_limit: usize, label: &str, cap: u64) -> DisjunctStormLimitRebindDescription {
    DisjunctStormLimitRebindDescription {
        after_limit,
        storms: vec![storm(
            &format!("expr:{label}"),
            vec![branch(&[cap], Vec::new(), Vec::new())],
        )],
    }
}

fn nested_stage(label: &str, limits: &[u64], leaf_cap: u64) -> DisjunctStormCascadeDescription {
    storm(
        &format!("expr:{label}"),
        vec![branch(
            limits,
            vec![rebind(limits.len(), &format!("{label}-rebind"), leaf_cap)],
            Vec::new(),
        )],
    )
}

fn top_level_stage(
    label: &str,
    limits: &[u64],
    nested_caps: [u64; 2],
) -> DisjunctStormCascadeDescription {
    // The route order contract is independent of the nesting depth where a
    // route was discovered: paths at depth one and two are grouped together
    // by depth, then ordered by typed ancestry.
    let rebinds = vec![
        rebind(1, &format!("{label}-first-limit"), nested_caps[1]),
        rebind(2, &format!("{label}-second-limit"), nested_caps[0]),
    ];
    let nested_storms = vec![
        nested_stage(&format!("{label}-nested-first"), &[16, 8], 4),
        nested_stage(&format!("{label}-nested-second"), &[12, 6], 3),
    ];
    storm(
        &format!("expr:{label}"),
        vec![branch(limits, rebinds, nested_storms)],
    )
}

fn query() -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:planner-paired-depth-route-stability"),
        source: ObjectRef::descriptive("table:PlannerPairedDepthPostStormRouteStability"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: Some(1_000),
            estimated_bytes: Some(8_192),
            mutable_branch: None,
        }),
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

fn routes_for_stage<'a>(
    explained: &'a orna_sys_v1::ExplainedPlan,
    stage: u64,
) -> &'a [orna_sys_v1::PlanByteCapHandoffRoute] {
    let filter = explained
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Filter
                && node.details().get("disjunct_storm") == Some(&PlanDetail::Integer(stage))
        })
        .expect("storm stage is visible");
    match filter
        .details()
        .get("limit_chain_rebind_byte_cap_handoff_route_records")
    {
        Some(PlanDetail::ByteCapHandoffRoutes(routes)) => routes,
        other => panic!("expected typed byte-cap route records, got {other:?}"),
    }
}

#[test]
fn paired_depth_routes_are_stable_across_nested_and_post_storm_ancestry() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let stages = vec![
        top_level_stage("first-stage", &[120, 60], [20, 40]),
        top_level_stage("post-storm-stage", &[96, 48], [16, 32]),
        top_level_stage("third-stage", &[72, 36], [12, 24]),
    ];
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(&query(), &stages, &[])
        .expect("paired-depth routes are emitted across both storm stages");
    let repeated = explain_query_with_disjunct_storm_branch_limit_chains(&query(), &stages, &[])
        .expect("repeated planning keeps the same route order");
    let serialized = serde_json::to_value(&explained).expect("plan serializes");
    assert_eq!(
        serialized,
        serde_json::to_value(&repeated).expect("repeated plan serializes")
    );

    let first_routes = routes_for_stage(&explained, 1);
    let post_storm_routes = routes_for_stage(&explained, 2);
    let third_stage_routes = routes_for_stage(&explained, 3);
    assert!(first_routes.iter().all(|route| {
        route.input_scope.starts_with("root/") && route.output_scope.starts_with("root/")
    }));
    for routes in [first_routes, post_storm_routes, third_stage_routes] {
        assert_eq!(
            routes.iter().map(|route| route.depth).collect::<Vec<_>>(),
            vec![1, 1, 2, 2]
        );
        assert!(routes.windows(2).all(|pair| {
            (
                pair[0].depth,
                pair[0].input_path.as_slice(),
                pair[0].output_path.as_slice(),
                pair[0].input_scope.as_str(),
                pair[0].output_scope.as_str(),
            ) <= (
                pair[1].depth,
                pair[1].input_path.as_slice(),
                pair[1].output_path.as_slice(),
                pair[1].input_scope.as_str(),
                pair[1].output_scope.as_str(),
            )
        }));
        assert!(routes.iter().all(|route| route.input_bytes.is_some()));
        assert!(routes.iter().all(|route| route.output_bytes.is_some()));
    }

    let first_route = &first_routes[0];
    assert_eq!(
        first_route.input_path,
        vec![
            PlanByteCapScopeSegment::StormStage { index: 1 },
            PlanByteCapScopeSegment::Branch { index: 1 },
            PlanByteCapScopeSegment::Limit { position: 1 },
        ]
    );
    let mut branch_output = first_routes[1].output_path.clone();
    branch_output.extend([
        PlanByteCapScopeSegment::RebindCascadeOutput {
            position: 2,
            index: 1,
        },
        PlanByteCapScopeSegment::BranchOutput { index: 1 },
    ]);
    let mut first_nested_input = branch_output.clone();
    first_nested_input.extend([
        PlanByteCapScopeSegment::NestedStorm { index: 1 },
        PlanByteCapScopeSegment::Branch { index: 1 },
        PlanByteCapScopeSegment::Limit { position: 1 },
        PlanByteCapScopeSegment::Limit { position: 2 },
    ]);
    assert_eq!(first_routes[2].input_path, first_nested_input);
    let mut second_nested_input = branch_output;
    second_nested_input.extend([
        PlanByteCapScopeSegment::NestedStorm { index: 1 },
        PlanByteCapScopeSegment::NestedStormOutput { index: 1 },
        PlanByteCapScopeSegment::NestedStorm { index: 2 },
        PlanByteCapScopeSegment::Branch { index: 1 },
        PlanByteCapScopeSegment::Limit { position: 1 },
        PlanByteCapScopeSegment::Limit { position: 2 },
    ]);
    assert_eq!(first_routes[3].input_path, second_nested_input);

    let post_storm_prefix = [
        PlanByteCapScopeSegment::StormStage { index: 1 },
        PlanByteCapScopeSegment::StormStageOutput { index: 1 },
        PlanByteCapScopeSegment::StormStage { index: 2 },
    ];
    for route in post_storm_routes {
        assert_eq!(
            &route.input_path[..post_storm_prefix.len()],
            post_storm_prefix
        );
        assert!(route.input_scope.starts_with("root/storm1/output/storm2/"));
        assert!(route.output_scope.starts_with("root/storm1/output/storm2/"));
    }
    assert_eq!(
        post_storm_routes[2].input_scope,
        "root/storm1/output/storm2/branch1/limit1/rebind1/cascade1/rebind_output1_1/limit2/rebind2/cascade1/rebind_output2_1/branch_output1/nested1/branch1/limit1/limit2"
    );
    assert_eq!(
        post_storm_routes[3].input_scope,
        "root/storm1/output/storm2/branch1/limit1/rebind1/cascade1/rebind_output1_1/limit2/rebind2/cascade1/rebind_output2_1/branch_output1/nested1/nested_output1/nested2/branch1/limit1/limit2"
    );
    let third_stage_prefix = [
        PlanByteCapScopeSegment::StormStage { index: 1 },
        PlanByteCapScopeSegment::StormStageOutput { index: 1 },
        PlanByteCapScopeSegment::StormStage { index: 2 },
        PlanByteCapScopeSegment::StormStageOutput { index: 2 },
        PlanByteCapScopeSegment::StormStage { index: 3 },
    ];
    for route in third_stage_routes {
        assert_eq!(
            &route.input_path[..third_stage_prefix.len()],
            third_stage_prefix
        );
        assert_eq!(
            &route.output_path[..third_stage_prefix.len()],
            third_stage_prefix
        );
        assert!(route
            .input_scope
            .starts_with("root/storm1/output/storm2/output/storm3/"));
        assert!(route
            .output_scope
            .starts_with("root/storm1/output/storm2/output/storm3/"));
    }
    assert_eq!(
        third_stage_routes[2].input_scope,
        "root/storm1/output/storm2/output/storm3/branch1/limit1/rebind1/cascade1/rebind_output1_1/limit2/rebind2/cascade1/rebind_output2_1/branch_output1/nested1/branch1/limit1/limit2"
    );
    assert_eq!(
        third_stage_routes[3].input_scope,
        "root/storm1/output/storm2/output/storm3/branch1/limit1/rebind1/cascade1/rebind_output1_1/limit2/rebind2/cascade1/rebind_output2_1/branch_output1/nested1/nested_output1/nested2/branch1/limit1/limit2"
    );
    assert!(
        post_storm_routes[2]
            .input_path
            .contains(&PlanByteCapScopeSegment::BranchOutput { index: 1 })
    );
    assert!(
        post_storm_routes[3]
            .input_path
            .contains(&PlanByteCapScopeSegment::NestedStormOutput { index: 1 })
    );

    for stage in 1..=3 {
        let serialized_filter = serialized["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["details"]["disjunct_storm"] == stage)
            .expect("storm stage serializes");
        assert_eq!(
            serialized_filter["details"]["limit_chain_rebind_byte_cap_handoff_route_order"],
            "ascending_depth_then_typed_input_path_then_typed_output_path"
        );
        assert_eq!(
            serialized_filter["details"]["limit_chain_rebind_byte_cap_handoff_route_records"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }
}
