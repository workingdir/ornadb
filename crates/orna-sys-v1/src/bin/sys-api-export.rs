use std::{
    collections::BTreeSet,
    env,
    error::Error,
    fs,
    io::{self, Write},
    path::Path,
};

use orna_sys_v1::{system_api_json, system_api_schema_json};
use serde_json::Value;

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

fn main() -> Result<(), Box<dyn Error>> {
    let api_json = system_api_json();
    let schema_json = system_api_schema_json();
    validate_embedded_schema(&api_json, schema_json)?;

    let mut arguments = env::args_os().skip(1);
    let first = arguments.next();
    let (content, output) = match first.as_deref().and_then(|arg| arg.to_str()) {
        Some("--schema") => (schema_json, arguments.next()),
        _ => (api_json.as_str(), first),
    };
    if arguments.next().is_some() {
        return Err("usage: sys-api-export [--schema] [output-path]".into());
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
