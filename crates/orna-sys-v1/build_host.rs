use std::{collections::BTreeMap, fs, path::Path};

use quote::ToTokens;
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

pub fn generate_host_registry_schema() -> Result<String, String> {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://orna.dev/schemas/sys-host-operations-v1.json",
        "title": "Orna generated sys host-operation registry",
        "type": "object",
        "additionalProperties": false,
        "required": ["abi_version", "operations", "roles"],
        "properties": {
            "abi_version": {"$ref": "#/$defs/version"},
            "operations": {
                "type": "array",
                "items": {"$ref": "#/$defs/operation"},
                "uniqueItems": true
            },
            "roles": {
                "type": "array",
                "items": {"$ref": "#/$defs/role"},
                "uniqueItems": true
            }
        },
        "$defs": {
            "version": {
                "type": "object",
                "additionalProperties": false,
                "required": ["major", "minor"],
                "properties": {"major": {"type": "integer", "minimum": 0}, "minor": {"type": "integer", "minimum": 0}}
            },
            "operation": {
                "type": "object",
                "additionalProperties": false,
                "required": ["name", "version", "signature", "parameters", "effects", "preconditions", "failures", "role", "provider", "implementation"],
                "properties": {
                    "name": {"type": "string", "minLength": 1},
                    "version": {"$ref": "#/$defs/version"},
                    "signature": {"type": "string", "minLength": 1},
                    "parameters": {"type": "array", "items": {"type": "string"}},
                    "effects": {"type": "array", "minItems": 1, "items": {"enum": ["read", "invoke", "admin"]}},
                    "preconditions": {"type": "array", "items": {"type": "string", "minLength": 1}},
                    "failures": {
                        "type": "array",
                        "minItems": 1,
                        "uniqueItems": true,
                        "items": {
                            "type": "string",
                            "pattern": "^sys\\.[a-z0-9_]+(\\.[a-z0-9_]+)*$"
                        }
                    },
                    "role": {"type": "string", "minLength": 1},
                    "provider": {"type": "string", "minLength": 1},
                    "implementation": {"type": "string", "minLength": 1}
                }
            },
            "role": {
                "type": "object",
                "additionalProperties": false,
                "required": ["name", "version", "provider", "effects", "operations", "required"],
                "properties": {
                    "name": {"type": "string", "minLength": 1},
                    "version": {"$ref": "#/$defs/version"},
                    "provider": {"type": "string", "minLength": 1},
                    "effects": {"type": "array", "minItems": 1, "items": {"enum": ["read", "invoke", "admin"]}},
                    "operations": {"type": "array", "minItems": 1, "items": {"type": "string", "minLength": 1}},
                    "required": {"type": "boolean"}
                }
            }
        }
    });
    serde_json::to_string_pretty(&schema).map_err(|error| error.to_string())
}

/// Validates the generated host-operation JSON against the generated schema's
/// deliberately small JSON Schema subset. The same path runs in build.rs and
/// parity tests, so malformed registry output cannot be embedded silently.
pub fn validate_host_registry_json(registry_json: &str, schema_json: &str) -> Result<(), String> {
    let registry: Value = serde_json::from_str(registry_json)
        .map_err(|error| format!("invalid registry JSON: {error}"))?;
    let schema: Value = serde_json::from_str(schema_json)
        .map_err(|error| format!("invalid registry schema JSON: {error}"))?;
    validate_schema_value(&registry, &schema, &schema, "$")
}

