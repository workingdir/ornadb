//! Structural equivalence between the engine-embedded `sys.orna` declarations and the
//! JSON catalogue the checker reads today (SN1 proof, not a byte-equality proof).
//!
//! Catalogue entries are keyed by their full `name` label. A `pub native fn` is keyed by its `spelling`
//! clause, or by `sys.<name>` without one. Each side is rendered as signature plus class; every
//! difference is reported and none is suppressed.

use std::collections::{BTreeMap, BTreeSet};

use orna_syntax::{
    Declaration, Expr, GenericParameter, LiteralKind, NativeEffectSet, Pattern, TypeExpr,
    parse_interface,
};

const SYS_ORNA: &str = include_str!("../../../orna-hir/interface/sys.orna");
const CATALOG_ORNA: &str = include_str!("../../../orna-hir/interface/sys/catalog.orna");
const SYS_JSON: &str = include_str!("../../../orna-sys-id/catalogue/sys-1.1.0.json");

/// Type names `scripts/gen_interface.py` leaves bare in the JSON (its `BUILTINS`). These are declared in
/// `sys.orna` but the catalogue never qualifies them, so the renderer must not either.
const GENERATOR_BUILTINS: [&str; 2] = ["Unit", "Query"];

/// Names declared at the top level of `sys.orna` that the JSON spells with a `sys.` prefix.
fn declared_names(items: &[orna_syntax::Item]) -> BTreeSet<String> {
    items
        .iter()
        .filter_map(|item| match &item.declaration {
            Declaration::Enum { name, .. }
            | Declaration::Type { name, .. }
            | Declaration::NativeType { name, .. }
            | Declaration::Protocol { name, .. }
            | Declaration::Dimension { name, .. }
            | Declaration::Unit { name, .. } => Some(name.clone()),
            _ => None,
        })
        .filter(|name| !GENERATOR_BUILTINS.contains(&name.as_str()))
        .collect()
}

fn render_generics(generics: &[GenericParameter]) -> String {
    if generics.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = generics
        .iter()
        .map(|generic| generic.name.as_str())
        .collect();
    format!("<{}>", names.join(", "))
}

fn render_type(ty: &TypeExpr, declared: &BTreeSet<String>) -> String {
    match ty {
        TypeExpr::Name {
            path, arguments, ..
        } => {
            let head = path.join(".");
            let qualified = if declared.contains(&head) {
                format!("sys.{head}")
            } else {
                head
            };
            if arguments.is_empty() {
                qualified
            } else {
                let rendered: Vec<String> = arguments
                    .iter()
                    .map(|arg| render_type(arg, declared))
                    .collect();
                format!("{qualified}<{}>", rendered.join(", "))
            }
        }
        TypeExpr::Optional { inner, .. } => format!("{}?", render_type(inner, declared)),
        TypeExpr::List { inner, .. } => format!("[{}]", render_type(inner, declared)),
        _ => "<unrendered-type>".to_owned(),
    }
}

/// The dotted path of a name or field chain, e.g. `current.snapshot`.
fn render_path(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name { text, .. } => Some(text.clone()),
        Expr::Field { base, name, .. } => Some(format!("{}.{name}", render_path(base)?)),
        _ => None,
    }
}

fn render_default(expr: &Expr) -> String {
    match expr {
        Expr::Name { text, .. } if text == "null" || text == "true" || text == "false" => {
            text.clone()
        }
        Expr::Literal { text, kind, .. } => match kind {
            LiteralKind::Null | LiteralKind::Boolean | LiteralKind::Integer => text.clone(),
            _ => "<unrendered-default>".to_owned(),
        },
        other => render_path(other).map_or_else(
            || "<unrendered-default>".to_owned(),
            |path| format!("sys.{path}"),
        ),
    }
}

/// The JSON head of a label without its overload or generic part: `sys.snapshot(BranchRef)` -> `sys.snapshot`.
fn label_head(label: &str) -> &str {
    label
        .find(['(', '<'])
        .map_or(label, |index| &label[..index])
}

/// Renders a `pub native fn` into the JSON `signature` grammar under the JSON head of its label.
fn render_native(
    label: &str,
    signature: &orna_syntax::FunctionSignature,
    declared: &BTreeSet<String>,
) -> String {
    let parameters: Vec<String> = signature
        .parameters
        .iter()
        .map(|parameter| {
            let name = match &parameter.pattern {
                Pattern::Name(name, _) => name.as_str(),
                _ => "<unrendered-pattern>",
            };
            let annotation = parameter
                .annotation
                .as_ref()
                .map_or_else(|| "<missing>".to_owned(), |ty| render_type(ty, declared));
            match &parameter.default {
                Some(default) => format!("{name}: {annotation} = {}", render_default(default)),
                None => format!("{name}: {annotation}"),
            }
        })
        .collect();
    let result = signature
        .result
        .as_ref()
        .map_or_else(|| "()".to_owned(), |ty| render_type(ty, declared));
    format!(
        "fn {}{}({}): {result}",
        label_head(label),
        render_generics(&signature.generics),
        parameters.join(", ")
    )
}

/// The catalogue class a native effect set stands for: `table_read` is `read`, `storage_admin` is `admin`,
/// `runtime_admin` is `invoke`. This is the only mapping, the same one `gen_interface.py` used.
fn catalogue_class(effects: Option<&NativeEffectSet>) -> String {
    let Some(NativeEffectSet::Classes { classes, .. }) = effects else {
        return "<unrendered-effect>".to_owned();
    };
    classes
        .iter()
        .map(|class| match class.text.as_str() {
            "table_read" => "read",
            "storage_admin" => "admin",
            "runtime_admin" => "invoke",
            _ => "<unknown-effect>",
        })
        .collect::<Vec<_>>()
        .join("|")
}

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

/// The label of a `pub native fn`: its `spelling` clause, or `sys.<name>` without one.
fn native_label(item: &orna_syntax::Item, name: &str) -> String {
    item.clauses
        .spelling
        .as_ref()
        .map_or_else(|| format!("sys.{name}"), |spelling| spelling.text.clone())
}

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

/// The relation handles in `sys/catalog.orna` against the catalogue's `grouped_handle` fields: each `sys.catalog.<name>`
/// must be a native static of type `Relation<T>`, where `T` is the catalogue relation's `name` without its `sys.`
/// prefix. Both sides must name the same 32 handles.
#[test]
fn catalog_handles_match_json_grouped_handles() {
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
            let row = name.strip_prefix("sys.").unwrap_or(name);
            (handle.to_owned(), format!("Relation<{row}>"))
        })
        .filter(|(handle, _)| handle.starts_with("sys.catalog."))
        .collect();
    let parsed = parse_interface(CATALOG_ORNA);
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
        actual.insert(
            format!("sys.catalog.{name}"),
            render_type(annotation, &declared),
        );
    }
    assert_eq!(
        actual, expected,
        "the handles in sys/catalog.orna must match the catalogue's grouped_handle fields"
    );
}
