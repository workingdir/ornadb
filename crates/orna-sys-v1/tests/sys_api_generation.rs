use orna_sys_v1::{
    SystemEffect, SYSTEM_FUNCTION_DESCRIPTORS, system_api_json, system_api_schema_json,
    system_function_descriptor,
};
use sha2::{Digest, Sha256};
use serde_json::Value;

#[path = "../build_support.rs"]
#[allow(dead_code)]
mod build_support;

const SYS_API_V1_SHA256: &str = "06ea44ae524baa1310c0b84b63f58c5ed2a90d7cc5b67d248c8cbbf9538f04fb";
const SYSTEM_API_FIXTURE: &str = include_str!("fixtures/system-api-annotation.orna");
const GENERIC_TYPE_GRAPH_FIXTURE: &str = include_str!("fixtures/sys-generic-type-graph.orna");

fn assert_canonical_object_key_order(value: &Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                assert_canonical_object_key_order(item);
            }
        }
        Value::Object(object) => {
            let keys = object.keys().collect::<Vec<_>>();
            assert!(
                keys.windows(2).all(|pair| pair[0] < pair[1]),
                "JSON object members must be lexicographically ordered: {keys:?}"
            );
            for child in object.values() {
                assert_canonical_object_key_order(child);
            }
        }
        _ => {}
    }
}

#[test]
fn published_artifact_is_the_deterministic_registry_projection() {
    let generated = system_api_json();
    let digest = format!("{:x}", Sha256::digest(generated.as_bytes()));
    assert_eq!(
        digest, SYS_API_V1_SHA256,
        "the on-demand api/sys.json export matches the reviewed final 1.1 artifact"
    );
}

#[test]
fn published_json_schema_covers_the_generated_artifact_and_closed_type_graph() {
    let schema: Value =
        serde_json::from_str(system_api_schema_json()).expect("embedded generated JSON Schema");
    let api: Value = serde_json::from_str(&system_api_json()).expect("generated system API JSON");
    let properties = schema["properties"].as_object().expect("schema root properties");
    let mut property_names = properties.keys().cloned().collect::<Vec<_>>();
    let mut required_names = schema["required"]
        .as_array()
        .expect("schema root required members")
        .iter()
        .map(|name| name.as_str().expect("required member name").to_owned())
        .collect::<Vec<_>>();
    property_names.sort();
    required_names.sort();

    assert_eq!(schema["$schema"], "https://json-schema.org/draft/2020-12/schema");
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(property_names, required_names);
    build_support::validate_published_schema_shape(&api, &schema)
        .expect("published schema exactly covers generated artifact fields");
    assert_canonical_object_key_order(&api);
    assert_eq!(
        build_support::canonical_pretty_json(&api).expect("canonical artifact serialization"),
        system_api_json().trim_end(),
        "the generator must emit stable canonical member order"
    );
    build_support::validate_api_document(&api)
        .expect("generated API type graph is closed and shape-valid");
}

#[test]
fn every_portable_function_has_the_collected_runtime_descriptor() {
    let api: Value = serde_json::from_str(&system_api_json()).expect("generated sys API JSON");
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
        assert_eq!(descriptor.since, function["since"].as_str(), "since for {name}");
        assert_eq!(
            descriptor.documentation,
            function["documentation"].as_str(),
            "documentation for {name}"
        );
        assert_eq!(descriptor.contract, function["contract"].as_str(), "contract for {name}");
        assert_eq!(
            descriptor.preconditions,
            function["preconditions"].as_str(),
            "preconditions for {name}"
        );
        assert_eq!(descriptor.ownership, function["ownership"].as_str(), "ownership for {name}");
        assert_eq!(
            descriptor.snapshot_rule,
            function["snapshot_rule"].as_str(),
            "snapshot rule for {name}"
        );
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
                #[ornasys(function = r###"{"effect":"read","name":"sys.inherent_method","purpose":"inherent fixture","signature":"fn sys.inherent_method(): Int"}"###)]
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
fn collector_registers_versioned_roles_and_rejects_incompatible_overloads() {
    const SOURCE: &str = r####"
        struct Binding;
        impl Binding {
            #[ornasys(
                function = r###"{"effect":"read","name":"sys.fixture.first","purpose":"fixture","signature":"fn sys.fixture.first(value: Int): Int"}"###,
                role = "langitem.fixture.invoke@1.0"
            )]
            fn first(&self) {}

            #[ornasys(
                function = r###"{"effect":"read","name":"sys.fixture.second","purpose":"fixture","signature":"fn sys.fixture.second(value: Str): Str"}"###,
                role = "langitem.fixture.invoke@1.0"
            )]
            fn second(&self) {}
        }
    "####;
    let mut collector = build_support::Collector::default();
    collector.collect_source("fixture.rs", SOURCE);
    assert!(collector.errors.is_empty(), "{:?}", collector.errors);
    assert_eq!(collector.functions.len(), 2);
    assert_eq!(
        collector.functions[0].role.as_deref(),
        Some("langitem.fixture.invoke@1.0")
    );
    build_support::validate_collection(&collector.functions).unwrap();

    let mut incompatible = collector.functions.clone();
    incompatible[1].metadata["name"] = Value::String("sys.admin.fixture".into());
    incompatible[1].metadata["signature"] =
        Value::String("fn sys.admin.fixture(value: Str): Str".into());
    incompatible[1].metadata["contract"] =
        Value::String("administrative-state-transitions".into());
    incompatible[1].metadata["effect"] = Value::String("admin".into());
    let error = build_support::validate_collection(&incompatible).unwrap_err();
    assert!(error.contains("incompatible effects"), "{error}");

    let mut mixed_versions = collector.functions;
    mixed_versions[1].role = Some("langitem.fixture.invoke@2.0".into());
    let error = build_support::validate_collection(&mixed_versions).unwrap_err();
    assert!(error.contains("multiple versions"), "{error}");
}

