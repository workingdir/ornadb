use serde_json::json;

use crate::build_support::canonical_pretty_json;

/// JSON Schema for the generated typed provider dispatch artifact.
///
/// This is a build artifact alongside the registry, not an independent
/// descriptor source. Strict object shapes keep the embedded dispatch format
/// and its dev-only conformance export aligned.
pub fn generate_provider_registry_schema() -> Result<String, String> {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://orna.dev/schemas/sys-provider-abi-v1.json",
        "title": "Orna generated typed sys provider dispatch registry",
        "type": "object",
        "additionalProperties": false,
        "required": ["abi_version", "operations", "roles"],
        "properties": {
            "abi_version": {"$ref": "#/$defs/version"},
            "operations": {
                "type": "array",
                "minItems": 1,
                "items": {"$ref": "#/$defs/operation"},
                "uniqueItems": true
            },
            "roles": {
                "type": "array",
                "minItems": 1,
                "items": {"$ref": "#/$defs/role"},
                "uniqueItems": true
            }
        },
        "$defs": {
            "version": {
                "type": "object",
                "additionalProperties": false,
                "required": ["major", "minor"],
                "properties": {
                    "major": {"type": "integer", "minimum": 0},
                    "minor": {"type": "integer", "minimum": 0}
                }
            },
            "operation": {
                "type": "object",
                "additionalProperties": false,
                "required": ["name", "version", "signature", "effect", "preconditions", "failures", "role"],
                "properties": {
                    "name": {"type": "string", "minLength": 1},
                    "version": {"$ref": "#/$defs/version"},
                    "signature": {"type": "string", "minLength": 1},
                    "effect": {"enum": ["read", "invoke", "admin"]},
                    "preconditions": {
                        "type": "array",
                        "items": {"type": "string", "minLength": 1}
                    },
                    "failures": {
                        "type": "array",
                        "uniqueItems": true,
                        "items": {
                            "type": "string",
                            "minLength": 1,
                            "pattern": "^sys\\.[a-z0-9_]+(\\.[a-z0-9_]+)*$"
                        }
                    },
                    "role": {"type": ["string", "null"], "minLength": 1}
                }
            },
            "role": {
                "type": "object",
                "additionalProperties": false,
                "required": ["name", "version", "effects", "operations", "required", "replaceable", "builtin_provider"],
                "properties": {
                    "name": {"type": "string", "minLength": 1},
                    "version": {"$ref": "#/$defs/version"},
                    "effects": {
                        "type": "array",
                        "minItems": 1,
                        "uniqueItems": true,
                        "items": {"enum": ["read", "invoke", "admin"]}
                    },
                    "operations": {
                        "type": "array",
                        "minItems": 1,
                        "uniqueItems": true,
                        "items": {"type": "string", "minLength": 1}
                    },
                    "required": {"type": "boolean"},
                    "replaceable": {"type": "boolean"},
                    "builtin_provider": {"type": ["string", "null"], "minLength": 1}
                }
            }
        }
    });
    let mut output = canonical_pretty_json(&schema).map_err(|error| error.to_string())?;
    output.push('\n');
    Ok(output)
}
