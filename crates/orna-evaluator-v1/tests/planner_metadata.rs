use orna_evaluator_v1::{Environment, Limits, planner_metadata};

#[test]
fn planner_metadata_reports_query_stages_and_no_reuse_without_materializing_rows() {
    let metadata = planner_metadata(
        "sys.Storage | filter (row => true)",
        &Environment::new(),
        Limits::default(),
    )
    .expect("query plan metadata");
    assert_eq!(metadata.nodes[0].kind, "scan");
    assert_eq!(metadata.nodes[0].source.as_deref(), Some("sys.Storage"));
    assert!(metadata.nodes.iter().any(|node| node.kind == "filter"));
    assert_eq!(metadata.filter_batch_count, 0);
    assert_eq!(metadata.reused_filter_batch_count, 0);
    assert!(!metadata.materializes_source_rows);
}

#[test]
fn planner_metadata_rejects_materializing_expressions_before_scanning() {
    for source in ["sys.Storage | count()", "sys.Storage | chunk(2)"] {
        let error = planner_metadata(source, &Environment::new(), Limits::default())
            .expect_err("materializing expressions cannot provide planner metadata");

        assert_eq!(error.code(), "ORNA-EVAL-UNSUPPORTED", "{source}");
    }
}