#[test]
fn annotation_metadata_and_generated_inventory_fail_closed() {
    let valid = serde_json::json!({
        "name": "sys.meta",
        "effect": "read",
        "signature": "fn sys.meta<T>(value: T): sys.ValueMetadata<T>",
        "purpose": "fixture purpose"
    });
    assert!(build_support::validate_function_metadata(&valid).is_ok());

    let valid_admin = serde_json::json!({
        "name": "sys.admin.checkout(SnapshotRef)",
        "effect": "admin",
        "signature": "fn sys.admin.checkout(target: sys.SnapshotRef): sys.SnapshotRef",
        "purpose": "fixture purpose",
        "contract": "administrative-state-transitions",
        "preconditions": "target is pinned"
    });
    assert!(build_support::validate_function_metadata(&valid_admin).is_ok());

    let valid_invocation = serde_json::json!({
        "name": "sys.start(Value)",
        "effect": "invoke",
        "signature": "fn sys.start(function: sys.FunctionRef): sys.InvocationHandle<sys.Value>",
        "purpose": "fixture purpose",
        "ownership": "child is owned by the call",
        "snapshot_rule": "the function pin is used"
    });
    assert!(build_support::validate_function_metadata(&valid_invocation).is_ok());

    let duplicate_callables = [
        build_support::Function {
            method: "first".into(),
            metadata: serde_json::json!({
                "name": "sys.fixture", "effect": "read",
                "signature": "fn sys.fixture(value: Int): Str", "purpose": "first label"
            }),
            role: None,
        },
        build_support::Function {
            method: "second".into(),
            metadata: serde_json::json!({
                "name": "sys.fixture(Int)", "effect": "read",
                "signature": "fn sys.fixture(value: Int): Bool", "purpose": "second label"
            }),
            role: None,
        },
    ];
    let duplicate_error = build_support::validate_collection(&duplicate_callables).unwrap_err();
    assert!(
        duplicate_error.contains("duplicate #[ornasys] callable signature"),
        "overload labels cannot disguise a duplicate callable shape: {duplicate_error}"
    );

    for invalid in [
        serde_json::json!({
            "name": "sys.admin.commit", "effect": "admin", "signature": "fn sys.admin.commit(): Int",
            "purpose": "purpose", "contract": "  "
        }),
        serde_json::json!({
            "name": "sys.start(Value)", "effect": "invoke", "signature": "fn sys.start(): Int",
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
        serde_json::json!({
            "name": "sys.admin.plan_checkout(SnapshotRef)", "effect": "admin",
            "signature": "fn sys.admin.plan_checkout(target: sys.SnapshotRef): sys.CheckoutPlan",
            "purpose": "purpose", "contract": "administrative-state-transitions"
        }),
        serde_json::json!({
            "name": "sys.admin.retry_failure", "effect": "admin",
            "signature": "fn sys.admin.retry_failure(): Int", "purpose": "purpose",
            "contract": "administrative-state-transitions"
        }),
        serde_json::json!({
            "name": "sys.start(Value)", "effect": "invoke",
            "signature": "fn sys.invoke(): Int", "purpose": "purpose",
            "ownership": "child is owned by the call", "snapshot_rule": "pinned"
        }),
        serde_json::json!({
            "name": "sys.snapshot(Bool)", "effect": "read",
            "signature": "fn sys.snapshot(reference: sys.CommitRef): sys.SnapshotRef",
            "purpose": "wrong overload label"
        }),
    ] {
        assert!(
            build_support::validate_function_metadata(&invalid).is_err(),
            "invalid function metadata was accepted: {invalid}"
        );
    }

    let generated: Value = serde_json::from_str(&orna_sys_v1::system_api_json()).unwrap();
    build_support::validate_api_document(&generated).expect("generated API inventory is valid");

    let mut unknown_top_level = generated.clone();
    unknown_top_level["legacy_api"] = serde_json::json!({});
    assert!(
        build_support::validate_api_document(&unknown_top_level)
            .unwrap_err()
            .contains("exactly the published top-level fields"),
        "unknown schema fields must fail closed"
    );

    let mut missing_version = generated.clone();
    missing_version.as_object_mut().unwrap().remove("sys_version");
    assert!(
        build_support::validate_api_document(&missing_version)
            .unwrap_err()
            .contains("exactly the published top-level fields"),
        "missing schema fields must fail closed"
    );

    let mut blank_source = generated.clone();
    blank_source["source_of_truth"] = serde_json::json!("  ");
    assert!(
        build_support::validate_api_document(&blank_source)
            .unwrap_err()
            .contains("`source_of_truth` must be a nonblank string"),
        "the generator's provenance field must remain populated"
    );

    let mut extra_singleton_field = generated.clone();
    extra_singleton_field["singletons"][0]["legacy_name"] = serde_json::json!("sys.old_database");
    assert!(
        build_support::validate_api_document(&extra_singleton_field)
            .unwrap_err()
            .contains("singletons[0] has unknown field `legacy_name`"),
        "nested inventory rows must reject schema drift"
    );

    let mut duplicate_nested_field = generated.clone();
    let value_type_index = duplicate_nested_field["value_types"]
        .as_array()
        .unwrap()
        .iter()
        .position(|value_type| {
            value_type["fields"]
                .as_array()
                .is_some_and(|fields| !fields.is_empty())
        })
        .expect("a published value type has fields");
    let first_field = duplicate_nested_field["value_types"][value_type_index]["fields"][0].clone();
    duplicate_nested_field["value_types"][value_type_index]["fields"]
        .as_array_mut()
        .unwrap()
        .push(first_field);
    assert!(
        build_support::validate_api_document(&duplicate_nested_field)
            .unwrap_err()
            .contains("duplicates field"),
        "nested field inventories must reject duplicate names"
    );

    let mut duplicate_enum_member = generated.clone();
    let enum_members = duplicate_enum_member["enums"]["sys.AssertionOwnerKind"]
        .as_array_mut()
        .unwrap();
    let duplicate_member = enum_members[0].clone();
    enum_members.push(duplicate_member);
    assert!(
        build_support::validate_api_document(&duplicate_enum_member)
            .unwrap_err()
            .contains("contains duplicate"),
        "enum members must remain unique"
    );

    let mut duplicate_failure_code = generated.clone();
    let codes = duplicate_failure_code["failure_codes"].as_array_mut().unwrap();
    let duplicate_code = codes[0].clone();
    codes.push(duplicate_code);
    let code_count = codes.len();
    duplicate_failure_code["counts"]["failure_codes"] = serde_json::json!(code_count);
    assert!(
        build_support::validate_api_document(&duplicate_failure_code)
            .unwrap_err()
            .contains("failure_codes contains duplicate"),
        "failure-code declarations must remain unique"
    );

    for invalid_code in ["vendor.failure", "sys.only", "sys.Upper.case", "sys.trailing."] {
        let mut malformed_failure_code = generated.clone();
        malformed_failure_code["failure_codes"][0] = serde_json::json!(invalid_code);
        let error = build_support::validate_api_document(&malformed_failure_code).unwrap_err();
        assert!(
            error.contains("failure_codes[0] must be a qualified lowercase `sys` failure code"),
            "the build-time catalog validator must match semantic admission's code grammar for {invalid_code}: {error}"
        );
    }

    let mut valid_additive_failure_code = generated.clone();
    valid_additive_failure_code["failure_codes"][0] =
        serde_json::json!("sys.vendor.future_code");
    build_support::validate_api_document(&valid_additive_failure_code)
        .expect("qualified lowercase failure codes remain additive");

    let mut wrong_effect = generated.clone();
    let invoke = wrong_effect["functions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|function| function["name"] == "sys.invoke(Value)")
        .unwrap();
    invoke["effect"] = serde_json::json!("read");
    assert!(
        build_support::validate_api_document(&wrong_effect)
            .unwrap_err()
            .contains("requires effect `invoke`"),
        "effect drift must fail document validation"
    );

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
fn schema_type_graph_rejects_dangling_and_misaligned_inventory_edges() {
    let generated: Value = serde_json::from_str(&orna_sys_v1::system_api_json()).unwrap();
    build_support::validate_api_document(&generated).expect("published type graph is closed");

    let mut dangling_nested_field = generated.clone();
    let argument = dangling_nested_field["value_types"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|value_type| value_type["name"] == "sys.Argument")
        .unwrap();
    argument["fields"][1]["type"] = serde_json::json!("sys.MissingType?");
    let error = build_support::validate_api_document(&dangling_nested_field).unwrap_err();
    assert!(
        error.contains("unresolved system API type `sys.MissingType`"),
        "supporting value-type fields must resolve through the shared inventory: {error}"
    );

    let mut missing_singleton_type = generated.clone();
    let value_types = missing_singleton_type["value_types"].as_array_mut().unwrap();
    let index = value_types
        .iter()
        .position(|value_type| value_type["name"] == "sys.DatabaseView")
        .unwrap();
    value_types.remove(index);
    missing_singleton_type["counts"]["value_types"] = serde_json::json!(value_types.len());
    let error = build_support::validate_api_document(&missing_singleton_type).unwrap_err();
    assert!(
        error.contains("unresolved system API type `sys.DatabaseView`"),
        "singleton type references must resolve to supporting type declarations: {error}"
    );

    let mut dangling_generic_result = generated.clone();
    let history = dangling_generic_result["functions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|function| function["name"] == "sys.history(ObjectRef)")
        .unwrap();
    history["signature"] = serde_json::json!(
        "fn sys.history(object: sys.ObjectRef): Relation<sys.MissingRow>"
    );
    let error = build_support::validate_api_document(&dangling_generic_result).unwrap_err();
    assert!(
        error.contains("unresolved system API type `sys.MissingRow`"),
        "generic function results must resolve their nested row type: {error}"
    );

    let source_document = generated["value_types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value_type| value_type["name"] == "sys.SourceDocument")
        .unwrap();
    assert_eq!(
        source_document["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["name"] == "maps")
            .unwrap()["type"],
        "Relation<sys.SourceMapEntry>",
        "the published record-row edge remains in the generated type graph"
    );

    let mut generic_record_row = generated.clone();
    let history = generic_record_row["functions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|function| function["name"] == "sys.history(ObjectRef)")
        .unwrap();
    history["signature"] = serde_json::json!(
        "fn sys.history(object: sys.ObjectRef): Relation<sys.ValueMetadata <sys.Value>>"
    );
    build_support::validate_api_document(&generic_record_row)
        .expect("applied record-generic row types allow parser whitespace before `<`");

    let mut generic_parameter_row = generated.clone();
    let meta = generic_parameter_row["functions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|function| function["name"] == "sys.meta")
        .unwrap();
    meta["signature"] = serde_json::json!(
        "fn sys.meta<T>(value: T): Relation<sys.ValueMetadata<T>>"
    );
    build_support::validate_api_document(&generic_parameter_row)
        .expect("an in-scope function parameter can specialize a record-generic relation row");

    let mut enum_relation_result = generated.clone();
    let history = enum_relation_result["functions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|function| function["name"] == "sys.history(ObjectRef)")
        .unwrap();
    history["signature"] =
        serde_json::json!("fn sys.history(object: sys.ObjectRef): Relation<sys.DiffScope>");
    let error = build_support::validate_api_document(&enum_relation_result).unwrap_err();
    assert!(
        error.contains("requires a declared relation or record row type, found `sys.DiffScope`"),
        "enum types cannot be relation rows: {error}"
    );

    let mut enum_relation_field = generated.clone();
    let target = enum_relation_field["value_types"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|value_type| value_type["name"] == "sys.ChangeTarget")
        .unwrap();
    target["fields"][0]["type"] = serde_json::json!("Relation<sys.DiffScope>");
    let error = build_support::validate_api_document(&enum_relation_field).unwrap_err();
    assert!(
        error.contains("requires a declared relation or record row type, found `sys.DiffScope`"),
        "value-type relation fields must reject enum rows: {error}"
    );

    let mut dangling_enum_default = generated.clone();
    dangling_enum_default["enums"]["sys.DiffScope"] = serde_json::json!(["semantic", "source", "rows", "storage"]);
    let error = build_support::validate_api_document(&dangling_enum_default).unwrap_err();
    assert!(
        error.contains("references unknown `sys.DiffScope.all`"),
        "function defaults must retain their closed-enum variants: {error}"
    );

    let mut mismatched_alias_target = generated.clone();
    let database_ref = mismatched_alias_target["reference_aliases"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|alias| alias["name"] == "sys.DatabaseRef")
        .unwrap();
    database_ref["target"] = serde_json::json!("sys.Snapshot");
    let error = build_support::validate_api_document(&mismatched_alias_target).unwrap_err();
    assert!(
        error.contains("definition does not reference its target `sys.Snapshot`"),
        "reference aliases must keep their definition and target aligned: {error}"
    );

    let mut broken_relation_reference = generated.clone();
    let database = broken_relation_reference["relations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|relation| relation["name"] == "sys.Database")
        .unwrap();
    database["reference_type"] = serde_json::json!("sys.SnapshotRef");
    let error = build_support::validate_api_document(&broken_relation_reference).unwrap_err();
    assert!(
        error.contains("must be its matching reference alias"),
        "a canonical relation must use its own reference alias: {error}"
    );

    let mut broken_nested_key = generated;
    let diff_entry = broken_nested_key["relations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|relation| relation["name"] == "sys.DiffEntry")
        .unwrap();
    diff_entry["key_fields"][2] = serde_json::json!("change.unknown");
    diff_entry["key"] = serde_json::json!("from + to + change.unknown + change.target");
    let error = build_support::validate_api_document(&broken_nested_key).unwrap_err();
    assert!(
        error.contains("cannot resolve field path `change.unknown`"),
        "nested key paths must resolve at every record hop: {error}"
    );

    let mut colliding_type_name: Value =
        serde_json::from_str(&orna_sys_v1::system_api_json()).unwrap();
    let alias = colliding_type_name["reference_aliases"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|alias| alias["name"] == "sys.DatabaseRef")
        .unwrap();
    alias["name"] = serde_json::json!("sys.Database");
    let error = build_support::validate_api_document(&colliding_type_name).unwrap_err();
    assert!(
        error.contains("is declared by both `relations` and `reference_aliases`"),
        "type names must be unique across published inventories: {error}"
    );

    let mut dangling_replacement: Value =
        serde_json::from_str(&orna_sys_v1::system_api_json()).unwrap();
    dangling_replacement["removed_names"]["sys.runtime"]["replacement"] =
        serde_json::json!("sys.unknown");
    let error = build_support::validate_api_document(&dangling_replacement).unwrap_err();
    assert!(
        error.contains("has unresolved replacement `sys.unknown`"),
        "removed names must point at a live public replacement: {error}"
    );
}

#[test]
fn relation_natural_key_summary_stays_aligned_with_key_paths() {
    let generated: Value = serde_json::from_str(&orna_sys_v1::system_api_json()).unwrap();
    build_support::validate_api_document(&generated).expect("published relation keys are aligned");

    let mut stale_summary = generated.clone();
    let storage = stale_summary["relations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|relation| relation["name"] == "sys.Storage")
        .unwrap();
    storage["key"] = serde_json::json!("profile");
    let error = build_support::validate_api_document(&stale_summary).unwrap_err();
    assert!(
        error.contains("relation `sys.Storage` key `profile` does not match key_fields `object`"),
        "human-readable natural keys must describe their structured key paths: {error}"
    );

    let mut stale_paths = generated.clone();
    let storage = stale_paths["relations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|relation| relation["name"] == "sys.Storage")
        .unwrap();
    storage["key_fields"][0] = serde_json::json!("profile");
    let error = build_support::validate_api_document(&stale_paths).unwrap_err();
    assert!(
        error.contains("relation `sys.Storage` key `object` does not match key_fields `profile`"),
        "structured key paths must not drift from their published summary: {error}"
    );

    let mut duplicate_paths = generated;
    let storage = duplicate_paths["relations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|relation| relation["name"] == "sys.Storage")
        .unwrap();
    storage["key_fields"] = serde_json::json!(["object", "object"]);
    storage["key"] = serde_json::json!("object + object");
    let error = build_support::validate_api_document(&duplicate_paths).unwrap_err();
    assert!(
        error.contains("key_fields contains duplicate `object`"),
        "natural keys must not repeat the same path: {error}"
    );
}

#[test]
fn removed_name_edges_resolve_and_reserve_all_public_api_paths() {
    let generated: Value = serde_json::from_str(&orna_sys_v1::system_api_json()).unwrap();
    build_support::validate_api_document(&generated).expect("published replacement edges are valid");

    for replacement in [
        "sys.Database",
        "sys.DatabaseRef",
        "sys.catalog.databases",
        "sys.history",
        "sys.DiffScope.all",
    ] {
        let mut valid_replacement = generated.clone();
        valid_replacement["removed_names"]["sys.runtime"]["replacement"] =
            serde_json::json!(replacement);
        build_support::validate_api_document(&valid_replacement)
            .unwrap_or_else(|error| panic!("live replacement path `{replacement}` rejected: {error}"));
    }

    for live_name in [
        "sys.Database",
        "sys.DatabaseRef",
        "sys.catalog.databases",
        "sys.history",
        "sys.DiffScope.all",
    ] {
        let mut shadowed_name = generated.clone();
        let descriptor = shadowed_name["removed_names"]["sys.runtime"].clone();
        shadowed_name["removed_names"][live_name] = descriptor;
        let error = build_support::validate_api_document(&shadowed_name).unwrap_err();
        assert!(
            error.contains(&format!("removed system API name `{live_name}` is still publicly declared")),
            "removed-name key `{live_name}` must not shadow a live path: {error}"
        );
    }

    let mut singleton_function_collision = generated.clone();
    singleton_function_collision["singletons"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "availability": "fixture singleton",
            "name": "sys.history",
            "type": "sys.DatabaseView"
        }));
    let singleton_count = singleton_function_collision["singletons"]
        .as_array()
        .unwrap()
        .len();
    singleton_function_collision["counts"]["singletons"] = serde_json::json!(singleton_count);
    assert!(
        build_support::validate_api_document(&singleton_function_collision)
            .unwrap_err()
            .contains("system API public value `sys.history` is declared more than once"),
        "singleton names must not collide with overloaded callable base paths"
    );

    let mut dangling_replacement = generated;
    dangling_replacement["removed_names"]["sys.runtime"]["replacement"] =
        serde_json::json!("sys.unknown");
    assert!(
        build_support::validate_api_document(&dangling_replacement)
            .unwrap_err()
            .contains("has unresolved replacement `sys.unknown`"),
        "replacement paths outside the published schema must fail closed"
    );
}

#[test]
fn function_defaults_resolve_paths_variants_and_parameter_order() {
    let generated: Value = serde_json::from_str(&orna_sys_v1::system_api_json()).unwrap();
    build_support::validate_api_document(&generated).expect("published function defaults are valid");

    for (default, expected) in [
        ("sys.MissingScope.all", "does not resolve to an enum variant"),
        ("sys.DiffScope.missing", "references unknown `sys.DiffScope.missing`"),
        ("sys.database.missing", "cannot resolve singleton field path"),
        ("sys.database.cwd", "has type `sys.SnapshotRef`, not parameter type `sys.DiffScope`"),
        ("true", "has type `Bool`, not parameter type `sys.DiffScope`"),
        ("null", "uses `null` with non-optional type `sys.DiffScope`"),
    ] {
        let mut invalid = generated.clone();
        let diff = invalid["functions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|function| function["name"] == "sys.diff")
            .unwrap();
        let signature = diff["signature"].as_str().unwrap();
        diff["signature"] = serde_json::json!(signature.replace("sys.DiffScope.all", default));
        let error = build_support::validate_api_document(&invalid).unwrap_err();
        assert!(error.contains(expected), "default {default}: {error}");
    }

    let mut singleton_enum_collision = generated.clone();
    singleton_enum_collision["singletons"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "availability": "fixture singleton",
            "name": "sys.DiffScope",
            "type": "sys.DatabaseView"
        }));
    let singleton_count = singleton_enum_collision["singletons"]
        .as_array()
        .unwrap()
        .len();
    singleton_enum_collision["counts"]["singletons"] = serde_json::json!(singleton_count);
    let error = build_support::validate_api_document(&singleton_enum_collision).unwrap_err();
    assert!(
        error.contains("cannot resolve singleton field path `sys.DiffScope.all`"),
        "singleton path prefixes resolve before enum variants: {error}"
    );

    let mut singleton_field_default = singleton_enum_collision;
    let diff = singleton_field_default["functions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|function| function["name"] == "sys.diff")
        .unwrap();
    let signature = diff["signature"].as_str().unwrap().replace(
        "scope: sys.DiffScope = sys.DiffScope.all",
        "scope: sys.SnapshotRef = sys.DiffScope.cwd",
    );
    diff["signature"] = serde_json::json!(signature);
    build_support::validate_api_document(&singleton_field_default)
        .expect("a colliding path resolves through the singleton field first");

    let mut required_after_default = generated;
    let resolve = required_after_default["functions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|function| function["name"] == "sys.resolve")
        .unwrap();
    let signature = resolve["signature"].as_str().unwrap();
    resolve["signature"] = serde_json::json!(signature.replace(
        "from: sys.ModuleRef? = null): sys.ObjectRef",
        "from: sys.ModuleRef? = null, required: Bool): sys.ObjectRef"
    ));
    let error = build_support::validate_api_document(&required_after_default).unwrap_err();
    assert!(
        error.contains("function `sys.resolve` parameter 4 is required after a defaulted parameter"),
        "required parameters must precede defaulted parameters: {error}"
    );
}

#[test]
fn erased_invoke_and_start_labels_require_matching_generic_tails() {
    let mut generated: Value = serde_json::from_str(&orna_sys_v1::system_api_json()).unwrap();
    build_support::validate_api_document(&generated).expect("published overload pairs are valid");

    let functions = generated["functions"].as_array_mut().unwrap();
    functions.retain(|function| function["name"] != "sys.invoke<T>");
    let function_count = functions.len();
    generated["counts"]["functions"] = serde_json::json!(function_count);
    let error = build_support::validate_api_document(&generated).unwrap_err();
    assert!(
        error.contains("erased overload `sys.invoke(Value)` requires a matching generic sibling"),
        "removing the generic overload must invalidate its erased partner: {error}"
    );

    let mut mismatched_input: Value = serde_json::from_str(&orna_sys_v1::system_api_json()).unwrap();
    let generic = mismatched_input["functions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|function| function["name"] == "sys.invoke<T>")
        .unwrap();
    generic["signature"] = serde_json::json!(
        "fn sys.invoke<T>(function: sys.FunctionRef, arguments: sys.Value, as: T, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.inherit, idempotency_key: Str? = null): T"
    );
    let error = build_support::validate_api_document(&mismatched_input).unwrap_err();
    assert!(
        error.contains("erased overload `sys.invoke(Value)` requires a matching generic sibling"),
        "the generic sibling must preserve all non-witness inputs: {error}"
    );

    let mut mismatched_result: Value = serde_json::from_str(&orna_sys_v1::system_api_json()).unwrap();
    let generic = mismatched_result["functions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|function| function["name"] == "sys.start<T>")
        .unwrap();
    generic["signature"] = serde_json::json!(
        "fn sys.start<T>(function: sys.FunctionRef, arguments: sys.ArgumentMap, as: T, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.separate, idempotency_key: Str? = null): sys.InvocationHandle<sys.Value>"
    );
    let error = build_support::validate_api_document(&mismatched_result).unwrap_err();
    assert!(
        error.contains("erased overload `sys.start(Value)` requires a matching generic sibling"),
        "the generic type witness must determine the erased result: {error}"
    );
}

#[test]
fn in_crate_orna_fixture_uses_a_published_collected_api_signature() {
    assert!(
        orna_syntax_v1::parse_module(SYSTEM_API_FIXTURE).is_ok(),
        "system API consumer fixture must parse"
    );
    let api: Value = serde_json::from_str(&system_api_json()).expect("generated system API JSON");
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
    assert!(SYSTEM_API_FIXTURE.contains(
        "pub fn diff_entries(from: sys.SnapshotRef, to: sys.SnapshotRef): Relation<sys.DiffEntry>"
    ));
    let diff = api["functions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|function| function["name"] == "sys.diff")
        .expect("collected diff function");
    assert_eq!(
        diff["signature"],
        "fn sys.diff(from: sys.SnapshotRef, to: sys.SnapshotRef, scope: sys.DiffScope = sys.DiffScope.all): Relation<sys.DiffEntry>"
    );
    assert!(api["relations"].as_array().unwrap().iter().any(|relation| {
        relation["name"] == "sys.DiffEntry"
            && relation["reference_type"] == "sys.DiffEntryRef"
            && relation["key_fields"][2] == "change.area"
    }));
    for (label, signature) in [
        (
            "sys.snapshot(SnapshotRef)",
            "fn sys.snapshot(reference: sys.SnapshotRef = sys.database.cwd): sys.SnapshotRef",
        ),
        (
            "sys.snapshot(CommitRef)",
            "fn sys.snapshot(reference: sys.CommitRef): sys.SnapshotRef",
        ),
    ] {
        let function = api["functions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|function| function["name"] == label)
            .unwrap_or_else(|| panic!("missing annotated overload {label}"));
        assert_eq!(function["signature"], signature);
    }
}

#[test]
fn generic_type_graph_fixture_resolves_through_the_published_inventory() {
    let parsed = orna_syntax_v1::parse_module(GENERIC_TYPE_GRAPH_FIXTURE);
    assert!(
        parsed.is_ok(),
        "generic type graph fixture must parse in supported Orna grammar"
    );
    let fixture_functions = parsed
        .value
        .items
        .iter()
        .map(|item| match &item.declaration {
            orna_syntax_v1::Declaration::Function { signature, .. } => signature.name.as_str(),
            declaration => panic!("fixture entry must be a function: {declaration:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        fixture_functions,
        [
            "metadata_value",
            "metadata_relation",
            "metadata_list",
            "await_result",
            "attribution_rows",
            "query_plan",
            "metadata_query",
            "nested_metadata_rows",
        ]
    );

    let mut api: Value = serde_json::from_str(&system_api_json()).expect("generated system API JSON");
    build_support::validate_api_document(&api).expect("published graph is closed before extension");
    let fixtures = [
        (
            "metadata_value",
            "fn sys.fixture.metadata_value<T>(value: T): sys.ValueMetadata<T>",
        ),
        (
            "metadata_relation",
            "fn sys.fixture.metadata_relation<T>(rows: Relation<sys.ValueMetadata<T>>): Relation<sys.ValueMetadata<T>>",
        ),
        (
            "metadata_list",
            "fn sys.fixture.metadata_list<T>(values: [sys.ValueMetadata<T>?]): [sys.ValueMetadata<T>?]",
        ),
        (
            "await_result",
            "fn sys.fixture.await_result<T>(invocation: sys.InvocationHandle<T>): sys.InvocationResult<T>",
        ),
        (
            "attribution_rows",
            "fn sys.fixture.attribution_rows<T>(target: sys.RowRef<T>): Relation<sys.Attribution>",
        ),
        (
            "query_plan",
            "fn sys.fixture.query_plan<T>(query: Query<T>): sys.Plan",
        ),
        (
            "metadata_query",
            "fn sys.fixture.metadata_query<T>(query: Query<sys.ValueMetadata<T>>): Query<sys.ValueMetadata<T>>",
        ),
        (
            "nested_metadata_rows",
            "fn sys.fixture.nested_metadata_rows<T>(rows: Relation<sys.ValueMetadata<[T?]>>): Relation<sys.ValueMetadata<[T?]>>",
        ),
    ];
    let functions = api["functions"]
        .as_array_mut()
        .expect("published functions are an array");
    for (local_name, signature) in fixtures {
        let fixture_signature = signature
            .strip_prefix("fn sys.fixture.")
            .expect("synthetic system signature has a fixture namespace");
        assert!(
            GENERIC_TYPE_GRAPH_FIXTURE.contains(&format!("pub fn {fixture_signature} =")),
            "fixture declaration must exercise `{signature}`"
        );
        functions.push(serde_json::json!({
            "effect": "read",
            "name": format!("sys.fixture.{local_name}<T>"),
            "purpose": "generic type graph test fixture",
            "signature": signature,
        }));
    }
    api["counts"]["functions"] = serde_json::json!(functions.len());
    build_support::validate_api_document(&api)
        .expect("generic, nested, optional, list, Query, Relation, and row types resolve");
}

#[test]
fn generic_type_graph_rejects_misapplied_and_unbound_arguments() {
    let generated: Value = serde_json::from_str(&system_api_json()).unwrap();

    let mut wrong_arity = generated.clone();
    let meta = wrong_arity["functions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|function| function["name"] == "sys.meta")
        .unwrap();
    meta["signature"] = serde_json::json!(
        "fn sys.meta<T>(value: sys.ValueMetadata<T, sys.Value>): sys.ValueMetadata<T>"
    );
    let error = build_support::validate_api_document(&wrong_arity).unwrap_err();
    assert!(
        error.contains("type `sys.ValueMetadata` expects 1 generic argument(s), found 2"),
        "nested generic applications must match their declared arity: {error}"
    );

    let mut dangling_nested_argument = generated.clone();
    let meta = dangling_nested_argument["functions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|function| function["name"] == "sys.meta")
        .unwrap();
    meta["signature"] = serde_json::json!(
        "fn sys.meta<T>(value: [sys.ValueMetadata<sys.MissingType>?]): sys.ValueMetadata<T>"
    );
    let error = build_support::validate_api_document(&dangling_nested_argument).unwrap_err();
    assert!(
        error.contains("unresolved system API type `sys.MissingType`"),
        "nested generic arguments must resolve through the closed inventory: {error}"
    );

    let mut unbound_function_type = generated.clone();
    let meta = unbound_function_type["functions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|function| function["name"] == "sys.meta")
        .unwrap();
    meta["signature"] = serde_json::json!(
        "fn sys.meta<T>(value: sys.InvocationHandle<T>): sys.InvocationResult<U>"
    );
    let error = build_support::validate_api_document(&unbound_function_type).unwrap_err();
    assert!(
        error.contains("unresolved system API type `U`"),
        "function signatures must not leak unbound generic variables: {error}"
    );

    let mut unbound_record_field = generated;
    let invocation_result = unbound_record_field["value_types"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|value_type| value_type["name"] == "sys.InvocationResult<T>")
        .unwrap();
    let value_field = invocation_result["fields"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|field| field["name"] == "value")
        .unwrap();
    value_field["type"] = serde_json::json!("sys.ValueMetadata<U>");
    let error = build_support::validate_api_document(&unbound_record_field).unwrap_err();
    assert!(
        error.contains("unresolved system API type `U`"),
        "record-generic fields must only reference declared type parameters: {error}"
    );
}
