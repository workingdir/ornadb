//! The catalogue document the identity generator reads, rendered in memory from the declarations.
//!
//! `orna-sys-id` builds its tables from this value during the build script, so no catalogue file is read. The keys
//! and shapes are the ones the generator parses: every member is present, and an absent optional is `null`.

use serde_json::{Map, Value, json};

use crate::{RawCounts, RawField, RawFunction, RawRelation, RawRemovedName, RawSingleton, RawSystemApi, RawValueType};

/// The language version the identity generator pins.
const LANGUAGE_VERSION: &str = "1.1.0";
/// The `sys` surface version the identity generator pins.
const SYS_VERSION: &str = "1.1";

/// Renders the declared `sys` descriptor as the catalogue document the identity generator parses.
pub fn catalogue_json(raw: &RawSystemApi) -> Value {
    json!({
        "counts": counts(&raw.counts),
        "language_version": LANGUAGE_VERSION,
        "sys_version": SYS_VERSION,
        "singletons": raw.singletons.iter().map(singleton).collect::<Vec<_>>(),
        "opaque_identifiers": raw.opaque_identifiers,
        "reference_aliases": raw.reference_aliases.iter().map(|alias| json!({
            "name": alias.name,
            "target": alias.target,
            "definition": alias.definition,
        })).collect::<Vec<_>>(),
        "value_types": raw.value_types.iter().map(value_type).collect::<Vec<_>>(),
        "enums": raw.enums,
        "relations": raw.relations.iter().map(relation).collect::<Vec<_>>(),
        "functions": raw.functions.iter().map(function).collect::<Vec<_>>(),
        "failure_codes": raw.failure_codes,
        "removed_names": raw.removed_names.iter()
            .map(|(name, removed)| (name.clone(), removed_name(removed)))
            .collect::<Map<String, Value>>(),
    })
}

fn counts(counts: &RawCounts) -> Value {
    json!({
        "singletons": counts.singletons,
        "opaque_identifiers": counts.opaque_identifiers,
        "reference_aliases": counts.reference_aliases,
        "value_types": counts.value_types,
        "enums": counts.enums,
        "relations": counts.relations,
        "functions": counts.functions,
        "failure_codes": counts.failure_codes,
    })
}

fn singleton(singleton: &RawSingleton) -> Value {
    json!({
        "name": singleton.name,
        "type": singleton.ty,
        "availability": singleton.availability,
    })
}

fn value_type(value_type: &RawValueType) -> Value {
    json!({
        "name": value_type.name,
        "kind": value_type.kind,
        "purpose": value_type.purpose,
        "type_parameters": value_type.type_parameters,
        "fields": value_type.fields.iter().map(field).collect::<Vec<_>>(),
        "invariants": value_type.invariants,
    })
}

fn relation(relation: &RawRelation) -> Value {
    json!({
        "name": relation.name,
        "grouped_handle": relation.grouped_handle,
        "kind": relation.kind,
        "availability": relation.availability,
        "key": relation.key,
        "purpose": relation.purpose,
        "key_fields": relation.key_fields,
        "invariants": relation.invariants,
        "fields": relation.fields.iter().map(field).collect::<Vec<_>>(),
        "writable": relation.writable,
        "reference_type": relation.reference_type,
    })
}

fn field(field: &RawField) -> Value {
    json!({ "name": field.name, "type": field.ty })
}

fn function(function: &RawFunction) -> Value {
    json!({
        "name": function.name,
        "signature": function.signature,
        "effect": function.effect,
        "purpose": function.purpose,
        "since": function.since,
        "contract": function.contract,
        "preconditions": function.preconditions,
        "ownership": function.ownership,
        "snapshot_rule": function.snapshot_rule,
    })
}

fn removed_name(removed: &RawRemovedName) -> Value {
    json!({
        "replacement": removed.replacement,
        "diagnostic": removed.diagnostic,
        "reason": removed.reason,
    })
}
