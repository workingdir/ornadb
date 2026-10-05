use std::{
    collections::BTreeSet,
    env,
    error::Error,
    fs,
    io::{self, Write},
    path::{Component, Path},
};

use orna_sys_v1::{
    SystemProviderAbi, system_api_json, system_api_schema_json, system_binding_modules_json,
    system_binding_stubs, system_dispatch_table, system_host_binding_modules_json,
    system_host_binding_stubs, system_host_operation_registry_json,
    system_host_operation_registry_schema_json, system_provider_abi_json,
    system_provider_abi_schema_json,
};
use serde_json::Value;

const USAGE: &str = "sys-api-export [--schema|--provider-abi|--provider-abi-schema|--host-operations|--host-operations-schema|--bindings|--binding-modules|--host-bindings|--host-binding-modules] [output-path] | --all output-directory";

fn write_export(root: &Path, relative_path: &Path, content: &str) -> Result<(), Box<dyn Error>> {
    let path = root.join(relative_path);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

fn export_module_tree(
    output_dir: &Path,
    tree_name: &str,
    modules_json: &str,
) -> Result<(), Box<dyn Error>> {
    let modules: std::collections::BTreeMap<String, String> = serde_json::from_str(modules_json)?;
    for (module_path, source) in modules {
        let relative_path = Path::new(&module_path);
        let valid_path = !relative_path.is_absolute()
            && relative_path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            && relative_path
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("orna");
        if !valid_path {
            return Err(format!("invalid generated binding module path `{module_path}`").into());
        }
        write_export(
            output_dir,
            &Path::new(tree_name).join(relative_path),
            &source,
        )?;
    }
    Ok(())
}

fn export_all(output_dir: &Path) -> Result<(), Box<dyn Error>> {
    let api_json = system_api_json();
    let artifacts = [
        ("api_sys.json", api_json.as_str()),
        ("system_api_schema.json", system_api_schema_json()),
        ("system_provider_abi.json", system_provider_abi_json()),
        (
            "system_provider_abi.schema.json",
            system_provider_abi_schema_json(),
        ),
        (
            "system_host_operations.json",
            system_host_operation_registry_json(),
        ),
        (
            "system_host_operations.schema.json",
            system_host_operation_registry_schema_json(),
        ),
        ("system_bindings.orna", system_binding_stubs()),
        ("system_binding_modules.json", system_binding_modules_json()),
        ("system_host_bindings.orna", system_host_binding_stubs()),
        (
            "system_host_binding_modules.json",
            system_host_binding_modules_json(),
        ),
    ];
    if output_dir.exists() && fs::read_dir(output_dir)?.next().is_some() {
        return Err(format!(
            "complete artifact export requires an empty output directory: {}",
            output_dir.display()
        )
        .into());
    }
    fs::create_dir_all(output_dir)?;
    for (name, content) in artifacts {
        write_export(output_dir, Path::new(name), content)?;
    }

    export_module_tree(output_dir, "system_bindings", system_binding_modules_json())?;
    export_module_tree(
        output_dir,
        "system_host_bindings",
        system_host_binding_modules_json(),
    )?;
    Ok(())
}

fn validate_embedded_schema(api_json: &str, schema_json: &str) -> Result<(), Box<dyn Error>> {
    let api: Value = serde_json::from_str(api_json)?;
    let schema: Value = serde_json::from_str(schema_json)?;
    if schema["$schema"] != "https://json-schema.org/draft/2020-12/schema"
        || schema["type"] != "object"
        || schema["additionalProperties"] != false
    {
        return Err("embedded sys schema is not a closed JSON Schema 2020-12 object".into());
    }
    let artifact_fields = api
        .as_object()
        .ok_or("generated sys API is not a JSON object")?
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let schema_fields = schema["properties"]
        .as_object()
        .ok_or("embedded sys schema has no root properties")?
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let required_fields = schema["required"]
        .as_array()
        .ok_or("embedded sys schema has no required root fields")?
        .iter()
        .map(|field| {
            field
                .as_str()
                .map(str::to_owned)
                .ok_or("embedded sys schema contains a non-string root field")
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if artifact_fields != schema_fields || artifact_fields != required_fields {
        return Err("generated sys API root does not match the embedded schema".into());
    }
    Ok(())
}

fn validate_embedded_provider_registry(
    registry_json: &str,
    schema_json: &str,
) -> Result<(), Box<dyn Error>> {
    let parsed = SystemProviderAbi::from_json(registry_json)
        .map_err(|error| format!("invalid typed provider registry: {error:?}"))?;
    if &parsed != system_dispatch_table() {
        return Err("provider registry export differs from the embedded dispatch table".into());
    }

    let registry: Value = serde_json::from_str(registry_json)?;
    let schema: Value = serde_json::from_str(schema_json)?;
    if schema["$schema"] != "https://json-schema.org/draft/2020-12/schema"
        || schema["type"] != "object"
        || schema["additionalProperties"] != false
    {
        return Err("embedded provider schema is not a closed JSON Schema 2020-12 object".into());
    }
    let artifact_fields = registry
        .as_object()
        .ok_or("generated provider registry is not a JSON object")?
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let schema_fields = schema["properties"]
        .as_object()
        .ok_or("embedded provider schema has no root properties")?
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let required_fields = schema["required"]
        .as_array()
        .ok_or("embedded provider schema has no required root fields")?
        .iter()
        .map(|field| {
            field
                .as_str()
                .map(str::to_owned)
                .ok_or("embedded provider schema contains a non-string root field")
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if artifact_fields != schema_fields || artifact_fields != required_fields {
        return Err("generated provider registry root does not match its embedded schema".into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let api_json = system_api_json();
    let schema_json = system_api_schema_json();
    validate_embedded_schema(&api_json, schema_json)?;
    let provider_json = system_provider_abi_json();
    let provider_schema_json = system_provider_abi_schema_json();
    validate_embedded_provider_registry(provider_json, provider_schema_json)?;

    let mut arguments = env::args_os().skip(1);
    let first = arguments.next();
    if first.as_deref().and_then(|arg| arg.to_str()) == Some("--all") {
        let Some(output_dir) = arguments.next() else {
            return Err(format!("--all requires an output directory; usage: {USAGE}").into());
        };
        if arguments.next().is_some() {
            return Err(format!("usage: {USAGE}").into());
        }
        export_all(Path::new(&output_dir))?;
        return Ok(());
    }
    let (content, output) = match first.as_deref().and_then(|arg| arg.to_str()) {
        Some("--schema") => (schema_json, arguments.next()),
        Some("--provider-abi") => (provider_json, arguments.next()),
        Some("--provider-abi-schema") => (provider_schema_json, arguments.next()),
        Some("--host-operations") => (system_host_operation_registry_json(), arguments.next()),
        Some("--host-operations-schema") => (
            system_host_operation_registry_schema_json(),
            arguments.next(),
        ),
        Some("--bindings") => (system_binding_stubs(), arguments.next()),
        Some("--binding-modules") => (system_binding_modules_json(), arguments.next()),
        Some("--host-bindings") => (system_host_binding_stubs(), arguments.next()),
        Some("--host-binding-modules") => (system_host_binding_modules_json(), arguments.next()),
        Some(argument) if argument.starts_with("--") => {
            return Err(format!("unknown option `{argument}`; usage: {USAGE}").into());
        }
        _ => (api_json.as_str(), first),
    };
    if arguments.next().is_some() {
        return Err(format!("usage: {USAGE}").into());
    }

    if let Some(output) = output {
        let output = Path::new(&output);
        if let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, content)?;
    } else {
        io::stdout().lock().write_all(content.as_bytes())?;
    }
    Ok(())
}
