//! The H3a effect-set comparison (`docs/design/hir.md` section 2.5, D2).
//!
//! "For every public function in every corpus program the old checker accepts
//! and the new one accepts, the effect set the HIR effect pass publishes equals
//! the checker's `EffectSummary` for that function, or the difference is
//! listed." The checker publishes its summary on the exported symbol; the HIR
//! publishes `FnDef.effects` on the same public signature. Both sides are
//! rendered as one canonical string so the comparison is by equality.
//!
//! Accepted differences live in the shared
//! `tests/differential/expected-differences.toml` under the `effects` field,
//! with a reason and the bead that closes it.

use std::collections::BTreeMap;

use orna_check::{EffectKind as CheckerEffect, EffectSummary, SymbolKind, Type};
use orna_hir::{DefKind, EffectKind, EffectSet, Idx};

use crate::case::Case;
use crate::compare::{Difference, difference};
use crate::observe_checker::{analysis, case_namespaces};
use crate::observe_new::LoweredProgram;

/// The canonical text of an effect set: the sorted kind names, then `Opaque`
/// and `MayFail` when present, joined by `+`. The empty set is `pure`.
fn render(mut kinds: Vec<String>, opaque: bool, may_fail: bool) -> String {
    kinds.sort();
    if opaque {
        kinds.push("Opaque".to_owned());
    }
    if may_fail {
        kinds.push("MayFail".to_owned());
    }
    if kinds.is_empty() {
        "pure".to_owned()
    } else {
        kinds.join("+")
    }
}

/// The checker's effect summary as canonical text.
#[must_use]
pub fn checker_text(summary: &EffectSummary) -> String {
    let kinds = summary
        .effects
        .iter()
        .map(|kind: &CheckerEffect| format!("{kind:?}"))
        .collect();
    render(kinds, summary.opaque, summary.may_fail)
}

/// The HIR effect set as canonical text.
#[must_use]
pub fn hir_text(set: EffectSet) -> String {
    let kinds = EffectKind::ALL
        .iter()
        .filter(|kind| set.contains(**kind))
        .map(|kind| format!("{kind:?}"))
        .collect();
    render(kinds, set.is_opaque(), set.may_fail())
}

/// The checker's effect set of every public function the case's own modules
/// export, keyed by `module.path::name`, as the signature oracle keys them.
pub fn checker_effects(case: &Case) -> Result<BTreeMap<String, String>, String> {
    let analysis = analysis(case)?;
    let namespaces = case_namespaces(case);
    let mut effects = BTreeMap::new();
    for (namespace, header) in &analysis.modules {
        if !namespaces.contains(&namespace.display()) {
            continue;
        }
        for (name, symbol) in &header.exports {
            if symbol.kind != SymbolKind::Function || !matches!(symbol.ty, Type::Function { .. }) {
                continue;
            }
            effects.insert(
                format!("{}::{name}", namespace.display()),
                checker_text(&symbol.effects),
            );
        }
    }
    Ok(effects)
}

/// The HIR effect set of every public function of the case's own modules,
/// keyed as [`checker_effects`] keys them.
#[must_use]
pub fn hir_effects(lowered: &LoweredProgram) -> BTreeMap<String, String> {
    let mut effects = BTreeMap::new();
    for (module_id, module) in lowered.program.modules.iter_enumerated() {
        if !lowered
            .owned
            .get(module_id.index())
            .copied()
            .unwrap_or(false)
        {
            continue;
        }
        for def in &module.items {
            let definition = &lowered.program.defs[*def];
            if definition.vis != orna_hir::Visibility::Public {
                continue;
            }
            let DefKind::Fn(function) = &definition.kind else {
                continue;
            };
            effects.insert(
                format!("{}::{}", module.path, definition.name),
                hir_text(function.effects),
            );
        }
    }
    effects
}

/// The differences between the checker's and the HIR's effect sets for one
/// case. A key only the checker publishes is a difference; a key only the HIR
/// publishes is counted by the caller as unpublished, not as a difference.
#[must_use]
pub fn compare_effects(
    case_id: &str,
    old: &BTreeMap<String, String>,
    new: &BTreeMap<String, String>,
) -> Vec<Difference> {
    let mut differences = Vec::new();
    for (key, old_text) in old {
        match new.get(key) {
            Some(new_text) if new_text == old_text => {}
            Some(new_text) => differences.push(difference(
                case_id,
                "effects",
                format!("{key}={old_text}"),
                format!("{key}={new_text}"),
            )),
            None => differences.push(difference(
                case_id,
                "effects",
                format!("{key}={old_text}"),
                format!("{key}=no HIR effect set"),
            )),
        }
    }
    differences
}
