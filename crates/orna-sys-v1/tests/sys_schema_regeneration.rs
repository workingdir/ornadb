use std::path::Path;

use orna_sys_v1::system_api_schema_json;
use serde_json::Value;

#[path = "../build_support.rs"]
mod build_support;

const SOURCE_SCHEMA: &str = include_str!("../src/system_api_schema.json");

#[test]
fn schema_source_regenerates_the_embedded_output() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source_root = manifest.join("src");
    let schema_path = source_root.join("system_api_schema.json");
    let collector = build_support::collect_rust_sources(&source_root)
        .expect("annotated implementation methods form a valid registry");
    assert!(
        collector.registry_assets.contains(&schema_path),
        "the schema source must be a tracked registry asset and Cargo rebuild input"
    );

    let source_schema: Value = serde_json::from_str(SOURCE_SCHEMA).expect("source JSON Schema");
    let registry = collector
        .type_graph
        .clone()
        .expect("annotated implementation registry owns its type graph");
    let schema = collector
        .schema
        .clone()
        .expect("annotated implementation registry owns its schema contract");
    let generated = build_support::generate_sys_artifacts(&collector.functions, registry, &schema)
        .expect("source schema regenerates the baked sys artifacts");
    let canonical_source = build_support::canonical_pretty_json(&source_schema)
        .expect("canonical source schema")
        + "\n";

    assert_eq!(generated.schema_json, canonical_source);
    assert_eq!(generated.schema_json, system_api_schema_json());
    assert_eq!(
        std::fs::read_to_string(Path::new(env!("OUT_DIR")).join("system_api_schema.json"))
            .expect("build-generated embedded schema"),
        generated.schema_json
    );
}
