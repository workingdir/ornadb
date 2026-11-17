//! Allocation-free check for whether canonicalization would change a module.
//!
//! `units::canonicalize_symbol` rewrites exactly the types it reaches: the
//! symbol type, every table field type, every admission key type and every
//! conversion source type. The walks here visit the same positions by
//! reference, so deciding that a module is unchanged never clones it.

use std::collections::BTreeMap;

use crate::{ModuleHeader, Symbol, Type};

/// Whether canonicalizing `module` would change one of its symbols. A module is
/// written only when this holds, so an unchanged module is never copied out of
/// the map that scopes share.
pub(crate) fn module_needs_canonicalization(
    module: &ModuleHeader,
    identities: &BTreeMap<String, String>,
) -> bool {
    module
        .symbols
        .values()
        .chain(module.exports.values())
        .any(|symbol| symbol_needs_canonicalization(symbol, identities))
}

/// Whether `canonicalize_symbol` would change `symbol`.
fn symbol_needs_canonicalization(symbol: &Symbol, identities: &BTreeMap<String, String>) -> bool {
    type_needs_canonicalization(&symbol.ty, identities)
        || symbol.table_schema.as_ref().is_some_and(|schema| {
            schema
                .fields
                .values()
                .any(|field| type_needs_canonicalization(field, identities))
                || schema.admission.as_ref().is_some_and(|admission| {
                    admission
                        .keys
                        .iter()
                        .any(|(_, key)| type_needs_canonicalization(key, identities))
                })
                || schema
                    .conversions
                    .iter()
                    .any(|(source, _)| type_needs_canonicalization(source, identities))
        })
}

