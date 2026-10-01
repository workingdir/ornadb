use orna_syntax_v1::{Declaration, TypeExpr, parse_module};
use orna_sys_v1::{AbiType, OperationContract, system_binding_stubs, system_provider_abi};

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