fn validate_schema_value(
    value: &Value,
    schema: &Value,
    root_schema: &Value,
    path: &str,
) -> Result<(), String> {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let target = resolve_schema_ref(root_schema, reference)
            .ok_or_else(|| format!("{path}: unresolved schema reference `{reference}`"))?;
        validate_schema_value(value, target, root_schema, path)?;
    }

    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        return Err(format!("{path}: value is outside the schema enum"));
    }

    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let object = value
                .as_object()
                .ok_or_else(|| format!("{path}: expected object"))?;
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for field in required.iter().filter_map(Value::as_str) {
                    if !object.contains_key(field) {
                        return Err(format!("{path}: missing required field `{field}`"));
                    }
                }
            }
            let properties = schema
                .get("properties")
                .and_then(Value::as_object)
                .ok_or_else(|| format!("{path}: object schema has no properties"))?;
            for (field, child) in object {
                match properties.get(field) {
                    Some(child_schema) => validate_schema_value(
                        child,
                        child_schema,
                        root_schema,
                        &format!("{path}.{field}"),
                    )?,
                    None if schema.get("additionalProperties") == Some(&Value::Bool(false)) => {
                        return Err(format!("{path}: unexpected field `{field}`"));
                    }
                    None => {}
                }
            }
        }
        Some("array") => {
            let array = value
                .as_array()
                .ok_or_else(|| format!("{path}: expected array"))?;
            if let Some(minimum) = schema.get("minItems").and_then(Value::as_u64)
                && array.len() < minimum as usize
            {
                return Err(format!("{path}: array has fewer than {minimum} entries"));
            }
            if schema.get("uniqueItems") == Some(&Value::Bool(true)) {
                for index in 0..array.len() {
                    if array[..index].contains(&array[index]) {
                        return Err(format!("{path}: array contains a duplicate entry"));
                    }
                }
            }
            let item_schema = schema
                .get("items")
                .ok_or_else(|| format!("{path}: array schema has no item schema"))?;
            for (index, item) in array.iter().enumerate() {
                validate_schema_value(item, item_schema, root_schema, &format!("{path}[{index}]"))?;
            }
        }
        Some("string") => {
            let string = value
                .as_str()
                .ok_or_else(|| format!("{path}: expected string"))?;
            if let Some(minimum) = schema.get("minLength").and_then(Value::as_u64)
                && string.chars().count() < minimum as usize
            {
                return Err(format!(
                    "{path}: string is shorter than {minimum} characters"
                ));
            }
            if schema.get("pattern").and_then(Value::as_str)
                == Some("^sys\\.[a-z0-9_]+(\\.[a-z0-9_]+)*$")
                && !valid_host_failure_code(string)
            {
                return Err(format!("{path}: invalid host failure code `{string}`"));
            }
        }
        Some("integer") => {
            let integer = value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
                .ok_or_else(|| format!("{path}: expected integer"))?;
            if let Some(minimum) = schema.get("minimum").and_then(Value::as_i64)
                && integer < minimum
            {
                return Err(format!("{path}: integer is below {minimum}"));
            }
        }
        Some("boolean") if !value.is_boolean() => {
            return Err(format!("{path}: expected boolean"));
        }
        Some("boolean") | None => {}
        Some(kind) => return Err(format!("{path}: unsupported schema type `{kind}`")),
    }
    Ok(())
}

fn resolve_schema_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    pointer.split('/').skip(1).try_fold(root, |value, segment| {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        value.get(segment.as_str())
    })
}

