//! Structural equivalence between the engine-embedded `sys.orna` declarations and the
//! JSON catalogue the checker reads today (SN1 proof, not a byte-equality proof).
//!
//! Catalogue entries are keyed by their full `name` label. A `pub native fn` is keyed by its `spelling`
//! clause, or by `sys.<name>` without one. Each side is rendered as signature plus class; every
//! difference is reported and none is suppressed.

use std::collections::{BTreeMap, BTreeSet};

use orna_syntax::{Declaration, TypeMember, TypeRepresentation, parse_interface};

use super::native_raw::{
    NAMESPACE_FILES, SYS_ORNA, catalogue_class, declared_names, native_label, render_generics,
    render_native, render_type,
};

const SYS_JSON: &str = include_str!("../../../orna-sys-id/catalogue/sys-1.1.0.json");

/// Native callables from `sys.orna`, keyed by label. With `retired` false these are the active callables; with
/// `retired` true they are the ones carrying a `removed` clause. Each value is the rendered signature followed by
/// the catalogue class.
fn native_entries(retired: bool) -> BTreeMap<String, String> {
    let parsed = parse_interface(SYS_ORNA);
    assert!(
        parsed.diagnostics.is_empty(),
        "sys.orna must parse without diagnostics: {:?}",
        parsed.diagnostics
    );
    let declared = declared_names(&parsed.value.items);
    let mut natives = BTreeMap::new();
    for item in &parsed.value.items {
        if let Declaration::Native { signature, effects } = &item.declaration {
            if item.clauses.removed.is_some() != retired {
                continue;
            }
            let label = native_label(item, &signature.name);
            let entry = format!(
                "{} class {}",
                render_native(&label, signature, &declared),
                catalogue_class(effects.as_ref())
            );
            let previous = natives.insert(label.clone(), entry);
            assert!(
                previous.is_none(),
                "two native declarations carry the label `{label}`"
            );
        }
    }
    natives
}

/// The JSON catalogue entries keyed by their full `name` label, each as the signature followed by the class.
fn catalogue_entries() -> BTreeMap<String, String> {
    let document: serde_json::Value =
        serde_json::from_str(SYS_JSON).expect("the catalogue must be JSON");
    let functions = document["functions"]
        .as_array()
        .expect("catalogue `functions` must be an array");
    let mut entries = BTreeMap::new();
    for function in functions {
        let label = function["name"]
            .as_str()
            .expect("catalogue `name` must be a string");
        let signature = function["signature"]
            .as_str()
            .expect("catalogue `signature` must be a string");
        let class = function["effect"]
            .as_str()
            .expect("catalogue `effect` must be a string");
        let previous = entries.insert(label.to_owned(), format!("{signature} class {class}"));
        assert!(previous.is_none(), "duplicate catalogue label `{label}`");
    }
    entries
}

#[test]
fn native_callable_signatures_match_json_catalogue() {
    assert_same_entries(
        "callable(s) differ between sys.orna and the JSON catalogue",
        &catalogue_entries(),
        &native_entries(false),
    );
}

const RETIRED_JSON: &str = include_str!("../../tests/fixtures/reference/api/retired.json");

