use orna_sys_v1::{
    ObjectRef, PlanDetail, PlanNodeKind, QueryPlanDescription, QuerySourceStatistics, SnapshotRef,
    explain_query,
};

const BYTE_WORK_FIXTURE: &str = include_str!("fixtures/byte_work_three_scan_first_byte_tail.orna");

fn obj(name: &str) -> ObjectRef {
    ObjectRef::descriptive(name)
}

#[test]
fn first_remainder_byte_crosses_the_scan_work_max_through_limit() {
    let parsed = orna_syntax_v1::parse_module(BYTE_WORK_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 7);

    let rows = u64::MAX - 1;
    for (bytes, expected_work, local_overflow) in
        [(4_096, Some(u64::MAX), false), (4_097, None, true)]
    {
        let explained = explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive(&format!("snapshot:first-byte-limit-{bytes}")),
            source: obj("table:ByteWorkThreeScanSource"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(rows),
                estimated_bytes: Some(bytes),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: Some(10),
            mutations: Vec::new(),
            materialize_into: None,
        })
        .expect("scan rounding boundary below a limit");

        assert_eq!(explained.root().estimated_rows(), Some(10));
        assert_eq!(explained.root().estimated_work(), Some(rows));
        assert_eq!(explained.plan().estimated_cost(), None);
        assert_eq!(
            explained.root().details().get("estimated_cost_overflow"),
            Some(&PlanDetail::Boolean(true)),
            "the exact MAX scan plus limit work also overflows aggregate cost"
        );

        let scan = explained
            .nodes()
            .iter()
            .find(|node| node.kind() == PlanNodeKind::Scan)
            .expect("source scan before limit");
        assert_eq!(scan.estimated_work(), expected_work);
        assert_eq!(
            scan.details().get("estimated_work_overflow"),
            local_overflow.then_some(&PlanDetail::Boolean(true))
        );
    }
}

#[test]
fn local_scan_overflow_remains_a_plan_overflow_under_zero_bounded_and_max_limits() {
    let parsed = orna_syntax_v1::parse_module(BYTE_WORK_FIXTURE);
    assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.diagnostics);
    assert_eq!(parsed.value.items.len(), 7);

    // ORNA-PLAN does not specify whether LIMIT can hide an already
    // overflowing scan estimate. Choose monotone accounting: scan work is
    // priced before limiting, so every output cap retains plan-cost overflow.
    let rows = u64::MAX - 1;
    for (limit, expected_rows) in [(0, 0), (10, 10), (u64::MAX, rows)] {
        let explained = explain_query(&QueryPlanDescription {
            snapshot: SnapshotRef::descriptive(&format!("snapshot:scan-overflow-limit-{limit}")),
            source: obj("table:ByteWorkThreeScanSource"),
            source_statistics: Some(QuerySourceStatistics {
                estimated_rows: Some(rows),
                estimated_bytes: Some(4_097),
                mutable_branch: None,
            }),
            joins: Vec::new(),
            predicate: None,
            projections: Vec::new(),
            distinct: false,
            ordering: Vec::new(),
            limit: Some(limit),
            mutations: Vec::new(),
            materialize_into: None,
        })
        .expect("scan overflow below a limit boundary");

        assert_eq!(explained.root().kind(), PlanNodeKind::Limit);
        assert_eq!(explained.root().estimated_rows(), Some(expected_rows));
        assert_eq!(explained.root().estimated_work(), Some(rows));
        assert_eq!(explained.plan().estimated_cost(), None);
        assert_eq!(
            explained.root().details().get("estimated_cost_overflow"),
            Some(&PlanDetail::Boolean(true)),
            "local scan overflow remains a plan overflow at LIMIT {limit}"
        );

        let scan = explained
            .nodes()
            .iter()
            .find(|node| node.kind() == PlanNodeKind::Scan)
            .expect("source scan before the limit");
        assert_eq!(scan.estimated_rows(), Some(rows));
        assert_eq!(scan.estimated_bytes(), Some(4_097));
        assert_eq!(scan.estimated_work(), None);
        assert_eq!(
            scan.details().get("estimated_work_overflow"),
            Some(&PlanDetail::Boolean(true))
        );

        let surface = serde_json::to_value(&explained).expect("scan-overflow limit surface");
        assert_eq!(
            surface["nodes"][0]["details"]["estimated_cost_overflow"],
            true
        );
        assert!(surface["nodes"].as_array().unwrap().iter().any(|node| {
            node["kind"] == "scan" && node["details"]["estimated_work_overflow"] == true
        }));
    }
}
