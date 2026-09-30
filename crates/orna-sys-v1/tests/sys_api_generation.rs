use orna_sys_v1::{
    SystemEffect, SYSTEM_FUNCTION_DESCRIPTORS, system_api_json, system_function_descriptor,
};
use serde_json::Value;

const PUBLISHED_SYS_API: &str = include_str!("../../../api/sys.json");

#[test]
fn published_schema_is_the_deterministic_annotated_method_projection() {
    assert_eq!(
        system_api_json(),
        PUBLISHED_SYS_API,
        "regenerate api/sys.json with `cargo run -p orna-sys-v1 --example write_sys_api > api/sys.json`"
    );
}

#[test]
fn every_portable_function_has_the_collected_runtime_descriptor() {
    let api: Value = serde_json::from_str(PUBLISHED_SYS_API).expect("published sys API JSON");
    let functions = api["functions"].as_array().expect("function declarations");

    assert_eq!(functions.len(), SYSTEM_FUNCTION_DESCRIPTORS.len());
    for function in functions {
        let name = function["name"].as_str().expect("function name");
        let descriptor = system_function_descriptor(name).expect("collected registry entry");
        let effect = match function["effect"].as_str().expect("function effect") {
            "read" => SystemEffect::Read,
            "invoke" => SystemEffect::Invoke,
            "admin" => SystemEffect::Admin,
            other => panic!("unknown system effect: {other}"),
        };
        assert_eq!(descriptor.effect, effect, "effect for {name}");
        assert_eq!(descriptor.signature, function["signature"], "signature for {name}");
        assert_eq!(descriptor.purpose, function["purpose"], "purpose for {name}");
    }
}
