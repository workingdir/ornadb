//! Bijection between the vendored 1.1.0 catalogue and every generated identity set.
//!
//! These tests are the acceptance oracle for stage S0: they read the catalogue bytes
//! independently of the generator and assert both directions of every closed set.

#[path = "../build/sha256.rs"]
mod sha256;

use orna_sys_id::{
    AbiVersion, EngineSignature, SYS_ENUM_COUNT, SYS_ENUM_VALUE_COUNT, SYS_FAILURE_CODE_COUNT,
    SYS_FIELD_COUNT, SYS_FUNCTION_COUNT, SYS_GROUPED_HANDLE_COUNT, SYS_RELATION_COUNT,
    SYS_SINGLETON_COUNT, SYS_TYPE_COUNT, SysEnum, SysEnumValue, SysFailureCode, SysField,
    SysFunction, SysGroupedHandle, SysMemberPath, SysRelation, SysSingleton, SysType,
};
use serde_json::Value;
use std::{collections::BTreeSet, fs, path::Path};

const CATALOGUE: &str = include_str!("../catalogue/sys-1.1.0.json");
const ENGINE: &str = include_str!("../catalogue/engine-1.1.0.json");
const SYS_API_SHA256: &str = "10ef7dab9665de4e065ee2b96797b2751c7c3f98f9886241011aee0cc8c0d40e";

fn catalogue() -> Value {
    serde_json::from_str(CATALOGUE).expect("vendored catalogue is valid JSON")
}

fn engine() -> Value {
    serde_json::from_str(ENGINE).expect("vendored engine inventory is valid JSON")
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("`{key}` must be an array"))
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("`{key}` must be a string"))
}

fn split_label(label: &str) -> (&str, Option<&str>) {
    if let Some(open) = label.find('(') {
        (&label[..open], Some(&label[open + 1..label.len() - 1]))
    } else if let Some(open) = label.find('<') {
        (&label[..open], Some(&label[open + 1..label.len() - 1]))
    } else {
        (label, None)
    }
}

fn value_type_fields(catalogue: &Value, name: &str) -> Vec<String> {
    array(catalogue, "value_types")
        .iter()
        .find(|entry| text(entry, "name") == name)
        .unwrap_or_else(|| panic!("value type `{name}` is declared"))["fields"]
        .as_array()
        .expect("fields array")
        .iter()
        .map(|field| text(field, "name").to_owned())
        .collect()
}

#[test]
fn vendored_catalogue_is_the_reviewed_contract() {
    assert_eq!(
        sha256::hex(&sha256::digest(CATALOGUE.as_bytes())),
        SYS_API_SHA256,
        "catalogue bytes changed; the generated identity tables must be reviewed"
    );
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../orna-conformance/tests/fixtures/reference/api/sys.json");
    let fixture_bytes = fs::read(&fixture).expect("conformance reference fixture is present");
    assert_eq!(
        CATALOGUE.as_bytes(),
        fixture_bytes.as_slice(),
        "vendored catalogue must equal the conformance reference fixture"
    );

    let catalogue = catalogue();
    assert_eq!(text(&catalogue, "language_version"), "1.1.0");
    assert_eq!(text(&catalogue, "sys_version"), "1.1");
    assert_eq!(
        orna_sys_id::SYS_VERSION.language(),
        text(&catalogue, "language_version")
    );
    assert_eq!(
        orna_sys_id::SYS_VERSION.sys(),
        text(&catalogue, "sys_version")
    );
    assert_eq!(orna_sys_id::SYS_API_LANGUAGE_VERSION, "1.1.0");
    assert_eq!(orna_sys_id::SYS_API_SYS_VERSION, "1.1");
    let counts = &catalogue["counts"];
    for (key, expected) in [
        ("functions", 82usize),
        ("failure_codes", 67),
        ("relations", 78),
        ("singletons", 4),
        ("enums", 42),
        ("value_types", 35),
        ("reference_aliases", 78),
        ("opaque_identifiers", 21),
    ] {
        assert_eq!(
            counts[key].as_u64().expect("count is an integer") as usize,
            expected,
            "catalogue count `{key}`"
        );
    }
    assert_eq!(
        counts["functions"].as_u64().unwrap() as usize,
        array(&catalogue, "functions").len()
    );
    assert_eq!(
        counts["failure_codes"].as_u64().unwrap() as usize,
        array(&catalogue, "failure_codes").len()
    );
}

