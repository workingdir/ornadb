use orna_syntax_v1::{Declaration, Expr, LiteralKind, TypeExpr, parse_module};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use orna_sys_v1::{
    AbiType, EffectSet, OperationContract, SystemEffect, system_api_json, system_api_schema_json,
    system_binding_modules_json, system_binding_stubs, system_function_descriptor,
    system_provider_abi, system_provider_abi_json, system_provider_abi_schema_json,
};
use serde_json::Value;

#[path = "../build_host.rs"]
#[allow(dead_code)]
mod build_host;
#[path = "../build_provider.rs"]
#[allow(dead_code)]
mod build_provider;
#[path = "../build_support.rs"]
#[allow(dead_code)]
mod build_support;

const GENERIC_KEYWORD_STUB_FIXTURE: &str = include_str!("fixtures/sys-invoke-generic-keyword.orna");
const GENERIC_START_KEYWORD_STUB_FIXTURE: &str =
    include_str!("fixtures/sys-start-generic-keyword.orna");
const GENERIC_START_KEYWORD_OVERLOAD_FIXTURE: &str =
    include_str!("fixtures/sys-start-generic-keyword-overloads.orna");
const GENERIC_INVOKE_KEYWORD_OVERLOAD_FIXTURE: &str =
    include_str!("fixtures/sys-invoke-generic-keyword-overloads.orna");
const GENERIC_OPERATION_STUB_FIXTURE: &str =
    include_str!("fixtures/sys-generic-operation-stubs.orna");
const STREAM_CONTROL_EDGE_STUB_FIXTURE: &str =
    include_str!("fixtures/sys-provider-stream-control-edges.orna");
const PROMISE_CALLBACK_STUB_FIXTURE: &str =
    include_str!("fixtures/sys-provider-promise-callback.orna");
const STREAM_ITERATOR_EDGE_STUB_FIXTURE: &str =
    include_str!("fixtures/sys-provider-stream-iterator-edge.orna");
const PROVIDER_MAP_ENTRY_EDGE_FIXTURE: &str =
    include_str!("fixtures/provider-map-entry-edges.json");
const PROVIDER_TUPLE_ENTRY_EDGE_FIXTURE: &str =
    include_str!("fixtures/provider-tuple-entry-edges.json");
const PROVIDER_SCHEMA_CAPTURE_EDGE_FIXTURE: &str =
    include_str!("fixtures/provider-schema-capture-edges.json");
const PROVIDER_FUNCTION_REFERENCE_EDGE_FIXTURE: &str =
    include_str!("fixtures/provider-function-reference-edges.json");
const PROVIDER_CANCEL_EDGE_FIXTURE: &str = include_str!("fixtures/provider-cancel-edges.json");
const PROVIDER_METADATA_EDGE_FIXTURE: &str = include_str!("fixtures/provider-metadata-edges.json");

fn resolve_type(ty: &TypeExpr) -> Result<AbiType, String> {
    match ty {
        TypeExpr::Name {
            path, arguments, ..
        } => {
            let constructor = path.join(".");
            if arguments.is_empty() {
                Ok(AbiType::Named(constructor))
            } else {
                Ok(AbiType::Applied {
                    constructor,
                    arguments: arguments
                        .iter()
                        .map(resolve_type)
                        .collect::<Result<_, _>>()?,
                })
            }
        }
        TypeExpr::Optional { inner, .. } => Ok(AbiType::Optional(Box::new(resolve_type(inner)?))),
        TypeExpr::List { inner, .. } => Ok(AbiType::List(Box::new(resolve_type(inner)?))),
        unsupported => Err(format!(
            "type is outside the baked provider ABI: {unsupported:?}"
        )),
    }
}

fn local_function_name(contract: &OperationContract) -> &str {
    contract
        .signature
        .callable
        .rsplit('.')
        .next()
        .expect("registered sys callable has a leaf name")
}

fn validate_stub_dispatch_inventory(
    source: &str,
    expected_operations: &[&str],
) -> Result<(), String> {
    let parsed = parse_module(source);
    if !parsed.is_ok() {
        return Err(format!(
            "generated declarations do not parse: {:?}",
            parsed.diagnostics
        ));
    }
    let markers = source
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    if markers.len() != parsed.value.items.len() {
        return Err(format!(
            "{} dispatch markers cover {} parsed declarations",
            markers.len(),
            parsed.value.items.len()
        ));
    }
    let unique_markers = markers
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    if unique_markers.len() != markers.len() {
        return Err("duplicate generated dispatch marker".to_owned());
    }
    if markers != expected_operations {
        return Err("generated dispatch markers differ from registry order".to_owned());
    }
    Ok(())
}

fn validate_stub_body(body: &Expr) -> Result<(), String> {
    let Expr::Call {
        callee, arguments, ..
    } = body
    else {
        return Err("stub body is not the generated declaration error".to_owned());
    };
    if !matches!(callee.as_ref(), Expr::Name { text, .. } if text == "error") {
        return Err("stub body does not call error".to_owned());
    }
    let is_string_argument = |index: usize, name: &str, value: &str| {
        arguments.get(index).is_some_and(|argument| {
            argument.name.as_deref() == Some(name)
                && matches!(
                    &argument.value,
                    Expr::Literal { text, kind: LiteralKind::String, .. }
                        if text == &format!("\"{value}\"")
                )
        })
    };
    if arguments.len() != 2
        || !is_string_argument(0, "code", "sys.binding.stub")
        || !is_string_argument(1, "message", "generated declaration stub")
    {
        return Err("stub body differs from the canonical generated declaration error".to_owned());
    }
    Ok(())
}

fn validate_stub_contract(source: &str, operation: &OperationContract) -> Result<(), String> {
    let parsed = parse_module(source);
    if !parsed.is_ok() {
        return Err(format!("stub does not parse: {:?}", parsed.diagnostics));
    }
    validate_stub_dispatch_inventory(source, &[operation.id.as_str()])?;

    let expected_module = operation
        .signature
        .callable
        .rsplit_once('.')
        .map(|(module, _)| module)
        .ok_or_else(|| "registered callable has no module path".to_owned())?;
    let module = source
        .lines()
        .find_map(|line| line.strip_prefix("// sys-module: "))
        .ok_or_else(|| "stub has no module marker".to_owned())?;
    if module != expected_module {
        return Err(format!(
            "stub module `{module}` differs from `{expected_module}`"
        ));
    }

    let mut aliases = std::collections::BTreeMap::new();
    for line in source.lines() {
        if let Some(alias) = line.strip_prefix("// sys-parameter-alias: ") {
            let (original, emitted) = alias
                .split_once('=')
                .ok_or_else(|| "stub parameter alias is malformed".to_owned())?;
            aliases.insert(original, emitted);
        }
    }

    let Declaration::Function { signature, body } = &parsed.value.items[0].declaration else {
        return Err("stub declaration is not a function".to_owned());
    };
    validate_stub_body(body)?;
    let expected_name = local_function_name(operation);
    if signature.name != expected_name {
        return Err(format!(
            "stub function `{}` differs from `{expected_name}`",
            signature.name
        ));
    }

    let generics = signature
        .generics
        .iter()
        .map(|generic| generic.name.as_str())
        .collect::<Vec<_>>();
    let expected_generics = operation
        .signature
        .type_parameters
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    if generics != expected_generics {
        return Err(format!(
            "stub generics {generics:?} differ from {expected_generics:?}"
        ));
    }
    if signature.parameters.len() != operation.signature.parameters.len() {
        return Err(format!(
            "stub has {} parameters; registry has {}",
            signature.parameters.len(),
            operation.signature.parameters.len()
        ));
    }

    for (parsed_parameter, registered_parameter) in signature
        .parameters
        .iter()
        .zip(&operation.signature.parameters)
    {
        let source_parameter = &source[parsed_parameter.span.start..parsed_parameter.span.end];
        let (name, _) = source_parameter
            .split_once(": ")
            .ok_or_else(|| "stub parameter has no explicit type".to_owned())?;
        let expected_name = aliases
            .get(registered_parameter.name.as_str())
            .copied()
            .unwrap_or(registered_parameter.name.as_str());
        if name != expected_name {
            return Err(format!(
                "stub parameter `{name}` differs from `{expected_name}`"
            ));
        }
        let parsed_type = resolve_type(
            parsed_parameter
                .annotation
                .as_ref()
                .ok_or_else(|| format!("stub parameter `{name}` has no type"))?,
        )?;
        if parsed_type != registered_parameter.ty {
            return Err(format!(
                "stub parameter `{name}` has a registry-incompatible type"
            ));
        }
        let default = parsed_parameter
            .default
            .as_ref()
            .map(|default| &source[default.span().start..default.span().end]);
        if default != registered_parameter.default.as_deref() {
            return Err(format!(
                "stub parameter `{name}` has a registry-incompatible default"
            ));
        }
    }

    let result = resolve_type(
        signature
            .result
            .as_ref()
            .ok_or_else(|| "stub has no result type".to_owned())?,
    )?;
    if result != operation.signature.result {
        return Err("stub result type differs from the typed registry".to_owned());
    }
    Ok(())
}

#[test]
fn generated_sys_stubs_parse_resolve_to_registry_types_and_dispatch_one_to_one() {
    let source = system_binding_stubs();
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "generated declarations must use supported Orna grammar: {:?}",
        parsed.diagnostics
    );

    let abi = system_provider_abi();
    let dispatch_metadata: Value =
        serde_json::from_str(system_provider_abi_json()).expect("generated dispatch metadata");
    let dispatch_rows = dispatch_metadata["operations"]
        .as_array()
        .expect("dispatch operation inventory")
        .iter()
        .map(|row| (row["name"].as_str().expect("dispatch operation name"), row))
        .collect::<std::collections::BTreeMap<_, _>>();
    let operations = abi.operations().collect::<Vec<_>>();
    let operation_count = operations.len();
    let expected_operation_ids = operations
        .iter()
        .map(|operation| operation.id.as_str())
        .collect::<Vec<_>>();
    validate_stub_dispatch_inventory(source, &expected_operation_ids)
        .expect("generated stub markers are a bijection with registry operations");
    let mut current_module = None;
    let mut operation_markers =
        Vec::<(String, String, std::collections::BTreeMap<String, String>)>::new();
    for line in source.lines() {
        if let Some(module) = line.strip_prefix("// sys-module: ") {
            current_module = Some(module.to_owned());
        } else if let Some(operation) = line.strip_prefix("// sys-op: ") {
            operation_markers.push((
                current_module
                    .clone()
                    .expect("operation has a module marker"),
                operation.to_owned(),
                Default::default(),
            ));
        } else if let Some(alias) = line.strip_prefix("// sys-parameter-alias: ") {
            let (original, emitted) = alias.split_once('=').expect("valid alias marker");
            operation_markers
                .last_mut()
                .expect("alias follows an operation marker")
                .2
                .insert(original.to_owned(), emitted.to_owned());
        }
    }
    assert_eq!(operation_markers.len(), operations.len());
    assert_eq!(
        operation_markers
            .iter()
            .map(|(_, operation, _)| operation)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        operation_markers.len(),
        "an operation may own only one emitted stub"
    );

    assert_eq!(parsed.value.items.len(), operation_count);
    let mut validated_stub_bodies = 0;
    for ((item, operation), (module_marker, marker, aliases)) in parsed
        .value
        .items
        .iter()
        .zip(operations)
        .zip(operation_markers)
    {
        assert_eq!(
            module_marker,
            operation
                .signature
                .callable
                .rsplit_once('.')
                .expect("sys operation has a module path")
                .0,
            "stub must be emitted into its registry module"
        );
        assert_eq!(
            marker,
            operation.id.as_str(),
            "dispatch marker must name its registry op"
        );
        let dispatch_row = dispatch_rows
            .get(marker.as_str())
            .unwrap_or_else(|| panic!("generated dispatch metadata omits {marker}"));
        assert_eq!(dispatch_row["name"].as_str(), Some(operation.id.as_str()));
        assert_eq!(
            dispatch_row["signature"].as_str(),
            Some(operation.signature.source.as_str()),
            "stub marker {marker} resolves to the same typed signature as dispatch metadata"
        );
        assert_eq!(
            dispatch_row["version"]["major"].as_u64(),
            Some(operation.version.major.into()),
            "stub marker {marker} resolves to the dispatch ABI version"
        );
        assert_eq!(
            dispatch_row["version"]["minor"].as_u64(),
            Some(operation.version.minor.into()),
            "stub marker {marker} resolves to the dispatch ABI version"
        );
        let expected_role = match (&operation.role, operation.role_version) {
            (Some(role), Some(version)) => Some(format!(
                "{}@{}.{}",
                role.as_str(),
                version.major,
                version.minor
            )),
            (None, None) => None,
            _ => panic!("role name/version must be present together for {marker}"),
        };
        assert_eq!(
            dispatch_row["role"].as_str(),
            expected_role.as_deref(),
            "stub marker {marker} resolves to the dispatch semantic role"
        );
        let Declaration::Function { signature, body } = &item.declaration else {
            panic!("each generated stub must be a function declaration")
        };
        validate_stub_body(body).unwrap_or_else(|error| {
            panic!(
                "generated stub body for {} is invalid: {error}",
                operation.id.as_str()
            )
        });
        validated_stub_bodies += 1;
        assert_eq!(signature.name, local_function_name(operation));
        assert_eq!(
            signature
                .generics
                .iter()
                .map(|generic| generic.name.as_str())
                .collect::<Vec<_>>(),
            operation
                .signature
                .type_parameters
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            "generic types for {} resolve from the registry",
            operation.id.as_str()
        );
        assert_eq!(
            signature.parameters.len(),
            operation.signature.parameters.len()
        );
        for (parsed_parameter, registered_parameter) in signature
            .parameters
            .iter()
            .zip(&operation.signature.parameters)
        {
            let parameter_source = &source[parsed_parameter.span.start..parsed_parameter.span.end];
            let (parameter_name, _) = parameter_source
                .split_once(": ")
                .expect("generated parameters have explicit types");
            let expected_name = aliases
                .get(&registered_parameter.name)
                .map(String::as_str)
                .unwrap_or(&registered_parameter.name);
            assert_eq!(parameter_name, expected_name);
            assert_eq!(
                resolve_type(
                    parsed_parameter
                        .annotation
                        .as_ref()
                        .expect("typed parameter")
                )
                .expect("supported provider type"),
                registered_parameter.ty,
                "parameter type for {}.{} resolves from the registry",
                operation.id.as_str(),
                registered_parameter.name
            );
            let parsed_default = parsed_parameter
                .default
                .as_ref()
                .map(|default| &source[default.span().start..default.span().end]);
            assert_eq!(parsed_default, registered_parameter.default.as_deref());
        }
        assert!(aliases.keys().all(|name| {
            operation
                .signature
                .parameters
                .iter()
                .any(|parameter| &parameter.name == name)
        }));
        assert_eq!(
            resolve_type(signature.result.as_ref().expect("typed result"))
                .expect("supported provider result type"),
            operation.signature.result,
            "result type for {} resolves from the registry",
            operation.id.as_str()
        );
    }
    assert_eq!(validated_stub_bodies, operation_count);
    println!(
        "generated_stub_validation operations={operation_count} canonical_placeholder_bodies={validated_stub_bodies}"
    );
}

