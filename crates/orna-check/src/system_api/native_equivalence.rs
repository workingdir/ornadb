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

/// Native callables from `sys.orna`, keyed by label: the `spelling` clause, or `sys.<name>` without one.
/// Each value is the rendered signature followed by the catalogue class.
fn native_entries() -> BTreeMap<String, String> {
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
            let label = item.clauses.spelling.as_ref().map_or_else(
                || format!("sys.{}", signature.name),
                |spelling| spelling.text.clone(),
            );
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
    let catalogue = catalogue_entries();
    let natives = native_entries();
    let mut differences = Vec::new();
    let labels: BTreeSet<&String> = catalogue.keys().chain(natives.keys()).collect();
    for label in labels {
        let json_side = catalogue.get(label);
        let native_side = natives.get(label);
        if json_side != native_side {
            differences.push(format!(
                "{label}:\n  json   = {json_side:?}\n  native = {native_side:?}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} callable(s) differ between sys.orna and the JSON catalogue:\n{}",
        differences.len(),
        differences.join("\n")
    );
}
