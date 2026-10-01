use orna_syntax_v1::{Declaration, TypeExpr, parse_module};
use orna_sys_v1::{
    AbiType, EffectSet, OperationContract, SystemEffect, system_binding_stubs, system_provider_abi,
};

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
    let operations = abi.operations().collect::<Vec<_>>();
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

    assert_eq!(parsed.value.items.len(), operations.len());
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
        let Declaration::Function { signature, .. } = &item.declaration else {
            panic!("each generated stub must be a function declaration")
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