#[test]
fn every_catalogue_callable_has_exactly_one_variant() {
    let catalogue = catalogue();
    let functions = array(&catalogue, "functions");
    assert_eq!(functions.len(), SYS_FUNCTION_COUNT);
    assert_eq!(SysFunction::ALL.len(), SYS_FUNCTION_COUNT);

    let mut labels = BTreeSet::new();
    let mut variants = BTreeSet::new();
    for entry in functions {
        let label = text(entry, "name");
        assert!(labels.insert(label), "duplicate label `{label}`");
        let function = SysFunction::from_label(label)
            .unwrap_or_else(|| panic!("`{label}` must have a variant"));
        assert!(variants.insert(function), "duplicate variant for `{label}`");
        assert_eq!(function.label(), label);
        assert_eq!(label.parse::<SysFunction>().unwrap(), function);
        assert_eq!(function.to_string(), label);

        let (path, overload) = split_label(label);
        assert_eq!(function.path(), path);
        assert_eq!(function.overload(), overload);
        assert_eq!(function.origin(), orna_sys_id::Origin::Portable);

        let descriptor = function.descriptor();
        assert_eq!(descriptor.function, function);
        assert_eq!(descriptor.label, label);
        assert_eq!(descriptor.signature, text(entry, "signature"));
        assert_eq!(descriptor.purpose, text(entry, "purpose"));
        assert_eq!(descriptor.effect.as_str(), text(entry, "effect"));
        assert_eq!(descriptor.overload, overload);
        assert_eq!(
            descriptor.since,
            entry["since"].as_str(),
            "`since` for {label}"
        );
        assert_eq!(
            descriptor.contract,
            entry["contract"].as_str(),
            "`contract` for {label}"
        );
    }

    for function in SysFunction::ALL {
        assert_eq!(SysFunction::from_label(function.label()), Some(function));
        assert_eq!(function.descriptor().label, function.label());
    }

    let mut paths: BTreeSet<&str> = BTreeSet::new();
    for function in SysFunction::ALL {
        paths.insert(function.path());
    }
    for path in paths {
        let candidates: Vec<SysFunction> = SysFunction::ALL
            .into_iter()
            .filter(|function| function.path() == path)
            .collect();
        match candidates.as_slice() {
            [only] => assert_eq!(SysFunction::from_path(path), Ok(*only)),
            many => match SysFunction::from_path(path) {
                Err(orna_sys_id::AmbiguousPath::Overloaded {
                    path: reported,
                    candidates: listed,
                }) => {
                    assert_eq!(reported, path);
                    assert_eq!(listed, many);
                }
                other => panic!("expected overloaded `{path}`, got {other:?}"),
            },
        }
    }
    assert_eq!(
        SysFunction::from_path("sys.not_a_member"),
        Err(orna_sys_id::AmbiguousPath::Unknown("sys.not_a_member"))
    );
}

