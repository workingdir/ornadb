//! Plain-text renderings of the explain values. The text is a presentation; the values are the result.

use orna_hir::{DefId, EffectKind, EffectSet, Program};

use super::{Callback, FunctionExplain, Operator, OperatorKind};

/// The operator tree, one operator per line, the root first and each input indented under the operator it feeds.
#[must_use]
pub fn render_operator(program: &Program, root: &Operator) -> String {
    let mut out = String::new();
    push_operator(program, root, 0, &mut out);
    out
}

/// The function explain: its signature, its effects, and the tables it reaches.
#[must_use]
pub fn render_function(program: &Program, explain: &FunctionExplain) -> String {
    let tables: Vec<&str> = explain
        .tables
        .iter()
        .map(|table| name(program, *table))
        .collect();
    format!(
        "{}\neffects: {}\ntables: {}\n",
        explain.signature,
        effects_text(explain.effects),
        if tables.is_empty() {
            "none".to_owned()
        } else {
            tables.join(", ")
        }
    )
}

fn push_operator(program: &Program, operator: &Operator, depth: usize, out: &mut String) {
    out.push_str(&"  ".repeat(depth));
    out.push_str(&label(program, operator));
    out.push_str(" effects=");
    out.push_str(&effects_text(operator.effects));
    out.push('\n');
    for input in &operator.inputs {
        push_operator(program, input, depth + 1, out);
    }
}

fn label(program: &Program, operator: &Operator) -> String {
    match operator.kind {
        OperatorKind::Scan => format!(
            "scan {}",
            operator.table.map_or("?", |table| name(program, table))
        ),
        OperatorKind::Filter => format!("filter {}", callback_text(program, operator.callback)),
        OperatorKind::Map => format!("map {}", callback_text(program, operator.callback)),
        OperatorKind::Take => "take".to_owned(),
        OperatorKind::Drop => "drop".to_owned(),
        OperatorKind::External => "external".to_owned(),
    }
}

fn callback_text(program: &Program, callback: Option<Callback>) -> String {
    match callback {
        Some(Callback::Def(def)) => name(program, def).to_owned(),
        Some(Callback::Lambda(_)) => "lambda".to_owned(),
        Some(Callback::Dynamic) | None => "dynamic".to_owned(),
    }
}

fn name(program: &Program, def: DefId) -> &str {
    program.defs[def].name.as_str()
}

/// The effects of a set, by their class names. An opaque set has no known kinds, so it reads as `opaque`.
fn effects_text(effects: EffectSet) -> String {
    if effects.is_opaque() {
        return "opaque".to_owned();
    }
    let kinds: Vec<&str> = effects.kinds().map(effect_class).collect();
    if kinds.is_empty() {
        "pure".to_owned()
    } else {
        kinds.join(",")
    }
}

const fn effect_class(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::TableRead => "table_read",
        EffectKind::TableWrite => "table_write",
        EffectKind::Network => "network",
        EffectKind::Filesystem => "filesystem",
        EffectKind::Process => "process",
        EffectKind::Clock => "clock",
        EffectKind::Randomness => "randomness",
        EffectKind::SecretAccess => "secret_access",
        EffectKind::Ui => "ui",
        EffectKind::Git => "git",
        EffectKind::StorageAdmin => "storage_admin",
        EffectKind::RuntimeAdmin => "runtime_admin",
    }
}
