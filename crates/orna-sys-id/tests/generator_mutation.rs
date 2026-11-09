//! Mutation proofs for the generator.
//!
//! The build refuses a catalogue whose bytes changed, and the generator refuses
//! structural edits that would break a closed set. These tests execute both proofs, so
//! a generator that silently regenerated stale tables could not pass CI.

#[path = "../build/generator.rs"]
mod generator;
#[path = "../build/sha256.rs"]
mod sha256;

use serde_json::{Value, json};

const SYS: &str = include_str!("../catalogue/sys-1.1.0.json");
const ENGINE: &str = include_str!("../catalogue/engine-1.1.0.json");

fn sys_value() -> Value {
    serde_json::from_str(SYS).expect("catalogue is valid JSON")
}

fn engine_value() -> Value {
    serde_json::from_str(ENGINE).expect("engine inventory is valid JSON")
}

fn render(value: &Value) -> String {
    serde_json::to_string(value).expect("JSON renders")
}

fn rejection(sys: &str, engine: &str) -> String {
    generator::generate_unchecked(sys, engine)
        .expect_err("the mutated catalogue must be rejected")
        .to_string()
}

fn expect_error(sys: &Value, engine: &Value, needle: &str) {
    let message = rejection(&render(sys), &render(engine));
    assert!(
        message.contains(needle),
        "expected error containing `{needle}`, got `{message}`"
    );
}

#[test]
fn the_unmutated_catalogues_generate() {
    let generated = generator::generate(SYS, ENGINE).expect("reviewed catalogues generate");
    assert!(generated.functions.contains("pub enum SysFunction"));
    assert!(generated.functions.contains("\"sys.meta\""));
    assert!(generated.codes.contains("\"sys.abi.precondition_failed\""));
    assert!(generated.relations.contains("\"sys.Database\""));
    assert!(generated.removed.contains("SysRemovedName"));
    assert!(generated.tables.contains("SYS_FUNCTION_DESCRIPTORS"));
    assert_eq!(generated.files().len(), 6);
}

#[test]
fn any_change_to_the_catalogue_bytes_breaks_the_pin() {
    let mutated = SYS.replacen("\"sys.meta\"", "\"sys.meta \"", 1);
    assert_ne!(mutated, SYS);
    let message = generator::generate(&mutated, ENGINE)
        .expect_err("a byte change must fail the sha256 pin")
        .to_string();
    assert!(message.contains("sha256"), "{message}");
    assert!(message.contains(generator::SYS_API_1_1_SHA256), "{message}");
}

#[test]
fn any_change_to_the_engine_inventory_bytes_breaks_the_pin() {
    let mutated = ENGINE.replacen("\"origin\": \"engine\"", "\"origin\": \"engine \"", 1);
    assert_ne!(mutated, ENGINE);
    let message = generator::generate(SYS, &mutated)
        .expect_err("an engine edit must fail the sha256 pin")
        .to_string();
    assert!(message.contains("engine-1.1.0.json sha256"), "{message}");
    assert!(
        message.contains(generator::ENGINE_1_1_0_SHA256),
        "{message}"
    );
}

#[test]
fn structural_catalogue_edits_are_rejected() {
    let mut counts = sys_value();
    counts["counts"]["functions"] = json!(81);
    expect_error(&counts, &engine_value(), "count `functions`");

    let mut effect = sys_value();
    effect["functions"][0]["effect"] = json!("write");
    expect_error(&effect, &engine_value(), "unknown effect");

    let mut duplicate_callable = sys_value();
    let first = duplicate_callable["functions"][0].clone();
    duplicate_callable["functions"]
        .as_array_mut()
        .expect("functions array")
        .push(first);
    expect_error(&duplicate_callable, &engine_value(), "count `functions`");

    let mut missing_code = sys_value();
    missing_code["failure_codes"]
        .as_array_mut()
        .expect("failure codes array")
        .pop();
    expect_error(&missing_code, &engine_value(), "count `failure_codes`");

    let mut version = sys_value();
    version["language_version"] = json!("1.2.0");
    expect_error(&version, &engine_value(), "catalogue versions changed");

    let mut unknown_field = sys_value();
    unknown_field["functions"][0]["surprise"] = json!(true);
    expect_error(&unknown_field, &engine_value(), "unknown field");

    let mut duplicate_handle = sys_value();
    duplicate_handle["relations"][1]["grouped_handle"] =
        duplicate_handle["relations"][0]["grouped_handle"].clone();
    expect_error(&duplicate_handle, &engine_value(), "duplicate");

    let mut undeclared_field = sys_value();
    undeclared_field["removed_names"]["sys.runtime"]["replacement"] = json!("sys.rt.nonexistent");
    expect_error(&undeclared_field, &engine_value(), "does not declare");
}

#[test]
fn structural_engine_edits_are_rejected() {
    let mut missing_sealed = engine_value();
    missing_sealed["sealed_functions"]
        .as_array_mut()
        .expect("sealed array")
        .pop();
    expect_error(&sys_value(), &missing_sealed, "sealed functions");

    let mut duplicate_id = engine_value();
    let copied = duplicate_id["sealed_functions"][1]["function_id"].clone();
    duplicate_id["sealed_functions"][0]["function_id"] = copied;
    expect_error(&sys_value(), &duplicate_id, "durable function id");

    let mut portable_code = engine_value();
    portable_code["failure_codes"][0] = json!("sys.object.not_found");
    expect_error(&sys_value(), &portable_code, "also a portable code");

    let mut missing_family_name = engine_value();
    let sealed = missing_family_name["sealed_functions"]
        .as_array_mut()
        .expect("sealed array");
    let inspect = sealed
        .iter_mut()
        .find(|entry| entry["name"] == json!("sys.inspect.calls"))
        .expect("the inspect calls entry exists");
    inspect["name"] = json!("sys.inspectx.calls");
    inspect["name_parts"] = json!(["sys", "inspectx", "calls"]);
    expect_error(&sys_value(), &missing_family_name, "family sys.inspect");

    let mut duplicate_fixture = engine_value();
    let fixture = duplicate_fixture["test_fixture_names"][0].clone();
    duplicate_fixture["test_fixture_names"]
        .as_array_mut()
        .expect("fixture array")
        .push(fixture);
    expect_error(&sys_value(), &duplicate_fixture, "appears twice");
}

#[test]
fn a_catalogue_edit_changes_the_generated_tables() {
    let baseline = generator::generate_unchecked(SYS, ENGINE).expect("baseline generates");
    assert!(baseline.functions.contains("\"sys.meta\""));
    assert!(!baseline.functions.contains("sys.meta_changed"));

    let mut edited = sys_value();
    edited["functions"][0]["name"] = json!("sys.meta_changed");
    let mutated = generator::generate_unchecked(&render(&edited), ENGINE)
        .expect("a well-formed rename is observable, not stale");

    assert!(mutated.functions.contains("\"sys.meta_changed\""));
    assert!(mutated.functions.contains("MetaChanged"));
    assert!(!mutated.functions.contains("\"sys.meta\""));
    assert_ne!(baseline.functions, mutated.functions);
    assert_ne!(baseline.tables, mutated.tables);
}