#[test]
fn generated_stream_control_edges_match_bindings_and_schema_contracts() {
    const EXPECTED_OPERATIONS: [&str; 2] = ["sys.admin.pause_stream", "sys.admin.resume_stream"];

    let api_json = system_api_json();
    let api_schema_json = system_api_schema_json();
    let provider_json = system_provider_abi_json();
    let provider_schema_json = system_provider_abi_schema_json();
    build_host::validate_json_against_schema(&api_json, api_schema_json)
        .expect("generated sys API validates against its published schema");
    build_host::validate_json_against_schema(provider_json, provider_schema_json)
        .expect("generated provider contracts validate against their schema");

    let api: Value = serde_json::from_str(&api_json).expect("generated sys API JSON");
    let api_functions = api["functions"]
        .as_array()
        .expect("generated sys API function inventory");
    let stream_reference = api["reference_aliases"]
        .as_array()
        .expect("generated reference aliases")
        .iter()
        .find(|alias| alias["name"] == "sys.StreamRef")
        .expect("stream observation reference alias");
    assert_eq!(stream_reference["target"], "sys.Stream");
    assert_eq!(stream_reference["definition"], "sys.RowRef<sys.Stream>");
    let stream_relation = api["relations"]
        .as_array()
        .expect("generated sys relations")
        .iter()
        .find(|relation| relation["name"] == "sys.Stream")
        .expect("stream observation relation");
    assert_eq!(stream_relation["reference_type"], "sys.StreamRef");

    let abi = system_provider_abi();
    let dispatch: Value = serde_json::from_str(provider_json).expect("provider JSON");
    let stream_edges = abi
        .operations()
        .filter(|operation| {
            operation
                .signature
                .parameters
                .iter()
                .any(|parameter| parameter.ty == AbiType::Named("sys.StreamRef".to_owned()))
        })
        .map(|operation| operation.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(stream_edges, EXPECTED_OPERATIONS);

    let fixture = STREAM_CONTROL_EDGE_STUB_FIXTURE.trim_end();
    let fixture_module = parse_module(fixture);
    assert!(
        fixture_module.is_ok(),
        "stream edge fixture parses as generated Orna bindings: {:?}",
        fixture_module.diagnostics
    );
    assert_eq!(fixture_module.value.items.len(), EXPECTED_OPERATIONS.len());

    let generated_bindings = system_binding_stubs();
    let mut operation_cases = 0;
    for stub in fixture.split("\n\n") {
        let operation_id = stub
            .lines()
            .find_map(|line| line.strip_prefix("// sys-op: "))
            .expect("stream edge fixture has a dispatch marker");
        assert!(EXPECTED_OPERATIONS.contains(&operation_id));
        let operation = abi
            .operation(operation_id)
            .expect("stream edge resolves in provider registry");
        validate_stub_contract(stub, operation)
            .unwrap_or_else(|error| panic!("{operation_id} fixture parity: {error}"));

        let declaration = stub
            .lines()
            .find(|line| line.starts_with("pub fn "))
            .expect("stream edge fixture has a declaration");
        assert!(
            generated_bindings.lines().any(|line| line == declaration),
            "generated binding bundle emits the pinned declaration for {operation_id}"
        );

        let function = api_functions
            .iter()
            .find(|function| function["name"] == operation_id)
            .unwrap_or_else(|| panic!("API schema omits {operation_id}"));
        assert_eq!(function["effect"], "admin");
        assert_eq!(function["signature"], operation.signature.source);
        assert_eq!(
            operation.signature.parameters[0].ty,
            AbiType::Named("sys.StreamRef".to_owned())
        );
        assert_eq!(
            operation.signature.result,
            AbiType::Named("Bool".to_owned())
        );

        let dispatch_row = dispatch["operations"]
            .as_array()
            .expect("provider operation inventory")
            .iter()
            .find(|row| row["name"] == operation_id)
            .expect("stream edge has generated provider metadata");
        assert_eq!(dispatch_row["signature"], operation.signature.source);
        operation_cases += 1;
    }
    assert_eq!(operation_cases, EXPECTED_OPERATIONS.len());
    println!(
        "stream_control_binding_edges operations={operation_cases} stream_reference=linked api_schema=valid provider_schema=valid total_cases={}",
        operation_cases + 2
    );
}

#[test]
fn generated_provider_operation_iterator_edges_match_schema_and_bindings() {
    let generated_schema = build_provider::generate_provider_registry_schema()
        .expect("provider iterator-edge schema regenerates from its typed source");
    assert_eq!(generated_schema, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(system_provider_abi_json(), &generated_schema)
        .expect("embedded provider iterator conforms to its regenerated schema");

    let registry_json: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded provider registry");
    let raw_operations = registry_json["operations"]
        .as_array()
        .expect("generated provider registry has operation rows");
    let raw_roles = registry_json["roles"]
        .as_array()
        .expect("generated provider registry has role rows");
    let registry = orna_sys_v1::SystemProviderAbi::from_json(system_provider_abi_json())
        .expect("schema-validated provider registry parses into typed contracts");
    let generated_source = system_binding_stubs();
    let parsed_generated = parse_module(generated_source);
    assert!(
        parsed_generated.is_ok(),
        "generated binding bundle parses: {:?}",
        parsed_generated.diagnostics
    );
    let binding_names = generated_source
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<BTreeSet<_>>();
    let operation_names = registry
        .operations()
        .map(|operation| operation.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(binding_names, operation_names);

    let empty_json = serde_json::json!({
        "abi_version": registry_json["abi_version"].clone(),
        "operations": [],
        "roles": [],
    })
    .to_string();
    build_host::validate_json_against_schema(&empty_json, &generated_schema)
        .expect_err("published provider schema rejects an empty operation iterator");
    let empty_registry = orna_sys_v1::SystemProviderAbi::from_json(&empty_json)
        .expect("empty provider iterator edge parses");
    assert!(empty_registry.operations().next().is_none());
    assert!(empty_registry.roles().next().is_none());

    let singleton = registry
        .operations()
        .find(|operation| operation.role.is_some())
        .expect("provider ABI has a linked operation for a singleton iterator edge");
    let singleton_name = singleton.id.as_str();
    let singleton_row = raw_operations
        .iter()
        .find(|row| row["name"] == singleton_name)
        .expect("singleton operation has a generated schema row")
        .clone();
    let singleton_role_name = singleton
        .role
        .as_ref()
        .expect("selected singleton operation has a role")
        .as_str();
    let mut singleton_role_row = raw_roles
        .iter()
        .find(|row| row["name"] == singleton_role_name)
        .expect("singleton operation has a generated role row")
        .clone();
    singleton_role_row["operations"] = serde_json::json!([singleton_name]);
    let singleton_json = serde_json::json!({
        "abi_version": registry_json["abi_version"].clone(),
        "operations": [singleton_row],
        "roles": [singleton_role_row],
    })
    .to_string();
    build_host::validate_json_against_schema(&singleton_json, &generated_schema)
        .expect("singleton provider iterator edge remains schema-valid");
    let singleton_registry = orna_sys_v1::SystemProviderAbi::from_json(&singleton_json)
        .expect("singleton provider iterator edge parses");
    let mut singleton_operations = singleton_registry.operations();
    assert_eq!(
        singleton_operations
            .next()
            .map(|operation| operation.id.as_str()),
        Some(singleton_name)
    );
    assert!(singleton_operations.next().is_none());
    let singleton_binding = system_function_descriptor(singleton_name)
        .expect("singleton iterator contract has a generated binding");
    assert_eq!(singleton_binding.signature, singleton.signature.source);
    assert!(binding_names.contains(singleton_name));

    let mut full_operations = registry.operations();
    let first = full_operations
        .next()
        .expect("generated provider iterator has a first operation");
    let last = registry
        .operations()
        .last()
        .expect("generated provider iterator has a last operation");
    assert!(binding_names.contains(first.id.as_str()));
    assert!(binding_names.contains(last.id.as_str()));
    assert_eq!(
        registry.operations().count(),
        raw_operations.len(),
        "provider iterator cardinality matches its schema rows"
    );
    println!(
        "generated_provider_operation_iterator_edges schema_validations=3 edge_cases=3 empty_schema_rejected=1 singleton=1 full_operations={} generated_bindings={} boundary_bindings=2 total_cases={}",
        raw_operations.len(),
        binding_names.len(),
        3 + raw_operations.len() + binding_names.len() + 2
    );
}

#[test]
fn generated_provider_default_values_match_schema_and_idl_stubs() {
    let registry_json = system_provider_abi_json();
    let schema_json = system_provider_abi_schema_json();
    build_host::validate_json_against_schema(registry_json, schema_json)
        .expect("embedded provider defaults conform to the generated schema");

    let registry: Value =
        serde_json::from_str(registry_json).expect("embedded provider registry is valid JSON");
    let raw_operations = registry["operations"]
        .as_array()
        .expect("embedded provider registry has operation rows");
    let abi = system_provider_abi();
    let source = system_binding_stubs();
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "generated declaration bundle parses: {:?}",
        parsed.diagnostics
    );
    let operations = abi.operations().collect::<Vec<_>>();
    let expected_operations = operations
        .iter()
        .map(|operation| operation.id.as_str())
        .collect::<Vec<_>>();
    validate_stub_dispatch_inventory(source, &expected_operations)
        .expect("generated stub markers cover the provider registry in order");

    let mut defaulted_operations = 0;
    let mut defaulted_parameters = 0;
    let mut null_defaults = 0;
    let mut qualified_defaults = 0;
    for ((item, operation), marker) in parsed
        .value
        .items
        .iter()
        .zip(operations)
        .zip(expected_operations)
    {
        if operation.role.is_none() {
            continue;
        }
        let has_default = operation
            .signature
            .parameters
            .iter()
            .any(|parameter| parameter.default.is_some());
        if !has_default {
            continue;
        }
        assert_eq!(marker, operation.id.as_str());

        let row = raw_operations
            .iter()
            .find(|row| row["name"] == operation.id.as_str())
            .expect("schema-validated registry row covers the defaulted operation");
        assert_eq!(
            row["signature"].as_str(),
            Some(operation.signature.source.as_str()),
            "schema row keeps the typed signature for {}",
            operation.id.as_str()
        );
        let Declaration::Function { signature, .. } = &item.declaration else {
            panic!(
                "generated operation {} is not a function stub",
                operation.id.as_str()
            )
        };
        assert_eq!(
            signature.parameters.len(),
            operation.signature.parameters.len(),
            "stub parameter count for {}",
            operation.id.as_str()
        );
        for (parsed_parameter, registered_parameter) in signature
            .parameters
            .iter()
            .zip(&operation.signature.parameters)
        {
            let parsed_default = parsed_parameter
                .default
                .as_ref()
                .map(|default| &source[default.span().start..default.span().end]);
            assert_eq!(
                parsed_default,
                registered_parameter.default.as_deref(),
                "stub default matches typed registry for {}.{}",
                operation.id.as_str(),
                registered_parameter.name
            );
            let Some(default) = registered_parameter.default.as_deref() else {
                continue;
            };
            defaulted_parameters += 1;
            if default == "null" {
                null_defaults += 1;
            } else if default.contains('.') {
                qualified_defaults += 1;
            } else {
                panic!(
                    "unclassified provider default {default:?} on {}.{}",
                    operation.id.as_str(),
                    registered_parameter.name
                );
            }
        }
        defaulted_operations += 1;
    }

    assert!(defaulted_operations > 0);
    assert_eq!(defaulted_parameters, 14);
    assert!(null_defaults > 0);
    assert!(qualified_defaults > 0);
    println!(
        "generated_provider_default_idl_parity operations={defaulted_operations} defaults={defaulted_parameters} null={null_defaults} qualified={qualified_defaults} total_cases={}",
        defaulted_operations + defaulted_parameters
    );
}

#[test]
fn generated_provider_optional_arguments_match_schema_and_idl_stubs() {
    let registry_json = system_provider_abi_json();
    let schema_json = system_provider_abi_schema_json();
    build_host::validate_json_against_schema(registry_json, schema_json)
        .expect("embedded optional provider arguments conform to the generated schema");
    let registry: Value =
        serde_json::from_str(registry_json).expect("embedded provider registry is valid JSON");
    let raw_operations = registry["operations"]
        .as_array()
        .expect("embedded provider registry has operation rows");
    let abi = system_provider_abi();
    let source = system_binding_stubs();
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "generated optional argument declarations parse: {:?}",
        parsed.diagnostics
    );
    let operations = abi.operations().collect::<Vec<_>>();
    let expected_operations = operations
        .iter()
        .map(|operation| operation.id.as_str())
        .collect::<Vec<_>>();
    validate_stub_dispatch_inventory(source, &expected_operations)
        .expect("generated stub inventory matches the typed provider registry");

    let mut optional_operations = 0;
    let mut optional_parameters = 0;
    let mut null_default_parameters = 0;
    for ((item, operation), marker) in parsed
        .value
        .items
        .iter()
        .zip(operations)
        .zip(expected_operations)
    {
        if operation.role.is_none() {
            continue;
        }
        let optional_indexes = operation
            .signature
            .parameters
            .iter()
            .enumerate()
            .filter_map(|(index, parameter)| {
                matches!(&parameter.ty, AbiType::Optional(_)).then_some(index)
            })
            .collect::<Vec<_>>();
        if optional_indexes.is_empty() {
            continue;
        }
        assert_eq!(marker, operation.id.as_str());
        let generated = system_function_descriptor(operation.id.as_str())
            .expect("optional provider operation has a macro-generated binding");
        assert_eq!(generated.signature, operation.signature.source);
        assert_eq!(
            operation.effects.iter().next(),
            Some(generated.effect),
            "generated binding effect matches optional provider operation"
        );
        let row = raw_operations
            .iter()
            .find(|row| row["name"] == operation.id.as_str())
            .expect("schema-validated registry row covers optional provider operation");
        assert_eq!(
            row["signature"].as_str(),
            Some(operation.signature.source.as_str()),
            "schema row preserves optional provider signature {}",
            operation.id.as_str()
        );

        let Declaration::Function { signature, .. } = &item.declaration else {
            panic!(
                "generated operation {} is not a function stub",
                operation.id.as_str()
            )
        };
        for index in optional_indexes {
            let registered_parameter = &operation.signature.parameters[index];
            let parsed_parameter = &signature.parameters[index];
            assert_eq!(
                resolve_type(
                    parsed_parameter
                        .annotation
                        .as_ref()
                        .expect("optional binding parameter has a type")
                )
                .expect("generated optional type resolves"),
                registered_parameter.ty,
                "generated IDL optional type matches {}.{}",
                operation.id.as_str(),
                registered_parameter.name
            );
            let parsed_default = parsed_parameter
                .default
                .as_ref()
                .map(|default| &source[default.span().start..default.span().end]);
            assert_eq!(
                parsed_default,
                registered_parameter.default.as_deref(),
                "generated IDL optional default matches {}.{}",
                operation.id.as_str(),
                registered_parameter.name
            );
            assert_eq!(
                registered_parameter.default.as_deref(),
                Some("null"),
                "optional provider argument {}.{} uses the null default",
                operation.id.as_str(),
                registered_parameter.name
            );
            optional_parameters += 1;
            null_default_parameters += 1;
        }
        optional_operations += 1;
    }

    assert!(optional_operations > 0);
    assert_eq!(null_default_parameters, optional_parameters);
    println!(
        "generated_provider_optional_idl_parity operations={optional_operations} optional_arguments={optional_parameters} null_defaults={null_default_parameters} schema_validated=1 total_cases={}",
        optional_operations + optional_parameters + 1
    );
}

#[test]
fn generated_provider_argument_map_variadics_keep_one_schema_and_idl_slot() {
    const VARIADIC_OPERATIONS: [&str; 4] = [
        "sys.invoke(Value)",
        "sys.invoke<T>",
        "sys.start(Value)",
        "sys.start<T>",
    ];

    let registry_json = system_provider_abi_json();
    let schema_json = build_provider::generate_provider_registry_schema()
        .expect("provider schema regenerates for argument-map parity");
    assert_eq!(schema_json, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(registry_json, &schema_json)
        .expect("embedded provider registry conforms to the regenerated schema");
    let registry: Value =
        serde_json::from_str(registry_json).expect("embedded provider registry is valid JSON");
    let rows = registry["operations"]
        .as_array()
        .expect("embedded registry has operation rows");

    let abi = system_provider_abi();
    let source = system_binding_stubs();
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "generated argument-map declarations parse: {:?}",
        parsed.diagnostics
    );
    let markers = source
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    let operations = abi.operations().collect::<Vec<_>>();
    let expected_operations = operations
        .iter()
        .map(|operation| operation.id.as_str())
        .collect::<Vec<_>>();
    validate_stub_dispatch_inventory(source, &expected_operations)
        .expect("generated declaration markers match the typed registry");

    let mut map_slots = 0;
    for operation_name in VARIADIC_OPERATIONS {
        let contract = abi
            .operation(operation_name)
            .expect("argument-map operation exists in the typed registry");
        let registry_row = rows
            .iter()
            .find(|row| row["name"] == operation_name)
            .expect("schema-validated operation row exists");
        assert_eq!(
            registry_row["signature"].as_str(),
            Some(contract.signature.source.as_str()),
            "schema row preserves the typed signature for {operation_name}"
        );
        let generated = system_function_descriptor(operation_name)
            .expect("argument-map operation has a macro-generated binding");
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(
            contract.effects.iter().next(),
            Some(generated.effect),
            "macro-generated effect matches {operation_name}"
        );

        let registry_map_slots = contract
            .signature
            .parameters
            .iter()
            .enumerate()
            .filter(|(_, parameter)| parameter.name == "arguments")
            .collect::<Vec<_>>();
        assert_eq!(
            registry_map_slots.len(),
            1,
            "one typed map slot in {operation_name}"
        );
        let (map_index, registry_map_parameter) = registry_map_slots[0];
        assert_eq!(
            registry_map_parameter.ty,
            AbiType::Named("sys.ArgumentMap".to_owned()),
            "map remains one fixed ABI type in {operation_name}"
        );

        let item_index = markers
            .iter()
            .position(|marker| *marker == operation_name)
            .expect("argument-map operation has a generated IDL marker");
        let Declaration::Function { signature, .. } = &parsed.value.items[item_index].declaration
        else {
            panic!("generated binding {operation_name} is not a function")
        };
        assert_eq!(
            signature.parameters.len(),
            contract.signature.parameters.len(),
            "IDL outer arity stays fixed for {operation_name}"
        );
        let parsed_map_parameter = &signature.parameters[map_index];
        let parsed_map_name = &source
            [parsed_map_parameter.span.start..parsed_map_parameter.span.end]
            .split_once(": ")
            .expect("generated argument-map parameter has an explicit type")
            .0;
        assert_eq!(*parsed_map_name, "arguments");
        assert_eq!(
            resolve_type(
                parsed_map_parameter
                    .annotation
                    .as_ref()
                    .expect("generated argument-map parameter has a type")
            )
            .expect("generated argument-map type resolves"),
            registry_map_parameter.ty,
            "IDL carries one sys.ArgumentMap slot for {operation_name}"
        );
        map_slots += 1;
    }

    assert_eq!(map_slots, VARIADIC_OPERATIONS.len());
    println!(
        "generated_provider_argument_map_idl_parity operations={} map_slots={map_slots} one_map_slot_per_operation=1 macro_parity=1 schema_validated=1 total_cases={}",
        VARIADIC_OPERATIONS.len(),
        VARIADIC_OPERATIONS.len() * 3 + 1
    );
}

#[test]
fn generated_provider_map_entry_fixture_matches_schema_and_bindings() {
    const OPERATIONS: [&str; 4] = [
        "sys.invoke(Value)",
        "sys.invoke<T>",
        "sys.start(Value)",
        "sys.start<T>",
    ];
    const FIXTURE_CASES: [(&str, usize); 3] = [
        ("empty", 0),
        ("empty-key", 1),
        ("unicode-control-heterogeneous-protected", 6),
    ];

    let generated_schema = build_provider::generate_provider_registry_schema()
        .expect("map-entry provider schema regenerates from its typed source");
    assert_eq!(generated_schema, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(system_provider_abi_json(), &generated_schema)
        .expect("embedded map-entry binding contracts conform to the generated schema");

    let fixture: Value = serde_json::from_str(PROVIDER_MAP_ENTRY_EDGE_FIXTURE)
        .expect("crate-local provider map-entry edge fixture is valid JSON");
    let cases = fixture["cases"]
        .as_array()
        .expect("provider map-entry fixture has cases");
    assert_eq!(cases.len(), FIXTURE_CASES.len());
    for (case_name, entry_count) in FIXTURE_CASES {
        let case = cases
            .iter()
            .find(|case| case["name"] == case_name)
            .unwrap_or_else(|| panic!("fixture retains {case_name} map-entry edges"));
        assert_eq!(
            case["entries"].as_array().map(Vec::len),
            Some(entry_count),
            "{case_name} fixture cardinality"
        );
    }

    let source = system_binding_stubs();
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "generated map-entry bindings parse: {:?}",
        parsed.diagnostics
    );
    let registry = system_provider_abi();
    let mut typed_contracts = 0;
    let mut generated_bindings = 0;
    let mut map_slots = 0;
    for operation_name in OPERATIONS {
        let contract = registry
            .operation(operation_name)
            .expect("map-entry operation exists in typed provider registry");
        let generated = system_function_descriptor(operation_name)
            .expect("map-entry operation has a macro-generated binding");
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(contract.effects.iter().next(), Some(generated.effect));
        typed_contracts += 1;

        let marker = format!("// sys-op: {operation_name}");
        let declaration = source
            .split("\n\n")
            .find(|declaration| declaration.lines().any(|line| line == marker))
            .expect("generated bundle contains each map-entry declaration");
        validate_stub_contract(declaration, contract)
            .unwrap_or_else(|error| panic!("{operation_name} binding parity: {error}"));
        generated_bindings += 1;

        let map_parameters = contract
            .signature
            .parameters
            .iter()
            .filter(|parameter| parameter.name == "arguments")
            .collect::<Vec<_>>();
        assert_eq!(map_parameters.len(), 1);
        assert_eq!(
            map_parameters[0].ty,
            AbiType::Named("sys.ArgumentMap".to_owned())
        );
        map_slots += map_parameters.len();
    }

    assert_eq!(typed_contracts, OPERATIONS.len());
    assert_eq!(generated_bindings, typed_contracts);
    assert_eq!(map_slots, OPERATIONS.len());
    println!(
        "generated_provider_map_entry_binding_parity operations={generated_bindings} fixture_cases={} fixture_entries={} map_slots={map_slots} schema_validated=1 typed_contracts={typed_contracts} total_cases={}",
        FIXTURE_CASES.len(),
        FIXTURE_CASES.iter().map(|(_, count)| count).sum::<usize>(),
        generated_bindings + FIXTURE_CASES.len() + map_slots + 1
    );
}

#[test]
fn generated_provider_tuple_entry_fixture_matches_schema_and_bindings() {
    const OPERATIONS: [&str; 4] = [
        "sys.invoke(Value)",
        "sys.invoke<T>",
        "sys.start(Value)",
        "sys.start<T>",
    ];
    const FIXTURE_CASES: [(&str, usize); 4] = [
        ("empty", 0),
        ("single-tuple", 1),
        ("nested-optional-control", 3),
        ("protected-tuple", 1),
    ];

    let generated_schema = build_provider::generate_provider_registry_schema()
        .expect("tuple-entry provider schema regenerates from its typed source");
    assert_eq!(generated_schema, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(system_provider_abi_json(), &generated_schema)
        .expect("embedded tuple-entry binding contracts conform to the generated schema");

    let fixture: Value = serde_json::from_str(PROVIDER_TUPLE_ENTRY_EDGE_FIXTURE)
        .expect("crate-local provider tuple-entry edge fixture is valid JSON");
    let cases = fixture["cases"]
        .as_array()
        .expect("provider tuple-entry fixture has cases");
    assert_eq!(cases.len(), FIXTURE_CASES.len());
    let mut tuple_entries = 0;
    for (case_name, entry_count) in FIXTURE_CASES {
        let case = cases
            .iter()
            .find(|case| case["name"] == case_name)
            .unwrap_or_else(|| panic!("fixture retains {case_name} tuple-entry edges"));
        let entries = case["entries"]
            .as_array()
            .unwrap_or_else(|| panic!("{case_name} fixture has an entries array"));
        assert_eq!(
            entries.len(),
            entry_count,
            "{case_name} fixture cardinality"
        );
        assert!(
            entries.iter().all(|entry| {
                entry["type"]
                    .as_str()
                    .is_some_and(|ty| ty.starts_with('(') && ty.ends_with(')'))
            }),
            "{case_name} entries retain tuple static types"
        );
        tuple_entries += entries.len();
    }

    let source = system_binding_stubs();
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "generated tuple-entry bindings parse: {:?}",
        parsed.diagnostics
    );
    let registry = system_provider_abi();
    let mut typed_contracts = 0;
    let mut generated_bindings = 0;
    let mut map_slots = 0;
    for operation_name in OPERATIONS {
        let contract = registry
            .operation(operation_name)
            .expect("tuple-entry operation exists in typed provider registry");
        let generated = system_function_descriptor(operation_name)
            .expect("tuple-entry operation has a macro-generated binding");
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(contract.effects.iter().next(), Some(generated.effect));
        typed_contracts += 1;

        let marker = format!("// sys-op: {operation_name}");
        let declaration = source
            .split("\n\n")
            .find(|declaration| declaration.lines().any(|line| line == marker))
            .expect("generated bundle contains each tuple-entry declaration");
        validate_stub_contract(declaration, contract)
            .unwrap_or_else(|error| panic!("{operation_name} binding parity: {error}"));
        generated_bindings += 1;

        let map_parameters = contract
            .signature
            .parameters
            .iter()
            .filter(|parameter| parameter.name == "arguments")
            .collect::<Vec<_>>();
        assert_eq!(map_parameters.len(), 1);
        assert_eq!(
            map_parameters[0].ty,
            AbiType::Named("sys.ArgumentMap".to_owned())
        );
        map_slots += map_parameters.len();
    }

    assert_eq!(typed_contracts, OPERATIONS.len());
    assert_eq!(generated_bindings, typed_contracts);
    assert_eq!(map_slots, OPERATIONS.len());
    assert_eq!(tuple_entries, 5);
    println!(
        "generated_provider_tuple_entry_binding_parity operations={generated_bindings} fixture_cases={} tuple_entries={tuple_entries} map_slots={map_slots} schema_validated=1 typed_contracts={typed_contracts} total_cases={}",
        FIXTURE_CASES.len(),
        generated_bindings + FIXTURE_CASES.len() + tuple_entries + map_slots + 1
    );
}

#[test]
fn generated_provider_function_reference_edges_match_schema_and_bindings() {
    let fixture: Value = serde_json::from_str(PROVIDER_FUNCTION_REFERENCE_EDGE_FIXTURE)
        .expect("crate-local provider function-reference fixture is valid JSON");
    let operation_name = fixture["operation"]
        .as_str()
        .expect("function-reference fixture identifies a provider operation");
    let parameter_name = fixture["parameter"]
        .as_str()
        .expect("function-reference fixture identifies a parameter");
    let expected_type = fixture["expected_type"]
        .as_str()
        .expect("function-reference fixture identifies its expected type");
    let cases = fixture["cases"]
        .as_array()
        .expect("function-reference fixture has edge cases");
    assert_eq!(cases.len(), 4);

    let generated_schema = build_provider::generate_provider_registry_schema()
        .expect("provider function-reference schema regenerates from its typed source");
    assert_eq!(generated_schema, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(system_provider_abi_json(), &generated_schema)
        .expect("embedded function-reference operation conforms to its generated schema");
    let registry = orna_sys_v1::SystemProviderAbi::from_json(system_provider_abi_json())
        .expect("schema-validated function-reference registry parses into typed contracts");
    let contract = registry
        .operation(operation_name)
        .expect("function-reference operation exists in the typed registry");
    let parameter_index = contract
        .signature
        .parameters
        .iter()
        .position(|parameter| parameter.name == parameter_name)
        .expect("function-reference parameter exists in its operation");
    assert_eq!(
        contract.signature.parameters[parameter_index]
            .ty
            .canonical(),
        expected_type
    );

    let generated = system_function_descriptor(operation_name)
        .expect("function-reference operation has a macro-generated descriptor");
    assert_eq!(generated.signature, contract.signature.source);
    assert_eq!(contract.effects.iter().next(), Some(generated.effect));
    let source = system_binding_stubs();
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "generated function-reference binding bundle parses: {:?}",
        parsed.diagnostics
    );
    let markers = source
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    let item_index = markers
        .iter()
        .position(|marker| *marker == operation_name)
        .expect("generated IDL has the function-reference operation marker");
    let Declaration::Function { signature, .. } = &parsed.value.items[item_index].declaration
    else {
        panic!("generated function-reference binding is not a function")
    };
    assert_eq!(
        signature.parameters.len(),
        contract.signature.parameters.len()
    );
    assert_eq!(
        resolve_type(
            signature.parameters[parameter_index]
                .annotation
                .as_ref()
                .expect("generated function-reference parameter has a type")
        )
        .expect("generated function-reference parameter type resolves")
        .canonical(),
        expected_type
    );
    assert_eq!(
        resolve_type(
            signature
                .result
                .as_ref()
                .expect("generated function-reference result is typed")
        )
        .expect("generated function-reference result type resolves"),
        contract.signature.result
    );

    for case in cases {
        let name = case["name"]
            .as_str()
            .expect("function-reference fixture case has a stable name");
        let argument_type = case["argument_type"]
            .as_str()
            .expect("function-reference fixture case has an argument type");
        let accepted = case["accepted"]
            .as_bool()
            .expect("function-reference fixture case states acceptance");
        assert_eq!(
            accepted,
            argument_type == expected_type,
            "fixture acceptance matches the generated {parameter_name} type for {name}"
        );
    }
    println!(
        "generated_provider_function_reference_binding_parity operation={operation_name} cases={} schema_validated=1 typed_contracts=1 macro_bindings=1 idl_parameters={} callback_parameter={parameter_name} callback_type={expected_type} total_cases={}",
        cases.len(),
        signature.parameters.len(),
        cases.len() + signature.parameters.len() + 5
    );
}

#[test]
fn generated_provider_schema_capture_edges_match_bindings() {
    let fixture: Value = serde_json::from_str(PROVIDER_SCHEMA_CAPTURE_EDGE_FIXTURE)
        .expect("crate-local provider schema-capture fixture is valid JSON");
    let operation_name = fixture["operation"]
        .as_str()
        .expect("schema-capture fixture identifies a provider operation");
    let baseline: Value = serde_json::from_str(system_provider_abi_json())
        .expect("embedded provider registry is valid JSON");
    let schema_json = build_provider::generate_provider_registry_schema()
        .expect("provider schema-capture schema regenerates from its typed source");
    assert_eq!(schema_json, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(system_provider_abi_json(), &schema_json)
        .expect("embedded provider registry conforms to its generated schema");

    let operation_index = baseline["operations"]
        .as_array()
        .expect("provider registry has operation rows")
        .iter()
        .position(|operation| operation["name"] == operation_name)
        .expect("schema-capture operation exists in the provider registry");
    let role_annotation = baseline["operations"][operation_index]["role"]
        .as_str()
        .expect("schema-capture operation has a provider role");
    let role_name = role_annotation
        .split_once('@')
        .expect("provider role annotation captures its version")
        .0;
    let role_index = baseline["roles"]
        .as_array()
        .expect("provider registry has role rows")
        .iter()
        .position(|role| role["name"] == role_name)
        .expect("schema-capture operation role exists in the provider registry");
    let cases = fixture["cases"]
        .as_array()
        .expect("schema-capture fixture has edge cases");
    assert_eq!(cases.len(), 4);

    let mut schema_acceptances = 0;
    let mut schema_rejections = 0;
    let mut typed_acceptances = 0;
    let mut typed_rejections = 0;
    let mut generated_bindings = 0;
    let source = system_binding_stubs();
    for case in cases {
        let case_name = case["name"]
            .as_str()
            .expect("schema-capture case has a stable name");
        let field = case["field"]
            .as_str()
            .expect("schema-capture case identifies a nullable field");
        let omit = case["omit"]
            .as_bool()
            .expect("schema-capture case states field presence");
        let mut captured = baseline.clone();
        match field {
            "operation.role" => {
                let operation = captured["operations"][operation_index]
                    .as_object_mut()
                    .expect("provider operation row is an object");
                if omit {
                    operation.remove("role");
                } else {
                    operation.insert("role".to_owned(), case["value"].clone());
                    if case["value"].is_null() {
                        let role_operations = captured["roles"][role_index]["operations"]
                            .as_array_mut()
                            .expect("provider role captures its operation list");
                        role_operations.retain(|operation| operation != operation_name);
                        assert!(
                            !role_operations.is_empty(),
                            "null-role capture keeps the shared role non-empty"
                        );
                    }
                }
            }
            "role.builtin_provider" => {
                let role = captured["roles"][role_index]
                    .as_object_mut()
                    .expect("provider role row is an object");
                if let Some(required) = case.get("required").and_then(Value::as_bool) {
                    role.insert("required".to_owned(), Value::Bool(required));
                }
                if omit {
                    role.remove("builtin_provider");
                } else {
                    role.insert("builtin_provider".to_owned(), case["value"].clone());
                }
            }
            other => panic!("unexpected schema-capture field {other:?}"),
        }

        let captured_json = captured.to_string();
        let schema_result = build_host::validate_json_against_schema(&captured_json, &schema_json);
        let schema_accepts = case["schema_accepts"]
            .as_bool()
            .expect("schema-capture fixture records schema acceptance");
        assert_eq!(
            schema_result.is_ok(),
            schema_accepts,
            "generated schema capture for {case_name}"
        );
        if schema_accepts {
            schema_acceptances += 1;
        } else {
            assert!(
                schema_result
                    .expect_err("omitted nullable fields violate the schema")
                    .contains("missing required field"),
                "schema identifies omitted nullable property in {case_name}"
            );
            schema_rejections += 1;
        }

        let parsed = orna_sys_v1::SystemProviderAbi::from_json(&captured_json);
        let typed_accepts = case["typed_parse_accepts"]
            .as_bool()
            .expect("schema-capture fixture records typed parser acceptance");
        assert_eq!(
            parsed.is_ok(),
            typed_accepts,
            "typed schema capture for {case_name}"
        );
        match parsed {
            Ok(table) => {
                table
                    .validate()
                    .unwrap_or_else(|error| panic!("captured provider table validates: {error:?}"));
                let contract = table
                    .operation(operation_name)
                    .expect("captured provider retains the generated operation");
                let generated = system_function_descriptor(operation_name)
                    .expect("captured provider operation retains its generated binding");
                assert_eq!(generated.signature, contract.signature.source);
                assert_eq!(contract.effects.iter().next(), Some(generated.effect));
                let marker = format!("// sys-op: {operation_name}");
                let declaration = source
                    .split("\n\n")
                    .find(|declaration| declaration.lines().any(|line| line == marker))
                    .expect("generated binding bundle retains the captured operation");
                validate_stub_contract(declaration, contract)
                    .unwrap_or_else(|error| panic!("{case_name} binding parity: {error}"));
                generated_bindings += 1;
                typed_acceptances += 1;
            }
            Err(error) => {
                assert_eq!(
                    error,
                    orna_sys_v1::ProviderAbiError::InvalidJson,
                    "omitted nullable fields are not captured by the typed parser"
                );
                typed_rejections += 1;
            }
        }
    }

    assert_eq!(schema_acceptances, 2);
    assert_eq!(schema_rejections, 2);
    assert_eq!(typed_acceptances, schema_acceptances);
    assert_eq!(typed_rejections, schema_rejections);
    assert_eq!(generated_bindings, schema_acceptances);
    println!(
        "generated_provider_schema_capture_binding_parity cases={} schema_acceptances={schema_acceptances} schema_rejections={schema_rejections} typed_acceptances={typed_acceptances} typed_rejections={typed_rejections} generated_bindings={generated_bindings} total_cases={}",
        cases.len(),
        cases.len()
            + schema_acceptances
            + schema_rejections
            + typed_acceptances
            + typed_rejections
            + generated_bindings
    );
}

#[test]
fn generated_provider_aliases_preserve_argument_map_binding_parity() {
    const PROVIDER_ALIASES: [&str; 4] = ["-", "_", "9", "_9.edge-name"];
    const VARIADIC_OPERATIONS: [&str; 4] = [
        "sys.invoke(Value)",
        "sys.invoke<T>",
        "sys.start(Value)",
        "sys.start<T>",
    ];

    let schema_json = build_provider::generate_provider_registry_schema()
        .expect("provider-alias schema regenerates from the typed source");
    assert_eq!(schema_json, system_provider_abi_schema_json());
    let baseline: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded provider registry");
    let base_abi = system_provider_abi();
    let raw_roles = baseline["roles"]
        .as_array()
        .expect("embedded registry has role rows");
    let mut operation_roles = Vec::new();
    for operation_name in VARIADIC_OPERATIONS {
        let role_name = base_abi
            .operation(operation_name)
            .and_then(|operation| operation.role.as_ref())
            .expect("invoke/start overload has a generated provider role")
            .as_str();
        if operation_roles
            .iter()
            .any(|(registered_role, _): &(&str, usize)| *registered_role == role_name)
        {
            continue;
        }
        let role_index = raw_roles
            .iter()
            .position(|role| role["name"] == role_name)
            .expect("invoke/start provider role has a generated row");
        operation_roles.push((role_name, role_index));
    }

    let source = system_binding_stubs();
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "generated invoke/start binding bundle parses: {:?}",
        parsed.diagnostics
    );
    let markers = source
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    let expected_operations = base_abi
        .operations()
        .map(|operation| operation.id.as_str())
        .collect::<Vec<_>>();
    validate_stub_dispatch_inventory(source, &expected_operations)
        .expect("generated binding markers match the typed registry");

    let mut schema_acceptances = 0;
    let mut typed_alias_parses = 0;
    let mut binding_cases = 0;
    let mut map_slots = 0;
    for provider_alias in PROVIDER_ALIASES {
        let mut alias_registry = baseline.clone();
        for (_, role_index) in &operation_roles {
            alias_registry["roles"][*role_index]["builtin_provider"] =
                Value::String(provider_alias.to_owned());
        }
        let registry_json = alias_registry.to_string();
        build_host::validate_json_against_schema(&registry_json, &schema_json).unwrap_or_else(
            |error| panic!("schema rejects valid provider alias {provider_alias:?}: {error}"),
        );
        schema_acceptances += 1;

        let alias_abi = orna_sys_v1::SystemProviderAbi::from_json(&registry_json)
            .expect("schema-valid provider alias parses into the typed registry");
        for (role_name, _) in &operation_roles {
            assert_eq!(
                alias_abi
                    .role(role_name)
                    .and_then(|role| role.builtin_provider.as_ref())
                    .map(|provider| provider.as_str()),
                Some(provider_alias)
            );
        }
        typed_alias_parses += 1;

        let rows = alias_registry["operations"]
            .as_array()
            .expect("mutated registry keeps its operation rows");
        for operation_name in VARIADIC_OPERATIONS {
            let contract = alias_abi
                .operation(operation_name)
                .expect("invoke/start operation survives provider alias parsing");
            let row = rows
                .iter()
                .find(|row| row["name"] == operation_name)
                .expect("typed operation has a generated registry row");
            assert_eq!(
                row["signature"].as_str(),
                Some(contract.signature.source.as_str()),
                "schema-valid row signature matches typed operation {operation_name}"
            );
            let generated = system_function_descriptor(operation_name)
                .expect("invoke/start operation has a macro-generated binding");
            assert_eq!(generated.signature, contract.signature.source);
            assert_eq!(
                contract.effects.iter().next(),
                Some(generated.effect),
                "generated effect matches provider-alias operation {operation_name}"
            );

            let map_indexes = contract
                .signature
                .parameters
                .iter()
                .enumerate()
                .filter_map(|(index, parameter)| (parameter.name == "arguments").then_some(index))
                .collect::<Vec<_>>();
            assert_eq!(map_indexes, [1], "one typed map slot in {operation_name}");
            assert_eq!(
                contract.signature.parameters[map_indexes[0]].ty,
                AbiType::Named("sys.ArgumentMap".to_owned())
            );

            let item_index = markers
                .iter()
                .position(|marker| *marker == operation_name)
                .expect("generated IDL has the aliased operation marker");
            let Declaration::Function { signature, .. } =
                &parsed.value.items[item_index].declaration
            else {
                panic!("generated binding {operation_name} is not a function")
            };
            assert_eq!(
                signature.parameters.len(),
                contract.signature.parameters.len(),
                "provider alias preserves fixed IDL arity for {operation_name}"
            );
            let parsed_map = &signature.parameters[map_indexes[0]];
            assert_eq!(
                resolve_type(
                    parsed_map
                        .annotation
                        .as_ref()
                        .expect("generated IDL map parameter has an annotation")
                )
                .expect("generated IDL argument-map type resolves"),
                AbiType::Named("sys.ArgumentMap".to_owned()),
                "provider alias keeps the generated sys.ArgumentMap binding for {operation_name}"
            );
            binding_cases += 1;
            map_slots += 1;
        }
    }

    assert_eq!(schema_acceptances, PROVIDER_ALIASES.len());
    assert_eq!(typed_alias_parses, PROVIDER_ALIASES.len());
    assert_eq!(
        binding_cases,
        PROVIDER_ALIASES.len() * VARIADIC_OPERATIONS.len()
    );
    assert_eq!(map_slots, binding_cases);
    println!(
        "generated_provider_alias_argument_map_binding_parity aliases={} schema_acceptances={schema_acceptances} typed_parses={typed_alias_parses} operations={} binding_cases={binding_cases} map_slots={map_slots} total_cases={}",
        PROVIDER_ALIASES.len(),
        VARIADIC_OPERATIONS.len(),
        schema_acceptances + typed_alias_parses + binding_cases + map_slots
    );
}

#[test]
fn generated_provider_alias_optional_defaults_match_schema_and_idl() {
    const PROVIDER_ALIASES: [&str; 4] = ["-", "_", "9", "_9.edge-name"];
    const OPTIONAL_OPERATIONS: [&str; 4] = [
        "sys.invoke(Value)",
        "sys.invoke<T>",
        "sys.start(Value)",
        "sys.start<T>",
    ];

    let generated_schema = build_provider::generate_provider_registry_schema()
        .expect("provider alias schema regenerates from its typed source");
    assert_eq!(generated_schema, system_provider_abi_schema_json());
    let baseline: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded provider registry");
    let base_abi = system_provider_abi();
    let raw_roles = baseline["roles"]
        .as_array()
        .expect("embedded registry has role rows");
    let mut operation_roles = Vec::new();
    for operation_name in OPTIONAL_OPERATIONS {
        let role_name = base_abi
            .operation(operation_name)
            .and_then(|operation| operation.role.as_ref())
            .expect("invoke/start overload has a generated provider role")
            .as_str();
        if operation_roles
            .iter()
            .any(|(registered_role, _): &(&str, usize)| *registered_role == role_name)
        {
            continue;
        }
        let role_index = raw_roles
            .iter()
            .position(|role| role["name"] == role_name)
            .expect("invoke/start provider role has a generated row");
        operation_roles.push((role_name, role_index));
    }

    let source = system_binding_stubs();
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "generated invoke/start binding bundle parses: {:?}",
        parsed.diagnostics
    );
    let markers = source
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();

    let mut schema_acceptances = 0;
    let mut typed_alias_parses = 0;
    let mut binding_cases = 0;
    let mut optional_defaults = 0;
    for provider_alias in PROVIDER_ALIASES {
        let mut alias_registry = baseline.clone();
        for (_, role_index) in &operation_roles {
            alias_registry["roles"][*role_index]["builtin_provider"] =
                Value::String(provider_alias.to_owned());
        }
        let registry_json = alias_registry.to_string();
        build_host::validate_json_against_schema(&registry_json, &generated_schema).unwrap_or_else(
            |error| panic!("schema rejects provider alias {provider_alias:?}: {error}"),
        );
        schema_acceptances += 1;
        let alias_abi = orna_sys_v1::SystemProviderAbi::from_json(&registry_json)
            .expect("schema-valid provider alias parses into the typed registry");
        for (role_name, _) in &operation_roles {
            assert_eq!(
                alias_abi
                    .role(role_name)
                    .and_then(|role| role.builtin_provider.as_ref())
                    .map(|provider| provider.as_str()),
                Some(provider_alias)
            );
        }
        typed_alias_parses += 1;

        for operation_name in OPTIONAL_OPERATIONS {
            let contract = alias_abi
                .operation(operation_name)
                .expect("invoke/start operation survives provider alias parsing");
            let generated = system_function_descriptor(operation_name)
                .expect("invoke/start operation has a macro-generated binding");
            assert_eq!(generated.signature, contract.signature.source);
            let item_index = markers
                .iter()
                .position(|marker| *marker == operation_name)
                .expect("generated IDL has the aliased operation marker");
            let Declaration::Function { signature, .. } =
                &parsed.value.items[item_index].declaration
            else {
                panic!("generated binding {operation_name} is not a function")
            };
            assert_eq!(
                signature.parameters.len(),
                contract.signature.parameters.len()
            );
            let mut saw_default = false;
            for (idl_parameter, registry_parameter) in signature
                .parameters
                .iter()
                .zip(&contract.signature.parameters)
            {
                let parsed_default = idl_parameter
                    .default
                    .as_ref()
                    .map(|default| &source[default.span().start..default.span().end]);
                assert_eq!(
                    parsed_default,
                    registry_parameter.default.as_deref(),
                    "aliased IDL default matches typed registry for {operation_name}.{}",
                    registry_parameter.name
                );
                let Some(default) = registry_parameter.default.as_deref() else {
                    assert!(!saw_default, "defaults stay trailing in {operation_name}");
                    continue;
                };
                saw_default = true;
                if matches!(&registry_parameter.ty, AbiType::Optional(_)) {
                    assert_eq!(default, "null");
                    optional_defaults += 1;
                }
            }
            binding_cases += 1;
        }
    }

    assert_eq!(schema_acceptances, PROVIDER_ALIASES.len());
    assert_eq!(typed_alias_parses, PROVIDER_ALIASES.len());
    assert_eq!(
        binding_cases,
        PROVIDER_ALIASES.len() * OPTIONAL_OPERATIONS.len()
    );
    assert_eq!(optional_defaults, 32);
    println!(
        "generated_provider_alias_optional_binding_parity aliases={} schema_acceptances={schema_acceptances} typed_parses={typed_alias_parses} operations={} binding_cases={binding_cases} optional_defaults={optional_defaults} total_cases={}",
        PROVIDER_ALIASES.len(),
        OPTIONAL_OPERATIONS.len(),
        schema_acceptances + typed_alias_parses + binding_cases + optional_defaults
    );
}

