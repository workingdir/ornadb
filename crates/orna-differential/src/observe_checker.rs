//! The old checker's resolution and public-signature oracle for stage H1/H2.
//!
//! The comparison H1 asks for is "for every name in every corpus program, the
//! HIR resolver and the old checker agree on resolved or unresolved, and on
//! `ORNA-S011` ambiguity". The checker's only per-name answer that is public is
//! its diagnostics: an unresolved name carries the spelling in a
//! `unresolved name: NAME` note, and an ambiguity is counted. Neither carries a
//! source span, so the comparison is by name and by count, not by span.
//!
//! Stage H2 compares the printed type of every public signature. The checker's
//! answer is its exported symbol table: a public function is exported with its
//! written parameter and result types, and a signature whose types contain a
//! diagnostic (`Type::Error`) is not exported at all. This module spells those
//! types exactly the way [`orna_hir::print_type`] does, so the two pipelines'
//! answers are directly comparable; see [`crate::compare::compare_signatures`].
//!
//! The checker analyses the case's own sources, exactly as the old pipeline's
//! admission does; the standard library reaches it through the catalogue, so
//! standard source is not re-analysed here and is not part of this comparison.

use std::collections::{BTreeMap, BTreeSet};

use orna_check::{
    Analysis, DIAG_AMBIGUOUS, DIAG_UNRESOLVED, ModuleInput, SymbolKind, Type,
    analyze_with_catalogue,
};

use crate::case::{Case, CaseKind};
use crate::observe_new::ResolutionObservation;
use crate::observe_old::catalogue_for;

/// Runs the checker's module analysis for one case.
pub fn analysis(case: &Case) -> Result<Analysis, String> {
    if case.kind == CaseKind::Repl {
        return Err(format!(
            "{}: checker analysis is a module analysis; the REPL path is stage H8",
            case.id
        ));
    }
    let inputs = case
        .sources
        .iter()
        .map(|source| ModuleInput {
            logical_path: source.logical_path.clone(),
            // An expression fixture is not a module; the checker analyses
            // modules, so the expression is wrapped without moving its names.
            source: match case.kind {
                CaseKind::Expression => format!("fn run() = {};\n", source.text),
                _ => source.text.clone(),
            },
            prelude_exports: BTreeSet::new(),
        })
        .collect::<Vec<_>>();
    let catalogue = catalogue_for(case)?;
    Ok(analyze_with_catalogue(&inputs, catalogue))
}

/// Whether the old checker accepts the case.
pub fn accepts(case: &Case) -> Result<bool, String> {
    Ok(analysis(case)?.is_ok())
}

/// The checker's unresolved and ambiguous answers for one case.
pub fn resolution(case: &Case) -> Result<ResolutionObservation, String> {
    let analysis = analysis(case)?;
    let mut observation = ResolutionObservation::default();
    for diagnostic in &analysis.diagnostics {
        match diagnostic.code() {
            DIAG_UNRESOLVED => match orna_check::unresolved_name(diagnostic) {
                Some(name) => {
                    *observation.unresolved.entry(name.to_owned()).or_insert(0) += 1;
                }
                None => observation.unresolved_without_name += 1,
            },
            DIAG_AMBIGUOUS => observation.ambiguous += 1,
            _ => {}
        }
    }
    Ok(observation)
}

/// The printed type of every public signature the checker exports, keyed by
/// `module.path::name`.
pub fn signatures(case: &Case) -> Result<BTreeMap<String, String>, String> {
    let analysis = analysis(case)?;
    let namespaces = case_namespaces(case);
    let mut signatures = BTreeMap::new();
    for (namespace, header) in &analysis.modules {
        if !namespaces.contains(&namespace.display()) {
            // The catalogue's standard modules are not the case's own source.
            continue;
        }
        for (name, symbol) in &header.exports {
            if symbol.kind != SymbolKind::Function {
                continue;
            }
            let Type::Function {
                parameters, result, ..
            } = &symbol.ty
            else {
                continue;
            };
            let parameters = parameters
                .iter()
                .map(checker_type)
                .collect::<Vec<_>>()
                .join(", ");
            let key = format!("{}::{name}", namespace.display());
            signatures.insert(
                key,
                format!("{name}({parameters}) -> {}", checker_type(result)),
            );
        }
    }
    Ok(signatures)
}

/// The module namespaces a case's own sources declare.
///
/// This mirrors `orna_check`'s `namespace_for_path`: the directory segments plus
/// the file stem, except that `main.orna` names its directory.
fn case_namespaces(case: &Case) -> BTreeSet<String> {
    case.sources
        .iter()
        .map(|source| {
            let trimmed = source
                .logical_path
                .strip_suffix(".orna")
                .unwrap_or(&source.logical_path);
            let mut parts = trimmed
                .split('/')
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            if parts.last() == Some(&"main") {
                parts.pop();
            }
            parts.join(".")
        })
        .collect()
}

/// Spells one checker type the way [`orna_hir::print_type`] does.
///
/// A nominal type is written with its declared name only: the checker records
/// the module-qualified identity, and the HIR `TyKind::Adt` records the
/// definition, so the last path segment compares the two identities' printed
/// names without pretending they are the same representation.
#[must_use]
pub fn checker_type(ty: &Type) -> String {
    match ty {
        Type::Bool => "Bool".to_owned(),
        Type::Int => "Int".to_owned(),
        Type::Decimal => "Decimal".to_owned(),
        Type::Float => "Float".to_owned(),
        Type::Text => "Str".to_owned(),
        Type::Date => "Date".to_owned(),
        Type::Instant => "Instant".to_owned(),
        Type::Null => "Null".to_owned(),
        Type::Unit => "Unit".to_owned(),
        Type::List(element) => format!("[{}]", checker_type(element)),
        Type::Range(element) => format!("Range<{}>", checker_type(element)),
        Type::Relation(element) => format!("Relation<{}>", checker_type(element)),
        Type::Stream(element) => format!("Stream<{}>", checker_type(element)),
        Type::Optional(inner) => format!("{}?", checker_type(inner)),
        Type::Tuple(items) => format!(
            "({})",
            items
                .iter()
                .map(checker_type)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Type::Record(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(name, field)| format!("{name}: {}", checker_type(field)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Type::Applied { base, arguments } => {
            if arguments.is_empty() {
                base.clone()
            } else {
                format!(
                    "{base}<{}>",
                    arguments
                        .iter()
                        .map(checker_type)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        Type::Quantity { .. } => "Quantity".to_owned(),
        Type::MoneyPerUnit { .. } => "MoneyPerUnit".to_owned(),
        Type::Function {
            parameters, result, ..
        } => format!(
            "({}) -> {}",
            parameters
                .iter()
                .map(checker_type)
                .collect::<Vec<_>>()
                .join(", "),
            checker_type(result)
        ),
        Type::Named(name) => {
            let printed = name.rsplit('.').next().unwrap_or(name);
            // The checker spells the engine scalars by their catalogue
            // identities; the HIR spells them as `TyKind` scalars. This is a
            // spelling difference, not a type difference.
            match printed {
                "DURATION" => "Duration".to_owned(),
                "UUID" | "Uuid" => "Uuid".to_owned(),
                other => other.to_owned(),
            }
        }
        Type::Bottom => "Never".to_owned(),
        Type::Error => "Error".to_owned(),
    }
}
