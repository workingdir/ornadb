use std::{collections::BTreeMap, fs, path::Path};

use serde_json::{Value, json};
use syn::visit::Visit;

#[derive(Default)]
struct Collector {
    operations: BTreeMap<String, Value>,
    errors: Vec<String>,
}

pub fn generate_host_registry(source_root: &Path) -> Result<String, String> {
    fn visit_sources(root: &Path, collector: &mut Collector) -> Result<(), String> {
        let mut entries = fs::read_dir(root)
            .map_err(|error| format!("read {}: {error}", root.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("list {}: {error}", root.display()))?;
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                visit_sources(&path, collector)?;
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let source = fs::read_to_string(&path)
                    .map_err(|error| format!("read {}: {error}", path.display()))?;
                let parsed = syn::parse_file(&source)
                    .map_err(|error| format!("parse {}: {error}", path.display()))?;
                collector.visit_file(&parsed);
            }
        }
        Ok(())
    }

    let mut collector = Collector::default();
    visit_sources(source_root, &mut collector)?;
    if !collector.errors.is_empty() {
        return Err(collector.errors.join("\n"));
    }
    if collector.operations.is_empty() {
        return Err("sys host-operation registry must not be empty".to_owned());
    }

    let mut roles = BTreeMap::<String, Value>::new();
    for operation in collector.operations.values() {
        let role = operation["role"]
            .as_str()
            .ok_or_else(|| "host operation role must be a string".to_owned())?;
        let provider = operation["provider"]
            .as_str()
            .ok_or_else(|| "host operation provider must be a string".to_owned())?;
        let version = operation["version"].clone();
        let effects = operation["effects"].clone();
        let entry = roles.entry(role.to_owned()).or_insert_with(|| {
            json!({
                "name": role,
                "version": version,
                "provider": provider,
                "effects": effects,
                "operations": [],
                "required": true,
            })
        });
        if entry["provider"] != provider
            || entry["version"] != version
            || entry["effects"] != effects
        {
            return Err(format!(
                "host role `{role}` has inconsistent provider contracts"
            ));
        }
        entry["operations"]
            .as_array_mut()
            .expect("generated role operation array")
            .push(operation["name"].clone());
    }
    let registry = json!({
        "abi_version": {"major": 1, "minor": 0},
        "operations": collector.operations.into_values().collect::<Vec<_>>(),
        "roles": roles.into_values().collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&registry).map_err(|error| error.to_string())
}

impl<'ast> Visit<'ast> for Collector {
    fn visit_impl_item_fn(&mut self, method: &'ast syn::ImplItemFn) {
        for attribute in method
            .attrs
            .iter()
            .filter(|attribute| attribute.path().is_ident("sys_host_operation"))
        {
            let parsed = attribute.parse_args::<syn::LitStr>();
            let result = parsed
                .map_err(|error| error.to_string())
                .and_then(|literal| {
                    let metadata: Value = serde_json::from_str(&literal.value())
                        .map_err(|error| format!("invalid host-operation JSON: {error}"))?;
                    validate_operation(&metadata, &method.sig)?;
                    let name = metadata["name"]
                        .as_str()
                        .ok_or_else(|| "host operation name must be a string".to_owned())?
                        .to_owned();
                    Ok((name, metadata))
                });
            match result {
                Ok((name, metadata)) => {
                    if self.operations.insert(name.clone(), metadata).is_some() {
                        self.errors
                            .push(format!("duplicate sys host operation `{name}`"));
                    }
                }
                Err(error) => self.errors.push(format!("{}: {error}", method.sig.ident)),
            }
        }
        syn::visit::visit_impl_item_fn(self, method);
    }
}

