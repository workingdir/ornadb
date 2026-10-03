use orna_sys_v1::{
    explain_query, ExpressionRef, ObjectRef, PlanDetail, PlanNodeKind, PlanNullOrder, PlanOrdering,
    PlanSortDirection, QueryJoinDescription, QueryPlanDescription, QuerySourceStatistics,
    SnapshotRef,
};

const QUERY_PLAN_FIXTURE: &str = include_str!("fixtures/query_plan_hint_statistics_azb4m.orna");

#[test]
fn plan_hint_keeps_per_operator_row_and_byte_estimates_in_order() {
    let parsed = orna_syntax_v1::parse_module(QUERY_PLAN_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);

    let explained = explain_query(&QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:query-plan-hint-depth"),
        source: ObjectRef::descriptive("table:Orders"),
        source_statistics: Some(QuerySourceStatistics {
            estimated_rows: Some(100),
            estimated_bytes: Some(6_400),
            mutable_branch: None,
        }),
        joins: vec![QueryJoinDescription {
            source: ObjectRef::descriptive("table:Customers"),
            statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(20),
                estimated_bytes: Some(640),
                mutable_branch: None,
            }),
            predicate: Some(ExpressionRef::descriptive("expr:customer-order-key")),
        }],
        predicate: Some(ExpressionRef::descriptive("expr:open-order")),
        projections: vec![ExpressionRef::descriptive("expr:order-total")],
        distinct: true,
        ordering: vec![PlanOrdering {
            expression: ExpressionRef::descriptive("expr:order-total"),
            direction: PlanSortDirection::Descending,
            null_order: PlanNullOrder::Last,
        }],
        limit: Some(5),
        mutations: Vec::new(),
        materialize_into: None,
    })
    .expect("a resolved query with supplied statistics produces a plan hint");

    assert!(!explained.plan().actual_available());
    assert_eq!(
        explained
            .nodes()
            .iter()
            .map(|node| (
                node.kind(),
                node.estimated_rows(),
                node.estimated_bytes(),
                node.actual_rows(),
                node.actual_bytes(),
            ))
            .collect::<Vec<_>>(),
        vec![
            (PlanNodeKind::Limit, Some(5), Some(480), None, None),
            (PlanNodeKind::Sort, Some(50), Some(4_800), None, None),
            (PlanNodeKind::Aggregate, Some(50), Some(4_800), None, None),
            (PlanNodeKind::Project, Some(100), Some(9_600), None, None),
            (PlanNodeKind::Filter, Some(100), Some(9_600), None, None),
            (PlanNodeKind::Join, Some(200), Some(19_200), None, None),
            (PlanNodeKind::Scan, Some(100), Some(6_400), None, None),
            (PlanNodeKind::Scan, Some(20), Some(640), None, None),
        ]
    );
    assert_eq!(
        explained.nodes()[5].details().get("strategy"),
        Some(&PlanDetail::Text("hash".to_owned())),
        "the structured hint exposes the planner's current join strategy"
    );
    assert_eq!(
        explained.nodes()[5].details().get("selectivity_assumption"),
        Some(&PlanDetail::Text("0.1_no_histogram".to_owned()))
    );
    assert_eq!(
        explained.nodes()[4].details().get("selectivity_assumption"),
        Some(&PlanDetail::Text("0.5_no_histogram".to_owned()))
    );
}