#[test]
fn portable_failure_codes_are_the_catalogue_set_in_order() {
    let catalogue = catalogue();
    let codes = array(&catalogue, "failure_codes");
    assert_eq!(codes.len(), 67);
    assert_eq!(SysFailureCode::PORTABLE.len(), 67);
    let expected: Vec<&str> = codes.iter().map(|code| code.as_str().unwrap()).collect();
    let portable: Vec<&str> = SysFailureCode::PORTABLE
        .iter()
        .map(|code| code.as_str())
        .collect();
    assert_eq!(portable, expected);

    assert_eq!(SysFailureCode::ALL.len(), SYS_FAILURE_CODE_COUNT);
    let mut variants = BTreeSet::new();
    for (index, code) in SysFailureCode::ALL.iter().copied().enumerate() {
        assert!(variants.insert(code.as_str()), "duplicate code bytes");
        assert_eq!(SysFailureCode::parse(code.as_str()), Some(code));
        assert_eq!(code.to_string(), code.as_str());
        assert_eq!(
            code.is_portable(),
            index < 67,
            "`{}` origin must follow the dense prefix",
            code.as_str()
        );
        assert_eq!(
            code.is_provider_abi(),
            code.as_str().starts_with("sys.abi."),
            "provider ABI predicate for `{}`",
            code.as_str()
        );
        assert_eq!(code.descriptor().code, code);
        assert_eq!(code.descriptor().bytes, code.as_str());
    }

    let engine = engine();
    let engine_codes: BTreeSet<&str> = array(&engine, "failure_codes")
        .iter()
        .map(|code| code.as_str().unwrap())
        .collect();
    for code in &SysFailureCode::ALL[67..] {
        assert!(
            engine_codes.contains(code.as_str()),
            "`{}` is not in the engine inventory",
            code.as_str()
        );
        assert_eq!(code.origin(), orna_sys_id::Origin::Engine);
        assert!(!expected.contains(&code.as_str()));
    }
    assert_eq!(engine_codes.len(), 34);
}

