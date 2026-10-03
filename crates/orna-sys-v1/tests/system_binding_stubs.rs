use orna_syntax_v1::{Declaration, Expr, LiteralKind, TypeExpr, parse_module};
use orna_sys_v1::{
    AbiType, EffectSet, OperationContract, SystemEffect, system_binding_stubs, system_provider_abi,
    system_provider_abi_json,
};
use serde_json::Value;

const GENERIC_KEYWORD_STUB_FIXTURE: &str = include_str!("fixtures/sys-invoke-generic-keyword.orna");
const GENERIC_START_KEYWORD_STUB_FIXTURE: &str =
    include_str!("fixtures/sys-start-generic-keyword.orna");
const GENERIC_START_KEYWORD_OVERLOAD_FIXTURE: &str =
    include_str!("fixtures/sys-start-generic-keyword-overloads.orna");
const GENERIC_INVOKE_KEYWORD_OVERLOAD_FIXTURE: &str =
    include_str!("fixtures/sys-invoke-generic-keyword-overloads.orna");
const GENERIC_OPERATION_STUB_FIXTURE: &str =
    include_str!("fixtures/sys-generic-operation-stubs.orna");

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
    let missing = source.replacen(&format!("{first_marker}\n"), "", 1);
    assert!(
        validate_stub_dispatch_inventory(&missing, &expected).is_err(),
        "omitting a generated dispatch row fails the parity guard"
    );

    let duplicate = format!("{source}\n{first_marker}\n");
    assert!(
        validate_stub_dispatch_inventory(&duplicate, &expected).is_err(),
        "duplicating a generated dispatch row fails the parity guard"
    );

    let unknown = source.replacen(first_marker, "// sys-op: sys.vendor.unknown", 1);
    assert!(
        validate_stub_dispatch_inventory(&unknown, &expected).is_err(),
        "redirecting a generated stub to an unknown registry operation fails the parity guard"
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