fn validate_operation(metadata: &Value, method: &syn::Signature) -> Result<(), String> {
    let object = metadata
        .as_object()
        .ok_or_else(|| "host operation metadata must be an object".to_owned())?;
    const REQUIRED: [&str; 9] = [
        "name",
        "version",
        "signature",
        "effects",
        "preconditions",
        "failures",
        "role",
        "provider",
        "implementation",
    ];
    if object.len() != REQUIRED.len() || object.keys().any(|key| !REQUIRED.contains(&key.as_str()))
    {
        return Err("host operation metadata has missing or unknown fields".to_owned());
    }
    let name = object["name"]
        .as_str()
        .ok_or_else(|| "host operation name must be a string".to_owned())?;
    let signature = object["signature"]
        .as_str()
        .ok_or_else(|| "host operation signature must be a string".to_owned())?;
    let callable = signature
        .strip_prefix("fn ")
        .and_then(|signature| signature.split_once('(').map(|(head, _)| head))
        .ok_or_else(|| "host operation signature must be a function signature".to_owned())?;
    if callable != name || name.rsplit('.').next() != Some(method.ident.to_string().as_str()) {
        return Err("host operation name, signature, and Rust method must agree".to_owned());
    }
    let expected_signature = match method.ident.to_string().as_str() {
        "get" => "fn std.io.environment.get(name: Str): Str?",
        "require" => "fn std.io.environment.require(name: Str): Str",
        _ => return Err("unsupported native environment operation method".to_owned()),
    };
    if signature != expected_signature {
        return Err("host operation signature must match its native method type".to_owned());
    }
    validate_native_method_signature(method)?;
    let role = object["role"]
        .as_str()
        .ok_or_else(|| "host operation role must be a string".to_owned())?;
    let provider = object["provider"]
        .as_str()
        .ok_or_else(|| "host operation provider must be a string".to_owned())?;
    let role_name = role
        .rsplit_once('@')
        .map(|(name, _version)| name)
        .ok_or_else(|| "host role requires an explicit @major.minor version".to_owned())?;
    for (label, value) in [
        ("role", role_name),
        ("provider", provider),
        ("operation", name),
    ] {
        if value.is_empty()
            || !value.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            })
        {
            return Err(format!("host {label} must be a qualified ASCII identifier"));
        }
    }
    if object["implementation"].as_str() != Some(method.ident.to_string().as_str()) {
        return Err("host operation implementation must name its annotated Rust method".to_owned());
    }
    let version = &object["version"];
    let role_version = role
        .rsplit_once('@')
        .map(|(_, version)| version)
        .expect("role version was checked above");
    let (role_major, role_minor) = role_version
        .split_once('.')
        .ok_or_else(|| "host role version must be major.minor".to_owned())?;
    if role_major.parse::<u64>().is_err()
        || role_minor.parse::<u64>().is_err()
        || version["major"].as_u64().is_none()
        || version["minor"].as_u64().is_none()
        || version["major"].as_u64() != role_major.parse::<u64>().ok()
        || version["minor"].as_u64() != role_minor.parse::<u64>().ok()
    {
        return Err("host operation version requires numeric major and minor".to_owned());
    }
    if object["effects"].as_array().is_none_or(Vec::is_empty)
        || object["effects"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| !matches!(item.as_str(), Some("read" | "invoke" | "admin")))
        })
    {
        return Err("host operation effects must be a non-empty known effect list".to_owned());
    }
    for field in ["preconditions", "failures"] {
        let Some(items) = object[field].as_array() else {
            return Err(format!("host operation {field} must be an array"));
        };
        if items
            .iter()
            .any(|item| item.as_str().is_none_or(str::is_empty))
        {
            return Err(format!(
                "host operation {field} entries must be non-empty strings"
            ));
        }
    }
    Ok(())
}

fn validate_native_method_signature(method: &syn::Signature) -> Result<(), String> {
    use syn::{FnArg, GenericArgument, Pat, PathArguments, ReturnType, Type, TypePath};

    let mut inputs = method.inputs.iter();
    let (Some(FnArg::Receiver(receiver)), Some(FnArg::Typed(argument))) =
        (inputs.next(), inputs.next())
    else {
        return Err("native host operation must take &self and one name argument".to_owned());
    };
    if inputs.next().is_some()
        || receiver.reference.is_none()
        || receiver.mutability.is_some()
        || !matches!(argument.pat.as_ref(), Pat::Ident(name) if name.ident == "name")
        || !matches!(argument.ty.as_ref(), Type::Reference(reference) if matches!(reference.elem.as_ref(), Type::Path(path) if path.path.is_ident("str")))
    {
        return Err(
            "native environment operation must have signature (&self, name: &str)".to_owned(),
        );
    }
    let ReturnType::Type(_, output) = &method.output else {
        return Err("native host operation must return a typed Result".to_owned());
    };
    let Type::Path(TypePath { path, .. }) = output.as_ref() else {
        return Err("native host operation must return Result".to_owned());
    };
    let Some(result) = path
        .segments
        .last()
        .filter(|segment| segment.ident == "Result")
    else {
        return Err("native host operation must return Result".to_owned());
    };
    let PathArguments::AngleBracketed(arguments) = &result.arguments else {
        return Err("native host operation Result must name its value and error types".to_owned());
    };
    let mut types = arguments.args.iter();
    let (Some(GenericArgument::Type(value)), Some(GenericArgument::Type(error))) =
        (types.next(), types.next())
    else {
        return Err("native host operation Result must name its value and error types".to_owned());
    };
    if types.next().is_some() {
        return Err("native host operation Result has extra generic types".to_owned());
    }
    if !matches!(error, Type::Path(path) if path.path.is_ident("EnvironmentProviderError")) {
        return Err("native host operation must use EnvironmentProviderError".to_owned());
    }
    let valid_value = match method.ident.to_string().as_str() {
        "get" => {
            matches!(value, Type::Path(path) if path.path.segments.last().is_some_and(|segment| {
                segment.ident == "Option"
                    && matches!(&segment.arguments, PathArguments::AngleBracketed(args)
                        if matches!(args.args.first(), Some(GenericArgument::Type(Type::Reference(reference)))
                            if matches!(reference.elem.as_ref(), Type::Path(path) if path.path.is_ident("str"))))
            }))
        }
        "require" => {
            matches!(value, Type::Reference(reference) if matches!(reference.elem.as_ref(), Type::Path(path) if path.path.is_ident("str")))
        }
        _ => false,
    };
    if !valid_value {
        return Err(
            "native environment result type must agree with its operation method".to_owned(),
        );
    }
    Ok(())
}
