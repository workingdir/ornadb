use orna_sys_v1::{
    SystemEffect, SYSTEM_FUNCTION_DESCRIPTORS, system_api_json, system_function_descriptor,
};
use serde_json::Value;

#[path = "../build_support.rs"]
mod build_support;

const PUBLISHED_SYS_API: &str = include_str!("../../../api/sys.json");
const SYSTEM_API_FIXTURE: &str = include_str!("fixtures/system-api-annotation.orna");

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

#[test]
fn collector_covers_trait_methods_and_nested_impls_and_rejects_silent_skips() {
    const SOURCE: &str = r####"
        mod nested {
            trait Declared {
                #[ornasys(function = r###"{"effect":"read","name":"sys.trait_declared","purpose":"trait fixture","signature":"fn sys.trait_declared(): Int"}"###)]
                fn declared(&self);
            }

            struct Binding;
            trait Implemented {
                fn bound(&self);
            }
            impl Implemented for Binding {
                #[ornasys(function = r###"{"effect":"read","name":"sys.impl_method","purpose":"impl fixture","signature":"fn sys.impl_method(): Int"}"###)]
                fn bound(&self) {}
            }

            impl Binding {
                #[ornasys(function = r###"{"effect":"admin","name":"sys.inherent_method","purpose":"inherent fixture","signature":"fn sys.inherent_method(): Int"}"###)]
                fn inherent(&self) {}
            }
        }
    "####;
    let mut collector = build_support::Collector::default();
    collector.collect_source("collector fixture", SOURCE);
    assert!(collector.errors.is_empty(), "{:?}", collector.errors);
    assert_eq!(
        collector
            .functions
            .iter()
            .map(|function| function.metadata["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["sys.trait_declared", "sys.impl_method", "sys.inherent_method"]
    );
    assert_eq!(
        collector
            .functions
            .iter()
            .map(|function| function.method.as_str())
            .collect::<Vec<_>>(),
        ["declared", "bound", "inherent"]
    );

    let unsupported = r####"
        #[ornasys(function = r###"{"effect":"read","name":"sys.free","purpose":"not a method","signature":"fn sys.free(): Int"}"###)]
        fn free() {}

        trait HasConst {
            #[ornasys(function = r###"{"effect":"read","name":"sys.const","purpose":"not a method","signature":"fn sys.const(): Int"}"###)]
            const VALUE: u8;
        }
    "####;
    let mut collector = build_support::Collector::default();
    collector.collect_source("unsupported fixture", unsupported);
    assert!(collector.functions.is_empty());
    assert_eq!(collector.errors.len(), 2, "{:?}", collector.errors);
    assert!(collector.errors.iter().any(|error| error.contains("free function")));
    assert!(collector.errors.iter().any(|error| error.contains("associated const")));
}

#[test]
fn annotation_metadata_and_generated_inventory_fail_closed() {
    let valid = serde_json::json!({
        "name": "sys.fixture",
        "effect": "invoke",
        "signature": "fn sys.fixture(): Int",
        "purpose": "fixture purpose",
        "contract": "fixture contract",
        "preconditions": "fixture preconditions",
        "ownership": "fixture ownership",
        "snapshot_rule": "fixture snapshot rule"
    });
    assert!(build_support::validate_function_metadata(&valid).is_ok());

    for invalid in [
        serde_json::json!({
            "name": "sys.fixture", "effect": "read", "signature": "fn sys.fixture(): Int",
            "purpose": "purpose", "contract": "  "
        }),
        serde_json::json!({
            "name": "sys.fixture", "effect": "read", "signature": "fn sys.fixture(): Int",
            "purpose": "purpose", "ownership": 7
        }),
        serde_json::json!({
            "name": "sys.fixture", "effect": "read", "signature": "fn sys.fixture(): Int",
            "purpose": "purpose", "snapshut_rule": "typo"
        }),
        serde_json::json!({
            "name": "sys.fixture", "effect": "read", "signature": "not a function",
            "purpose": "purpose"
        }),
    ] {
        assert!(
            build_support::validate_function_metadata(&invalid).is_err(),
            "invalid function metadata was accepted: {invalid}"
        );
    }

    let generated: Value = serde_json::from_str(&orna_sys_v1::system_api_json()).unwrap();
    build_support::validate_api_document(&generated).expect("generated API inventory is valid");
    let mut stale_count = generated;
    stale_count["counts"]["functions"] = serde_json::json!(65);
    assert!(
        build_support::validate_api_document(&stale_count)
            .unwrap_err()
            .contains("count `functions` declares"),
        "stale counts must fail generation validation"
    );
}

#[test]
fn in_crate_orna_fixture_uses_a_published_collected_api_signature() {
    assert!(
        orna_syntax_v1::parse_module(SYSTEM_API_FIXTURE).is_ok(),
        "system API consumer fixture must parse"
    );
    let api: Value = serde_json::from_str(PUBLISHED_SYS_API).expect("published system API JSON");
    let metadata = api["functions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|function| function["name"] == "sys.meta")
        .expect("collected sys.meta function");
    assert_eq!(
        metadata["signature"],
        "fn sys.meta<T>(value: T): sys.ValueMetadata<T>"
    );
    assert!(api["value_types"].as_array().unwrap().iter().any(|value_type| {
        value_type["name"] == "sys.ValueMetadata<T>"
    }));
}