#[test]
fn generated_object_and_stream_return_bindings_match_schema_registry_and_idl() {
    const OBJECT_RETURN_OPERATIONS: [&str; 2] = ["sys.invoke<T>", "sys.start<T>"];

    let generated_schema = build_provider::generate_provider_registry_schema()
        .expect("provider object-return schema regenerates from its typed source");
    assert_eq!(generated_schema, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(system_provider_abi_json(), &generated_schema)
        .expect("embedded provider registry conforms to its regenerated schema");
    let api_inventory: Value =
        serde_json::from_str(&system_api_json()).expect("embedded system API inventory");
    let stream_alias = api_inventory["reference_aliases"]
        .as_array()
        .expect("system API inventory exposes reference aliases")
        .iter()
        .find(|alias| alias["name"] == "sys.StreamRef")
        .expect("stream return witness is a generated reference alias");
    assert_eq!(stream_alias["target"], "sys.Stream");
    assert_eq!(stream_alias["definition"], "sys.RowRef<sys.Stream>");
    let registry_json: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded provider registry");
    let raw_operations = registry_json["operations"]
        .as_array()
        .expect("embedded provider registry has operation rows");
    let registry = orna_sys_v1::SystemProviderAbi::from_json(system_provider_abi_json())
        .expect("embedded registry parses into the typed provider table");
    let source = system_binding_stubs();
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "generated generic invoke/start bundle parses: {:?}",
        parsed.diagnostics
    );
    let markers = source
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();

    let mut schema_rows = 0;
    let mut typed_contracts = 0;
    let mut generated_bindings = 0;
    let mut idl_results = 0;
    let mut generic_result_witnesses = 0;
    for operation_name in OBJECT_RETURN_OPERATIONS {
        let contract = registry
            .operation(operation_name)
            .expect("generic provider operation exists in the typed registry");
        let row = raw_operations
            .iter()
            .find(|row| row["name"] == operation_name)
            .expect("generated schema input contains the generic provider operation");
        assert_eq!(
            row["signature"].as_str(),
            Some(contract.signature.source.as_str()),
            "schema row carries the typed object-return signature for {operation_name}"
        );
        schema_rows += 1;
        typed_contracts += 1;

        let generated = system_function_descriptor(operation_name)
            .expect("generic object-return operation has a macro-generated descriptor");
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(contract.effects.iter().next(), Some(generated.effect));
        generated_bindings += 1;

        let item_index = markers
            .iter()
            .position(|marker| *marker == operation_name)
            .expect("generated IDL has the generic provider operation marker");
        let Declaration::Function { signature, .. } = &parsed.value.items[item_index].declaration
        else {
            panic!("generated binding {operation_name} is not a function")
        };
        assert_eq!(signature.generics.len(), 1);
        assert_eq!(signature.generics[0].name, "T");
        assert_eq!(contract.signature.type_parameters, vec!["T".to_owned()]);
        let parsed_result = resolve_type(
            signature
                .result
                .as_ref()
                .expect("generic provider IDL declares its result type"),
        )
        .expect("generic provider IDL result type resolves");
        assert_eq!(
            parsed_result, contract.signature.result,
            "IDL retains the typed object-return result shape for {operation_name}"
        );
        idl_results += 1;

        let type_witness_parameter = contract
            .signature
            .parameters
            .iter()
            .find(|parameter| parameter.name == "as")
            .expect("generic object-return operation has an explicit type witness");
        assert_eq!(type_witness_parameter.ty, AbiType::Named("T".to_owned()));
        generic_result_witnesses += 1;
    }

    assert_eq!(schema_rows, OBJECT_RETURN_OPERATIONS.len());
    assert_eq!(typed_contracts, OBJECT_RETURN_OPERATIONS.len());
    assert_eq!(generated_bindings, OBJECT_RETURN_OPERATIONS.len());
    assert_eq!(idl_results, OBJECT_RETURN_OPERATIONS.len());
    assert_eq!(generic_result_witnesses, OBJECT_RETURN_OPERATIONS.len());
    println!(
        "generated_object_and_stream_return_binding_parity operations={} schema_rows={schema_rows} typed_contracts={typed_contracts} macro_bindings={generated_bindings} idl_results={idl_results} result_witnesses={generic_result_witnesses} stream_alias=1 total_cases={}",
        OBJECT_RETURN_OPERATIONS.len(),
        schema_rows + typed_contracts + generated_bindings + idl_results + generic_result_witnesses
    );
}

