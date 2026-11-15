//! Allocation-free probes for whether canonicalization rewrites a nominal name.
//!
//! Each probe mirrors the recursion of `canonicalize_type_with_identities` and
//! `canonicalize_symbol` without building the rewritten value, so an unaffected
//! module is tested in place and never copied out of the map that scopes share.

use std::collections::BTreeMap;

use crate::{Symbol, Type};

/// Whether canonicalizing `ty` rewrites a `Named` reference to another identity.
pub(crate) fn type_needs_canonicalization(
    ty: &Type,
    identities: &BTreeMap<String, String>,
) -> bool {
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
            .iter()
            .any(|(_, field)| type_needs_canonicalization(field, identities)),
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

/// Whether canonicalizing `symbol` would rewrite its type or any table-schema type.
pub(crate) fn symbol_needs_canonicalization(
    symbol: &Symbol,
    identities: &BTreeMap<String, String>,
) -> bool {
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
                        .any(|(_, field)| type_needs_canonicalization(field, identities))
                })
                || schema
                    .conversions
                    .iter()
                    .any(|(source, _)| type_needs_canonicalization(source, identities))
        })
}
