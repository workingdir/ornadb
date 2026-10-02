use orna_sys_v1::{
    DisjunctStormBranchDescription, DisjunctStormCascadeDescription,
    DisjunctStormLimitRebindDescription, ExpressionRef, ObjectRef, PlanByteCapScopeSegment,
    PlanDetail, PlanNodeKind, QueryPlanDescription, SnapshotRef,
    explain_query_with_disjunct_storm_branch_limit_chains,
};

const FIXTURE: &str =
    include_str!("fixtures/planner_storm_unknown_rebind_ancestry_consolidation.orna");

fn branch(
    limits: &[u64],
    rebinds: Vec<DisjunctStormLimitRebindDescription>,
) -> DisjunctStormBranchDescription {
    DisjunctStormBranchDescription {
        nested_limits: limits.to_vec(),
        conjunct_count: 1,
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
        snapshot: SnapshotRef::descriptive("snapshot:planner-rebind-cascade-ancestry"),
        source: ObjectRef::descriptive("table:PlannerUnknownRebindAncestryConsolidation"),
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
fn unknown_rebind_outputs_keep_consolidated_ancestry_across_limit_chains() {
    let parsed = orna_syntax_v1::parse_module(FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 2);

    let cascades = [24, 16, 12, 8].map(|limit| {
        storm(
            &format!("expr:unknown-consolidation-{limit}"),
            vec![branch(&[limit], Vec::new())],
        )
    });
    let explained = explain_query_with_disjunct_storm_branch_limit_chains(
        &query(),
        &[storm(
            "expr:unknown-consolidation-root",
            vec![branch(
                &[96, 48],
                vec![
                    rebind(1, vec![cascades[0].clone(), cascades[1].clone()]),
                    rebind(1, vec![cascades[2].clone()]),
                    rebind(2, vec![cascades[3].clone()]),
                ],
            )],
        )],
        &[],
    )
    .expect("unknown rebind results retain ancestry at each later handoff");
    let filter = explained
        .nodes()
        .iter()
        .find(|node| {
            node.kind() == PlanNodeKind::Filter && node.details().contains_key("disjunct_storm")
        })
        .expect("storm is visible");
    assert_eq!(filter.estimated_rows(), None);
    assert_eq!(filter.estimated_bytes(), None);
    let routes = match filter
        .details()
        .get("limit_chain_rebind_byte_cap_handoff_route_records")
    {
        Some(PlanDetail::ByteCapHandoffRoutes(routes)) => routes,
        other => panic!("expected typed routes, got {other:?}"),
    };
    assert_eq!(routes.len(), 4);
    assert!(routes.iter().all(|route| {
        route.depth == 1 && route.input_bytes.is_none() && route.output_bytes.is_none()
    }));

    for (route, prior, position, index) in [
        (&routes[1], &routes[0], 1, 1),
        (&routes[2], &routes[1], 1, 2),
        (&routes[3], &routes[2], 2, 1),
    ] {
        let mut prior_output = prior.output_path.clone();
        prior_output.push(PlanByteCapScopeSegment::RebindCascadeOutput { position, index });
        assert!(route.input_path.starts_with(&prior_output));
    }
    assert!(
        routes[3]
            .input_path
            .contains(&PlanByteCapScopeSegment::RebindCascadeOutput {
                position: 1,
                index: 1,
            })
    );

    let serialized = serde_json::to_value(&explained).expect("explained plans serialize");
    let serialized_filter = serialized["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["details"]["disjunct_storm"] == 1)
        .expect("storm is serialized");
    let fourth_input_path = serialized_filter["details"]
        ["limit_chain_rebind_byte_cap_handoff_route_records"][3]["input_path"]
        .as_array()
        .expect("input path serializes as an array");
    for (position, index) in [(1, 1), (1, 2), (2, 1)] {
        assert!(fourth_input_path.contains(&serde_json::json!({
            "kind": "rebind_cascade_output",
            "position": position,
            "index": index
        })));
    }
}