#[test]
fn generated_provider_callback_binding_matches_schema_and_idl() {
    const OPERATION_NAME: &str = "sys.invoke(Value)";

    let generated_schema = build_provider::generate_provider_registry_schema()
        .expect("provider callback schema regenerates from its typed source");
    assert_eq!(generated_schema, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(system_provider_abi_json(), &generated_schema)
        .expect("embedded callback operation conforms to its regenerated schema");
    let registry_json: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded provider registry");
    let raw_operations = registry_json["operations"]
        .as_array()
        .expect("embedded provider registry has operation rows");
    let registry = orna_sys_v1::SystemProviderAbi::from_json(system_provider_abi_json())
        .expect("embedded provider registry parses into typed contracts");
    let contract = registry
        .operation(OPERATION_NAME)
        .expect("invoke callback operation exists in typed registry");
    assert!(contract.role.is_some());
    let row = raw_operations
        .iter()
        .find(|row| row["name"] == OPERATION_NAME)
        .expect("schema registry contains the invoke operation row");
    assert_eq!(
        row["signature"].as_str(),
        Some(contract.signature.source.as_str())
    );
    let generated = system_function_descriptor(OPERATION_NAME)
        .expect("invoke operation has a macro-generated binding");
    assert_eq!(generated.signature, contract.signature.source);
    assert_eq!(contract.effects.iter().next(), Some(generated.effect));

    let source = system_binding_stubs();
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "generated invoke binding bundle parses: {:?}",
        parsed.diagnostics
    );
    let markers = source
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    let item_index = markers
        .iter()
        .position(|marker| *marker == OPERATION_NAME)
        .expect("generated IDL has the invoke operation marker");
    let Declaration::Function { signature, .. } = &parsed.value.items[item_index].declaration
    else {
        panic!("generated callback binding is not a function")
    };
    assert_eq!(
        signature.parameters.len(),
        contract.signature.parameters.len()
    );
    for (idl_parameter, registry_parameter) in signature
        .parameters
        .iter()
        .zip(&contract.signature.parameters)
    {
        let parsed_type = resolve_type(
            idl_parameter
                .annotation
                .as_ref()
                .expect("generated invoke IDL parameter has a type"),
        )
        .expect("generated invoke IDL parameter type resolves");
        assert_eq!(parsed_type, registry_parameter.ty);
        let parsed_default = idl_parameter
            .default
            .as_ref()
            .map(|default| &source[default.span().start..default.span().end]);
        assert_eq!(parsed_default, registry_parameter.default.as_deref());
    }
    assert_eq!(
        resolve_type(
            signature
                .result
                .as_ref()
                .expect("generated invoke IDL declares its result")
        )
        .expect("generated invoke IDL result resolves"),
        contract.signature.result
    );
    println!(
        "generated_provider_callback_binding_parity schema_validated=1 typed_contracts=1 macro_bindings=1 idl_parameters={} idl_results=1 provider_role=1 total_cases={}",
        signature.parameters.len(),
        5 + signature.parameters.len()
    );
}