/// Whether `canonicalize_type_with_identities` would change `ty`. The walk
/// mirrors its recursion; `Applied::base`, quantity dimensions and function
/// parameter names are copied verbatim there, so they never count here.
fn type_needs_canonicalization(ty: &Type, identities: &BTreeMap<String, String>) -> bool {
    match ty {
        Type::Named(name) => identities
            .get(name)
            .is_some_and(|identity| identity != name),
        Type::List(inner)
        | Type::Range(inner)
        | Type::Relation(inner)
        | Type::Stream(inner)
        | Type::Optional(inner)
        | Type::Quantity { numeric: inner, .. } => type_needs_canonicalization(inner, identities),
        Type::Record(fields) => fields
            .values()
            .any(|field| type_needs_canonicalization(field, identities)),
        Type::Tuple(elements) => elements
            .iter()
            .any(|element| type_needs_canonicalization(element, identities)),
        Type::Applied { arguments, .. } => arguments
            .iter()
            .any(|argument| type_needs_canonicalization(argument, identities)),
        Type::MoneyPerUnit { currency, unit } => {
            type_needs_canonicalization(currency, identities)
                || type_needs_canonicalization(unit, identities)
        }
        Type::Function {
            parameters, result, ..
        } => {
            parameters
                .iter()
                .any(|parameter| type_needs_canonicalization(parameter, identities))
                || type_needs_canonicalization(result, identities)
        }
        Type::Int
        | Type::Decimal
        | Type::Float
        | Type::Date
        | Type::Instant
        | Type::Text
        | Type::Bool
        | Type::Null
        | Type::Unit
        | Type::Bottom
        | Type::Error => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{module_needs_canonicalization, symbol_needs_canonicalization};
    use crate::units::canonicalize_symbol;
    use crate::{
        EffectSummary, Namespace, SourceUnitRational, Symbol, SymbolKind, TableAdmission,
        TableSchema, Type, fixture_module, fixture_symbol,
    };

    fn identities() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("Customer".to_string(), "app.Customer".to_string()),
            ("Self".to_string(), "Self".to_string()),
        ])
    }

    fn named(name: &str) -> Type {
        Type::Named(name.to_string())
    }

    fn rational() -> SourceUnitRational {
        SourceUnitRational {
            numerator: "1".to_string(),
            denominator: "1".to_string(),
        }
    }

    fn schema(fields: &[(&str, Type)], keys: &[(&str, Type)], conversions: &[Type]) -> TableSchema {
        TableSchema {
            fields: fields
                .iter()
                .map(|(name, ty)| (name.to_string(), ty.clone()))
                .collect(),
            references: Vec::new(),
            declaration_order: fields.iter().map(|(name, _)| name.to_string()).collect(),
            admission: Some(TableAdmission {
                required: BTreeSet::new(),
                private_required: false,
                computed: BTreeSet::new(),
                keys: keys
                    .iter()
                    .map(|(name, ty)| (name.to_string(), ty.clone()))
                    .collect(),
                automatic_key: false,
            }),
            conversions: conversions
                .iter()
                .map(|source| (source.clone(), EffectSummary::default()))
                .collect(),
            currency: false,
        }
    }

    fn table_symbol(schema: TableSchema) -> Symbol {
        let mut symbol = fixture_symbol(SymbolKind::Table, named("Row"));
        symbol.table_schema = Some(schema);
        symbol
    }

    /// One case per position canonicalization reaches, plus the positions it
    /// must leave alone. The expectation is checked against the real rewrite.
    fn cases() -> Vec<(&'static str, Symbol, bool)> {
        let plain = |ty| fixture_symbol(SymbolKind::Let, ty);
        let mut without_admission = schema(&[("owner", named("Customer"))], &[], &[]);
        without_admission.admission = None;
        let mut declaration_names = schema(&[("id", Type::Int)], &[], &[]);
        declaration_names.declaration_order = vec!["Customer".to_string()];
        vec![
            ("bare name", plain(named("Customer")), true),
            ("self-mapped name", plain(named("Self")), false),
            ("unmapped name", plain(named("Unmapped")), false),
            (
                "nested self-map",
                plain(Type::List(Box::new(named("Self")))),
                false,
            ),
            (
                "list element",
                plain(Type::List(Box::new(named("Customer")))),
                true,
            ),
            (
                "record field",
                plain(Type::Record(BTreeMap::from([(
                    "owner".to_string(),
                    named("Customer"),
                )]))),
                true,
            ),
            (
                "tuple element",
                plain(Type::Tuple(vec![Type::Int, named("Customer")])),
                true,
            ),
            (
                "applied base is verbatim",
                plain(Type::Applied {
                    base: "Customer".to_string(),
                    arguments: vec![Type::Int],
                }),
                false,
            ),
            (
                "applied argument",
                plain(Type::Applied {
                    base: "Box".to_string(),
                    arguments: vec![named("Customer")],
                }),
                true,
            ),
            (
                "quantity numeric",
                plain(Type::Quantity {
                    numeric: Box::new(named("Customer")),
                    dimension_basis: BTreeMap::new(),
                    scale: rational(),
                    offset: rational(),
                    affine: false,
                }),
                true,
            ),
            (
                "money unit",
                plain(Type::MoneyPerUnit {
                    currency: Box::new(Type::Int),
                    unit: Box::new(named("Customer")),
                }),
                true,
            ),
            (
                "function parameter name is verbatim",
                plain(Type::Function {
                    parameters: vec![Type::Int],
                    parameter_names: Some(vec!["Customer".to_string()]),
                    default_parameters: BTreeSet::new(),
                    result: Box::new(Type::Int),
                }),
                false,
            ),
            (
                "function result",
                plain(Type::Function {
                    parameters: vec![Type::Int],
                    parameter_names: None,
                    default_parameters: BTreeSet::new(),
                    result: Box::new(named("Customer")),
                }),
                true,
            ),
            (
                "table field",
                table_symbol(schema(&[("owner", named("Customer"))], &[], &[])),
                true,
            ),
            (
                "table admission key",
                table_symbol(schema(
                    &[("id", Type::Int)],
                    &[("owner", named("Customer"))],
                    &[],
                )),
                true,
            ),
            (
                "table conversion source",
                table_symbol(schema(&[("id", Type::Int)], &[], &[named("Customer")])),
                true,
            ),
            (
                "table field without admission",
                table_symbol(without_admission),
                true,
            ),
            (
                "table declaration order is verbatim",
                table_symbol(declaration_names),
                false,
            ),
        ]
    }

    #[test]
    fn predicate_agrees_with_the_rewrite_for_every_position() {
        let identities = identities();
        for (label, symbol, expected) in cases() {
            let mut rewritten = symbol.clone();
            canonicalize_symbol(&mut rewritten, &identities);
            assert_eq!(rewritten != symbol, expected, "rewrite changes {label}");
            assert_eq!(
                symbol_needs_canonicalization(&symbol, &identities),
                expected,
                "predicate for {label}"
            );
        }
    }

    #[test]
    fn module_check_reads_exports_as_well_as_symbols() {
        let identities = identities();
        let mut module = fixture_module(
            Namespace(vec!["app".to_string()]),
            [(
                "plain".to_string(),
                fixture_symbol(SymbolKind::Let, Type::Int),
            )],
            false,
        );
        assert!(!module_needs_canonicalization(&module, &identities));
        module.exports.insert(
            "hidden".to_string(),
            fixture_symbol(SymbolKind::Let, named("Customer")),
        );
        assert!(module_needs_canonicalization(&module, &identities));
    }
}