#[test]
fn relations_handles_singletons_enums_and_types_match_the_catalogue() {
    let catalogue = catalogue();
    let relations = array(&catalogue, "relations");
    assert_eq!(relations.len(), SYS_RELATION_COUNT);
    assert_eq!(SysRelation::ALL.len(), SYS_RELATION_COUNT);
    assert_eq!(SysGroupedHandle::ALL.len(), SYS_GROUPED_HANDLE_COUNT);

    for (index, entry) in relations.iter().enumerate() {
        let relation = SysRelation::ALL[index];
        assert_eq!(relation.name(), text(entry, "name"));
        assert_eq!(SysRelation::parse(relation.name()), Some(relation));
        assert_eq!(relation.to_string(), relation.name());
        assert_eq!(relation.handle().name(), text(entry, "grouped_handle"));
        let descriptor = relation.descriptor();
        assert_eq!(descriptor.relation, relation);
        assert_eq!(descriptor.name, relation.name());
        assert_eq!(descriptor.kind, text(entry, "kind"));
        assert_eq!(descriptor.availability, text(entry, "availability"));
        assert_eq!(descriptor.key, text(entry, "key"));
        assert_eq!(descriptor.reference_type, text(entry, "reference_type"));
        assert_eq!(descriptor.purpose, text(entry, "purpose"));
        assert_eq!(descriptor.writable, entry["writable"].as_bool().unwrap());
        let fields = array(entry, "fields");
        assert_eq!(descriptor.fields.len(), fields.len());
        for (field, expected) in descriptor.fields.iter().zip(fields) {
            assert_eq!(field.field.name(), text(expected, "name"));
            assert_eq!(field.type_name, text(expected, "type"));
        }
    }
    for handle in SysGroupedHandle::ALL {
        assert_eq!(SysGroupedHandle::parse(handle.name()), Some(handle));
        assert_eq!(handle.to_string(), handle.name());
    }

    let singletons = array(&catalogue, "singletons");
    assert_eq!(singletons.len(), SYS_SINGLETON_COUNT);
    for (index, entry) in singletons.iter().enumerate() {
        let singleton = SysSingleton::ALL[index];
        assert_eq!(singleton.name(), text(entry, "name"));
        assert_eq!(SysSingleton::parse(singleton.name()), Some(singleton));
        let descriptor = singleton.descriptor();
        assert_eq!(descriptor.view_type, text(entry, "type"));
        assert_eq!(descriptor.availability, text(entry, "availability"));
        let expected = value_type_fields(&catalogue, text(entry, "type"));
        let actual: Vec<String> = descriptor
            .fields
            .iter()
            .map(|field| field.name().to_owned())
            .collect();
        assert_eq!(actual, expected);
        for field in descriptor.fields.iter().copied() {
            assert_eq!(
                SysMemberPath::singleton_field(singleton, field),
                Some(SysMemberPath::SingletonField(singleton, field))
            );
        }
    }
    assert_eq!(
        SysMemberPath::singleton_field(SysSingleton::Rt, SysField::Writable),
        None,
        "sys.RuntimeView does not declare `writable`"
    );

    let enums = catalogue["enums"].as_object().expect("enums object");
    let mut enum_names: Vec<&String> = enums.keys().collect();
    enum_names.sort();
    assert_eq!(enum_names.len(), SYS_ENUM_COUNT);
    assert_eq!(SysEnum::ALL.len(), SYS_ENUM_COUNT);
    assert_eq!(SysEnumValue::ALL.len(), SYS_ENUM_VALUE_COUNT);
    for (index, name) in enum_names.iter().enumerate() {
        let enumeration = SysEnum::ALL[index];
        assert_eq!(enumeration.name(), name.as_str());
        assert_eq!(SysEnum::parse(enumeration.name()), Some(enumeration));
        let expected: Vec<&str> = enums[*name]
            .as_array()
            .expect("enum values array")
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        let actual: Vec<&str> = enumeration
            .values()
            .iter()
            .map(|value| value.value())
            .collect();
        assert_eq!(actual, expected);
        for value in enumeration.values().iter().copied() {
            assert_eq!(value.enum_(), enumeration);
            assert_eq!(
                SysEnumValue::parse(value.label()),
                Some(value),
                "`{}` round-trips",
                value.label()
            );
            assert_eq!(value.to_string(), value.label());
            assert!(value.label().starts_with(enumeration.name()));
        }
    }

    let mut expected_types: Vec<(String, &str, Option<String>)> = Vec::new();
    for entry in array(&catalogue, "value_types") {
        expected_types.push((text(entry, "name").to_owned(), text(entry, "kind"), None));
    }
    for entry in array(&catalogue, "reference_aliases") {
        expected_types.push((
            text(entry, "name").to_owned(),
            "alias",
            Some(text(entry, "target").to_owned()),
        ));
    }
    for entry in array(&catalogue, "opaque_identifiers") {
        expected_types.push((
            entry.as_str().expect("opaque name").to_owned(),
            "opaque",
            None,
        ));
    }
    assert_eq!(expected_types.len(), SYS_TYPE_COUNT);
    assert_eq!(SysType::ALL.len(), SYS_TYPE_COUNT);
    for (index, (name, kind, target)) in expected_types.iter().enumerate() {
        let system_type = SysType::ALL[index];
        assert_eq!(system_type.name(), name);
        assert_eq!(system_type.kind().as_str(), *kind);
        assert_eq!(system_type.target(), target.as_deref());
        assert_eq!(SysType::parse(name), Some(system_type));
        assert_eq!(system_type.to_string(), *name);
        assert_eq!(system_type.descriptor().system_type, system_type);
    }
    assert_eq!(SysField::ALL.len(), SYS_FIELD_COUNT);
    let mut field_names = BTreeSet::new();
    for field in SysField::ALL {
        assert!(field_names.insert(field.name()));
        assert_eq!(SysField::parse(field.name()), Some(field));
        assert_eq!(field.to_string(), field.name());
    }
}