#[test]
fn generated_provider_promise_callback_binding_matches_schema_and_idl() {
    const OPERATIONS: [&str; 2] = ["sys.await", "sys.start<T>"];

    let generated_schema = build_provider::generate_provider_registry_schema()
        .expect("provider promise schema regenerates from its typed source");
    assert_eq!(generated_schema, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(system_provider_abi_json(), &generated_schema)
        .expect("embedded promise operations conform to the regenerated provider schema");

    let registry_json: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded provider registry");
    let raw_operations = registry_json["operations"]
        .as_array()
        .expect("generated provider registry has operation rows");
    let registry = orna_sys_v1::SystemProviderAbi::from_json(system_provider_abi_json())
        .expect("schema-validated promise registry parses into typed contracts");
    let fixture = PROMISE_CALLBACK_STUB_FIXTURE.trim_end();
    let parsed = parse_module(fixture);
    assert!(
        parsed.is_ok(),
        "generated promise callback fixture parses: {:?}",
        parsed.diagnostics
    );
    assert_eq!(parsed.value.items.len(), 3);
    let markers = fixture
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    assert_eq!(markers, OPERATIONS);
    let generated_source = system_binding_stubs();
    let generated_parsed = parse_module(generated_source);
    assert!(
        generated_parsed.is_ok(),
        "complete generated promise binding bundle parses: {:?}",
        generated_parsed.diagnostics
    );
    let generated_markers = generated_source
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();

    let mut schema_rows = 0;
    let mut typed_contracts = 0;
    let mut macro_bindings = 0;
    let mut idl_operations = 0;
    for (index, operation_name) in OPERATIONS.iter().enumerate() {
        let contract = registry
            .operation(operation_name)
            .expect("promise callback operation exists in the typed provider registry");
        let row = raw_operations
            .iter()
            .find(|row| row["name"] == *operation_name)
            .expect("generated schema input contains the promise callback operation");
        assert_eq!(
            row["signature"].as_str(),
            Some(contract.signature.source.as_str()),
            "schema row and typed promise contract agree for {operation_name}"
        );
        schema_rows += 1;
        typed_contracts += 1;

        let generated = system_function_descriptor(operation_name)
            .expect("promise operation has a macro-generated binding descriptor");
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(contract.effects.iter().next(), Some(generated.effect));
        assert!(contract.role.is_some());
        macro_bindings += 1;

        let Declaration::Function { signature, .. } = &parsed.value.items[index].declaration else {
            panic!("generated promise binding {operation_name} is not a function")
        };
        let generated_index = generated_markers
            .iter()
            .position(|marker| *marker == *operation_name)
            .expect("generated binding bundle has the promise operation marker");
        let Declaration::Function {
            signature: generated_signature,
            ..
        } = &generated_parsed.value.items[generated_index].declaration
        else {
            panic!("generated bundle entry {operation_name} is not a function")
        };
        assert_eq!(
            signature
                .generics
                .iter()
                .map(|generic| generic.name.as_str())
                .collect::<Vec<_>>(),
            contract
                .signature
                .type_parameters
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            generated_signature
                .generics
                .iter()
                .map(|generic| generic.name.as_str())
                .collect::<Vec<_>>(),
            contract
                .signature
                .type_parameters
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            signature.parameters.len(),
            contract.signature.parameters.len()
        );
        assert_eq!(
            generated_signature.parameters.len(),
            contract.signature.parameters.len()
        );
        for (idl_parameter, registry_parameter) in signature
            .parameters
            .iter()
            .zip(&contract.signature.parameters)
        {
            assert_eq!(
                resolve_type(
                    idl_parameter
                        .annotation
                        .as_ref()
                        .expect("generated promise IDL parameter is typed")
                )
                .expect("generated promise IDL parameter type resolves"),
                registry_parameter.ty
            );
        }
        for (idl_parameter, registry_parameter) in generated_signature
            .parameters
            .iter()
            .zip(&contract.signature.parameters)
        {
            assert_eq!(
                resolve_type(
                    idl_parameter
                        .annotation
                        .as_ref()
                        .expect("generated binding parameter is typed")
                )
                .expect("generated binding parameter type resolves"),
                registry_parameter.ty
            );
        }
        assert_eq!(
            resolve_type(
                signature
                    .result
                    .as_ref()
                    .expect("generated promise IDL result is typed")
            )
            .expect("generated promise IDL result type resolves"),
            contract.signature.result
        );
        assert_eq!(
            resolve_type(
                generated_signature
                    .result
                    .as_ref()
                    .expect("generated binding result is typed")
            )
            .expect("generated binding result type resolves"),
            contract.signature.result
        );
        idl_operations += 1;
    }

    let await_contract = registry
        .operation("sys.await")
        .expect("await callback contract exists");
    let start_contract = registry
        .operation("sys.start<T>")
        .expect("promise producer contract exists");
    assert_eq!(
        start_contract.signature.result, await_contract.signature.parameters[0].ty,
        "start handle flows directly into await's invocation parameter"
    );
    assert_eq!(
        start_contract.signature.result,
        AbiType::Applied {
            constructor: "sys.InvocationHandle".to_owned(),
            arguments: vec![AbiType::Named("T".to_owned())],
        }
    );
    assert_eq!(
        await_contract.signature.result,
        AbiType::Applied {
            constructor: "sys.InvocationResult".to_owned(),
            arguments: vec![AbiType::Named("T".to_owned())],
        }
    );

    let Declaration::Function {
        signature: callback_signature,
        ..
    } = &parsed.value.items[2].declaration
    else {
        panic!("promise completion callback fixture is not a function")
    };
    assert_eq!(callback_signature.name, "promise_callback");
    assert_eq!(callback_signature.generics[0].name, "T");
    assert_eq!(
        resolve_type(
            callback_signature.parameters[0]
                .annotation
                .as_ref()
                .expect("completion callback accepts an invocation handle")
        )
        .expect("completion callback input type resolves"),
        start_contract.signature.result
    );
    assert_eq!(
        resolve_type(
            callback_signature
                .result
                .as_ref()
                .expect("completion callback returns the await result")
        )
        .expect("completion callback result type resolves"),
        await_contract.signature.result
    );
    assert!(fixture.ends_with("= sys.await(invocation);"));

    println!(
        "generated_provider_promise_callback_binding_parity operations={} schema_validated=1 schema_rows={schema_rows} typed_contracts={typed_contracts} macro_bindings={macro_bindings} idl_operations={idl_operations} handle_result_pairs=1 completion_callback=1 total_cases={}",
        OPERATIONS.len(),
        1 + schema_rows + typed_contracts + macro_bindings + idl_operations + 3
    );
}

#[test]
fn generated_provider_cancel_binding_matches_schema_and_idl() {
    const OPERATION: &str = "sys.cancel";

    let fixture: Value = serde_json::from_str(PROVIDER_CANCEL_EDGE_FIXTURE)
        .expect("crate-local provider cancellation fixture is valid JSON");
    assert_eq!(fixture["operation"], OPERATION);
    let generated_schema = build_provider::generate_provider_registry_schema()
        .expect("provider cancellation schema regenerates from its typed source");
    assert_eq!(generated_schema, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(system_provider_abi_json(), &generated_schema)
        .expect("embedded cancellation operation conforms to the regenerated provider schema");

    let registry = orna_sys_v1::SystemProviderAbi::from_json(system_provider_abi_json())
        .expect("schema-validated cancellation registry parses into typed contracts");
    let contract = registry
        .operation(OPERATION)
        .expect("generic cancellation operation exists in the typed provider registry");
    let descriptor = system_function_descriptor(OPERATION)
        .expect("cancellation operation has a macro-generated descriptor");
    assert_eq!(descriptor.signature, contract.signature.source);
    assert_eq!(contract.effects.iter().next(), Some(descriptor.effect));
    assert!(contract.role.is_some());
    assert_eq!(
        contract
            .signature
            .parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect::<Vec<_>>(),
        ["invocation", "reason"]
    );
    assert_eq!(
        contract.signature.parameters[0].ty.canonical(),
        "sys.InvocationHandle<T>"
    );
    assert_eq!(
        contract.signature.parameters[1].ty.canonical(),
        fixture["reason_type"]
            .as_str()
            .expect("fixture reason type")
    );
    assert_eq!(
        contract.signature.parameters[1].default.as_deref(),
        Some("null")
    );
    assert_eq!(
        contract.signature.result.canonical(),
        fixture["result_type"]
            .as_str()
            .expect("fixture result type")
    );

    let source = system_binding_stubs();
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "generated cancellation binding bundle parses: {:?}",
        parsed.diagnostics
    );
    let marker = format!("// sys-op: {OPERATION}");
    let declaration = source
        .split("\n\n")
        .find(|declaration| declaration.lines().any(|line| line == marker))
        .expect("generated binding bundle contains the cancellation operation");
    validate_stub_contract(declaration, contract)
        .unwrap_or_else(|error| panic!("cancellation binding parity: {error}"));
    let cases = fixture["cases"]
        .as_array()
        .expect("cancellation fixture has edge cases");
    assert_eq!(cases.len(), 5);
    assert!(cases.iter().all(|case| {
        case["accepted"].as_bool() == Some(case["reason_type"].as_str() == Some("Str?"))
    }));
    println!(
        "generated_provider_cancel_binding_parity operation={OPERATION} cases={} schema_validated=1 typed_contract=1 macro_binding=1 generated_stub=1 handle_type=sys.InvocationHandle<T> reason_default=null result_type=Bool total_cases={}",
        cases.len(),
        cases.len() + contract.signature.parameters.len() + 6
    );
}

#[test]
fn generated_provider_metadata_edges_match_schema_and_bindings() {
    let fixture: Value = serde_json::from_str(PROVIDER_METADATA_EDGE_FIXTURE)
        .expect("crate-local provider metadata fixture is valid JSON");
    let operation_name = fixture["operation"]
        .as_str()
        .expect("metadata fixture identifies its generic operation");
    let api_function = fixture["api_function"]
        .as_str()
        .expect("metadata fixture identifies its public API function");

    let generated_provider_schema = build_provider::generate_provider_registry_schema()
        .expect("provider metadata schema regenerates from its typed source");
    assert_eq!(generated_provider_schema, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(
        system_provider_abi_json(),
        &generated_provider_schema,
    )
    .expect("generated metadata provider contract validates against its schema");
    build_host::validate_json_against_schema(&system_api_json(), system_api_schema_json())
        .expect("generated metadata API inventory validates against its schema");

    let registry = system_provider_abi();
    let operation = registry
        .operation(operation_name)
        .expect("sys.meta generic operation exists in the generated provider registry");
    assert_eq!(operation.signature.source, fixture["signature"]);
    let role = operation
        .role
        .as_ref()
        .expect("sys.meta has a provider role");
    assert_eq!(role.as_str(), fixture["role"].as_str().unwrap());

    let descriptor = system_function_descriptor(api_function)
        .expect("sys.meta has a macro-generated public API descriptor");
    assert_eq!(descriptor.signature, fixture["signature"]);

    let api: Value = serde_json::from_str(&system_api_json())
        .expect("generated system API inventory is valid JSON");
    let generated_function = api["functions"]
        .as_array()
        .expect("generated system API exposes functions")
        .iter()
        .find(|function| function["name"] == api_function)
        .expect("generated system API includes sys.meta");
    assert_eq!(generated_function["signature"], fixture["signature"]);
    let metadata_type = api["value_types"]
        .as_array()
        .expect("generated system API exposes value type schemas")
        .iter()
        .find(|value_type| value_type["name"] == fixture["metadata_type"])
        .expect("generated system API includes sys.ValueMetadata<T>");
    assert_eq!(metadata_type["fields"], fixture["fields"]);

    let cases = fixture["cases"]
        .as_array()
        .expect("metadata fixture has value edge cases");
    let generated_stub_bundle = system_binding_stubs();
    let generated_parse = parse_module(generated_stub_bundle);
    assert!(
        generated_parse.is_ok(),
        "generated metadata binding bundle parses: {:?}",
        generated_parse.diagnostics
    );
    let marker = format!("// sys-op: {operation_name}");
    let declaration = generated_stub_bundle
        .split("\n\n")
        .find(|declaration| declaration.lines().any(|line| line == marker))
        .expect("generated binding bundle contains the generic sys.meta operation");
    validate_stub_contract(declaration, operation)
        .unwrap_or_else(|error| panic!("sys.meta generated binding parity: {error}"));

    for case in cases {
        let value_type = case["value_type"]
            .as_str()
            .expect("metadata edge names its input type");
        assert_eq!(
            case["result_type"],
            format!("sys.ValueMetadata<{value_type}>")
        );
        assert_eq!(case["metadata"]["static_type"], value_type);
        assert_eq!(case["metadata"]["redacted"], case["redacted"]);
    }

    println!(
        "generated_provider_metadata_binding_parity operation={operation_name} cases={} schema_validated=2 api_schema_validated=1 metadata_fields={} typed_contract=1 macro_binding=1 generated_stub=1 total_cases={}",
        cases.len(),
        fixture["fields"].as_array().unwrap().len(),
        cases.len() + fixture["fields"].as_array().unwrap().len() + 7
    );
}

#[test]
fn generated_provider_stream_iterator_edge_binding_matches_schema_and_idl() {
    const OPERATIONS: [&str; 2] = ["sys.await", "sys.start<T>"];

    let generated_provider_schema = build_provider::generate_provider_registry_schema()
        .expect("provider iterator-edge schema regenerates from its typed source");
    assert_eq!(generated_provider_schema, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(
        system_provider_abi_json(),
        &generated_provider_schema,
    )
    .expect("embedded provider iterator-edge contracts validate against their schema");
    build_host::validate_json_against_schema(&system_api_json(), system_api_schema_json())
        .expect("embedded stream reference metadata validates against the system API schema");

    let api: Value = serde_json::from_str(&system_api_json())
        .expect("embedded system API inventory is valid JSON");
    let stream_alias = api["reference_aliases"]
        .as_array()
        .expect("system API inventory exposes reference aliases")
        .iter()
        .find(|alias| alias["name"] == "sys.StreamRef")
        .expect("iterator-edge witness is a generated stream reference alias");
    assert_eq!(stream_alias["target"], "sys.Stream");
    assert_eq!(stream_alias["definition"], "sys.RowRef<sys.Stream>");

    let fixture = STREAM_ITERATOR_EDGE_STUB_FIXTURE.trim_end();
    let parsed = parse_module(fixture);
    assert!(
        parsed.is_ok(),
        "stream iterator-edge fixture parses: {:?}",
        parsed.diagnostics
    );
    assert_eq!(parsed.value.items.len(), 3);
    let markers = fixture
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    assert_eq!(markers, OPERATIONS);

    let generated_source = system_binding_stubs();
    let generated_parsed = parse_module(generated_source);
    assert!(
        generated_parsed.is_ok(),
        "generated iterator-edge binding bundle parses: {:?}",
        generated_parsed.diagnostics
    );
    let generated_markers = generated_source
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    let registry = system_provider_abi();
    for (index, operation_name) in OPERATIONS.iter().enumerate() {
        let contract = registry
            .operation(operation_name)
            .expect("stream iterator-edge operation exists in the typed provider registry");
        let declaration = fixture
            .split("\n\n")
            .nth(index)
            .expect("fixture includes both generated provider declarations");
        validate_stub_contract(declaration, contract)
            .unwrap_or_else(|error| panic!("{operation_name} fixture parity: {error}"));
        assert!(
            generated_source.contains(declaration),
            "generated binding bundle retains the {operation_name} iterator-edge declaration"
        );
        let generated_index = generated_markers
            .iter()
            .position(|marker| *marker == *operation_name)
            .expect("generated binding bundle marks each iterator-edge operation");
        let Declaration::Function {
            signature: generated_signature,
            ..
        } = &generated_parsed.value.items[generated_index].declaration
        else {
            panic!("generated binding {operation_name} is not a function")
        };
        let Declaration::Function { signature, .. } = &parsed.value.items[index].declaration else {
            panic!("fixture binding {operation_name} is not a function")
        };
        assert_eq!(
            signature
                .generics
                .iter()
                .map(|generic| generic.name.as_str())
                .collect::<Vec<_>>(),
            generated_signature
                .generics
                .iter()
                .map(|generic| generic.name.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            signature.parameters.len(),
            contract.signature.parameters.len()
        );
        assert_eq!(
            resolve_type(signature.result.as_ref().expect("typed fixture result"))
                .expect("fixture result type resolves"),
            contract.signature.result
        );
    }

    let Declaration::Function {
        signature: callback,
        ..
    } = &parsed.value.items[2].declaration
    else {
        panic!("stream iterator callback fixture is not a function")
    };
    assert_eq!(callback.name, "stream_iterator_callback");
    let stream_ref = AbiType::Named("sys.StreamRef".to_owned());
    assert_eq!(
        resolve_type(
            callback.parameters[0]
                .annotation
                .as_ref()
                .expect("callback accepts an iterator-edge handle")
        )
        .expect("callback handle type resolves"),
        AbiType::Applied {
            constructor: "sys.InvocationHandle".to_owned(),
            arguments: vec![stream_ref.clone()],
        }
    );
    assert_eq!(
        resolve_type(callback.result.as_ref().expect("typed callback result"))
            .expect("callback result type resolves"),
        AbiType::Applied {
            constructor: "sys.InvocationResult".to_owned(),
            arguments: vec![stream_ref],
        }
    );
    assert!(fixture.ends_with("= sys.await(invocation);"));

    println!(
        "generated_provider_stream_iterator_edge_binding_parity operations={} schema_validated=1 api_schema_validated=1 stream_alias=1 typed_contracts={} macro_bindings={} iterator_callback=1 total_cases={}",
        OPERATIONS.len(),
        OPERATIONS.len(),
        OPERATIONS.len(),
        2 + OPERATIONS.len() * 3 + 3
    );
}

#[test]
fn generated_provider_variadic_error_bindings_match_schema_and_idl() {
    const VARIADIC_ERROR_OPERATIONS: [&str; 4] = [
        "sys.invoke(Value)",
        "sys.invoke<T>",
        "sys.start(Value)",
        "sys.start<T>",
    ];
    const SHARED_PROVIDER_FAILURES: [&str; 3] = [
        "sys.abi.precondition_failed",
        "sys.abi.unavailable",
        "sys.abi.provider_failed",
    ];

    let generated_schema = build_provider::generate_provider_registry_schema()
        .expect("provider variadic error schema regenerates from its typed source");
    assert_eq!(generated_schema, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(system_provider_abi_json(), &generated_schema)
        .expect("embedded provider error contracts conform to their regenerated schema");
    let registry_json: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded provider registry");
    let raw_operations = registry_json["operations"]
        .as_array()
        .expect("embedded provider registry has operation rows");
    let registry = orna_sys_v1::SystemProviderAbi::from_json(system_provider_abi_json())
        .expect("embedded provider registry parses into typed contracts");
    let source = system_binding_stubs();
    let parsed = parse_module(source);
    assert!(
        parsed.is_ok(),
        "generated invoke/start bundle parses: {:?}",
        parsed.diagnostics
    );
    let markers = source
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();

    let mut schema_rows = 0;
    let mut typed_contracts = 0;
    let mut generated_bindings = 0;
    let mut map_slots = 0;
    let mut declared_failure_codes = 0;
    let mut idl_parameters = 0;
    let mut idl_results = 0;
    for operation_name in VARIADIC_ERROR_OPERATIONS {
        let contract = registry
            .operation(operation_name)
            .expect("variadic provider operation exists in typed registry");
        let row = raw_operations
            .iter()
            .find(|row| row["name"] == operation_name)
            .expect("generated schema input contains each variadic operation");
        assert_eq!(
            row["signature"].as_str(),
            Some(contract.signature.source.as_str()),
            "schema row carries typed signature for {operation_name}"
        );
        let schema_failure_codes = row["failures"]
            .as_array()
            .expect("schema row publishes its operation failure vocabulary")
            .iter()
            .map(|code| code.as_str().expect("failure code is a string").to_owned())
            .collect::<BTreeSet<_>>();
        let typed_operation_failures = contract
            .failures
            .iter()
            .filter(|code| !SHARED_PROVIDER_FAILURES.contains(&code.as_str()))
            .map(|code| code.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(schema_failure_codes, typed_operation_failures);
        assert!(contract.role.is_some());
        schema_rows += 1;
        typed_contracts += 1;

        let generated = system_function_descriptor(operation_name)
            .expect("variadic provider operation has a macro-generated binding");
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(contract.effects.iter().next(), Some(generated.effect));
        generated_bindings += 1;

        let map_indexes = contract
            .signature
            .parameters
            .iter()
            .enumerate()
            .filter_map(|(index, parameter)| (parameter.name == "arguments").then_some(index))
            .collect::<Vec<_>>();
        assert_eq!(map_indexes, [1], "one outer map slot in {operation_name}");
        assert_eq!(
            contract.signature.parameters[map_indexes[0]].ty,
            AbiType::Named("sys.ArgumentMap".to_owned())
        );
        map_slots += 1;
        declared_failure_codes += schema_failure_codes.len();

        let item_index = markers
            .iter()
            .position(|marker| *marker == operation_name)
            .expect("generated IDL contains each variadic operation marker");
        let Declaration::Function { signature, .. } = &parsed.value.items[item_index].declaration
        else {
            panic!("generated variadic operation {operation_name} is not a function")
        };
        assert_eq!(
            signature.parameters.len(),
            contract.signature.parameters.len()
        );
        for (idl_parameter, registry_parameter) in signature
            .parameters
            .iter()
            .zip(&contract.signature.parameters)
        {
            let parsed_type = resolve_type(
                idl_parameter
                    .annotation
                    .as_ref()
                    .expect("generated variadic IDL parameter has a type"),
            )
            .expect("generated variadic IDL parameter type resolves");
            assert_eq!(parsed_type, registry_parameter.ty);
            let parsed_default = idl_parameter
                .default
                .as_ref()
                .map(|default| &source[default.span().start..default.span().end]);
            assert_eq!(parsed_default, registry_parameter.default.as_deref());
        }
        assert_eq!(
            resolve_type(
                signature
                    .result
                    .as_ref()
                    .expect("generated variadic IDL declares its result")
            )
            .expect("generated variadic IDL result resolves"),
            contract.signature.result
        );
        idl_parameters += signature.parameters.len();
        idl_results += 1;
    }

    assert_eq!(schema_rows, VARIADIC_ERROR_OPERATIONS.len());
    assert_eq!(typed_contracts, VARIADIC_ERROR_OPERATIONS.len());
    assert_eq!(generated_bindings, VARIADIC_ERROR_OPERATIONS.len());
    assert_eq!(map_slots, VARIADIC_ERROR_OPERATIONS.len());
    assert_eq!(idl_results, VARIADIC_ERROR_OPERATIONS.len());
    assert!(declared_failure_codes > 0);
    println!(
        "generated_provider_variadic_error_binding_parity operations={generated_bindings} schema_rows={schema_rows} typed_contracts={typed_contracts} map_slots={map_slots} operation_failure_codes={declared_failure_codes} idl_parameters={idl_parameters} idl_results={idl_results} total_cases={}",
        schema_rows
            + typed_contracts
            + generated_bindings
            + map_slots
            + declared_failure_codes
            + idl_parameters
            + idl_results
    );
}

#[test]
fn generated_idl_modules_parse_and_validate_registry_contracts_independently() {
    let abi = system_provider_abi();
    let modules: BTreeMap<String, String> = serde_json::from_str(system_binding_modules_json())
        .expect("generated binding-module manifest maps paths to source");
    let module_root = Path::new(env!("OUT_DIR")).join("system_bindings");
    assert!(!modules.is_empty(), "generated IDL has module files");
    let mut validated_modules = 0;
    let mut validated_operations = 0;

    for (relative_path, manifest_source) in &modules {
        let module = relative_path
            .strip_suffix(".orna")
            .expect("generated module paths use the Orna extension")
            .replace('/', ".");
        let file_source = fs::read_to_string(module_root.join(relative_path))
            .unwrap_or_else(|error| panic!("read generated IDL module {relative_path}: {error}"));
        assert_eq!(
            &file_source, manifest_source,
            "generated module {relative_path} matches its embedded manifest source"
        );
        let parsed = parse_module(&file_source);
        assert!(
            parsed.is_ok(),
            "generated IDL module {relative_path} parses independently: {:?}",
            parsed.diagnostics
        );

        let expected_operations = abi
            .operations()
            .filter(|operation| {
                operation
                    .signature
                    .callable
                    .rsplit_once('.')
                    .is_some_and(|(parent, _)| parent == module)
            })
            .collect::<Vec<_>>();
        let markers = file_source
            .lines()
            .filter_map(|line| line.strip_prefix("// sys-op: "))
            .collect::<Vec<_>>();
        assert_eq!(
            markers,
            expected_operations
                .iter()
                .map(|operation| operation.id.as_str())
                .collect::<Vec<_>>(),
            "module {relative_path} owns exactly its registry operations in order"
        );
        assert_eq!(parsed.value.items.len(), expected_operations.len());

        for (block, operation) in file_source
            .split("\n\n")
            .filter(|block| block.lines().any(|line| line.starts_with("// sys-op: ")))
            .zip(expected_operations)
        {
            let contract_source = format!("// sys-module: {module}\n{block}\n");
            validate_stub_contract(&contract_source, operation).unwrap_or_else(|error| {
                panic!(
                    "generated IDL stub for {} in {relative_path} violates its registry contract: {error}",
                    operation.id.as_str()
                )
            });
            validated_operations += 1;
        }
        validated_modules += 1;
    }

    assert_eq!(
        validated_operations,
        abi.operations().count(),
        "standalone module validation covers every typed registry operation"
    );
    println!(
        "generated_idl_module_validation modules={validated_modules} operations={validated_operations} standalone_parse=true manifest_bytes_match=true typed_contracts=true"
    );
}

#[test]
fn generated_stub_parity_guard_rejects_missing_duplicate_and_unknown_dispatch_rows() {
    let source = system_binding_stubs();
    let expected = system_provider_abi()
        .operations()
        .map(|operation| operation.id.as_str())
        .collect::<Vec<_>>();
    validate_stub_dispatch_inventory(source, &expected).unwrap();

    let first_marker = source
        .lines()
        .find(|line| line.starts_with("// sys-op: "))
        .expect("generated stubs have dispatch markers");
    let mut rejected_drift_cases = 0;
    let missing = source.replacen(&format!("{first_marker}\n"), "", 1);
    assert!(
        validate_stub_dispatch_inventory(&missing, &expected).is_err(),
        "omitting a generated dispatch row fails the parity guard"
    );
    rejected_drift_cases += 1;

    let duplicate = format!("{source}\n{first_marker}\n");
    assert!(
        validate_stub_dispatch_inventory(&duplicate, &expected).is_err(),
        "duplicating a generated dispatch row fails the parity guard"
    );
    rejected_drift_cases += 1;

    let unknown = source.replacen(first_marker, "// sys-op: sys.vendor.unknown", 1);
    assert!(
        validate_stub_dispatch_inventory(&unknown, &expected).is_err(),
        "redirecting a generated stub to an unknown registry operation fails the parity guard"
    );
    rejected_drift_cases += 1;
    assert_eq!(rejected_drift_cases, 3);
    println!(
        "generated_stub_dispatch_parity operations={} rejected_drift_cases={} categories=missing,duplicate,unknown total_cases={}",
        expected.len(),
        rejected_drift_cases,
        expected.len() + rejected_drift_cases
    );
}

#[test]
fn generated_stub_contract_validation_rejects_parseable_and_syntactic_drift() {
    let fixture = GENERIC_INVOKE_KEYWORD_OVERLOAD_FIXTURE.trim_end();
    let stub = fixture
        .split("\n\n")
        .find(|block| block.lines().any(|line| line == "// sys-op: sys.invoke<T>"))
        .expect("in-crate fixture includes the generic invoke stub");
    let operation = system_provider_abi()
        .operation("sys.invoke<T>")
        .expect("generic invoke dispatch contract");
    assert_eq!(
        validate_stub_contract(stub, operation),
        Ok(()),
        "fixture stub conforms to the generated registry contract"
    );

    fn replace_once(source: &str, from: &str, to: &str) -> String {
        let replaced = source.replacen(from, to, 1);
        assert_ne!(
            replaced, source,
            "mutation target `{from}` exists in fixture"
        );
        replaced
    }

    let parseable_mutations = [
        (
            "module",
            replace_once(stub, "// sys-module: sys", "// sys-module: std"),
        ),
        (
            "dispatch marker",
            replace_once(
                stub,
                "// sys-op: sys.invoke<T>",
                "// sys-op: sys.invoke(Value)",
            ),
        ),
        (
            "function name",
            replace_once(stub, "pub fn invoke<T>", "pub fn call<T>"),
        ),
        (
            "generic parameter",
            replace_once(stub, "pub fn invoke<T>", "pub fn invoke<U>"),
        ),
        (
            "keyword parameter alias",
            replace_once(stub, "as_: T", "target: T"),
        ),
        (
            "parameter type",
            replace_once(stub, "as_: T", "as_: sys.Ghost"),
        ),
        (
            "default value",
            replace_once(
                stub,
                "transaction: sys.InvokeTransaction = sys.InvokeTransaction.inherit",
                "transaction: sys.InvokeTransaction = sys.InvokeTransaction.separate",
            ),
        ),
        (
            "result type",
            replace_once(stub, "): T =", "): sys.Value ="),
        ),
        (
            "stub body",
            replace_once(
                stub,
                "error(code: \"sys.binding.stub\", message: \"generated declaration stub\")",
                "error(code: \"sys.abi.unavailable\", message: \"generated declaration stub\")",
            ),
        ),
        (
            "parameter inventory",
            replace_once(stub, ", idempotency_key: Str? = null", ""),
        ),
    ];
    for (field, mutated) in parseable_mutations {
        let parsed = parse_module(&mutated);
        assert!(
            parsed.is_ok(),
            "{field} drift remains syntactically valid and must reach contract validation: {:?}",
            parsed.diagnostics
        );
        assert!(
            validate_stub_contract(&mutated, operation).is_err(),
            "registry contract validation rejects {field} drift"
        );
    }

    let malformed = replace_once(stub, "as_: T", "as_: sys.Value<");
    assert!(
        !parse_module(&malformed).is_ok(),
        "the Orna parser rejects malformed generated type syntax"
    );
    assert!(
        validate_stub_contract(&malformed, operation).is_err(),
        "stub contract validation rejects syntactic drift before dispatch parity"
    );
}

#[test]
fn generated_idl_placeholder_error_drift_is_rejected_for_every_operation() {
    const CANONICAL_BODY: &str =
        "error(code: \"sys.binding.stub\", message: \"generated declaration stub\")";
    const DRIFTED_BODY: &str =
        "error(code: \"sys.abi.unavailable\", message: \"generated declaration stub\")";

    let abi = system_provider_abi();
    let source = system_binding_stubs();
    let mut validated_stubs = 0;
    let mut rejected_body_drift = 0;

    for stub in source
        .split("\n\n")
        .filter(|block| block.lines().any(|line| line.starts_with("// sys-op: ")))
    {
        let operation_id = stub
            .lines()
            .find_map(|line| line.strip_prefix("// sys-op: "))
            .expect("each generated IDL stub has an operation marker");
        let operation = abi
            .operation(operation_id)
            .unwrap_or_else(|| panic!("generated IDL stub names unknown operation {operation_id}"));
        validate_stub_contract(stub, operation).unwrap_or_else(|error| {
            panic!("canonical IDL stub {operation_id} violates its contract: {error}")
        });
        validated_stubs += 1;

        let drifted = stub.replacen(CANONICAL_BODY, DRIFTED_BODY, 1);
        assert_ne!(
            drifted, stub,
            "{operation_id} has the canonical placeholder body"
        );
        assert!(
            parse_module(&drifted).is_ok(),
            "placeholder error drift remains parseable for {operation_id}"
        );
        let error = validate_stub_contract(&drifted, operation)
            .expect_err("generated IDL contract rejects placeholder diagnostic drift");
        assert_eq!(
            error, "stub body differs from the canonical generated declaration error",
            "contract identifies the placeholder error drift for {operation_id}"
        );
        rejected_body_drift += 1;
    }

    assert_eq!(validated_stubs, abi.operations().count());
    assert_eq!(rejected_body_drift, validated_stubs);
    println!(
        "generated_idl_placeholder_error_parity operations={validated_stubs} canonical_contracts={validated_stubs} rejected_body_drift={rejected_body_drift} diagnostic=sys.binding.stub total_cases={}",
        validated_stubs + rejected_body_drift
    );
}

#[test]
fn generated_generic_stub_family_matches_registry_types_and_dispatch() {
    let fixture = GENERIC_OPERATION_STUB_FIXTURE.trim_end();
    let bundle = system_binding_stubs();
    let parsed = parse_module(fixture);
    assert!(
        parsed.is_ok(),
        "the generic operation fixture must parse in supported Orna grammar: {:?}",
        parsed.diagnostics
    );

    let generic_operations = system_provider_abi()
        .operations()
        .filter(|operation| !operation.signature.type_parameters.is_empty())
        .collect::<Vec<_>>();
    let markers = fixture
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    assert_eq!(markers.len(), generic_operations.len());
    assert_eq!(parsed.value.items.len(), generic_operations.len());
    assert_eq!(
        markers,
        generic_operations
            .iter()
            .map(|operation| operation.id.as_str())
            .collect::<Vec<_>>(),
        "fixture dispatch markers cover every generic operation in registry order"
    );

    for block in fixture.split("\n\n") {
        assert!(
            bundle.contains(block.trim_end()),
            "generic stub block must be emitted from the registry: {block}"
        );
    }
    for (item, operation) in parsed.value.items.iter().zip(generic_operations) {
        let Declaration::Function { signature, .. } = &item.declaration else {
            panic!("each generic stub must be a function declaration")
        };
        assert_eq!(signature.name, local_function_name(operation));
        assert_eq!(
            signature
                .generics
                .iter()
                .map(|generic| generic.name.as_str())
                .collect::<Vec<_>>(),
            operation
                .signature
                .type_parameters
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            signature.parameters.len(),
            operation.signature.parameters.len()
        );
        for (parsed_parameter, registered_parameter) in signature
            .parameters
            .iter()
            .zip(&operation.signature.parameters)
        {
            let parameter_source = &fixture[parsed_parameter.span.start..parsed_parameter.span.end];
            let (parameter_name, _) = parameter_source
                .split_once(": ")
                .expect("generic stub parameter has an explicit type");
            let expected_name = if registered_parameter.name == "as" {
                "as_"
            } else {
                registered_parameter.name.as_str()
            };
            assert_eq!(parameter_name, expected_name);
            assert_eq!(
                resolve_type(
                    parsed_parameter
                        .annotation
                        .as_ref()
                        .expect("typed generic parameter")
                )
                .expect("supported generic parameter type"),
                registered_parameter.ty
            );
            let parsed_default = parsed_parameter
                .default
                .as_ref()
                .map(|default| &fixture[default.span().start..default.span().end]);
            assert_eq!(parsed_default, registered_parameter.default.as_deref());
        }
        assert_eq!(
            resolve_type(signature.result.as_ref().expect("typed generic result"))
                .expect("supported generic result type"),
            operation.signature.result
        );
    }
}

#[test]
fn generated_generic_keyword_stub_tail_matches_the_in_crate_fixture() {
    let fixture = GENERIC_KEYWORD_STUB_FIXTURE.trim_end();
    assert!(
        system_binding_stubs().contains(fixture),
        "generated bundle must retain the generic `as` keyword alias fixture"
    );

    let parsed = parse_module(fixture);
    assert!(
        parsed.is_ok(),
        "the focused generated stub must parse in supported Orna grammar: {:?}",
        parsed.diagnostics
    );
    assert_eq!(parsed.value.items.len(), 1);
    let Declaration::Function { signature, .. } = &parsed.value.items[0].declaration else {
        panic!("the keyword fixture must contain one function declaration")
    };
    let operation = system_provider_abi()
        .operation("sys.invoke<T>")
        .expect("generic invoke dispatch entry");
    assert_eq!(
        signature
            .generics
            .iter()
            .map(|generic| generic.name.as_str())
            .collect::<Vec<_>>(),
        operation
            .signature
            .type_parameters
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    let parameter = &signature.parameters[2];
    let parameter_source = &fixture[parameter.span.start..parameter.span.end];
    assert_eq!(parameter_source.split_once(": ").unwrap().0, "as_");
    assert!(fixture.contains("// sys-parameter-alias: as=as_\n"));
    assert_eq!(operation.signature.parameters[2].name, "as");
    assert_eq!(
        resolve_type(
            parameter
                .annotation
                .as_ref()
                .expect("typed alias parameter")
        )
        .expect("supported alias type"),
        operation.signature.parameters[2].ty
    );
    assert_eq!(
        resolve_type(signature.result.as_ref().expect("typed generic result"))
            .expect("supported generic result"),
        operation.signature.result
    );
}

#[test]
fn generated_start_generic_keyword_tail_matches_the_in_crate_fixture() {
    let fixture = GENERIC_START_KEYWORD_STUB_FIXTURE.trim_end();
    assert!(
        system_binding_stubs().contains(fixture),
        "generated bundle must retain the generic sys.start keyword alias fixture"
    );

    let parsed = parse_module(fixture);
    assert!(
        parsed.is_ok(),
        "the focused generated start stub must parse in supported Orna grammar: {:?}",
        parsed.diagnostics
    );
    assert_eq!(parsed.value.items.len(), 1);
    let Declaration::Function { signature, .. } = &parsed.value.items[0].declaration else {
        panic!("the start fixture must contain one function declaration")
    };
    let operation = system_provider_abi()
        .operation("sys.start<T>")
        .expect("generic start dispatch entry");
    assert_eq!(operation.effects, EffectSet::one(SystemEffect::Invoke));
    assert_eq!(
        operation.role.as_ref().unwrap().as_str(),
        "langitem.sys.start"
    );
    assert_eq!(
        signature
            .generics
            .iter()
            .map(|generic| generic.name.as_str())
            .collect::<Vec<_>>(),
        operation
            .signature
            .type_parameters
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    let parameter = &signature.parameters[2];
    let parameter_source = &fixture[parameter.span.start..parameter.span.end];
    assert_eq!(parameter_source.split_once(": ").unwrap().0, "as_");
    assert!(fixture.contains("// sys-parameter-alias: as=as_\n"));
    assert_eq!(operation.signature.parameters[2].name, "as");
    assert_eq!(
        resolve_type(
            parameter
                .annotation
                .as_ref()
                .expect("typed start alias parameter")
        )
        .expect("supported alias type"),
        operation.signature.parameters[2].ty
    );
    let parsed_transaction_default = signature.parameters[4]
        .default
        .as_ref()
        .map(|default| &fixture[default.span().start..default.span().end]);
    assert_eq!(
        parsed_transaction_default,
        operation.signature.parameters[4].default.as_deref()
    );
    assert_eq!(
        resolve_type(signature.result.as_ref().expect("typed handle result"))
            .expect("supported InvocationHandle result"),
        operation.signature.result
    );
}

#[test]
fn generated_start_generic_keyword_overloads_match_the_in_crate_fixture() {
    let fixture = GENERIC_START_KEYWORD_OVERLOAD_FIXTURE.trim_end();
    assert!(
        system_binding_stubs().contains(fixture),
        "generated bundle must keep erased and generic start stubs paired"
    );

    let parsed = parse_module(fixture);
    assert!(
        parsed.is_ok(),
        "the focused overload fixture must parse in supported Orna grammar: {:?}",
        parsed.diagnostics
    );
    assert_eq!(parsed.value.items.len(), 2);
    let operations = ["sys.start(Value)", "sys.start<T>"];
    let markers = fixture
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    assert_eq!(markers, operations);

    for (index, (item, operation_name)) in parsed.value.items.iter().zip(operations).enumerate() {
        let Declaration::Function { signature, .. } = &item.declaration else {
            panic!("each start overload must be a function declaration")
        };
        let operation = system_provider_abi()
            .operation(operation_name)
            .expect("overload dispatch entry");
        assert_eq!(signature.name, "start");
        assert_eq!(
            signature
                .generics
                .iter()
                .map(|generic| generic.name.as_str())
                .collect::<Vec<_>>(),
            operation
                .signature
                .type_parameters
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            signature.parameters.len(),
            operation.signature.parameters.len()
        );
        assert_eq!(
            signature.parameters.iter().any(|parameter| {
                let source = &fixture[parameter.span.start..parameter.span.end];
                source.starts_with("as_:")
            }),
            index == 1,
            "only the generic overload emits the registry's reserved `as` parameter as `as_`"
        );
        assert_eq!(
            resolve_type(signature.result.as_ref().expect("typed overload result"))
                .expect("supported overload result"),
            operation.signature.result
        );
    }
}

#[test]
fn generated_invoke_generic_keyword_overloads_match_the_in_crate_fixture() {
    let fixture = GENERIC_INVOKE_KEYWORD_OVERLOAD_FIXTURE.trim_end();
    assert!(
        system_binding_stubs().contains(fixture),
        "generated bundle must keep erased and generic invoke stubs paired"
    );

    let parsed = parse_module(fixture);
    assert!(
        parsed.is_ok(),
        "the focused invoke overload fixture must parse in supported Orna grammar: {:?}",
        parsed.diagnostics
    );
    assert_eq!(parsed.value.items.len(), 2);
    let operations = ["sys.invoke(Value)", "sys.invoke<T>"];
    let markers = fixture
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    assert_eq!(markers, operations);

    for (index, (item, operation_name)) in parsed.value.items.iter().zip(operations).enumerate() {
        let Declaration::Function { signature, .. } = &item.declaration else {
            panic!("each invoke overload must be a function declaration")
        };
        let operation = system_provider_abi()
            .operation(operation_name)
            .expect("overload dispatch entry");
        assert_eq!(signature.name, "invoke");
        assert_eq!(
            signature
                .generics
                .iter()
                .map(|generic| generic.name.as_str())
                .collect::<Vec<_>>(),
            operation
                .signature
                .type_parameters
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            signature.parameters.len(),
            operation.signature.parameters.len()
        );
        for (parsed_parameter, registered_parameter) in signature
            .parameters
            .iter()
            .zip(&operation.signature.parameters)
        {
            let parameter_source = &fixture[parsed_parameter.span.start..parsed_parameter.span.end];
            let (parameter_name, _) = parameter_source
                .split_once(": ")
                .expect("invoke overload parameters have explicit types");
            let expected_name = if index == 1 && registered_parameter.name == "as" {
                "as_"
            } else {
                registered_parameter.name.as_str()
            };
            assert_eq!(parameter_name, expected_name);
            assert_eq!(
                resolve_type(
                    parsed_parameter
                        .annotation
                        .as_ref()
                        .expect("typed invoke overload parameter")
                )
                .expect("supported invoke overload parameter type"),
                registered_parameter.ty
            );
            let parsed_default = parsed_parameter
                .default
                .as_ref()
                .map(|default| &fixture[default.span().start..default.span().end]);
            assert_eq!(parsed_default, registered_parameter.default.as_deref());
        }
        assert_eq!(
            signature.parameters.iter().any(|parameter| {
                let source = &fixture[parameter.span.start..parameter.span.end];
                source.starts_with("as_:")
            }),
            index == 1,
            "only the generic overload emits the registry's reserved `as` parameter as `as_`"
        );
        assert_eq!(
            resolve_type(signature.result.as_ref().expect("typed overload result"))
                .expect("supported overload result"),
            operation.signature.result
        );
    }
}

#[test]
fn generated_invocation_overload_families_match_the_registry_as_a_set() {
    let bundle = system_binding_stubs();
    let families = [
        (
            GENERIC_INVOKE_KEYWORD_OVERLOAD_FIXTURE,
            ["sys.invoke(Value)", "sys.invoke<T>"],
            "invoke",
            "langitem.sys.invoke",
        ),
        (
            GENERIC_START_KEYWORD_OVERLOAD_FIXTURE,
            ["sys.start(Value)", "sys.start<T>"],
            "start",
            "langitem.sys.start",
        ),
    ];
    let mut previous_bundle_position = None;
    for (fixture, expected_operations, function_name, role_name) in families {
        let fixture = fixture.trim_end();
        let parsed = parse_module(fixture);
        assert!(
            parsed.is_ok(),
            "invocation overload fixtures must parse in supported Orna grammar: {:?}",
            parsed.diagnostics
        );
        assert_eq!(parsed.value.items.len(), expected_operations.len());
        let markers = fixture
            .lines()
            .filter_map(|line| line.strip_prefix("// sys-op: "))
            .collect::<Vec<_>>();
        assert_eq!(markers, expected_operations);

        for block in fixture.split("\n\n") {
            let block = block.trim_end();
            let position = bundle
                .find(block)
                .expect("each overload fixture block is emitted in the generated bundle");
            if let Some(previous) = previous_bundle_position {
                assert!(
                    previous < position,
                    "invoke and start overload pairs retain registry emission order"
                );
            }
            previous_bundle_position = Some(position);
        }

        for (index, (item, operation_name)) in parsed
            .value
            .items
            .iter()
            .zip(expected_operations)
            .enumerate()
        {
            let Declaration::Function { signature, .. } = &item.declaration else {
                panic!("each invocation overload must be a function declaration")
            };
            let operation = system_provider_abi()
                .operation(operation_name)
                .expect("invocation overload dispatch entry");
            assert_eq!(operation.effects, EffectSet::one(SystemEffect::Invoke));
            assert_eq!(
                operation.role.as_ref().map(|role| role.as_str()),
                Some(role_name)
            );
            assert_eq!(signature.name, function_name);
            assert_eq!(
                signature
                    .generics
                    .iter()
                    .map(|generic| generic.name.as_str())
                    .collect::<Vec<_>>(),
                operation
                    .signature
                    .type_parameters
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                signature.parameters.len(),
                operation.signature.parameters.len()
            );
            for (parsed_parameter, registered_parameter) in signature
                .parameters
                .iter()
                .zip(&operation.signature.parameters)
            {
                let parameter_source =
                    &fixture[parsed_parameter.span.start..parsed_parameter.span.end];
                let (parameter_name, _) = parameter_source
                    .split_once(": ")
                    .expect("overload fixture parameter has an explicit type");
                let expected_name = if index == 1 && registered_parameter.name == "as" {
                    "as_"
                } else {
                    registered_parameter.name.as_str()
                };
                assert_eq!(parameter_name, expected_name);
                assert_eq!(
                    resolve_type(
                        parsed_parameter
                            .annotation
                            .as_ref()
                            .expect("typed overload parameter")
                    )
                    .expect("supported overload parameter type"),
                    registered_parameter.ty
                );
                let parsed_default = parsed_parameter
                    .default
                    .as_ref()
                    .map(|default| &fixture[default.span().start..default.span().end]);
                assert_eq!(parsed_default, registered_parameter.default.as_deref());
            }
            assert_eq!(
                resolve_type(signature.result.as_ref().expect("typed overload result"))
                    .expect("supported overload result type"),
                operation.signature.result
            );
        }
    }
}