/// Fails with every key whose two renderings differ; `what` completes the message's count.
fn assert_same_entries(
    what: &str,
    expected: &BTreeMap<String, String>,
    actual: &BTreeMap<String, String>,
) {
    let labels: BTreeSet<&String> = expected.keys().chain(actual.keys()).collect();
    let mut differences = Vec::new();
    for label in labels {
        let expected_side = expected.get(label);
        let actual_side = actual.get(label);
        if expected_side != actual_side {
            differences.push(format!(
                "{label}:\n  expected = {expected_side:?}\n  actual   = {actual_side:?}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} {what}:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// Names of the declared types that carry a `removed` clause, qualified as the catalogue spells them.
fn removed_declared_types() -> BTreeSet<String> {
    parse_interface(SYS_ORNA)
        .value
        .items
        .iter()
        .filter(|item| item.clauses.removed.is_some())
        .filter_map(|item| match &item.declaration {
            Declaration::Enum { name, .. }
            | Declaration::Type { name, .. }
            | Declaration::NativeType { name, .. } => Some(format!("sys.{name}")),
            _ => None,
        })
        .collect()
}

/// The `removed` reason of each removed native callable, keyed by label.
fn removed_reasons() -> BTreeMap<String, Option<String>> {
    parse_interface(SYS_ORNA)
        .value
        .items
        .iter()
        .filter_map(|item| {
            let removed = item.clauses.removed.as_ref()?;
            let Declaration::Native { signature, .. } = &item.declaration else {
                return None;
            };
            let reason = removed.reason.as_ref().map(|reason| reason.text.clone());
            Some((native_label(item, &signature.name), reason))
        })
        .collect()
}

/// Removed callables and types against the frozen `retired.json`: the signature and class of each removed callable,
/// the set of removed types, and each removed callable's reason against `removed_names`.
#[test]
fn removed_callables_match_retired_json() {
    let retired: serde_json::Value =
        serde_json::from_str(RETIRED_JSON).expect("retired.json must be JSON");
    let expected: BTreeMap<String, String> = retired["functions"]
        .as_array()
        .expect("retired `functions` must be an array")
        .iter()
        .map(|function| {
            let label = function["name"]
                .as_str()
                .expect("retired `name` must be a string");
            let signature = function["signature"]
                .as_str()
                .expect("retired `signature` must be a string");
            let class = function["effect"]
                .as_str()
                .expect("retired `effect` must be a string");
            (label.to_owned(), format!("{signature} class {class}"))
        })
        .collect();
    assert_same_entries(
        "retired callable(s) differ between sys.orna and retired.json",
        &expected,
        &native_entries(true),
    );

    let mut expected_types: BTreeSet<String> = retired["enums"]
        .as_object()
        .expect("retired `enums` must be an object")
        .keys()
        .cloned()
        .collect();
    expected_types.extend(
        retired["value_types"]
            .as_array()
            .expect("retired `value_types` must be an array")
            .iter()
            .map(|name| {
                name.as_str()
                    .expect("retired value type must be a string")
                    .to_owned()
            }),
    );
    assert_eq!(
        removed_declared_types(),
        expected_types,
        "the removed types in sys.orna must be exactly the retired.json types"
    );

    let catalogue: serde_json::Value =
        serde_json::from_str(SYS_JSON).expect("the catalogue must be JSON");
    let removed_names = catalogue["removed_names"]
        .as_object()
        .expect("catalogue `removed_names` must be an object");
    for (label, reason) in removed_reasons() {
        let recorded = removed_names
            .get(&label)
            .and_then(|entry| entry["reason"].as_str())
            .map(str::to_owned);
        assert_eq!(
            reason, recorded,
            "the removal reason of `{label}` must match removed_names"
        );
    }
}

/// `formerly` clauses against `removed_names`: each names a retired name whose replacement is the declaration carrying
/// the clause, and whose diagnostic is the clause's. Every `removed_names` replacement must have a declaration that
/// carries the matching `formerly` clause, in both directions. Native callables and native statics both carry it.
#[test]
fn formerly_clauses_agree_with_removed_names() {
    let catalogue: serde_json::Value =
        serde_json::from_str(SYS_JSON).expect("the catalogue must be JSON");
    let removed_names = catalogue["removed_names"]
        .as_object()
        .expect("catalogue `removed_names` must be an object");
    let mut differences = Vec::new();
    let mut declared_formerly: BTreeSet<String> = BTreeSet::new();
    for item in &parse_interface(SYS_ORNA).value.items {
        let Some(clause) = &item.clauses.formerly else {
            continue;
        };
        let name = match &item.declaration {
            Declaration::Native { signature, .. } => signature.name.as_str(),
            Declaration::NativeStatic { name, .. } => name.as_str(),
            _ => {
                differences.push(format!(
                    "`formerly {}` is not on a native callable or native static",
                    clause.name.text
                ));
                continue;
            }
        };
        let label = native_label(item, name);
        declared_formerly.insert(clause.name.text.clone());
        let entry = removed_names.get(&clause.name.text);
        let replacement = entry.and_then(|entry| entry["replacement"].as_str());
        let diagnostic = entry.and_then(|entry| entry["diagnostic"].as_str());
        let declared = clause
            .diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.text.as_str());
        if replacement != Some(label.as_str()) || diagnostic != declared {
            differences.push(format!(
                "{label}: formerly `{}` declares diagnostic {declared:?}; removed_names has replacement \
                 {replacement:?} and diagnostic {diagnostic:?}",
                clause.name.text
            ));
        }
    }
    for (old, entry) in removed_names {
        let Some(replacement) = entry["replacement"].as_str() else {
            continue;
        };
        if !declared_formerly.contains(old) {
            differences.push(format!(
                "`{old}` is replaced by `{replacement}` in removed_names but no declaration carries `formerly \"{old}\"`"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} formerly clause(s) disagree with removed_names:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// The native statics in `sys.orna` against the catalogue's `singletons`: each name with its type. Availability is
/// catalogue prose that no declaration carries, so it is not compared.
#[test]
fn singletons_match_json_catalogue() {
    let catalogue: serde_json::Value =
        serde_json::from_str(SYS_JSON).expect("the catalogue must be JSON");
    let expected: BTreeMap<String, String> = catalogue["singletons"]
        .as_array()
        .expect("catalogue `singletons` must be an array")
        .iter()
        .map(|singleton| {
            let name = singleton["name"]
                .as_str()
                .expect("catalogue singleton `name` must be a string");
            let ty = singleton["type"]
                .as_str()
                .expect("catalogue singleton `type` must be a string");
            (name.to_owned(), ty.to_owned())
        })
        .collect();
    let parsed = parse_interface(SYS_ORNA);
    let declared = declared_names(&parsed.value.items);
    let mut actual = BTreeMap::new();
    for item in &parsed.value.items {
        let Declaration::NativeStatic {
            name,
            annotation: Some(annotation),
            ..
        } = &item.declaration
        else {
            continue;
        };
        actual.insert(native_label(item, name), render_type(annotation, &declared));
    }
    assert_eq!(
        actual, expected,
        "the native statics in sys.orna must match the catalogue's singletons"
    );
}

/// Joins prose parts with single spaces after normalising whitespace, so line wrapping never changes a comparison.
fn flatten(parts: &[String]) -> String {
    parts
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A non-empty string field of a catalogue entry.
fn json_text<'a>(value: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    value[field].as_str().filter(|text| !text.is_empty())
}

/// The string items of a catalogue list field, empty when the field is absent.
fn json_list<'a>(value: &'a serde_json::Value, field: &str) -> Vec<&'a str> {
    value.get(field).map_or_else(Vec::new, |list| {
        list.as_array()
            .expect("catalogue list fields must be arrays")
            .iter()
            .map(|item| item.as_str().expect("catalogue list items must be strings"))
            .collect()
    })
}

/// The prose parts of one entry in the order `sys.orna` writes them: the purpose, each labelled field, then each
/// list as its label followed by one `- item` part per item.
fn prose_parts(
    purpose: Option<&str>,
    labelled: &[(&str, Option<&str>)],
    lists: &[(&str, Vec<&str>)],
) -> Vec<String> {
    let mut parts: Vec<String> = purpose.map(str::to_owned).into_iter().collect();
    for (label, text) in labelled {
        if let Some(text) = text {
            parts.push(format!("{label}: {text}"));
        }
    }
    for (label, items) in lists {
        if !items.is_empty() {
            parts.push(format!("{label}:"));
            parts.extend(items.iter().map(|item| format!("- {item}")));
        }
    }
    parts
}

/// Records an entry's flattened prose under `key`. Entries with no prose at all are not recorded, so a declaration
/// that carries prose the JSON lacks shows up as an unexpected key.
fn insert_prose(prose: &mut BTreeMap<String, String>, key: String, parts: &[String]) {
    let text = flatten(parts);
    if !text.is_empty() {
        prose.insert(key, text);
    }
}

/// The flattened prose of every catalogue entry, keyed by kind and name: `fn:` callables, `static:` singletons,
/// `relation:` row types and `value:` value types.
fn json_prose(catalogue: &serde_json::Value) -> BTreeMap<String, String> {
    let mut prose = BTreeMap::new();
    for function in catalogue["functions"]
        .as_array()
        .expect("catalogue `functions` must be an array")
    {
        let name = function["name"]
            .as_str()
            .expect("catalogue function `name` must be a string");
        let parts = prose_parts(
            json_text(function, "purpose"),
            &[
                ("Contract", json_text(function, "contract")),
                ("Preconditions", json_text(function, "preconditions")),
                ("Ownership", json_text(function, "ownership")),
                ("Snapshot rule", json_text(function, "snapshot_rule")),
            ],
            &[],
        );
        insert_prose(&mut prose, format!("fn:{name}"), &parts);
    }
    for singleton in catalogue["singletons"]
        .as_array()
        .expect("catalogue `singletons` must be an array")
    {
        let name = singleton["name"]
            .as_str()
            .expect("catalogue singleton `name` must be a string");
        let parts = prose_parts(
            None,
            &[("Availability", json_text(singleton, "availability"))],
            &[],
        );
        insert_prose(&mut prose, format!("static:{name}"), &parts);
    }
    for relation in catalogue["relations"]
        .as_array()
        .expect("catalogue `relations` must be an array")
    {
        let name = relation["name"]
            .as_str()
            .expect("catalogue relation `name` must be a string");
        let parts = prose_parts(
            json_text(relation, "purpose"),
            &[],
            &[("Invariants", json_list(relation, "invariants"))],
        );
        insert_prose(&mut prose, format!("relation:{name}"), &parts);
    }
    for value in catalogue["value_types"]
        .as_array()
        .expect("catalogue `value_types` must be an array")
    {
        let name = value["name"]
            .as_str()
            .expect("catalogue value type `name` must be a string");
        let parts = prose_parts(
            json_text(value, "purpose"),
            &[("Kind", json_text(value, "kind"))],
            &[("Invariants", json_list(value, "invariants"))],
        );
        insert_prose(&mut prose, format!("value:{name}"), &parts);
    }
    prose
}

/// The flattened `///` prose of each declaration in `sys.orna`, keyed the way `json_prose` keys the catalogue. A type
/// is a relation when the catalogue names it as one, otherwise a value type.
fn declared_prose(
    items: &[orna_syntax::Item],
    catalogue: &serde_json::Value,
) -> BTreeMap<String, String> {
    let relations: BTreeSet<&str> = catalogue["relations"]
        .as_array()
        .expect("catalogue `relations` must be an array")
        .iter()
        .filter_map(|relation| relation["name"].as_str())
        .collect();
    let mut prose = BTreeMap::new();
    for item in items.iter().filter(|item| !item.docs.is_empty()) {
        let key = match &item.declaration {
            Declaration::Native { signature, .. } => {
                format!("fn:{}", native_label(item, &signature.name))
            }
            Declaration::NativeStatic { name, .. } => {
                format!("static:{}", native_label(item, name))
            }
            Declaration::Type { name, generics, .. } => {
                let qualified = format!("sys.{name}");
                if relations.contains(qualified.as_str()) {
                    format!("relation:{qualified}")
                } else {
                    format!("value:{qualified}{}", render_generics(generics))
                }
            }
            _ => format!("undocumented:{}", item.docs.join(" ")),
        };
        insert_prose(&mut prose, key, &split_failure_codes(&item.docs).0);
    }
    prose
}

/// The `///` prose on each declaration against the JSON prose fields: purpose, contract, preconditions, ownership,
/// snapshot rule, invariants, kind and availability. Both sides must name the same entries with the same prose, so a
/// doc comment that drifts from the catalogue, or a catalogue prose field no declaration carries, fails here.
#[test]
fn prose_matches_json_catalogue() {
    let catalogue: serde_json::Value =
        serde_json::from_str(SYS_JSON).expect("the catalogue must be JSON");
    let parsed = parse_interface(SYS_ORNA);
    assert!(
        parsed.diagnostics.is_empty(),
        "sys.orna must parse without diagnostics: {:?}",
        parsed.diagnostics
    );
    assert_same_entries(
        "prose differs between sys.orna doc comments and the JSON catalogue",
        &json_prose(&catalogue),
        &declared_prose(&parsed.value.items, &catalogue),
    );
}

/// Splits a declaration's doc lines into its prose lines and the `- CODE` items of its `Failure codes:` section. The
/// section runs from its heading to the first doc line that is not a list item.
fn split_failure_codes(docs: &[String]) -> (Vec<String>, Vec<String>) {
    let mut prose = Vec::new();
    let mut codes = Vec::new();
    let mut in_codes = false;
    for line in docs {
        let trimmed = line.trim();
        match trimmed.strip_prefix("- ") {
            Some(code) if in_codes => codes.push(code.trim().to_owned()),
            _ if trimmed == "Failure codes:" => in_codes = true,
            _ => {
                in_codes = false;
                prose.push(line.clone());
            }
        }
    }
    (prose, codes)
}

/// The `Failure codes:` section of `sys.orna` against the catalogue's flat `failure_codes` list. Exactly one
/// declaration owns the section, and it names every catalogue code in catalogue order.
#[test]
fn failure_codes_match_json_catalogue() {
    let catalogue: serde_json::Value =
        serde_json::from_str(SYS_JSON).expect("the catalogue must be JSON");
    let parsed = parse_interface(SYS_ORNA);
    assert!(
        parsed.diagnostics.is_empty(),
        "sys.orna must parse without diagnostics: {:?}",
        parsed.diagnostics
    );
    let owners: Vec<Vec<String>> = parsed
        .value
        .items
        .iter()
        .map(|item| split_failure_codes(&item.docs).1)
        .filter(|codes| !codes.is_empty())
        .collect();
    assert_eq!(
        owners.len(),
        1,
        "exactly one declaration must own the failure codes: {owners:?}"
    );
    let declared: Vec<&str> = owners[0].iter().map(String::as_str).collect();
    assert_eq!(
        declared,
        json_list(&catalogue, "failure_codes"),
        "failure codes differ between sys.orna and the JSON catalogue"
    );
}

/// The `since` version of each native callable that carries one, against the catalogue's `since` field. Both sides
/// must name the same callables with the same version.
#[test]
fn since_clauses_match_json_catalogue() {
    let catalogue: serde_json::Value =
        serde_json::from_str(SYS_JSON).expect("the catalogue must be JSON");
    let expected: BTreeMap<String, String> = catalogue["functions"]
        .as_array()
        .expect("catalogue `functions` must be an array")
        .iter()
        .filter_map(|function| {
            let since = function["since"].as_str()?;
            let label = function["name"]
                .as_str()
                .expect("catalogue `name` must be a string");
            Some((label.to_owned(), since.to_owned()))
        })
        .collect();
    let mut actual = BTreeMap::new();
    for item in &parse_interface(SYS_ORNA).value.items {
        let (Some(since), Declaration::Native { signature, .. }) =
            (&item.clauses.since, &item.declaration)
        else {
            continue;
        };
        actual.insert(native_label(item, &signature.name), since.text.clone());
    }
    assert_eq!(
        actual, expected,
        "the `since` clauses in sys.orna must match the catalogue's `since` fields"
    );
}

/// The `key` and `availability` clauses of each relation row type, against the catalogue's `key_fields` and
/// `availability` fields. Both sides must name the same row types with the same key columns and availability word.
#[test]
fn key_and_availability_clauses_match_json_catalogue() {
    let catalogue: serde_json::Value =
        serde_json::from_str(SYS_JSON).expect("the catalogue must be JSON");
    let expected: BTreeMap<String, String> = catalogue["relations"]
        .as_array()
        .expect("catalogue `relations` must be an array")
        .iter()
        .map(|relation| {
            let name = relation["name"]
                .as_str()
                .expect("catalogue `name` must be a string");
            let key: Vec<&str> = relation["key_fields"]
                .as_array()
                .expect("catalogue `key_fields` must be an array")
                .iter()
                .map(|field| {
                    field
                        .as_str()
                        .expect("catalogue key field must be a string")
                })
                .collect();
            let availability = relation["availability"]
                .as_str()
                .expect("catalogue `availability` must be a string");
            (
                name.to_owned(),
                format!("key {} availability {availability}", key.join(", ")),
            )
        })
        .collect();
    let mut actual = BTreeMap::new();
    for item in &parse_interface(SYS_ORNA).value.items {
        let (Some(availability), Declaration::Type { name, .. }) =
            (&item.clauses.availability, &item.declaration)
        else {
            continue;
        };
        let key: Vec<&str> = item
            .clauses
            .key
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        actual.insert(
            format!("sys.{name}"),
            format!("key {} availability {}", key.join(", "), availability.text),
        );
    }
    assert_eq!(
        actual, expected,
        "the `key` and `availability` clauses in sys.orna must match the catalogue's relation fields"
    );
}

/// The relation handles in the namespace modules against the catalogue's `grouped_handle` fields, excluding
/// `sys.rt.*` (those are fields of `RuntimeView`, not declarations). Each `sys.<module>.<name>` must be a native
/// static of type `Relation<T>`, where `T` is the catalogue relation's `name`. Both sides must name the same 59
/// handles.
#[test]
fn namespace_handles_match_json_grouped_handles() {
    let catalogue: serde_json::Value =
        serde_json::from_str(SYS_JSON).expect("the catalogue must be JSON");
    let expected: BTreeMap<String, String> = catalogue["relations"]
        .as_array()
        .expect("catalogue `relations` must be an array")
        .iter()
        .map(|relation| {
            let handle = relation["grouped_handle"]
                .as_str()
                .expect("catalogue `grouped_handle` must be a string");
            let name = relation["name"]
                .as_str()
                .expect("catalogue `name` must be a string");
            (handle.to_owned(), format!("Relation<{name}>"))
        })
        .filter(|(handle, _)| {
            NAMESPACE_FILES
                .iter()
                .any(|(module, _)| handle.starts_with(&format!("sys.{module}.")))
        })
        .collect();
    let declared = declared_names(&parse_interface(SYS_ORNA).value.items);
    let mut actual = BTreeMap::new();
    for (module, text) in NAMESPACE_FILES {
        let parsed = parse_interface(text);
        assert!(
            parsed.diagnostics.is_empty(),
            "sys/{module}.orna must parse without diagnostics: {:?}",
            parsed.diagnostics
        );
        for item in &parsed.value.items {
            let Declaration::NativeStatic {
                name,
                annotation: Some(annotation),
                ..
            } = &item.declaration
            else {
                continue;
            };
            actual.insert(
                format!("sys.{module}.{name}"),
                render_type(annotation, &declared),
            );
        }
    }
    assert_eq!(
        actual, expected,
        "the handles in the sys namespace modules must match the catalogue's grouped_handle fields"
    );
}

/// The `sys.rt.*` relation handles against the `Relation` fields of `sys.RuntimeView`. The catalogue names each
/// handle `sys.rt.<field>`; each `Relation` field of the record must carry the same element type. Both sides must
/// name the same 19 fields.
#[test]
fn runtime_view_relation_fields_match_json_rt_handles() {
    let catalogue: serde_json::Value =
        serde_json::from_str(SYS_JSON).expect("the catalogue must be JSON");
    let expected: BTreeMap<String, String> = catalogue["relations"]
        .as_array()
        .expect("catalogue `relations` must be an array")
        .iter()
        .filter_map(|relation| {
            let handle = relation["grouped_handle"]
                .as_str()
                .expect("catalogue `grouped_handle` must be a string");
            let name = relation["name"]
                .as_str()
                .expect("catalogue `name` must be a string");
            handle
                .strip_prefix("sys.rt.")
                .map(|field| (field.to_owned(), format!("Relation<{name}>")))
        })
        .collect();
    let parsed = parse_interface(SYS_ORNA);
    // `Query` is both a generator builtin (bare in `Query<T>` signatures) and the declared `sys.Query` relation. The
    // relation field must qualify it, so it is re-added to the declared set for this check only.
    let mut declared = declared_names(&parsed.value.items);
    declared.insert("Query".to_owned());
    let members = parsed
        .value
        .items
        .iter()
        .find_map(|item| match &item.declaration {
            Declaration::Type {
                name,
                representation: TypeRepresentation::Nominal { members },
                ..
            } if name == "RuntimeView" => Some(members),
            _ => None,
        })
        .expect("sys.orna must declare the RuntimeView record");
    let actual: BTreeMap<String, String> = members
        .iter()
        .filter_map(|member| match member {
            TypeMember::Field { name, ty, .. } => {
                let rendered = render_type(ty, &declared);
                rendered
                    .starts_with("Relation<")
                    .then(|| (name.clone(), rendered))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        actual, expected,
        "the Relation fields of sys.RuntimeView must match the catalogue's sys.rt.* handles"
    );
}