fn valid_host_failure_code(code: &str) -> bool {
    let Some(namespace) = code.strip_prefix("sys.") else {
        return false;
    };
    !namespace.is_empty()
        && namespace.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
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
                    let mut metadata: Value = serde_json::from_str(&literal.value())
                        .map_err(|error| format!("invalid host-operation JSON: {error}"))?;
                    validate_operation(&metadata, &method.sig)?;
                    metadata["parameters"] = Value::Array(
                        method
                            .sig
                            .inputs
                            .iter()
                            .filter_map(|input| match input {
                                syn::FnArg::Typed(argument) => match argument.pat.as_ref() {
                                    syn::Pat::Ident(name) => {
                                        Some(Value::String(name.ident.to_string()))
                                    }
                                    _ => None,
                                },
                                syn::FnArg::Receiver(_) => None,
                            })
                            .collect(),
                    );
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
    let expected_signature = match name {
        "std.io.environment.get" => "fn std.io.environment.get(name: Str): Str?",
        "std.io.environment.require" => "fn std.io.environment.require(name: Str): Str",
        "std.io.process.run" => {
            "fn std.io.process.run(executable: Str, arguments: [Str], working_directory: Str, environment: [(Str, Str)], input: Blob?, timeout: Duration?, max_output_bytes: Int): (Int?, Blob, Blob)"
        }
        "std.concurrent.sleep" => "fn std.concurrent.sleep(duration: Duration): Null",
        "std.io.fs.read_text" => "fn std.io.fs.read_text(root: Str, path: Str): Str",
        "std.io.fs.write_text" => {
            "fn std.io.fs.write_text(root: Str, path: Str, contents: Str, overwrite: Bool): Unit"
        }
        "std.io.fs.append_text" => {
            "fn std.io.fs.append_text(root: Str, path: Str, contents: Str): Unit"
        }
        "std.io.fs.exists" => "fn std.io.fs.exists(root: Str, path: Str): Bool",
        "std.io.fs.is_directory" => "fn std.io.fs.is_directory(root: Str, path: Str): Bool",
        "std.io.fs.list" => "fn std.io.fs.list(root: Str, path: Str): [Str]",
        "std.io.fs.metadata" => {
            "fn std.io.fs.metadata(root: Str, path: Str): (Str, Int?, Instant?, Instant?)"
        }
        "std.io.fs.symlink_metadata" => {
            "fn std.io.fs.symlink_metadata(root: Str, path: Str): (Str, Int?, Instant?, Instant?)"
        }
        "std.io.fs.create_dir" => {
            "fn std.io.fs.create_dir(root: Str, path: Str, parents: Bool, exist_ok: Bool): Unit"
        }
        "std.io.fs.remove_file" => "fn std.io.fs.remove_file(root: Str, path: Str): Unit",
        "std.io.fs.copy_file" => {
            "fn std.io.fs.copy_file(root: Str, source_path: Str, destination_path: Str, overwrite: Bool): Unit"
        }
        "std.io.fs.move_file" => {
            "fn std.io.fs.move_file(root: Str, source_path: Str, destination_path: Str, overwrite: Bool): Unit"
        }
        "std.net.http.send" => {
            "fn std.net.http.send(method: Str, url: Str, headers: [(Str, Str)], request_body: Blob?, timeout: Duration?, max_header_bytes: Int, max_body_bytes: Int): (Int, [(Str, Str)], Blob)"
        }
        "std.net.http.start" => {
            "fn std.net.http.start(method: Str, url: Str, headers: [(Str, Str)], request_body: Blob?, timeout: Duration?, max_header_bytes: Int, max_body_bytes: Int): Uuid"
        }
        "std.net.http.wait" => {
            "fn std.net.http.wait(handle: Uuid, timeout: Duration?): (Int, [(Str, Str)], Blob)"
        }
        "std.net.http.cancel" => "fn std.net.http.cancel(handle: Uuid): Bool",
        _ => return Err("unsupported native host operation".to_owned()),
    };
    if signature != expected_signature {
        return Err("host operation signature must match its native method type".to_owned());
    }
    validate_native_method_signature(name, method)?;
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

fn validate_native_method_signature(
    operation: &str,
    method: &syn::Signature,
) -> Result<(), String> {
    use syn::{FnArg, GenericArgument, Pat, PathArguments, ReturnType, Type, TypePath};

    let (expected_arguments, expected_result, expected_error): (&[(&str, &str)], &str, &str) =
        match operation {
            "std.io.environment.get" => (
                &[("name", "&str")],
                "Option<&str>",
                "EnvironmentProviderError",
            ),
            "std.io.environment.require" => {
                (&[("name", "&str")], "&str", "EnvironmentProviderError")
            }
            "std.io.process.run" => (
                &[
                    ("executable", "&str"),
                    ("arguments", "&[String]"),
                    ("working_directory", "&str"),
                    ("environment", "&[(String, String)]"),
                    ("input", "Option<&[u8]>"),
                    ("timeout", "Option<Duration>"),
                    ("max_output_bytes", "usize"),
                ],
                "ProcessRunOutput",
                "ProcessProviderError",
            ),
            "std.concurrent.sleep" => (&[("duration", "Duration")], "()", "ClockProviderError"),
            "std.io.fs.read_text" => (
                &[("root", "&str"), ("path", "&str")],
                "String",
                "FilesystemProviderError",
            ),
            "std.io.fs.write_text" => (
                &[
                    ("root", "&str"),
                    ("path", "&str"),
                    ("contents", "&str"),
                    ("overwrite", "bool"),
                ],
                "()",
                "FilesystemProviderError",
            ),
            "std.io.fs.append_text" => (
                &[("root", "&str"), ("path", "&str"), ("contents", "&str")],
                "()",
                "FilesystemProviderError",
            ),
            "std.io.fs.exists" => (
                &[("root", "&str"), ("path", "&str")],
                "bool",
                "FilesystemProviderError",
            ),
            "std.io.fs.is_directory" => (
                &[("root", "&str"), ("path", "&str")],
                "bool",
                "FilesystemProviderError",
            ),
            "std.io.fs.list" => (
                &[("root", "&str"), ("path", "&str")],
                "Vec<String>",
                "FilesystemProviderError",
            ),
            "std.io.fs.metadata" | "std.io.fs.symlink_metadata" => (
                &[("root", "&str"), ("path", "&str")],
                "HostFilesystemMetadata",
                "FilesystemProviderError",
            ),
            "std.io.fs.create_dir" => (
                &[
                    ("root", "&str"),
                    ("path", "&str"),
                    ("parents", "bool"),
                    ("exist_ok", "bool"),
                ],
                "()",
                "FilesystemProviderError",
            ),
            "std.io.fs.remove_file" => (
                &[("root", "&str"), ("path", "&str")],
                "()",
                "FilesystemProviderError",
            ),
            "std.io.fs.copy_file" => (
                &[
                    ("root", "&str"),
                    ("source_path", "&str"),
                    ("destination_path", "&str"),
                    ("overwrite", "bool"),
                ],
                "()",
                "FilesystemProviderError",
            ),
            "std.io.fs.move_file" => (
                &[
                    ("root", "&str"),
                    ("source_path", "&str"),
                    ("destination_path", "&str"),
                    ("overwrite", "bool"),
                ],
                "()",
                "FilesystemProviderError",
            ),
            "std.net.http.send" => (
                &[
                    ("method", "&str"),
                    ("url", "&str"),
                    ("headers", "&[(String, String)]"),
                    ("request_body", "Option<&[u8]>"),
                    ("timeout", "Option<Duration>"),
                    ("max_header_bytes", "usize"),
                    ("max_body_bytes", "usize"),
                ],
                "HostHttpResponse",
                "HttpProviderError",
            ),
            "std.net.http.start" => (
                &[
                    ("method", "&str"),
                    ("url", "&str"),
                    ("headers", "&[(String, String)]"),
                    ("request_body", "Option<&[u8]>"),
                    ("timeout", "Option<Duration>"),
                    ("max_header_bytes", "usize"),
                    ("max_body_bytes", "usize"),
                ],
                "HostHttpHandle",
                "HttpProviderError",
            ),
            "std.net.http.wait" => (
                &[
                    ("handle", "HostHttpHandle"),
                    ("timeout", "Option<Duration>"),
                ],
                "HostHttpResponse",
                "HttpProviderError",
            ),
            "std.net.http.cancel" => (&[("handle", "HostHttpHandle")], "bool", "HttpProviderError"),
            _ => return Err("unsupported native host operation".to_owned()),
        };
    let mut inputs = method.inputs.iter();
    let Some(FnArg::Receiver(receiver)) = inputs.next() else {
        return Err("native host operation must take &self".to_owned());
    };
    if receiver.reference.is_none() || receiver.mutability.is_some() {
        return Err("native host operation must take an immutable &self".to_owned());
    }
    for (expected_name, expected_type) in expected_arguments {
        let Some(FnArg::Typed(argument)) = inputs.next() else {
            return Err("native host operation has an unexpected receiver".to_owned());
        };
        if !matches!(argument.pat.as_ref(), Pat::Ident(name) if name.ident == *expected_name)
            || compact_tokens(argument.ty.as_ref()) != expected_type.replace(' ', "")
        {
            return Err(format!(
                "native `{operation}` argument `{expected_name}` has the wrong name or type"
            ));
        }
    }
    if inputs.next().is_some() || method.inputs.len() != expected_arguments.len() + 1 {
        return Err(format!("native `{operation}` has the wrong argument count"));
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
    if compact_tokens(value) != expected_result || compact_tokens(error) != expected_error {
        return Err(format!(
            "native `{operation}` result type does not match its descriptor"
        ));
    }
    Ok(())
}

fn compact_tokens<T: ToTokens + ?Sized>(value: &T) -> String {
    value.to_token_stream().to_string().replace(' ', "")
}