#[test]
fn engine_inventory_matches_the_generated_tables() {
    let engine = engine();
    assert_eq!(
        orna_sys_id::ENGINE_SEALED_FUNCTION_COUNT,
        array(&engine, "sealed_functions").len()
    );
    assert_eq!(
        orna_sys_id::ENGINE_ONLY_FUNCTION_COUNT,
        array(&engine, "engine_only_functions").len()
    );
    assert_eq!(
        orna_sys_id::ENGINE_ONLY_TYPE_COUNT,
        array(&engine, "engine_only_types").len()
    );
    assert_eq!(
        orna_sys_id::ENGINE_RETIRED_FUNCTION_COUNT,
        array(&engine, "retired_functions").len()
    );
    assert_eq!(
        orna_sys_id::ENGINE_FAILURE_CODE_COUNT,
        array(&engine, "failure_codes").len()
    );
    assert_eq!(
        orna_sys_id::ENGINE_TEST_FIXTURE_NAME_COUNT,
        array(&engine, "test_fixture_names").len()
    );
    assert_eq!(
        orna_sys_id::ENGINE_FUNCTIONS.len(),
        orna_sys_id::ENGINE_SEALED_FUNCTION_COUNT + orna_sys_id::ENGINE_ONLY_FUNCTION_COUNT
    );

    let mut names = BTreeSet::new();
    for entry in array(&engine, "sealed_functions") {
        let name = text(entry, "name");
        assert!(names.insert(name));
        let descriptor =
            orna_sys_id::descriptor::engine_function(name).expect("sealed function is generated");
        assert_eq!(descriptor.name, name);
        assert!(descriptor.sealed);
        assert!(descriptor.function_id.is_some());
        assert!(descriptor.name_parts.to_vec().join(".") == name);
        match descriptor.signature {
            Some(
                EngineSignature::Health
                | EngineSignature::SourceIntrospection
                | EngineSignature::Invocation { .. }
                | EngineSignature::Inspect { .. }
                | EngineSignature::Security { .. }
                | EngineSignature::State { .. },
            ) => {}
            None => panic!("sealed `{name}` must carry a signature"),
        }
    }
    for entry in array(&engine, "engine_only_functions") {
        let name = text(entry, "name");
        assert!(names.insert(name));
        let descriptor = orna_sys_id::descriptor::engine_function(name)
            .expect("engine-only function is generated");
        assert!(!descriptor.sealed);
        assert!(descriptor.function_id.is_none());
        assert!(descriptor.signature.is_none());
    }
    for entry in array(&engine, "engine_only_types") {
        let name = text(entry, "name");
        assert!(names.insert(name));
        let descriptor =
            orna_sys_id::descriptor::engine_type(name).expect("engine type is generated");
        assert_eq!(
            descriptor.representation_contract,
            text(entry, "representation_contract")
        );
    }
    for entry in array(&engine, "retired_functions") {
        let name = text(entry, "name");
        assert!(names.insert(name));
        assert!(
            orna_sys_id::RETIRED_ENGINE_FUNCTIONS
                .iter()
                .any(|descriptor| descriptor.name == name)
        );
    }
    for entry in array(&engine, "failure_codes") {
        let code = entry.as_str().unwrap();
        assert!(names.insert(code));
        assert_eq!(
            SysFailureCode::parse(code).map(SysFailureCode::origin),
            Some(orna_sys_id::Origin::Engine)
        );
    }
    for entry in array(&engine, "test_fixture_names") {
        let name = entry.as_str().unwrap();
        assert!(names.insert(name));
        assert!(orna_sys_id::ENGINE_TEST_FIXTURE_NAMES.contains(&name));
    }
    assert_eq!(names.len(), orna_sys_id::ENGINE_NAME_COUNT);
    for family in ["inspect", "security", "state"] {
        let prefix = format!("sys.{family}.");
        assert_eq!(
            names
                .iter()
                .filter(|name| name.starts_with(&prefix))
                .count(),
            match family {
                "inspect" => 13,
                "security" => 13,
                _ => 2,
            }
        );
    }
    let portable_labels: BTreeSet<&str> = SysFunction::ALL.iter().map(|f| f.label()).collect();
    for name in names {
        assert!(
            !portable_labels.contains(name),
            "engine name `{name}` duplicates a portable callable"
        );
    }
    assert_eq!(AbiVersion::new(1, Some(0)).to_string(), "1.0");
}
