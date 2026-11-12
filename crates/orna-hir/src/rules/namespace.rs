//! Namespace rules for source modules: reserved roots (S003), Unicode-folded
//! sibling collisions and duplicate module paths (S002), and reserved table
//! names (E1008, E1009).

use std::collections::{BTreeMap, HashSet};

use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

use super::Found;
use crate::{DefKind, HirDiagnostic, ModuleId, ModuleOrigin, Program, diag};
use orna_syntax_ast::SourceSpan as SyntaxSpan;

/// Per-parent map from a folded sibling key to the first spelling seen.
type Siblings = BTreeMap<Vec<String>, BTreeMap<String, String>>;

/// Checks every source module's namespace and reserved table names.
pub(super) fn check(program: &Program) -> Vec<Found> {
    let mut found = Vec::new();
    let mut siblings = Siblings::new();
    let mut seen: HashSet<Vec<String>> = HashSet::new();
    for (id, module) in program.modules.iter_enumerated() {
        if module.origin != ModuleOrigin::Project {
            continue;
        }
        let segments: Vec<String> = module
            .path
            .segments()
            .iter()
            .map(|n| n.as_str().to_owned())
            .collect();
        let span = module.items.first().map_or_else(
            || SyntaxSpan::new(0, 0),
            |item| program.defs[*item].span.clone(),
        );
        if matches!(segments.first().map(String::as_str), Some("sys" | "std")) {
            let message = "reserved namespace cannot be defined by source";
            found.push((id, HirDiagnostic::new(diag::RESERVED, message, span)));
            continue;
        }
        register_siblings(&segments, &mut siblings, id, &span, &mut found);
        if !seen.insert(segments) {
            let message = "duplicate module namespace";
            found.push((id, HirDiagnostic::new(diag::NAMESPACE, message, span)));
            continue;
        }
        check_table_names(program, id, &mut found);
    }
    found
}

/// Records each prefix segment under its parent and reports a sibling pair
/// whose Unicode-folded keys collide while the spellings differ.
fn register_siblings(
    segments: &[String],
    siblings: &mut Siblings,
    id: ModuleId,
    span: &SyntaxSpan,
    found: &mut Vec<Found>,
) {
    for (index, child) in segments.iter().enumerate() {
        let entry = siblings.entry(segments[..index].to_vec()).or_default();
        if let Some(prior) = entry.insert(sibling_key(child), child.clone())
            && prior != *child
        {
            let message = "Unicode-folded sibling namespace collision";
            found.push((
                id,
                HirDiagnostic::new(diag::NAMESPACE, message, span.clone()),
            ));
        }
    }
}

/// The key two namespace segments share when they are the same namespace
/// once compatibility-normalised and case-folded.
fn sibling_key(value: &str) -> String {
    value.nfkc().case_fold().collect()
}

/// Reports every table whose name is one of the reserved roots.
fn check_table_names(program: &Program, id: ModuleId, found: &mut Vec<Found>) {
    for item in &program.modules[id].items {
        let def = &program.defs[*item];
        if !matches!(def.kind, DefKind::Table(_)) {
            continue;
        }
        match def.name.as_str() {
            "std" => found.push((
                id,
                HirDiagnostic::new(diag::RESERVED_STD, "`std` is reserved", def.span.clone()),
            )),
            "sys" => found.push((
                id,
                HirDiagnostic::new(diag::RESERVED_SYS, "`sys` is reserved", def.span.clone()),
            )),
            _ => {}
        }
    }
}
